//! The Hydra adapter: hydra-synth (AGPL-3.0, installed separately) as the
//! generative engine's shader compiler, behind [`VisualBackend`].
//!
//! Hydra runs in its own Node process (`adapters/hydra/main.mjs`, MIT,
//! part of Patch) under Node's permission model: it reads its own
//! directory and nothing else. Patch sends a scene, gets back GLSL ES 1.0
//! and the uniform ↔ signal mapping, and shims it to its own GLSL. Hydra
//! never renders, times or sees audio.
//!
//! Where it's looked for: `$PATCH_HYDRA_DIR`, then
//! `~/.local/share/omarchy-patch/adapters/hydra`, then the source tree's
//! `adapters/hydra`. Node: `$PATCH_NODE`, else `node`.

use crate::generative::{Capabilities, CompiledShader, Param, VisualBackend, VisualScene, shim};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

pub struct Hydra {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    caps: Capabilities,
    pub timeout: Duration,
}

pub fn locate() -> Option<PathBuf> {
    let ready = |d: &Path| d.join("main.mjs").is_file() && d.join("node_modules/hydra-synth").is_dir();
    let mut tries = Vec::new();
    if let Some(d) = std::env::var_os("PATCH_HYDRA_DIR") {
        tries.push(PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        tries.push(h.join("omarchy-patch/adapters/hydra"));
    }
    tries.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/hydra"));
    tries.into_iter().find(|d| ready(d)).map(|d| d.canonicalize().unwrap_or(d))
}

impl Hydra {
    pub fn start(dir: &Path) -> Result<Hydra, String> {
        let node = std::env::var_os("PATCH_NODE").unwrap_or_else(|| "node".into());
        let mut child = Command::new(node)
            .arg("--permission")
            .arg(format!("--allow-fs-read={}", dir.display()))
            .arg("main.mjs")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("can't start node for Hydra: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("patch-hydra-read".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let mut h = Hydra {
            child,
            stdin,
            rx,
            next_id: 0,
            caps: Capabilities::default(),
            timeout: Duration::from_secs(5),
        };
        let hello = h.call(json!({"op": "hello"}))?;
        // Every op of Patch's vocabulary must exist in this Hydra.
        for (name, ..) in crate::generative::OPS {
            if hello["ops"].get(*name).is_none() {
                return Err(format!("this hydra-synth has no '{name}'"));
            }
        }
        h.caps = Capabilities {
            engine: "hydra".into(),
            version: hello["version"].as_str().unwrap_or("").into(),
            feedback: hello["feedback"].as_bool().unwrap_or(false),
            blur: false,
            out_of_process: true,
            sandboxed: hello["sandboxed"].as_bool().unwrap_or(false),
        };
        Ok(h)
    }

    fn call(&mut self, mut req: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        req["id"] = json!(id);
        writeln!(self.stdin, "{req}").map_err(|e| format!("Hydra stopped: {e}"))?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        loop {
            let line = self.rx.recv_timeout(self.timeout).map_err(|_| "Hydra didn't answer".to_string())?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] != json!(id) {
                continue;
            }
            if let Some(e) = v["error"].as_str() {
                return Err(e.to_string());
            }
            return Ok(v["ok"].clone());
        }
    }
}

/// The scene with its signal arguments numbered: Hydra sees `{"signal":
/// "s3"}`, and the reply maps uniforms back to those numbers.
fn numbered(scene: &VisualScene) -> (Value, Vec<Param>) {
    let mut signals = vec![];
    let nodes: Vec<Value> = scene
        .nodes
        .iter()
        .map(|n| {
            let args: Vec<Value> = n
                .args
                .iter()
                .map(|p| match p {
                    Param::Const { value } => json!({"const": value}),
                    s @ Param::Signal { .. } => {
                        signals.push(s.clone());
                        json!({"signal": format!("s{}", signals.len() - 1)})
                    }
                })
                .collect();
            json!({"op": n.op, "args": args, "inputs": n.inputs})
        })
        .collect();
    (json!({"nodes": nodes, "output": scene.output}), signals)
}

impl VisualBackend for Hydra {
    fn compile(&mut self, scene: &VisualScene) -> Result<CompiledShader, String> {
        scene.validate()?;
        let (wire, signals) = numbered(scene);
        let r = self.call(json!({"op": "compile", "scene": wire}))?;
        let frag = r["frag"].as_str().ok_or("Hydra returned no shader")?;
        let mut uniforms = vec![];
        for u in r["uniforms"].as_array().into_iter().flatten() {
            let name = u["name"].as_str().ok_or("a uniform without a name")?;
            let idx: usize = u["signal"]
                .as_str()
                .and_then(|s| s.strip_prefix('s'))
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("uniform {name} maps to no signal"))?;
            let p = signals.get(idx).ok_or_else(|| format!("uniform {name}: no signal {idx}"))?;
            uniforms.push((name.to_string(), p.clone()));
        }
        Ok(CompiledShader {
            // Hydra declares `prevBuffer` in every shader: the scene says
            // whether it reads it.
            feedback: scene.nodes.iter().any(|n| n.op == "prev"),
            frag: shim(frag)?,
            uniforms,
            compile_us: r["compile_us"].as_f64().unwrap_or(0.0),
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }
}

impl Drop for Hydra {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the adapter installed (`cd adapters/hydra && npm install`):
    /// every preset compiles, each signal argument gets its uniform, and
    /// the shimmed shader is valid GLSL 3.30 (glslangValidator, when
    /// present). Skipped cleanly without them.
    #[test]
    fn presets_compile_through_hydra_into_patch_glsl() {
        let Some(dir) = locate() else {
            eprintln!("Hydra adapter not installed: skipped");
            return;
        };
        let mut h = match Hydra::start(&dir) {
            Ok(h) => h,
            Err(e) if e.contains("can't start node") => {
                eprintln!("{e}: skipped");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        assert_eq!(h.capabilities().engine, "hydra");
        assert!(h.capabilities().sandboxed, "runs under Node's permission model");
        let validator = Command::new("glslangValidator").arg("--version").output().is_ok();
        for (name, scene) in crate::generative::presets() {
            let c = h.compile(&scene).unwrap_or_else(|e| panic!("{name}: {e}"));
            let signals = scene.nodes.iter().flat_map(|n| &n.args).filter(|p| matches!(p, Param::Signal { .. })).count();
            assert_eq!(c.uniforms.len(), signals, "{name}: one uniform per signal");
            assert_eq!(c.feedback, scene.nodes.iter().any(|n| n.op == "prev"), "{name}");
            if validator {
                let file = std::env::temp_dir().join(format!("patch-hydra-{}-{name}.frag", std::process::id()));
                std::fs::write(&file, format!("#version 330 core\n{}", c.frag)).unwrap();
                let r = Command::new("glslangValidator").arg(&file).output().unwrap();
                let _ = std::fs::remove_file(&file);
                assert!(r.status.success(), "{name}: {}", String::from_utf8_lossy(&r.stdout));
            }
        }
        // A scene Patch refuses never reaches Hydra.
        let bad = VisualScene { nodes: vec![], output: 0 };
        assert!(h.compile(&bad).is_err());
    }
}
