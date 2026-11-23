//! Persistent fader settings for the event-sound channel.

use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch/system-sounds.json"))
}

pub fn load() -> (f32, bool) {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return (0.0, false);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (0.0, false);
    };
    (
        value["gain_db"].as_f64().unwrap_or(0.0).clamp(-120.0, 0.0) as f32,
        value["mute"].as_bool().unwrap_or(false),
    )
}

pub fn save(gain_db: f32, mute: bool) {
    let Some(p) = path() else { return };
    let Some(parent) = p.parent() else { return };
    if std::fs::create_dir_all(parent).is_ok() {
        let value = serde_json::json!({"gain_db": gain_db, "mute": mute});
        let _ = std::fs::write(p, format!("{value}\n"));
    }
}
