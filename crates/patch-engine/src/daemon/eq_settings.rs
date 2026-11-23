//! Saved EQ: per channel and per application, presets, the master EQ's state.

use super::*;

/// Where the master EQ is kept.
pub(super) fn eq_path() -> std::path::PathBuf {
    crate::session::state_dir().join("eq.json")
}

pub(super) fn load_eq() -> std::collections::BTreeMap<String, patch_core::eq::EqState> {
    use patch_core::eq::EqState;
    let text = std::fs::read_to_string(eq_path()).unwrap_or_default();
    let mut all: std::collections::BTreeMap<String, EqState> = serde_json::from_str(&text)
        .ok()
        // The first format held the master EQ alone.
        .or_else(|| {
            serde_json::from_str::<EqState>(&text)
                .ok()
                .map(|m| [("master".to_string(), m)].into())
        })
        .unwrap_or_default();
    // Before the DJ Booth had one EQ, Deck A's was the booth's.
    if !all.contains_key("dj")
        && let Some(a) = all.get("deck-a").cloned()
    {
        all.insert("dj".into(), a);
    }
    all.retain(|k, _| {
        patch_core::eq::target(k) == Some(k.as_str())
            || (k.starts_with("app:") && patch_core::eq::valid_target(k))
    });
    for (t, _) in patch_core::eq::TARGETS {
        all.entry(t.to_string()).or_default();
    }
    all.into_iter().map(|(k, v)| (k, v.normalized())).collect()
}

pub(super) fn app_eq_path() -> std::path::PathBuf {
    crate::session::state_dir().join("app-eq.json")
}

pub(super) fn load_app_eq() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(app_eq_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub(super) fn save_app_eq(map: &std::collections::BTreeMap<String, String>) {
    if let Ok(t) = serde_json::to_string_pretty(map) {
        let _ = std::fs::create_dir_all(crate::session::state_dir());
        let _ = std::fs::write(app_eq_path(), t);
    }
}

/// Publish one target's settings to the engine (an app's to its channel,
/// while it has one).
pub(super) fn publish_eq(s: &Shared, target: &str, st: &patch_core::eq::EqState) {
    let channel = match target.strip_prefix("app:") {
        Some(app) if patch_core::stream::is_key(app) => {
            let channel = s.links.lock().unwrap().stream_lanes.assigned.get(app).cloned();
            let Some(c) = channel.and_then(|name| s.params.channel(&name)) else { return };
            if let Ok(_owner) = s.params.stream_owner(c, app) { s.params.channel_eq[c].set(st); }
            return;
        }
        Some(app) => {
            let channel = s.app_eq.lock().unwrap().get(app).cloned();
            // Explicit application-wide EQ remains intentional: publish it
            // to each currently exposed stream as well as its legacy lane.
            let channels: Vec<_> = {
                let links = s.links.lock().unwrap();
                links.stream_apps.iter().filter(|(_, name)| name.eq_ignore_ascii_case(app))
                    .filter_map(|(key, _)| links.stream_lanes.assigned.get(key).map(|ch| (key.clone(), ch.clone()))).collect()
            };
            for (key, name) in channels {
                if let Some(c) = s.params.channel(&name) && let Ok(_owner) = s.params.stream_owner(c, &key) { s.params.channel_eq[c].set(st); }
            }
            let Some(channel) = channel else { return };
            channel
        }
        None => target.to_string(),
    };
    let target = channel.as_str();
    if target == "dj" {
        for deck in patch_core::eq::DJ_DECKS {
            publish_eq(s, deck, st);
        }
    } else if target == "master" {
        s.params.master_eq.set(st);
    } else if let Some(c) = s.params.channel(target)
        && let Some(slot) = s.params.channel_eq.get(c)
    {
        slot.set(st);
    }
}

/// Store, persist and publish new settings for `target` (the RT thread
/// picks them up on its next cycle and glides to them).
pub(super) fn set_eq_state(s: &Shared, target: &str, st: patch_core::eq::EqState) -> Value {
    let st = st.normalized();
    if let Some(key) = target.strip_prefix("app:").filter(|key| patch_core::stream::is_key(key)) {
        let channel = s.links.lock().unwrap().stream_lanes.assigned.get(key).cloned();
        if let Some(c) = channel.and_then(|name| s.params.channel(&name)) { s.params.touch_stream(c); }
    }
    publish_eq(s, target, &st);
    let mut all = s.eq.lock().unwrap();
    all.insert(target.to_string(), st.clone());
    if target == "dj" {
        for deck in patch_core::eq::DJ_DECKS {
            all.insert(deck.to_string(), st.clone());
        }
    }
    if let Ok(t) = serde_json::to_string_pretty(&*all) {
        let _ = std::fs::create_dir_all(crate::session::state_dir());
        let tmp = eq_path().with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, eq_path());
        }
    }
    master_eq_json(&st)
}

/// Saved EQ presets (`patch-eq` JSON files, one per preset).
pub(super) fn preset_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omarchy-patch/eq-presets")
}

