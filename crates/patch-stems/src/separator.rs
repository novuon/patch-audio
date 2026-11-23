//! Model and inference runtime are separately described. External adapters are isolated.
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

// Install the parent-death signal after spawning, without a pre_exec hook
// that would force the engine to fork. The adapter lock lives through exec
// and closes when inference exits, including when the engine is killed.
const ADAPTER_LAUNCHER: &str = r#"
import ctypes, fcntl, os, signal, sys
if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
    raise OSError(ctypes.get_errno(), 'prctl(PR_SET_PDEATHSIG)')
if os.getppid() != int(sys.argv[1]):
    sys.exit(1)
with open(sys.argv[2], 'ab') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    os.set_inheritable(lock.fileno(), True)
    os.execv(sys.executable, [sys.executable, sys.argv[3]])
"#;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub version: String,
    pub weights_digest: String,
    pub layout: Vec<String>,
    pub profile: String,
    pub adapter: PathBuf,
    pub python: PathBuf,
    pub weights: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backend {
    pub id: String,
    pub device: String,
    pub precision: String,
    pub threads: u32,
}
pub trait InferenceBackend: Send + Sync {
    fn descriptor(&self) -> &Backend;
}
impl InferenceBackend for Backend {
    fn descriptor(&self) -> &Backend {
        self
    }
}
pub trait StemSeparator: Send + Sync {
    fn model(&self) -> &Model;
    fn separate(
        &self,
        source: &Path,
        output: &Path,
        backend: &dyn InferenceBackend,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u8),
    ) -> Result<(), String>;
}
pub struct ProcessSeparator(pub Model);
impl StemSeparator for ProcessSeparator {
    fn model(&self) -> &Model {
        &self.0
    }
    fn separate(
        &self,
        source: &Path,
        output: &Path,
        backend: &dyn InferenceBackend,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u8),
    ) -> Result<(), String> {
        use std::io::Write;
        let log = std::fs::File::create(output.join("adapter.log")).map_err(|e| e.to_string())?;
        let cache = output.parent().ok_or("Missing stem cache directory")?;
        let mut child = Command::new(&self.0.python)
            .args(["-c", ADAPTER_LAUNCHER])
            .arg(std::process::id().to_string())
            .arg(cache.join(".separator.lock"))
            .arg(&self.0.adapter)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("Separator runtime: {e}"))?;
        let request =
            json!({"model":self.0,"backend":backend.descriptor(),"source":source,"output":output});
        if let Err(e) = child
            .stdin
            .take()
            .ok_or("No adapter input")?
            .write_all(&serde_json::to_vec(&request).map_err(|e| e.to_string())?)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.to_string());
        }
        loop {
            if let Ok(bytes) = std::fs::read(output.join("progress.json"))
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(p) = v["progress"].as_u64()
            {
                progress(p.min(84) as u8);
            }
            if cancel() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Cancelled".into());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "Separator failed ({status}): {}",
                        std::fs::read_to_string(output.join("adapter.log"))
                            .unwrap_or_default()
                            .chars()
                            .rev()
                            .take(1500)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ))
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
/// The weights file's SHA-256, hashed again only when the file changed
/// (path, size or modification time). Every stem preparation and the Auto
/// Mix's stem-speed check verify the model: hashing its 84 MB each time
/// stalled the engine thread that asked.
fn weights_digest(path: &Path) -> Result<String, String> {
    type Seen = (PathBuf, Option<std::time::SystemTime>, u64, String);
    static MEMO: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let (modified, len) = (meta.modified().ok(), meta.len());
    if let Some((p, m, l, digest)) = MEMO.lock().unwrap().as_ref()
        && p == path
        && *m == modified
        && *l == len
    {
        return Ok(digest.clone());
    }
    let digest = crate::hash_file(path)?;
    *MEMO.lock().unwrap() = Some((path.to_path_buf(), modified, len, digest.clone()));
    Ok(digest)
}

pub fn configured() -> Result<(ProcessSeparator, Backend), String> {
    let home = std::env::var_os("HOME").ok_or("HOME unavailable")?;
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config"));
    let path = std::env::var_os("PATCH_STEM_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("omarchy-patch/stems.json"));
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path)
            .map_err(|_| format!("Configure a benchmarked separator in {}", path.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let model: Model = serde_json::from_value(v["model"].clone()).map_err(|e| e.to_string())?;
    if model.weights_digest.len() != 64
        || !model.weights_digest.bytes().all(|c| c.is_ascii_hexdigit())
        || model.layout.is_empty()
        || model
            .layout
            .iter()
            .any(|id| !patch_core::stems::valid_id(id))
    {
        return Err("Model requires a verified weights digest and valid layout".into());
    }
    if let Some(weights) = &model.weights
        && weights_digest(weights)? != model.weights_digest
    {
        return Err("Model weights checksum mismatch".into());
    }
    let backend: Backend =
        serde_json::from_value(v["backend"].clone()).map_err(|e| e.to_string())?;
    Ok((ProcessSeparator(model), backend))
}
