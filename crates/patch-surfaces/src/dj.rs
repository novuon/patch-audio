//! DJ Booth Unit: two decks, a DJ mixer and a library.
//!
//! Each deck is an mpv process with its own IPC socket, started detached
//! (`setsid`) and playing into its own virtual soundcard (`patch.deck-a`,
//! `patch.deck-b`). The Patch engine mixes both soundcards with the channel
//! faders and crossfader in the same graph cycle, so the decks are
//! sample-synchronous. Decks and engine keep playing when Patch's UI restarts
//! or crashes; Patch reconnects to their sockets and restores the faders.

#[path = "library_history.rs"]
mod library_history;
use library_history::{play_recency, played_local_time};

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DeckStatus {
    pub loaded: bool,
    /// Native decks: the load count `source` belongs to.
    #[serde(default)]
    pub load: Option<u32>,
    pub title: String,
    /// Performer, from the file's tags (empty when the file has none).
    #[serde(default)]
    pub artist: String,
    pub source: String,
    pub time: f64,
    pub cue_point: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    /// False for live streams (internet radio): no cue, no seek.
    pub seekable: bool,
    /// Played by the Patch engine (local files); false: mpv (radio).
    pub native: bool,
    /// Engine-side deck state (native decks only).
    pub looping: Option<(f64, f64)>,
    pub loop_on: bool,
    pub slip: bool,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub censor: bool,
    #[serde(default)]
    pub has_cue: bool,
    /// Playing only while Cue is held.
    pub previewing: bool,
    pub quantize: bool,
    pub sync_on: bool,
    pub sync_error_ms: f64,
    pub sync_hold: bool,
    /// Effective tempo and temporary bend reported by the engine.
    pub tempo: f64,
    pub bend: f64,
    /// Native-deck pitch settings reported by the engine.
    pub keylock: bool,
    pub key_shift: i32,
    pub underruns: u64,
    /// "FLAC 44.1 kHz / 16-bit".
    pub format: String,
    /// Decoded share (0…1) while loading.
    pub decoded: f64,
    /// The beat grid the engine plays this deck to (BPM, first beat in
    /// seconds, downbeat): the analysis with manual edits and refinements.
    #[serde(default)]
    pub grid: Option<(f64, f64, Option<u8>)>,
}

impl DeckStatus {
    /// A loaded deck for headless screenshots (the mock has no players).
    pub fn demo(deck: usize) -> Self {
        let (title, artist, source, time, duration, paused) = if deck == 0 {
            (
                "Midnight Transit",
                "Kasper Lind",
                "~/Music/Kasper Lind/Midnight Transit.flac",
                83.4,
                Some(312.0),
                false,
            )
        } else {
            (
                "Harbour Lights (Extended Mix)",
                "Oda Vei",
                "~/Music/Oda Vei/Harbour Lights.mp3",
                12.9,
                Some(401.0),
                true,
            )
        };
        DeckStatus {
            loaded: true,
            title: title.into(),
            artist: artist.into(),
            source: source.into(),
            time,
            duration,
            paused,
            seekable: true,
            native: true,
            // Exactly 4 beats at the demo track's 124 BPM.
            looping: (deck == 0).then_some((79.35, 79.35 + 4.0 * 60.0 / 124.0)),
            loop_on: deck == 0,
            grid: (deck == 0).then_some((124.0, 0.0, None)),
            quantize: true,
            sync_on: deck == 1,
            sync_error_ms: 0.18,
            keylock: true,
            format: if deck == 0 {
                "FLAC 44.1 kHz / 24-bit".into()
            } else {
                "MP3 44.1 kHz".into()
            },
            decoded: 1.0,
            ..Default::default()
        }
    }
}

/// Artist from an mpv `metadata` map. Tag keys vary by file (`artist`,
/// `ARTIST`); the first one with text wins.
fn metadata_artist(v: &Value) -> String {
    let Some(obj) = v.as_object() else {
        return String::new();
    };
    for key in ["artist", "Artist", "ARTIST", "album_artist", "ALBUMARTIST"] {
        if let Some(name) = obj
            .get(key)
            .and_then(|a| a.as_str())
            .filter(|s| !s.is_empty())
        {
            return name.to_string();
        }
    }
    String::new()
}

pub struct Deck {
    pub id: &'static str,
    pub name: &'static str,
    /// Engine input this deck plays into.
    pub sink: &'static str,
    sock: PathBuf,
    /// The Rubber Band key-shift filter is in the player's chain.
    key_filter: std::cell::Cell<bool>,
    /// The engine's last answer about this deck, kept 8 ms: one frame's
    /// many readers share it. Every command clears it.
    status_seen: std::cell::RefCell<Option<(std::time::Instant, Option<Value>)>>,
    polls: crate::poll::Poller,
    snapshot_seen: std::cell::RefCell<Option<DeckStatus>>,
    poll_at: std::cell::Cell<Option<std::time::Instant>>,
    changed_at: std::cell::Cell<Option<std::time::Instant>>,
    /// Where a waveform drag began (source seconds), while it lasts.
    scrub_base: std::cell::Cell<Option<f64>>,
}

/// Tempo and key of one deck, as a DJ sets them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pitch {
    /// Tempo fader: −range … +range (0.02 = +2 %).
    pub tempo: f64,
    /// Fader range: 0.08, 0.16 or 0.50.
    pub range: f64,
    /// Master tempo: tempo changes keep the key (time-stretch).
    pub keylock: bool,
    /// Transpose in semitones (Rubber Band, formant-free, quality mode).
    pub key_shift: i32,
    /// Temporary nudge while held (±0.04).
    pub bend: f64,
}

impl Default for Pitch {
    fn default() -> Self {
        Pitch {
            tempo: 0.0,
            range: 0.08,
            keylock: true,
            key_shift: 0,
            bend: 0.0,
        }
    }
}

pub const RANGES: [f64; 3] = [0.08, 0.16, 0.50];

