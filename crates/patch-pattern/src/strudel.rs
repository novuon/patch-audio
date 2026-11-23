//! The Strudel adapter: Strudel (AGPL-3.0-or-later, installed separately)
//! as a pattern engine behind Patch's [`Backend`] contract.
//!
//! Strudel runs in its own Node process (`adapters/strudel/main.mjs`, MIT,
//! part of Patch), started under Node's permission model: it may read its
//! own directory and nothing else; no processes, no network, no writes.
//! Patch speaks one JSON object per line on its stdin and reads one reply
//! per line from its stdout. The process never schedules anything: Patch
//! asks for the events of a span and does the rest.
//!
//! Where it's looked for: `$PATCH_STRUDEL_DIR`, then
//! `~/.local/share/omarchy-patch/adapters/strudel`, then the source tree's
//! `adapters/strudel` (development). Node: `$PATCH_NODE`, else `node`.

use crate::backend::{Backend, Capabilities, Definition, LayerInfo, Loaded, PatternError};
use crate::event::Event;
use crate::time::Span;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

pub struct Strudel {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    caps: Capabilities,
    /// What Strudel produced that Patch has no target for, by control
    /// names (a finding, reported, not an error).
    pub unmapped: std::collections::BTreeMap<String, u64>,
    /// Translation notes from the last load (where Patch semantics differ).
    pub notes: Vec<String>,
    pub eval_timeout: Duration,
    pub query_timeout: Duration,
}

/// Where the adapter is installed, if anywhere.
pub fn locate() -> Option<PathBuf> {
    let ready =
        |d: &Path| d.join("main.mjs").is_file() && d.join("node_modules/@strudel/core").is_dir();
    let mut tries = Vec::new();
    if let Some(d) = std::env::var_os("PATCH_STRUDEL_DIR") {
        tries.push(PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        tries.push(h.join("omarchy-patch/adapters/strudel"));
    }
    tries.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/strudel"));
    tries
        .into_iter()
        .find(|d| ready(d))
        .map(|d| d.canonicalize().unwrap_or(d))
}

impl Strudel {
    /// Start the sidecar in `dir` and greet it.
    pub fn start(dir: &Path) -> Result<Strudel, String> {
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
            .map_err(|e| format!("can't start node for Strudel: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("patch-strudel-read".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let mut s = Strudel {
            child,
            stdin,
            rx,
            next_id: 1,
            caps: Capabilities::default(),
            unmapped: Default::default(),
            notes: vec![],
            // Node and Strudel load in ~0.3 s; a first evaluation can take longer.
            eval_timeout: Duration::from_secs(5),
            query_timeout: Duration::from_millis(50),
        };
        let hello = s.call(json!({"op": "hello"}), Duration::from_secs(10))?;
        s.caps =
            serde_json::from_value(hello["capabilities"].clone()).map_err(|e| e.to_string())?;
        Ok(s)
    }

    fn call(&mut self, mut req: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = json!(id);
        let mut line = req.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("Strudel stopped: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(l) => {
                    // A late reply to an earlier request, or a stray line.
                    let Ok(v) = serde_json::from_str::<Value>(&l) else {
                        continue;
                    };
                    if v["id"] == json!(id) {
                        return Ok(v);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("Strudel took over {} ms", timeout.as_millis()));
                }
                Err(RecvTimeoutError::Disconnected) => return Err("Strudel stopped".into()),
            }
        }
    }

    /// The process is still there.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Strudel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Backend for Strudel {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn load(&mut self, slot: u32, def: &Definition) -> Result<Loaded, Vec<PatternError>> {
        let r = self
            .call(
                json!({"op": "load", "slot": slot, "definition": def}),
                self.eval_timeout,
            )
            .map_err(|e| vec![PatternError::new(e)])?;
        if r["ok"] != true {
            let errs: Vec<PatternError> =
                serde_json::from_value(r["errors"].clone()).unwrap_or_default();
            return Err(if errs.is_empty() {
                vec![PatternError::new(
                    r["error"].as_str().unwrap_or("Strudel refused it"),
                )]
            } else {
                errs
            });
        }
        self.notes = serde_json::from_value(r["notes"].clone()).unwrap_or_default();
        Ok(Loaded {
            layers: serde_json::from_value::<Vec<LayerInfo>>(r["layers"].clone())
                .unwrap_or_default(),
            eval_us: r["eval_us"].as_u64().unwrap_or(0),
            ..Default::default()
        })
    }

    fn query(&mut self, slot: u32, span: Span) -> Result<Vec<Event>, PatternError> {
        let r = self
            .call(
                json!({"op": "query", "slot": slot, "begin": span.begin.to_string(), "end": span.end.to_string()}),
                self.query_timeout,
            )
            .map_err(PatternError::new)?;
        if r["ok"] != true {
            return Err(PatternError::new(
                r["error"].as_str().unwrap_or("query failed"),
            ));
        }
        if let Some(u) = r["unmapped"].as_object() {
            for (k, n) in u {
                *self.unmapped.entry(k.clone()).or_default() = n.as_u64().unwrap_or(0);
            }
        }
        serde_json::from_value(r["events"].clone())
            .map_err(|e| PatternError::new(format!("Strudel sent an event Patch can't read: {e}")))
    }

    fn unload(&mut self, slot: u32) {
        let _ = self.call(json!({"op": "unload", "slot": slot}), self.query_timeout);
    }
}
