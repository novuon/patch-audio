//! Profile file operations run exclusively on the service control plane.
use patch_core::mic::{Profile, factory_profiles};
use std::path::PathBuf;

fn directory() -> PathBuf {
    crate::session::state_dir().join("mic-profiles")
}
fn filename(name: &str) -> String {
    // An injective encoding, independent of locale and safe for every name.
    name.as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        + ".json"
}

pub fn list() -> Result<Vec<Profile>, String> {
    let mut profiles = factory_profiles();
    let dir = directory();
    if !dir.exists() {
        return Ok(profiles);
    }
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if metadata.len() > 64 * 1024 {
            return Err(format!("profile {} exceeds 64 KiB", path.display()));
        }
        let p: Profile = serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        p.validate()?;
        profiles.push(p);
    }
    Ok(profiles)
}

pub fn find(name: &str) -> Result<Profile, String> {
    list()?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("no microphone profile '{name}'"))
}

pub fn save(p: &Profile, replace: bool) -> Result<(), String> {
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _writer = WRITER.lock().map_err(|_| "profile writer unavailable")?;
    p.validate()?;
    if factory_profiles().iter().any(|f| f.name == p.name) {
        return Err("factory profiles are read-only; choose a new name".into());
    }
    let dir = directory();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(filename(&p.name));
    if path.exists() && !replace {
        return Err("profile exists; explicitly request replace to edit it".into());
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    use std::io::Write;
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