impl Pitch {
    /// Playback speed ratio.
    pub fn speed(&self) -> f64 {
        (1.0 + self.tempo) * (1.0 + self.bend)
    }

    /// How far the key moved, in (fractional) semitones: the key shift, plus
    /// the tempo when keylock is off (vinyl-style).
    pub fn semitones(&self) -> f64 {
        let vari = if self.keylock {
            0.0
        } else {
            12.0 * self.speed().log2()
        };
        self.key_shift as f64 + vari
    }

    pub fn next_range(&self) -> f64 {
        let i = RANGES
            .iter()
            .position(|r| (*r - self.range).abs() < 1e-9)
            .unwrap_or(0);
        RANGES[(i + 1) % RANGES.len()]
    }
}

fn runtime_dir() -> PathBuf {
    patch_core::ipc::runtime_dir()
}

impl Deck {
    pub fn new(id: &'static str, name: &'static str) -> Self {
        let sink = match id { "b" => "patch.deck-b", "c" => "patch.deck-c", "d" => "patch.deck-d", _ => "patch.deck-a" };
        Deck {
            id,
            name,
            sink,
            sock: runtime_dir().join(format!("deck-{id}.sock")),
            key_filter: std::cell::Cell::new(false),
            status_seen: std::cell::RefCell::new(None),
            polls: Default::default(),
            snapshot_seen: Default::default(),
            poll_at: Default::default(),
            changed_at: Default::default(),
            scrub_base: Default::default(),
        }
    }

    /// Stream name the deck plays under (what Graph and Mixer show).
    pub fn stream_name(&self) -> String {
        format!("Patch {}", self.name)
    }

    fn connect(&self) -> Option<UnixStream> {
        let s = UnixStream::connect(&self.sock).ok()?;
        s.set_read_timeout(Some(Duration::from_millis(250))).ok()?;
        Some(s)
    }

    /// The deck has something to play: a track on the engine (local files)
    /// or a running player (streams on mpv).
    pub fn alive(&self) -> bool {
        self.is_native() || self.connect().is_some()
    }