/// Built-in presets, then the saved ones (skipping files that don't
/// validate: a broken file never breaks the list).
pub(super) fn all_presets() -> Vec<(patch_core::eq::Preset, bool)> {
    let mut out: Vec<(patch_core::eq::Preset, bool)> = patch_core::eq::builtin_presets()
        .into_iter()
        .map(|p| (p, true))
        .collect();
    if let Ok(dir) = std::fs::read_dir(preset_dir()) {
        let mut user: Vec<_> = dir
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str::<patch_core::eq::Preset>(&t).ok())
            .filter(|p| p.validate().is_ok())
            .collect();
        user.sort_by_key(|a| a.name.to_lowercase());
        out.extend(user.into_iter().map(|p| (p, false)));
    }
    out
}

/// A preset by id or name (case-insensitive).
pub(super) fn find_preset(name: &str) -> Result<patch_core::eq::Preset, String> {
    let n = name.trim().to_lowercase();
    all_presets()
        .into_iter()
        .map(|(p, _)| p)
        .find(|p| {
            p.id.to_lowercase() == n
                || p.name.to_lowercase() == n
                || p.id.rsplit('.').next().is_some_and(|x| x == n)
        })
        .ok_or_else(|| format!("no EQ preset '{name}' (omarchy-patch eq presets)"))
}

/// A file-safe id for a user preset name.
pub(super) fn preset_slug(name: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "preset".into() } else { s }
}

/// The canonical target of a request or command (default: the master):
/// "master", "bus", "deck-a", "deck-b", or "app:<name>" for an
/// application with its own EQ.
pub(super) fn eq_target(t: Option<&str>) -> Result<String, String> {
    match t {
        None => Ok("master".into()),
        Some(t)
            if (t.starts_with("app:") || patch_core::mic::channel(t))
                && patch_core::eq::valid_target(t) =>
        {
            Ok(t.to_string())
        }
        Some(t) => patch_core::eq::target(t)
            .map(String::from)
            .ok_or_else(|| format!("no EQ target '{t}' (master, apps, dj, mic-<n>, app:<name>)")),
    }
}

/// Give `app` its own channel and EQ (true), or take it away (false).
pub(super) fn set_app_eq(s: &Shared, app: &str, on: bool) -> Result<Value, String> {
    let app = app.trim().to_string();
    if patch_core::stream::is_key(&app) {
        let channel = s.links.lock().unwrap().stream_lanes.assigned.get(&app).cloned()
            .ok_or("this playback stream no longer has a native lane")?;
        if !on { set_eq_state(s, &format!("app:{app}"), Default::default()); }
        return Ok(json!({"app": app, "channel": channel}));
    }
    let mut map = s.app_eq.lock().unwrap();
    if on {
        let channel = match map.get(&app) {
            Some(c) => c.clone(),
            None => patch_core::eq::APP_CHANNELS
                .iter()
                .find(|c| !map.values().any(|v| v == *c))
                .map(|c| c.to_string())
                .ok_or(
                    "four applications have their own EQ already (eq app <name> off frees one)",
                )?,
        };
        map.insert(app.clone(), channel.clone());
        save_app_eq(&map);
        s.links
            .lock()
            .unwrap()
            .apps
            .insert(channel.clone(), app.clone());
        crate::events::kick();
        drop(map);
        // Its EQ (kept by application) on its channel.
        let st =
            s.eq.lock()
                .unwrap()
                .get(&format!("app:{app}"))
                .cloned()
                .unwrap_or_default();
        set_eq_state(s, &format!("app:{app}"), st);
        Ok(json!({"app": app, "channel": channel}))
    } else {
        let Some(channel) = map.remove(&app) else {
            return Ok(json!({"app": app, "channel": null}));
        };
        save_app_eq(&map);
        drop(map);
        // The channel is flat before anyone else gets it.
        if let Some(c) = s.params.channel(&channel) {
            s.params.channel_eq[c].set(&patch_core::eq::EqState::default());
        }
        s.links.lock().unwrap().apps.remove(&channel);
        crate::events::kick();
        // Its audio back where apps play: the Patch bus if it exists, else
        // the system's default output.
        let sink = format!("patch.{channel}");
        let to = if std::process::Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.split('\t').nth(1) == Some(crate::links::BUS))
            }) {
            crate::links::BUS.to_string()
        } else {
            "@DEFAULT_SINK@".to_string()
        };
        for st in crate::links::app_streams()
            .iter()
            .filter(|x| x.sink == sink)
        {
            let _ = std::process::Command::new("pactl")
                .args(["move-sink-input", &st.index.to_string(), &to])
                .output();
        }
        Ok(json!({"app": app, "channel": null}))
    }
}

pub(super) fn master_eq_json(st: &patch_core::eq::EqState) -> Value {
    json!({"bypass": st.bypass, "bands": st.bands, "gain_db": st.gain_db, "active": st.active(), "preset": st.preset, "edited": st.edited})
}
