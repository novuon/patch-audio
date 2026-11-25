//! The Player in the desktop's media controls (MPRIS), outside Omarchy.
//!
//! GNOME's and KDE's media controls, lock screens and media keys reach
//! Patch's Player: what plays (title, artist, length), play/pause, stop,
//! next/previous in its queue and seeking. Only the Player: the decks and
//! an Auto Mix set never appear there, so a stray media key never stops a
//! DJ set (the user, 2026-11-24).
//!
//! The tray process hosts it (it runs with the session), on the tray's own
//! D-Bus connection. It follows the Player's mpv through mpv's own
//! property events on `player.sock`: nothing polls while the Player plays,
//! and while it doesn't run the tray's two-second look only checks that
//! the socket is there. The bus name (`org.mpris.MediaPlayer2.patch`) is
//! held only while the Player has something loaded, so the media controls
//! show no empty Patch player.
//!
//! Commands go through the Player's own actions (`patch_surfaces::player`),
//! as its window and the CLI send them.

use patch_surfaces::player::{self, Action};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value as ZValue};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NAME: &str = "org.mpris.MediaPlayer2.patch";

#[derive(Clone, Default, PartialEq)]
pub struct Now {
    path: String,
    title: String,
    artist: String,
    duration: f64,
    paused: bool,
    ended: bool,
    idle: bool,
}

impl Now {
    fn status(&self) -> &'static str {
        if self.path.is_empty() || self.idle || self.ended {
            "Stopped"
        } else if self.paused {
            "Paused"
        } else {
            "Playing"
        }
    }

    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        let id = ObjectPath::try_from("/app/patchaudio/Player/track").expect("a valid path");
        m.insert("mpris:trackid".into(), OwnedValue::from(id));
        if !self.title.is_empty() {
            m.insert("xesam:title".into(), owned(ZValue::from(self.title.as_str())));
        }
        if !self.artist.is_empty() {
            m.insert("xesam:artist".into(), owned(ZValue::from(vec![self.artist.as_str()])));
        }
        if self.duration > 0.0 {
            m.insert("mpris:length".into(), OwnedValue::from((self.duration * 1e6) as i64));
        }
        if self.path.starts_with('/') {
            m.insert("xesam:url".into(), owned(ZValue::from(format!("file://{}", self.path))));
        }
        m
    }
}

fn owned(v: ZValue<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("plain values have no file descriptors")
}

fn act(a: Action) {
    std::thread::spawn(move || {
        let _ = player::command(a);
    });
}

pub struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Patch Player"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "omarchy-patch"
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["file".into()]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
    /// The Player's window.
    fn raise(&self) {
        std::thread::spawn(|| {
            if let Ok(exe) = std::env::current_exe() {
                use std::os::unix::process::CommandExt as _;
                let _ = std::process::Command::new(exe).args(["open", "player"]).process_group(0).spawn();
            }
        });
    }
    fn quit(&self) {}
}