    /// Start the deck's player if it isn't running, playing into its deck
    /// sink. Start the engine first: that sink belongs to it.
    pub fn ensure(&self) -> bool {
        if self.alive() {
            return true;
        }
        let _ = std::fs::create_dir_all(runtime_dir());
        let _ = std::fs::remove_file(&self.sock);
        let mut cmd = Command::new("setsid");
        cmd.args([
            "-f",
            "mpv",
            "--idle=yes",
            "--no-video",
            "--no-terminal",
            "--really-quiet",
            "--keep-open=yes",
            "--pause",
        ])
        .arg(format!("--input-ipc-server={}", self.sock.display()))
        .arg("--ao=pipewire")
        .arg(format!("--audio-client-name={}", self.stream_name()))
        .arg("--volume=100")
        .arg("--replaygain=no")
        .arg("--audio-pitch-correction=yes")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        cmd.arg(format!("--audio-device=pipewire/{}", self.sink));
        // The player plays into the deck's soundcard: the engine makes it.
        let _ = patch_engine::client::call(&serde_json::json!({"op": "deck_sinks"}));
        if cmd.status().is_err() {
            return false;
        }
        for _ in 0..40 {
            if self.alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// This deck's native (engine) side: `{"op":"deck","deck":…,"do":…}`.
    fn engine(&self, action: &str, mut args: Value) -> Result<Value, String> {
        args["op"] = "deck".into();
        args["deck"] = self.id.into();
        args["do"] = action.into();
        self.status_seen.replace(None);
        let result = patch_engine::client::call(&args);
        self.changed_at.set(Some(std::time::Instant::now()));
        self.poll_at.set(None);
        result
    }

    fn engine_status(&self) -> Option<Value> {
        if let Some((at, v)) = self.status_seen.borrow().as_ref()
            && at.elapsed() < Duration::from_millis(8)
        {
            return v.clone();
        }
        let v =
            patch_engine::client::call(&serde_json::json!({"op": "deck_status", "deck": self.id}))
                .ok();
        self.status_seen
            .replace(Some((std::time::Instant::now(), v.clone())));
        v
    }

    /// The engine plays this deck (a local file is loaded there).
    pub fn is_native(&self) -> bool {
        self.engine_status()
            .is_some_and(|v| v["loaded"].as_bool().unwrap_or(false))
    }

    /// Send the beat grid to the engine (quantize, loops, jumps, sync).
    pub fn set_grid(&self, bpm: Option<f64>, offset: f64, downbeat: Option<u8>) {
        let _ = self.engine(
            "grid",
            serde_json::json!({"bpm": bpm, "offset": offset, "downbeat": downbeat}),
        );
    }

    pub fn deck_action(&self, action: &str, args: Value) -> Result<(), String> {
        self.engine(action, args).map(|_| ())
    }

    pub fn sync(&self, on: bool, master: Option<&str>) -> Result<(), String> {
        self.status_seen.replace(None);
        patch_engine::client::call(
            &serde_json::json!({"op": "sync", "deck": self.id, "on": on, "master": master}),
        )
        .map(|_| ())
    }

    /// Send commands; returns each command's `data` (in order).
    fn call(&self, commands: &[Value]) -> Option<Vec<Value>> {
        let mut s = self.connect()?;
        for (i, c) in commands.iter().enumerate() {
            let line = serde_json::json!({ "command": c, "request_id": i + 1 });
            writeln!(s, "{line}").ok()?;
        }
        let mut out = vec![Value::Null; commands.len()];
        let mut got = 0;
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        while got < commands.len() {
            line.clear();
            if reader.read_line(&mut line).ok()? == 0 {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(id) = v["request_id"].as_u64()
                && (1..=commands.len() as u64).contains(&id)
            {
                out[id as usize - 1] = v["data"].clone();
                got += 1;
            }
        }
        Some(out)
    }

    /// Load a track: local files play in the engine (sample-accurate
    /// transport, loops, sync); streams (radio) on mpv, and an online track
    /// that is one finished file moves to the engine once it is fetched.
    pub fn load(&self, url: &str) -> Result<(), String> {
        let local = !url.contains("://") && std::path::Path::new(url).is_file();
        // An online track that is one finished file, fetched before: a file.
        let file = if local {
            Some(url.to_string())
        } else {
            crate::remote_track::cached(url).map(|p| p.to_string_lossy().into_owned())
        };
        if let Some(file) = file {
            // Silence the mpv side first (the engine ignores its port while a
            // native track is loaded anyway).
            let _ = self.call(&[serde_json::json!(["stop"])]);
            return self
                .engine("load", serde_json::json!({"path": file}))
                .map(|_| ());
        }
        let _ = self.engine("eject", serde_json::json!({}));
        if !self.ensure() {
            return Err("couldn't start the deck player (mpv)".into());
        }
        self.call(&[
            serde_json::json!(["loadfile", url, "replace"]),
            serde_json::json!(["set_property", "pause", true]),
        ])
        .ok_or_else(|| "deck didn't answer".to_string())?;
        crate::remote_track::take_over_soon(self.id, self.sock.clone(), url.to_string());
        Ok(())
    }

    /// Point a running player at its deck sink (decks started by older
    /// Patch versions played straight into the bus).
    pub fn attach(&self) {
        let want = format!("pipewire/{}", self.sink);
        let now = self.call(&[serde_json::json!(["get_property", "audio-device"])]);
        if now.is_some_and(|v| v[0].as_str() != Some(want.as_str())) {
            let _ = self.call(&[serde_json::json!(["set_property", "audio-device", want])]);
        }
    }

    /// Stop the player. Its stream goes away; nothing else is touched.
    pub fn quit(&self) {
        let _ = self.engine("eject", serde_json::json!({}));
        let _ = self.call(&[serde_json::json!(["quit"])]);
    }

    pub fn toggle(&self) {
        if self.is_native() {
            let _ = self.engine("toggle", serde_json::json!({}));
        } else {
            let _ = self.call(&[serde_json::json!(["cycle", "pause"])]);
        }
    }

    /// Set tempo, keylock and key. Speed changes use mpv's time-stretch
    /// (scaletempo2) with keylock, plain resampling without; key shifts go
    /// through Rubber Band's R3 ("finer") engine, added only while needed.
    pub fn set_pitch(&self, p: &Pitch) {
        if self.is_native() {
            let _ = self.engine("tempo", serde_json::json!({"value": p.tempo}));
            let _ = self.engine("bend", serde_json::json!({"value": p.bend}));
            let _ = self.engine("keylock", serde_json::json!({"on": p.keylock}));
            let _ = self.engine("key", serde_json::json!({"semitones": p.key_shift}));
            return;
        }
        self.set_pitch_mpv(p);
    }

    fn set_pitch_mpv(&self, p: &Pitch) {
        let mut cmds = vec![
            serde_json::json!(["set_property", "audio-pitch-correction", p.keylock]),
            serde_json::json!(["set_property", "speed", p.speed()]),
        ];
        if p.key_shift != 0 {
            if !self.key_filter.get() {
                cmds.push(serde_json::json!(["af", "remove", "@rb"]));
                cmds.push(serde_json::json!([
                    "af",
                    "add",
                    "@rb:rubberband=pitch=quality:engine=finer:transients=mixed"
                ]));
                self.key_filter.set(true);
            }
            cmds.push(serde_json::json!([
                "af-command",
                "rb",
                "set-pitch",
                2f64.powf(p.key_shift as f64 / 12.0)
            ]));
        } else if self.key_filter.get() {
            cmds.push(serde_json::json!(["af", "remove", "@rb"]));
            self.key_filter.set(false);
        }
        let _ = self.call(&cmds);
    }

    /// Back to the start of the track, paused.
    pub fn cue(&self) {
        if self.is_native() {
            let _ = self.engine("cue", serde_json::json!({}));
            return;
        }
        let _ = self.call(&[
            serde_json::json!(["seek", 0, "absolute"]),
            serde_json::json!(["set_property", "pause", true]),
        ]);
    }

    /// Cue released: a held preview returns to the cue point (engine decks;
    /// streams have no preview).
    pub fn cue_release(&self) {
        if self.is_native() {
            let _ = self.engine("cue_release", serde_json::json!({}));
        }
    }

    pub fn seek_by(&self, secs: f64) {
        if let Some(v) = self
            .engine_status()
            .filter(|v| v["loaded"].as_bool().unwrap_or(false))
        {
            let t = v["position"].as_f64().unwrap_or(0.0) + secs;
            let _ = self.engine("seek", serde_json::json!({"seconds": t.max(0.0)}));
            return;
        }
        let _ = self.call(&[serde_json::json!(["seek", secs, "relative"])]);
    }

    /// Jump to `secs` exactly (hot cues, beat alignment).
    /// The waveform dragged by hand (engine decks): "start" holds the music
    /// under the hand, "move" glides it `offset` seconds from where the
    /// drag began (the engine follows like a platter), "end" lets go: it
    /// plays on, or rests there paused, ready for a cue.
    pub fn scrub(&self, phase: &str, offset: f64) {
        if !self.is_native() {
            return;
        }
        match phase {
            "start" => {
                let Some(at) = self.engine_status().and_then(|v| v["position"].as_f64()) else {
                    return;
                };
                self.scrub_base.set(Some(at));
                let _ = self.engine(
                    "scratch",
                    serde_json::json!({"seconds": at, "velocity": 0.0}),
                );
            }
            "move" => {
                if let Some(base) = self.scrub_base.get() {
                    let to = (base + offset).max(0.0);
                    let _ = self.engine(
                        "scratch",
                        serde_json::json!({"seconds": to, "velocity": 0.0}),
                    );
                }
            }
            _ => {
                if self.scrub_base.take().is_some() {
                    let _ = self.engine("scratch_end", serde_json::json!({}));
                }
            }
        }
    }

    /// Headphones only: hear the track from `secs` while the program plays
    /// on (`None` stops). Engine decks.
    pub fn audition(&self, secs: Option<f64>) {
        if self.is_native() {
            let req = secs.map_or(serde_json::json!({}), |s| serde_json::json!({"seconds": s.max(0.0)}));
            let _ = self.engine("audition", req);
        }
    }

    /// A hot cue pressed on an engine deck: paused, it plays from `secs`
    /// while held and `cue_release` brings it back there; playing, it jumps.
    pub fn gate_cue(&self, secs: f64) {
        let _ = self.engine("gate_cue", serde_json::json!({"seconds": secs.max(0.0)}));
    }

    pub fn seek_abs(&self, secs: f64) {
        if self.is_native() {
            let _ = self.engine("seek", serde_json::json!({"seconds": secs.max(0.0)}));
            return;
        }
        let _ = self.call(&[serde_json::json!(["seek", secs.max(0.0), "absolute+exact"])]);
    }

    pub fn seek_to(&self, frac: f64) {
        if let Some(v) = self
            .engine_status()
            .filter(|v| v["loaded"].as_bool().unwrap_or(false))
        {
            let d = v["duration"].as_f64().unwrap_or(0.0);
            let _ = self.engine(
                "seek",
                serde_json::json!({"seconds": frac.clamp(0.0, 1.0) * d}),
            );
            return;
        }
        let _ = self.call(&[serde_json::json!([
            "seek",
            (frac * 100.0).clamp(0.0, 100.0),
            "absolute-percent"
        ])]);
    }

    /// Ask for a fresh snapshot now, not at the next 100 ms.
    pub fn poll_soon(&self) {
        self.poll_at.set(None);
    }

    /// Read-only UI snapshot. Action/guard callers keep using `status`, so
    /// an asynchronous read cannot change a transport decision.
    pub fn snapshot(&self) -> DeckStatus {
        if self
            .poll_at
            .get()
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(100))
        {
            let (id, name) = (self.id, self.name);
            if self.polls.request_with("deck", Value::Null, move |_| {
                serde_json::to_value(Deck::new(id, name).read_status()).map_err(|e| e.to_string())
            }) {
                self.poll_at.set(Some(std::time::Instant::now()));
            }
        }
        if let Some(reply) = self.polls.take("deck")
            && self.changed_at.get().is_none_or(|at| at <= reply.requested)
            && let Ok(value) = reply.result
            && let Ok(status) = serde_json::from_value(value)
        {
            *self.snapshot_seen.borrow_mut() = Some(status);
        }
        self.snapshot_seen.borrow().clone().unwrap_or_default()
    }

    pub fn status(&self) -> DeckStatus {
        let status = self.read_status();
        *self.snapshot_seen.borrow_mut() = Some(status.clone());
        status
    }

    fn read_status(&self) -> DeckStatus {
        if let Some(v) = self
            .engine_status()
            .filter(|v| v["loaded"].as_bool().unwrap_or(false))
        {
            let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
            let path = v["path"].as_str().unwrap_or("").to_string();
            let title = v["title"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(String::from)
                .unwrap_or_else(|| {
                    std::path::Path::new(&path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            let rate = f("native_rate");
            let bits = v["bits"].as_u64().unwrap_or(0);
            let format = format!(
                "{} {} kHz{}",
                v["codec"].as_str().unwrap_or("").to_uppercase(),
                format!("{:.1}", rate / 1000.0).trim_end_matches(".0"),
                if bits > 0 {
                    format!(" / {bits}-bit")
                } else {
                    String::new()
                }
            );
            let sync = &v["sync"];
            return DeckStatus {
                loaded: true,
                load: v["load"].as_u64().map(|l| l as u32),
                title,
                artist: v["artist"].as_str().unwrap_or("").to_string(),
                source: path,
                time: f("position"),
                cue_point: f("cue_point"),
                duration: Some(f("duration")).filter(|d| *d > 0.0),
                paused: !v["playing"].as_bool().unwrap_or(false),
                seekable: true,
                native: true,
                looping: v["loop_in"].as_f64().zip(v["loop_out"].as_f64()),
                loop_on: v["loop_on"].as_bool().unwrap_or(false),
                slip: v["slip"].as_bool().unwrap_or(false),
                reverse: v["reverse"].as_bool().unwrap_or(false),
                censor: v["censor"].as_bool().unwrap_or(false),
                has_cue: v["cue_point"].is_number(),
                previewing: v["previewing"].as_bool().unwrap_or(false),
                quantize: v["quantize"].as_bool().unwrap_or(true),
                sync_on: sync["on"].as_bool().unwrap_or(false),
                sync_error_ms: sync["error_ms"].as_f64().unwrap_or(0.0),
                sync_hold: sync["phase_hold"].as_bool().unwrap_or(false),
                tempo: f("tempo"),
                bend: f("bend"),
                keylock: v["keylock"].as_bool().unwrap_or(true),
                key_shift: v["key"].as_i64().unwrap_or(0) as i32,
                underruns: v["underruns"].as_u64().unwrap_or(0),
                format,
                decoded: f("decoded"),
                grid: v["grid"]["bpm"]
                    .as_f64()
                    .filter(|bpm| *bpm > 0.0)
                    .map(|bpm| {
                        let downbeat = v["grid"]["downbeat"].as_u64().map(|d| d as u8);
                        (bpm, v["grid"]["offset"].as_f64().unwrap_or(0.0), downbeat)
                    }),
            };
        }
        let props = [
            "media-title",
            "path",
            "time-pos",
            "duration",
            "pause",
            "idle-active",
            "seekable",
            "metadata",
        ];
        let cmds: Vec<Value> = props
            .iter()
            .map(|p| serde_json::json!(["get_property", p]))
            .collect();
        let Some(v) = self.call(&cmds) else {
            return DeckStatus::default();
        };
        let idle = v[5].as_bool().unwrap_or(true);
        DeckStatus {
            loaded: !idle,
            title: v[0].as_str().unwrap_or("").to_string(),
            artist: metadata_artist(&v[7]),
            source: v[1].as_str().unwrap_or("").to_string(),
            time: v[2].as_f64().unwrap_or(0.0),
            duration: v[3].as_f64().filter(|d| *d > 0.0),
            paused: v[4].as_bool().unwrap_or(true),
            seekable: v[6].as_bool().unwrap_or(false),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LibraryItem {
    pub played: String,
    pub played_at: String,
    pub title: String,
    pub url: String,
    /// "radio" or the file extension.
    pub kind: String,
    pub detail: String,
    pub favourite: bool,
    /// The song's first release year, and this release's (its own tags),
    /// 0 when unknown.
    pub year: u16,
    pub year_release: u16,
    /// Its genre: the file's tag when it names one, else MusicBrainz's.
    pub genre: String,
}

/// The favourite tracks' paths, for the decks and the Library's sort.
pub(crate) fn favourite_paths(items: &[LibraryItem]) -> std::collections::HashSet<String> {
    items.iter().filter(|i| i.favourite).map(|i| i.url.clone()).collect()
}

/// Where the screenshot library's USB stick is mounted.
const DEMO_STICK: &str = "/run/media/randy/STICK";

/// A library for headless screenshots, with real folders under `music_dir`
/// and one stick, so places, Where and collections can be reviewed.
pub fn demo_library() -> Vec<LibraryItem> {
    let music = music_dir();
    let file = |root: &std::path::Path, rel: &str, title: &str, detail: &str, played: &str, played_at: &str| {
        let path = root.join(rel);
        let kind = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("audio")
            .to_string();
        LibraryItem {
            title: title.into(),
            url: path.to_string_lossy().into_owned(),
            kind,
            detail: detail.into(),
            played: played.into(),
            played_at: played_at.into(),
            favourite: title.starts_with("Harbour"),
            // Screenshots: a re-release beside a song of this year.
            year: if title.starts_with("Harbour") { 1998 } else { 2024 },
            year_release: 2024,
            genre: if detail.contains("Techno") { "Techno (Peak Time / Driving)" } else if title.starts_with("Paper") { "Nu Disco / Disco" } else { "Deep House" }.into(),
        }
    };
    let radio = |title: &str, url: &str| LibraryItem {
        title: title.into(),
        url: url.into(),
        kind: "radio".into(),
        detail: String::new(),
        played: String::new(),
        played_at: String::new(),
        favourite: false,
        ..Default::default()
    };
    let stick = std::path::Path::new(DEMO_STICK);
    vec![
        file(&music, "House/2024/Harbour Lights (Extended Mix).mp3", "Harbour Lights (Extended Mix)", "Oda Vei", "Today", "Last played today · 12 plays"),
        file(&music, "House/2024/Midnight Transit.flac", "Midnight Transit", "Kasper Lind", "3 days ago", "Last played 3 days ago · 4 plays"),
        file(&music, "House/2023/Night Ferry.flac", "Night Ferry", "Kasper Lind", "Unplayed", "Never played"),
        file(&music, "Nu Disco/Paper Satellites.opus", "Paper Satellites", "Loma Reyes", "Unplayed", "Never played"),
        file(&music, "Demos/Salt & Static.wav", "Salt & Static", "Oda Vei", "Unplayed", "Never played"),
        file(&music, "Slow Signal.m4a", "Slow Signal", "The Quiet Rooms", "Unplayed", "Never played"),
        file(stick, "Techno/Warehouse.wav", "Warehouse", "Anika", "Unplayed", "Never played"),
        file(stick, "Techno/Cold Harbour.aiff", "Cold Harbour", "Anika", "Unplayed", "Never played"),
        radio("Radio Paradise", "https://stream.radioparadise.com/aac-320"),
        radio("SomaFM Groove Salad", "https://ice1.somafm.com/groovesalad-128-mp3"),
    ]
}

/// Folders and drives behind [`demo_library`], so a screenshot's Where list
/// matches the paths.
pub(crate) fn demo_library_status(items: &[LibraryItem]) -> patch_engine::librarian::Status {
    let music = music_dir().to_string_lossy().into_owned();
    let tally = |root: &str| {
        let n = items.iter().filter(|i| i.url.starts_with(&format!("{root}/"))).count();
        let check = items
            .iter()
            .filter(|i| i.url.starts_with(&format!("{root}/")) && i.title.starts_with("Salt"))
            .count();
        patch_library::Counts {
            tracks: n,
            ready: n - check,
            soft: 0,
            check,
            waiting: 0,
            failed: 0,
        }
    };
    patch_engine::librarian::Status {
        sources: vec![
            patch_engine::librarian::SourceStatus {
                name: "Music".into(),
                path: music.clone(),
                connected: true,
                removable: false,
                counts: tally(&music),
            },
            patch_engine::librarian::SourceStatus {
                name: "Stick".into(),
                path: DEMO_STICK.into(),
                connected: true,
                removable: true,
                counts: tally(DEMO_STICK),
            },
        ],
        ..Default::default()
    }
}

/// Readiness for the screenshot rows. Salt is the one to check; streams
/// have none.
pub(crate) fn demo_readiness(
    items: &[LibraryItem],
) -> std::collections::HashMap<String, (patch_library::Readiness, Option<String>)> {
    items
        .iter()
        .filter(|i| !i.url.contains("://"))
        .map(|i| {
            let state = if i.title.starts_with("Salt") {
                patch_library::Readiness::Check
            } else {
                patch_library::Readiness::Ready
            };
            (i.url.clone(), (state, None))
        })
        .collect()
}

/// Which screenshot tracks sit in which demo collection, so a crate can
/// show the folder its files share.
pub(crate) fn demo_memberships(
    items: &[LibraryItem],
) -> std::collections::HashMap<i64, std::collections::HashSet<String>> {
    let take = |pred: fn(&LibraryItem) -> bool| {
        items.iter().filter(|i| pred(i)).map(|i| i.url.clone()).collect()
    };
    std::collections::HashMap::from([
        (3, take(|i| i.url.contains("/House/2024/"))),
        (4, take(|i| i.url.contains("/Nu Disco/") || i.url.contains("/Demos/") || i.url.ends_with("/Slow Signal.m4a"))),
        (6, take(|i| i.url.contains("/STICK/"))),
        (9, take(|i| i.url.contains("/House/2023/"))),
    ])
}

/// Local files under ~/Music plus the stations in cliamp's history.
pub fn library() -> Vec<LibraryItem> {
    library_result().unwrap_or_else(|e| {
        eprintln!("Patch DJ library unavailable: {e}");
        let mut items = Vec::new();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        append_history(&mut items, &home);
        items
    })
}

pub(crate) fn library_result() -> Result<Vec<LibraryItem>, String> {
    let mut items = vec![];
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let music = Command::new("xdg-user-dir")
        .arg("MUSIC")
        .output()
        .ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.join("Music"));
    let db = patch_library::open_quick()?;
    let tracks = db.tracks()?;
    let years = db.years();
    let mut genres = db.genres();
    let now = patch_engine::session::now_s() as i64;
    // Read only: Session::load also changes process-wide level preferences.
    let session = std::fs::read_to_string(patch_engine::session::path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let started = session
        .as_ref()
        .filter(|s| s["set"]["on"] == true)
        .and_then(|s| s["set"]["started"].as_i64());
    for t in tracks
        .into_iter()
        .filter(|t| !t.missing && std::path::Path::new(&t.path).is_file())
    {
        let p = PathBuf::from(&t.path);
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("audio")
            .to_lowercase();
        let played = play_recency(t.last_played, now, started);
        let favourite = t.tags.iter().any(|x| x == patch_library::FAVOURITE_TAG);
        let played_at = t
            .last_played
            .map(|at| format!("Last played {} · {} plays", played_local_time(at), t.plays))
            .unwrap_or_else(|| "Never played".into());
        let (original, release) = years.get(&t.path).copied().unwrap_or((None, None));
        items.push(LibraryItem {
            year: original.or(release).unwrap_or(0),
            year_release: release.unwrap_or(0),
            genre: genres.remove(&t.path).unwrap_or_default(),
            favourite,
            played,
            played_at,
            title: if t.title.is_empty() {
                p.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                t.title
            },
            url: t.path,
            detail: if t.artist.is_empty() {
                p.parent()
                    .and_then(|d| d.strip_prefix(&music).ok())
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            } else {
                t.artist
            },
            kind: ext,
        });
    }
    items.sort_by_cached_key(|a| a.title.to_lowercase());
    append_history(&mut items, &home);
    Ok(items)
}

fn append_history(items: &mut Vec<LibraryItem>, home: &std::path::Path) {
    // cliamp's history: [[entry]] path = "…" title = "…"
    if let Ok(text) = std::fs::read_to_string(home.join(".config/cliamp/history.toml")) {
        let mut path = None;
        for line in text.lines() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("path = ") {
                path = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = l.strip_prefix("title = ")
                && let Some(p) = path.take()
                && !items.iter().any(|i| i.url == p)
            {
                let radio = p.starts_with("http");
                items.push(LibraryItem {
                    title: v.trim_matches('"').to_string(),
                    kind: if radio { "radio".into() } else { "file".into() },
                    detail: if radio {
                        "internet radio · from cliamp".into()
                    } else {
                        "from cliamp".into()
                    },
                    url: p,
                    played: String::new(),
                    played_at: String::new(),
                    favourite: false,
                    ..Default::default()
                });
            }
        }
    }
}

pub fn music_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    Command::new("xdg-user-dir")
        .arg("MUSIC")
        .output()
        .ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.join("Music"))
}

pub fn scan_native_library() -> Result<(), String> {
    let db = patch_library::open_default()?;
    db.scan(&music_dir(), patch_library::probe_audio_facts)
        .map(|_| ())
}

pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Analyses what a deck just loaded at once (BPM and grid, key, loudness,
/// waveform), in the background. The rest of the library is the
/// librarian's (patch_engine::librarian); results are cached on disk by
/// `patch_analysis`, so each file is analysed once.
pub struct Analyst {
    /// Files waiting for analysis (for diagnostics).
    pub pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    urgent: std::sync::mpsc::Sender<String>,
    done: std::sync::mpsc::Receiver<(String, Result<patch_analysis::Analysis, String>)>,
}

impl Analyst {
    pub fn start() -> Analyst {
        let (urgent, urgent_rx) = std::sync::mpsc::channel::<String>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let queue = pending.clone();
        let _ = std::thread::Builder::new()
            .name("patch-analyst".into())
            .spawn(move || {
                // Spare CPU and I/O only (its key threads and decoder inherit
                // it): a load must never stutter the music or the windows.
                // SAFETY: plain syscalls on this thread with valid arguments.
                unsafe {
                    let param = libc::sched_param { sched_priority: 0 };
                    libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
                    // ioprio_set(IOPRIO_WHO_PROCESS, this thread, IDLE)
                    libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
                }
                let mut seen = std::collections::HashSet::new();
                loop {
                    let Ok(path) = urgent_rx.recv() else { return };
                    // One fewer, never below zero.
                    let mut n = queue.load(std::sync::atomic::Ordering::Relaxed);
                    while n > 0 {
                        match queue.compare_exchange_weak(
                            n,
                            n - 1,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(current) => n = current,
                        }
                    }
                    if !seen.insert(path.clone())
                        && patch_analysis::cached(std::path::Path::new(&path)).is_none()
                    {
                        continue; // failed before: don't retry in a loop
                    }
                    let source = std::path::Path::new(&path);
                    let r =
                        if let Some(mut analysis) = patch_engine::track_analysis::stored_analysis(&path) {
                            if analysis.waveform.is_empty() {
                                analysis.waveform = patch_library::open_quick()
                                    .ok()
                                    .and_then(|db| db.waveform_bins(&path))
                                    .unwrap_or_default();
                            }
                            patch_analysis::ensure_detail(source).map(|()| analysis)
                        } else {
                            patch_analysis::analyse_file(source)
                        };
                    if done_tx.send((path, r)).is_err() {
                        return;
                    }
                }
            });
        Analyst {
            pending,
            urgent,
            done,
        }
    }

    /// Analyse a local file soon (a deck just loaded it).
    pub fn now(&self, path: &str) {
        if is_local(path) {
            self.pending
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = self.urgent.send(path.to_string());
        }
    }

    /// Finished analyses since the last call.
    pub fn finished(&self) -> Vec<(String, Result<patch_analysis::Analysis, String>)> {
        self.done.try_iter().collect()
    }
}

fn is_local(path: &str) -> bool {
    !path.contains("://") && std::path::Path::new(path).is_file()
}

/// Analyses for headless screenshots: decks and library rows get realistic
/// tempo, key, loudness and a waveform (clearly the demo tracks).
pub fn demo_analysis(deck: usize) -> patch_analysis::Analysis {
    let (bpm, key, lufs, dur) = if deck == 0 {
        (124.0, (9, true), -9.4, 312.0)
    } else {
        (125.5, (4, true), -8.1, 401.0)
    };
    let waveform = (0..patch_analysis::WAVEFORM_BINS)
        .map(|i| {
            let x = i as f64 / patch_analysis::WAVEFORM_BINS as f64;
            // Intro, build, breakdown, drop, outro.
            let shape = if x < 0.08 {
                0.35
            } else if x < 0.45 {
                0.8
            } else if x < 0.55 {
                0.4
            } else if x < 0.9 {
                0.95
            } else {
                0.5
            };
            let wobble = 0.85 + 0.15 * ((i as f64 * 1.7).sin() * (i as f64 * 0.37).cos());
            let p = (shape * wobble).min(1.0);
            [(p * 255.0) as u8, (p * 140.0) as u8]
        })
        .collect();
    patch_analysis::Analysis {
        version: patch_analysis::VERSION,
        analyzers: patch_analysis::AnalyzerVersions::CURRENT,
        human_grid: None,
        source: None,
        duration: dur,
        bpm: Some(bpm),
        beat_offset: 0.12,
        bpm_confidence: 0.9,
        key: Some(patch_analysis::key::Key {
            tonic: key.0,
            minor: key.1,
        }),
        key_confidence: 0.6,
        lufs,
        lra: 6.0,
        true_peak: -0.3,
        waveform,
        downbeat: Some(0),
        downbeat_confidence: 0.7,
        // The waveform's shape, second by second: a kick-led intro, the
        // body, a breakdown without the kick, the drop, the outro.
        energy: (0..dur as usize)
            .map(|i| {
                let x = i as f64 / dur;
                let e = if x < 0.08 {
                    90.0
                } else if x < 0.45 {
                    190.0
                } else if x < 0.55 {
                    70.0
                } else if x < 0.9 {
                    215.0
                } else {
                    100.0
                };
                (e + 8.0 * ((i as f64) * 0.7).sin()) as u8
            })
            .collect(),
        key_source: Default::default(),
        key_detected: None,
        key_detected_confidence: 0.0,
        tag_bpm: None,
        grid: Some(patch_analysis::grid::GridQuality {
            click_offset_ms: None,
            method: patch_analysis::grid::METHOD,
            precise: false,
            bpm,
            offset: 0.12,
            downbeat: Some(0),
            downbeat_confidence: 0.8,
            jitter_ms: 0.4,
            drift_ms: 1.0,
            coverage: 0.85,
            beatless: vec![[(dur * 0.45) as f32, (dur * 0.55) as f32]],
            mixability: 0.95,
            phase_confidence: 1.0,
        }),
        rhythm: None,
        deep_regions: Vec::new(),
        structure: None,
        harmony: None,
        vocals: None,
    }
}

pub fn demo_library_analysis(row: usize) -> Option<patch_analysis::Analysis> {
    let table = [
        (125.5, 4, true, -8.1),
        (124.0, 9, true, -9.4),
        (122.0, 0, false, -10.2),
        (128.0, 2, true, -7.9),
        (174.0, 11, true, -8.8),
        (98.0, 7, false, -12.4),
        (130.0, 6, true, -8.4),
        (126.0, 8, true, -9.0),
    ];
    let (bpm, tonic, minor, lufs) = *table.get(row)?;
    // Measured grids: machine-straight, except a live-played one.
    let loose = row == 5;
    let grid = patch_analysis::grid::GridQuality {
        click_offset_ms: None,
        method: patch_analysis::grid::METHOD,
        precise: false,
        bpm,
        offset: 0.1,
        downbeat: Some(0),
        downbeat_confidence: 0.8,
        jitter_ms: if loose { 6.5 } else { 0.3 },
        drift_ms: if loose { 14.0 } else { 0.6 },
        coverage: 0.9,
        beatless: vec![],
        mixability: if loose { 0.42 } else { 0.97 },
        phase_confidence: 1.0,
    };
    Some(patch_analysis::Analysis {
        bpm: Some(bpm),
        key: Some(patch_analysis::key::Key { tonic, minor }),
        lufs,
        waveform: vec![],
        grid: Some(grid),
        ..demo_analysis(0)
    })
}

/// How many hot cues a deck holds, from the one place that decides it. The booth's
/// pad strip draws four of them and the waveform draws all eight, so a controller's
/// pads 5 to 8 reach cues the pad strip cannot; this is the number that keeps the
/// unguarded `cues[n]` in `dj_actions` safe, so it moves with the command's own
/// limit.
pub use patch_core::command::HOT_CUES;

/// Hot cues per track, kept in ~/.local/share/omarchy-patch.
pub type Cues = [Option<f64>; HOT_CUES];

fn cues_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("omarchy-patch").join("cues.json")
}

/// The library's own stamp: the newest write among the files SQLite touches.
///
/// Deliberately not `default_path()`'s own timestamp. The library runs in WAL
/// mode, so a write lands in the `-wal` sidecar and the database file's
/// timestamp stays where it was until a checkpoint — minutes later, or not
/// before the next clean close. Keying on the database file alone missed
/// precisely the writes this exists to catch, so a cue the DJ removed went on
/// showing.
pub fn library_stamp() -> Option<std::time::SystemTime> {
    patch_library::stamp()
}

/// Whether a copy of the cue table read under `was` is stale now.
///
/// Two ways it is not: the shot platform is driving, in which case the table is a
/// fixture and must stay as it is, or the library has not moved since the last
/// read. Named rather than inlined because it is the whole of the rule that decides
/// whether a cue set from hardware appears in an open booth.
pub fn cues_are_stale(
    frozen: bool,
    was: Option<std::time::SystemTime>,
    now: Option<std::time::SystemTime>,
) -> bool {
    !frozen && was != now
}

#[cfg(test)]
#[test]
fn a_frozen_booth_never_reloads_and_an_unmoved_library_need_not() {
    let t1 = std::time::SystemTime::UNIX_EPOCH;
    let t2 = t1 + std::time::Duration::from_secs(1);
    assert!(
        cues_are_stale(false, Some(t1), Some(t2)),
        "the library moved"
    );
    assert!(!cues_are_stale(false, Some(t1), Some(t1)), "nothing moved");
    assert!(
        !cues_are_stale(false, None, None),
        "a library that stays missing"
    );
    assert!(
        cues_are_stale(false, None, Some(t1)),
        "a library that appeared"
    );
    // The shot platform's cue table is a fixture: reloading it would replace the
    // demo's cues with whatever the running library holds.
    assert!(!cues_are_stale(true, Some(t1), Some(t2)));
}

pub fn load_cues() -> std::collections::HashMap<String, Cues> {
    let mut cues = std::collections::HashMap::new();
    if let Ok(db) = patch_library::open_quick()
        && let Ok(tracks) = db.tracks()
    {
        for t in tracks {
            let values = db.performance(t.id).hot_cues;
            let mut hot = [None; HOT_CUES];
            for (i, v) in values.into_iter().take(HOT_CUES).enumerate() {
                hot[i] = v;
            }
            if hot.iter().any(Option::is_some) {
                cues.insert(t.path, hot);
            }
        }
    }
    let legacy: std::collections::HashMap<String, Cues> = std::fs::read_to_string(cues_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for (path, old) in legacy {
        cues.entry(path).or_insert(old);
    }
    cues
}

pub fn save_cues(all: &std::collections::HashMap<String, Cues>) {
    let p = cues_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(all).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

/// Collections the Library folds away under "Inactive"
/// (`~/.config/omarchy-patch/library-collections.json`). New collections
/// start active, so only the ones put away are kept.
fn collections_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("omarchy-patch/library-collections.json")
}

pub fn load_inactive_collections() -> std::collections::HashSet<i64> {
    std::fs::read_to_string(collections_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v["inactive"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
        })
        .unwrap_or_default()
}

pub fn save_inactive_collections(ids: &std::collections::HashSet<i64>) {
    let p = collections_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let mut ids: Vec<i64> = ids.iter().copied().collect();
    ids.sort_unstable();
    let tmp = p.with_extension("tmp");
    let body =
        serde_json::to_vec_pretty(&serde_json::json!({ "inactive": ids })).unwrap_or_default();
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

/// Collections for headless screenshots: as many, and as long-named, as a
/// real crate shelf, two of them put away.
pub fn demo_collections() -> (Vec<(i64, String, usize)>, std::collections::HashSet<i64>) {
    let names = [
        ("Beatport Top 100 Downloads October 2026", 100),
        ("Tech House Top 100 September 2026", 100),
        ("Warm-up · deep and slow", 2),
        ("Reference", 3),
        ("Beatport Top 100 Downloads September 2026", 100),
        ("USB stick", 2),
        ("Nu Disco August 2026", 47),
        ("Beatport Top 100 Downloads August 2026", 100),
        ("Peak time", 1),
    ];
    let list = names
        .iter()
        .enumerate()
        .map(|(i, (n, c))| (i as i64 + 1, n.to_string(), *c))
        .collect();
    (list, [5, 7, 8].into_iter().collect())
}

/// Snap `t` to the nearest beat of the grid (when the track has one).
pub fn snap_to_beat(t: f64, a: Option<&patch_analysis::Analysis>) -> f64 {
    match a.and_then(|a| {
        a.bpm
            .filter(|_| a.bpm_confidence >= 0.5)
            .map(|b| (b, a.beat_offset))
    }) {
        Some((bpm, off)) => {
            let per = 60.0 / bpm;
            (off + ((t - off) / per).round() * per).max(0.0)
        }
        None => t,
    }
}

/// A detailed waveform for headless screenshots, from the demo overview.
pub fn demo_detail(deck: usize) -> patch_analysis::detail::Detail {
    let a = demo_analysis(deck);
    let n = (a.duration * patch_analysis::detail::COLUMNS_PER_SECOND as f64) as usize;
    let columns = (0..n)
        .map(|i| {
            let o = a.waveform[i * a.waveform.len() / n.max(1)][0] as f64 / 255.0;
            // A kick-like pulse on every beat of the demo grid.
            let t = i as f64 / patch_analysis::detail::COLUMNS_PER_SECOND as f64;
            let per = 60.0 / a.bpm.unwrap_or(124.0);
            let ph = ((t - a.beat_offset) / per).rem_euclid(1.0);
            let kick = (-ph * 9.0).exp();
            let low = (o * (0.35 + 0.65 * kick) * 255.0) as u8;
            let mid = (o * 0.55 * (0.8 + 0.2 * (i as f64 * 0.9).sin()) * 255.0) as u8;
            let high = (o * 0.35 * (0.6 + 0.4 * (i as f64 * 2.3).cos().abs()) * 255.0) as u8;
            [low, mid, high, low.max(mid).max(high)]
        })
        .collect();
    patch_analysis::detail::Detail {
        columns_per_second: patch_analysis::detail::COLUMNS_PER_SECOND,
        columns,
    }
}
