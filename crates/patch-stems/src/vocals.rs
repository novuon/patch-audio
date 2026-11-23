//! The vocal meter: the configured separator measures where tracks sing,
//! for their analysis (patch_analysis::vocals). One process serves many
//! tracks, so the model loads once; dropping the meter ends it (and frees
//! the GPU). Background only, like the rest of this crate.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const SCRIPT: &str = include_str!("../../../tools/stems/vocal_adapter.py");

pub struct VocalMeter {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    /// Which separator measured ("htdemucs 955717e8").
    pub model: String,
}

impl VocalMeter {
    /// Start one with the configured separator; Err without one.
    pub fn start() -> Result<Self, String> {
        let (separator, backend) = crate::separator::configured()?;
        let model = separator.0;
        if !model.layout.iter().any(|s| s == "vocals") {
            return Err("The separator has no vocals stem".into());
        }
        // Beside the installed adapter, written again when Patch changes it.
        let dir = model
            .adapter
            .parent()
            .ok_or("The separator adapter has no folder")?;
        let script = dir.join("vocal_adapter.py");
        if std::fs::read_to_string(&script).ok().as_deref() != Some(SCRIPT) {
            let tmp = script.with_extension("py.tmp");
            std::fs::write(&tmp, SCRIPT).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &script).map_err(|e| e.to_string())?;
        }
        let log =
            std::fs::File::create(dir.join("vocal_adapter.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(&model.python)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("Separator runtime: {e}"))?;
        let mut input = child.stdin.take().ok_or("No meter input")?;
        let output = BufReader::new(child.stdout.take().ok_or("No meter output")?);
        let hello = json!({"model": model, "backend": backend});
        writeln!(input, "{hello}").map_err(|e| e.to_string())?;
        Ok(Self {
            child,
            input,
            output,
            model: format!("{} {}", model.id, model.version),
        })
    }

    /// Per second of `source`: the vocal stem's level against the mix (dB)
    /// and whether the mix is audible then.
    pub fn measure(&mut self, source: &std::path::Path) -> Result<Vec<(f32, bool)>, String> {
        writeln!(self.input, "{}", json!({"source": source})).map_err(|e| e.to_string())?;
        let mut line = String::new();
        if self
            .output
            .read_line(&mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err("The vocal meter stopped (see vocal_adapter.log)".into());
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(e) = v["error"].as_str() {
            return Err(e.into());
        }
        Ok(v["ok"]["seconds"]
            .as_array()
            .ok_or("The vocal meter answered nothing")?
            .iter()
            .map(|s| {
                (
                    s[0].as_f64().unwrap_or(-120.0) as f32,
                    s[1].as_bool().unwrap_or(false),
                )
            })
            .collect())
    }
}

impl Drop for VocalMeter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