pub struct Player(Arc<Mutex<Now>>);

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn playback_status(&self) -> &str {
        self.0.lock().unwrap().status()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.0.lock().unwrap().metadata()
    }
    /// Read when asked (hosts ask while they show it): one mpv request.
    #[zbus(property)]
    fn position(&self) -> i64 {
        player::snapshot().map(|s| (s.position * 1e6) as i64).unwrap_or(0)
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }

    fn play(&self) {
        act(Action::Play(true));
    }
    fn pause(&self) {
        act(Action::Play(false));
    }
    fn play_pause(&self) {
        act(Action::Toggle);
    }
    fn stop(&self) {
        act(Action::Stop);
    }
    fn next(&self) {
        act(Action::Next);
    }
    fn previous(&self) {
        act(Action::Previous);
    }
    /// Microseconds from where it plays.
    fn seek(&self, offset: i64) {
        act(Action::Skip(offset as f64 / 1e6));
    }
    fn set_position(&self, _track: OwnedObjectPath, position: i64) {
        act(Action::Seek(position as f64 / 1e6));
    }
    fn open_uri(&self, uri: String) {
        if let Some(path) = uri.strip_prefix("file://") {
            act(Action::Load(path.to_string()));
        }
    }

    #[zbus(signal)]
    async fn seeked(emitter: &zbus::object_server::SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

/// mpv's properties this follows, by observe id.
const OBSERVED: [&str; 7] = ["path", "pause", "media-title", "metadata", "duration", "eof-reached", "idle-active"];

fn apply(now: &mut Now, name: &str, data: &Value) {
    match name {
        "path" => now.path = data.as_str().unwrap_or("").to_string(),
        "pause" => now.paused = data.as_bool().unwrap_or(true),
        "media-title" => now.title = data.as_str().unwrap_or("").to_string(),
        "metadata" => {
            now.artist = ["artist", "ARTIST", "Artist", "album_artist"]
                .iter()
                .find_map(|k| data[*k].as_str())
                .unwrap_or("")
                .to_string()
        }
        "duration" => now.duration = data.as_f64().unwrap_or(0.0),
        "eof-reached" => now.ended = data.as_bool().unwrap_or(false),
        "idle-active" => now.idle = data.as_bool().unwrap_or(false),
        _ => {}
    }
}

/// Serve the MPRIS objects on the tray's connection (the name comes and
/// goes with the Player).
pub fn serve(conn: &zbus::blocking::Connection) -> zbus::Result<Arc<Mutex<Now>>> {
    let now = Arc::new(Mutex::new(Now::default()));
    conn.object_server().at(PATH, Root)?;
    conn.object_server().at(PATH, Player(now.clone()))?;
    Ok(now)
}

/// While the Player runs: follow it, then let the name go. Called from the
/// tray's look loop when `player.sock` is there; returns when mpv exits.
pub fn follow(conn: &zbus::blocking::Connection, now: &Arc<Mutex<Now>>) {
    let Ok(stream) = UnixStream::connect(patch_core::ipc::runtime_dir().join("player.sock")) else {
        return;
    };
    let Ok(mut writer) = stream.try_clone() else { return };
    for (i, p) in OBSERVED.iter().enumerate() {
        let line = json!({"command": ["observe_property", i + 1, p]}).to_string() + "\n";
        if writer.write_all(line.as_bytes()).is_err() {
            return;
        }
    }
    let Ok(iface) = conn.object_server().interface::<_, Player>(PATH) else { return };
    let mut named = false;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        if event["event"] != "property-change" {
            continue;
        }
        let (before, after) = {
            let mut n = now.lock().unwrap();
            let before = n.clone();
            apply(&mut n, event["name"].as_str().unwrap_or(""), &event["data"]);
            (before, n.clone())
        };
        // The media controls show Patch's Player once it has something.
        let loaded = !after.path.is_empty();
        if loaded && !named {
            named = conn.request_name(NAME).is_ok();
        } else if !loaded && named {
            let _ = conn.release_name(NAME);
            named = false;
        }
        let guard = iface.get();
        let e = iface.signal_emitter();
        if before.status() != after.status() {
            let _ = zbus::block_on(guard.playback_status_changed(e));
        }
        if (before.path.as_str(), before.title.as_str(), before.artist.as_str(), before.duration)
            != (after.path.as_str(), after.title.as_str(), after.artist.as_str(), after.duration)
        {
            let _ = zbus::block_on(guard.metadata_changed(e));
        }
    }
    if named {
        let _ = conn.release_name(NAME);
    }
    *now.lock().unwrap() = Now::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpvs_events_make_the_media_controls_state() {
        let mut n = Now::default();
        assert_eq!(n.status(), "Stopped");
        apply(&mut n, "path", &json!("/music/a.flac"));
        apply(&mut n, "pause", &json!(false));
        apply(&mut n, "media-title", &json!("A"));
        apply(&mut n, "metadata", &json!({"ARTIST": "B"}));
        apply(&mut n, "duration", &json!(61.5));
        assert_eq!(n.status(), "Playing");
        let m = n.metadata();
        assert_eq!(i64::try_from(&*m["mpris:length"]).unwrap(), 61_500_000);
        assert!(m.contains_key("xesam:artist") && m.contains_key("xesam:url"));
        apply(&mut n, "pause", &json!(true));
        assert_eq!(n.status(), "Paused");
        apply(&mut n, "eof-reached", &json!(true));
        assert_eq!(n.status(), "Stopped", "played to its end");
    }
}
