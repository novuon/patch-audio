//! The Player's carts: pads that start a sound at once, over whatever
//! else plays (a podcast's intro, a commercial, B-roll, a sting).
//!
//! The board (four banks of twelve pads) is a small JSON file,
//! `~/.config/omarchy-patch/carts.json`: the window, the CLI and a desktop
//! key binding all read the same pads.
//!
//! Each sound plays on a *voice*: its own detached mpv on the Player's
//! stream name, so every cart shares the Player's Mixer strip, EQ and PFL.
//! A voice plays one sound and exits (`--idle=once`). One spare voice waits
//! idle, so a pad starts in the time mpv needs to open a file (35 ms
//! measured, against 83 ms for a cold start), and nothing is left running
//! once the last sound ends and the Player's windows are closed.
//!
//! What plays is asked of the voices themselves (each carries its pad in
//! mpv's `user-data`), so a new window or a CLI call sees the same state.

use super::{Ipc, validate_source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Pads in a bank: the number row, 1 to =.
pub const PADS: usize = 12;
pub const BANKS: usize = 4;
/// Sounds at once, the idle spare included.
pub const VOICES: usize = 8;
/// A pad's key in the window, by position.
pub const KEYS: [&str; PADS] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="];
/// A pad set to fade, and "Fade all".
pub const FADE: Duration = Duration::from_millis(2000);
/// The others, when only one may play: short, but not a click.
const SWAP_FADE: Duration = Duration::from_millis(250);
/// Colours a pad can carry.
pub const COLOURS: u8 = 5;
/// How far a ducking pad turns the transport down, dB.
pub const DUCK_DB: f64 = -12.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pad {
    pub label: String,
    /// A file or an HTTP(S) address; empty: the pad is free.
    pub file: String,
    /// 0 to 100, as the Player's volume.
    pub gain: f64,
    /// Plays round until stopped (a music bed).
    pub looped: bool,
    /// Stopping it fades it out over `FADE`.
    pub fade: bool,
    /// Its length when known (0: not known, or a stream).
    pub seconds: f64,
    /// Plays only while its pad or key is held down (a bleep, a sting).
    pub hold: bool,
    /// While it plays, the Player's transport is turned down under it.
    pub duck: bool,
    /// A colour to know it by: 0 none, 1 to `COLOURS`.
    pub colour: u8,
}

impl Default for Pad {
    fn default() -> Self {
        Self {
            label: String::new(),
            file: String::new(),
            gain: 100.0,
            looped: false,
            fade: false,
            seconds: 0.0,
            hold: false,
            duck: false,
            colour: 0,
        }
    }
}

impl Pad {
    pub fn assigned(&self) -> bool {
        !self.file.trim().is_empty()
    }

    /// The name shown: its label, else the file's name.
    pub fn name(&self) -> String {
        if !self.label.trim().is_empty() {
            return self.label.trim().to_owned();
        }
        title_of(&self.file)
    }
}

/// A file's name without folder and extension ("ads/Spot 30s.wav" → "Spot 30s").
pub fn title_of(file: &str) -> String {
    let name = file.trim().trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem.to_owned(),
        _ => name.to_owned(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bank {
    pub name: String,
    pub pads: Vec<Pad>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Board {
    /// The bank shown, and the one the number keys and `player cart <n>` play.
    pub bank: usize,
    /// One at a time: starting a pad stops the others.
    pub solo: bool,
    pub banks: Vec<Bank>,
}

impl Default for Board {
    fn default() -> Self {
        let mut board = Self { bank: 0, solo: false, banks: Vec::new() };
        board.tidy();
        board
    }
}

/// What a new bank is called.
const BANK_NAMES: [&str; BANKS] = ["Intros", "Commercials", "B-roll", "Effects"];

impl Board {
    /// Always four banks of twelve pads, whatever the file held.
    pub fn tidy(&mut self) {
        self.banks.truncate(BANKS);
        while self.banks.len() < BANKS {
            self.banks.push(Bank::default());
        }
        for (i, bank) in self.banks.iter_mut().enumerate() {
            if bank.name.trim().is_empty() {
                bank.name = BANK_NAMES[i].into();
            }
            bank.pads.resize_with(PADS, Pad::default);
            for pad in &mut bank.pads {
                pad.gain = if pad.gain.is_finite() { pad.gain.clamp(0.0, 100.0) } else { 100.0 };
                if !pad.seconds.is_finite() || pad.seconds < 0.0 {
                    pad.seconds = 0.0;
                }
                pad.colour = pad.colour.min(COLOURS);
            }
        }
        self.bank = self.bank.min(BANKS - 1);
    }

    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("omarchy-patch/carts.json"))
    }

    /// The saved board; an empty one when there is none (or it can't be read).
    pub fn load() -> Self {
        Self::path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let mut board: Self = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        board.tidy();
        board
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path().ok_or("No home folder to save the carts in.")?)
    }

    /// Written beside and renamed: a reader never sees half a board.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let err = |e: std::io::Error| format!("Could not save the carts: {e}");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.new");
        std::fs::write(&tmp, text).map_err(err)?;
        std::fs::rename(&tmp, path).map_err(err)
    }

    pub fn pad(&self, bank: usize, index: usize) -> Option<&Pad> {
        self.banks.get(bank)?.pads.get(index)
    }

    /// Put files on a bank's pads: the first on `from`, the others on the
    /// free pads after it (never over a pad that has a sound). Returns how
    /// many found a pad.
    pub fn place(&mut self, bank: usize, from: usize, files: &[(String, f64)]) -> usize {
        let Some(bank) = self.banks.get_mut(bank) else { return 0 };
        let mut placed = 0;
        let mut at = from;
        for (file, seconds) in files {
            if placed > 0 {
                while at < PADS && bank.pads[at].assigned() {
                    at += 1;
                }
            }
            if at >= PADS {
                break;
            }
            let pad = &mut bank.pads[at];
            // A new sound on a pad that had one keeps its settings, not its name.
            if pad.file != *file {
                pad.label.clear();
            }
            pad.file = file.clone();
            pad.seconds = *seconds;
            placed += 1;
            at += 1;
        }
        placed
    }
}

/// A file's length in seconds (0 when ffprobe is missing or can't tell).
/// Blocks for the probe: never on the UI thread.
pub fn seconds_of(file: &str) -> f64 {
    if file.contains("://") {
        return 0.0;
    }
    Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(file)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(0.0)
}

/// A sound that plays now.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Playing {
    pub bank: usize,
    pub pad: usize,
    pub label: String,
    pub position: f64,
    pub duration: f64,
    pub looped: bool,
    /// On its way out.
    pub fading: bool,
    /// The transport is turned down under it.
    pub duck: bool,
    /// The voice it plays on.
    #[serde(skip)]
    pub voice: usize,
}

impl Playing {
    /// Seconds left (0 for a loop or an unknown length).
    pub fn remaining(&self) -> f64 {
        if self.looped || self.duration <= 0.0 { 0.0 } else { (self.duration - self.position).max(0.0) }
    }
}

/// What to do with the carts; the Player's worker and the CLI both run these.
#[derive(Clone, Debug)]
pub enum Do {
    /// A pad pressed: start it; if it plays, stop it (a second press while
    /// it fades cuts it).
    Trigger { bank: usize, index: usize, pad: Pad, solo: bool },
    /// A hold pad went down: start it, and leave it be if it plays already
    /// (a key held down repeats).
    Start { bank: usize, index: usize, pad: Pad, solo: bool },
    /// Stop one pad, fading or at once.
    Stop { bank: usize, index: usize, fade: bool },
    /// A pad's level changed while it plays (0 to 100).
    Gain { bank: usize, index: usize, gain: f64 },
    /// Stop everything that plays.
    StopAll { fade: bool },
    /// Have the spare voice ready (a Player window opened).
    Warm,
    /// Let the spare voice go (the last Player window closed).
    Rest,
}

/// Run it, leaving fades to finish by themselves.
pub fn run(action: Do) -> Result<(), String> {
    perform(action).map(|_| ())
}

/// Run it and wait for its fades: a CLI call must not exit mid-fade.
pub fn run_and_wait(action: Do) -> Result<(), String> {
    for fade in perform(action)? {
        let _ = fade.join();
    }
    Ok(())
}

/// Everything that plays, in voice order. A few socket reads; nothing at
/// all when no voice runs.
pub fn status() -> Vec<Playing> {
    (0..VOICES)
        .filter_map(|voice| match look(voice) {
            Voice::Busy(_, playing) => Some(playing),
            _ => None,
        })
        .collect()
}

/// Whether any voice runs (a socket is there): what a caller checks before
/// asking for `status`.
pub fn any() -> bool {
    (0..VOICES).any(|voice| socket(voice).exists())
}

fn socket(voice: usize) -> PathBuf {
    patch_core::ipc::runtime_dir().join(format!("cart-{voice}.sock"))
}

enum Voice {
    /// No process: the slot is free.
    Gone,
    /// Idle and ready for a sound.
    Spare(Ipc),
    Busy(Ipc, Playing),
}

fn look(voice: usize) -> Voice {
    look_at(socket(voice), voice)
}

fn look_at(path: PathBuf, voice: usize) -> Voice {
    if !path.exists() {
        return Voice::Gone;
    }
    let Ok(mut ipc) = Ipc::connect(path.clone()) else {
        // A voice that ended leaves its socket file behind.
        let _ = std::fs::remove_file(&path);
        return Voice::Gone;
    };
    match ipc.command(json!(["get_property", "idle-active"])) {
        Ok(Value::Bool(true)) => Voice::Spare(ipc),
        Ok(_) => {
            let mut get = |name: &str| ipc.command(json!(["get_property", name])).unwrap_or(Value::Null);
            let cart = get("user-data/patch-cart");
            let playing = Playing {
                bank: cart["bank"].as_u64().unwrap_or(0) as usize,
                pad: cart["pad"].as_u64().unwrap_or(0) as usize,
                label: cart["label"].as_str().unwrap_or_default().to_owned(),
                fading: cart["fading"] == true,
                duck: cart["duck"] == true,
                looped: get("loop-file") == "inf",
                position: get("time-pos").as_f64().unwrap_or(0.0).max(0.0),
                duration: get("duration").as_f64().unwrap_or(0.0).max(0.0),
                voice,
            };
            Voice::Busy(ipc, playing)
        }
        Err(_) => Voice::Gone,
    }
}

/// One process at a time chooses and starts voices: the window and a CLI
/// call pressed together must not take the same spare.
fn lock() -> Result<std::fs::File, String> {
    let dir = patch_core::ipc::runtime_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("carts.lock"))
        .map_err(|e| e.to_string())?;
    file.lock().map_err(|e| e.to_string())?;
    Ok(file)
}

/// What a voice runs beside its sound: a pad set to duck asks the Player
/// to turn its transport down when the sound starts and to bring it back
/// when the voice ends, however it ends (played out, stopped, faded). The
/// voice does it itself, so it works with every window closed.
fn duck_script(binary: &str, voice: usize) -> String {
    let binary = binary.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"local ducked = false
mp.register_event("file-loaded", function()
    if mp.get_property_native("user-data/patch-cart/duck") then
        ducked = true
        mp.command_native_async({{name = "subprocess", args = {{"{binary}", "player", "duck", "on"}}, playback_only = false, detach = true}}, function() end)
    end
end)
mp.register_event("shutdown", function()
    if ducked then
        mp.command_native({{name = "subprocess", args = {{"{binary}", "player", "duck", "off", "{voice}"}}, playback_only = false, detach = true}})
    end
end)
"#
    )
}

/// Start an idle voice and wait until it answers.
fn spawn(voice: usize) -> Result<Ipc, String> {
    let path = socket(voice);
    let _ = std::fs::remove_file(&path);
    // No script, no ducking: the sound still plays.
    let script = path.with_extension("lua");
    let scripted = std::env::current_exe()
        .ok()
        .is_some_and(|exe| std::fs::write(&script, duck_script(&exe.to_string_lossy(), voice)).is_ok());
    let started = Command::new("setsid")
        .args([
            "-f",
            "mpv",
            "--no-config",
            "--idle=once",
            "--no-video",
            "--no-terminal",
            "--really-quiet",
            "--keep-open=no",
            "--ao=pipewire",
            if patch_core::setup::load().system_audio { "--audio-device=pipewire/patch" } else { "--audio-device=auto" },
            "--volume=100",
            "--replaygain=no",
            // The Player's stream name: one Mixer strip, EQ and PFL for
            // the Player and its carts.
            "--audio-client-name=Patch Player",
        ])
        .arg(format!("--input-ipc-server={}", path.display()))
        .args(scripted.then(|| format!("--script={}", script.display())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !started.success() {
        return Err("Could not launch mpv.".into());
    }
    let since = Instant::now();
    while since.elapsed() < Duration::from_millis(1500) {
        if path.exists()
            && let Ok(mut ipc) = Ipc::connect(path.clone())
            && ipc.command(json!(["get_property", "idle-active"])).is_ok()
        {
            patch_core::ipc::private_socket(&path);
            // A fresh connection: this one's reply deadline is partly spent.
            return Ipc::connect(path);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("The carts did not start; install mpv with PipeWire support.".into())
}

fn expand(file: &str) -> String {
    let file = file.trim();
    match file.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest).to_string_lossy().into_owned(),
        None => file.to_owned(),
    }
}

fn perform(action: Do) -> Result<Vec<JoinHandle<()>>, String> {
    let _lock = lock()?;
    let mut fades = Vec::new();
    // A hold pad going down starts its sound; it never stops one.
    let starting = matches!(action, Do::Start { .. });
    match action {
        Do::Trigger { bank, index, pad, solo } | Do::Start { bank, index, pad, solo } => {
            let mut spare = None;
            let mut free = Vec::new();
            let mut others = Vec::new();
            let mut same = None;
            for voice in 0..VOICES {
                match look(voice) {
                    Voice::Gone => free.push(voice),
                    Voice::Spare(ipc) if spare.is_none() => spare = Some(ipc),
                    Voice::Spare(_) => {}
                    Voice::Busy(ipc, p) if p.bank == bank && p.pad == index => same = Some((ipc, p)),
                    Voice::Busy(_, p) => others.push(p),
                }
            }
            // Pressed while it plays: stop it. While it fades: cut it.
            if same.is_some() && starting {
                return Ok(fades);
            }
            if let Some((mut ipc, playing)) = same {
                if pad.fade && !playing.fading {
                    fades.extend(fade_out(playing.voice, FADE));
                } else {
                    let _ = ipc.command(json!(["quit"]));
                }
                return Ok(fades);
            }
            if !pad.assigned() {
                return Err(format!("Pad {} has no sound yet. Right-click it to choose one.", KEYS.get(index).unwrap_or(&"?")));
            }
            let file = expand(&pad.file);
            validate_source(&file).map_err(|_| format!("Pad {} · {}: the file is not there.", KEYS.get(index).unwrap_or(&"?"), pad.name()))?;
            if solo {
                for other in others.iter().filter(|p| !p.fading) {
                    fades.extend(fade_out(other.voice, SWAP_FADE));
                }
            }
            let mut ipc = match spare {
                Some(ipc) => ipc,
                None => {
                    let voice = free.pop().ok_or("Every cart voice is playing: stop one first.")?;
                    spawn(voice)?
                }
            };
            ipc.command(json!(["set_property", "user-data/patch-cart", {"bank": bank, "pad": index, "label": pad.name(), "fading": false, "duck": pad.duck}]))?;
            ipc.command(json!(["set_property", "volume", pad.gain.clamp(0.0, 100.0)]))?;
            ipc.command(json!(["set_property", "loop-file", if pad.looped { "inf" } else { "no" }]))?;
            ipc.command(json!(["loadfile", file, "replace"]))?;
            drop(ipc);
            // The sound runs; now the next spare, so the next pad is as quick.
            if let Some(voice) = free.pop() {
                let _ = spawn(voice);
            }
        }
        Do::Stop { bank, index, fade } => {
            for voice in 0..VOICES {
                if let Voice::Busy(mut ipc, p) = look(voice)
                    && p.bank == bank
                    && p.pad == index
                {
                    if fade && !p.fading {
                        fades.extend(fade_out(voice, FADE));
                    } else if !fade {
                        let _ = ipc.command(json!(["quit"]));
                    }
                }
            }
        }
        Do::Gain { bank, index, gain } => {
            for voice in 0..VOICES {
                if let Voice::Busy(mut ipc, p) = look(voice)
                    && p.bank == bank
                    && p.pad == index
                    && !p.fading
                    && gain.is_finite()
                {
                    let _ = ipc.command(json!(["set_property", "volume", gain.clamp(0.0, 100.0)]));
                }
            }
        }
        Do::StopAll { fade } => {
            for voice in 0..VOICES {
                if let Voice::Busy(mut ipc, p) = look(voice) {
                    if fade && !p.fading {
                        fades.extend(fade_out(voice, FADE));
                    } else if !fade {
                        let _ = ipc.command(json!(["quit"]));
                    }
                }
            }
        }
        Do::Warm => {
            let mut free = None;
            for voice in 0..VOICES {
                match look(voice) {
                    Voice::Spare(_) => return Ok(fades),
                    Voice::Gone if free.is_none() => free = Some(voice),
                    _ => {}
                }
            }
            if let Some(voice) = free {
                spawn(voice)?;
            }
        }
        Do::Rest => {
            for voice in 0..VOICES {
                if let Voice::Spare(mut ipc) = look(voice) {
                    let _ = ipc.command(json!(["quit"]));
                }
            }
        }
    }
    Ok(fades)
}

/// The volume a fade has reached `t` (0 to 1) of the way through: mpv's
/// volume is already a loudness curve, so a straight line down sounds even.
fn fade_level(start: f64, t: f64) -> f64 {
    (start * (1.0 - t.clamp(0.0, 1.0))).max(0.0)
}

/// Fade a voice out and end it, on a thread of its own. The voice says it
/// fades (so the pad shows it and a second press cuts it); a voice that is
/// gone ends the fade.
fn fade_out(voice: usize, over: Duration) -> Option<JoinHandle<()>> {
    let path = socket(voice);
    let mut ipc = Ipc::connect(path.clone()).ok()?;
    let start = ipc.command(json!(["get_property", "volume"])).ok()?.as_f64().unwrap_or(100.0);
    let _ = ipc.command(json!(["set_property", "user-data/patch-cart/fading", true]));
    std::thread::Builder::new()
        .name("patch-cart-fade".into())
        .spawn(move || {
            const STEP: Duration = Duration::from_millis(25);
            let since = Instant::now();
            loop {
                let t = since.elapsed().as_secs_f64() / over.as_secs_f64().max(0.001);
                // A connection's replies share one deadline: a new one each step.
                let Ok(mut ipc) = Ipc::connect(path.clone()) else { return };
                if t >= 1.0 {
                    let _ = ipc.command(json!(["quit"]));
                    return;
                }
                if ipc.command(json!(["set_property", "volume", fade_level(start, t)])).is_err() {
                    return;
                }
                std::thread::sleep(STEP);
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_board_is_always_four_banks_of_twelve_pads() {
        let board = Board::default();
        assert_eq!(board.banks.len(), BANKS);
        assert!(board.banks.iter().all(|b| b.pads.len() == PADS));
        assert_eq!(board.banks[0].name, "Intros");
        // A hand-edited file: one bank, one pad, a wild gain, a bank too far.
        let mut board: Board = serde_json::from_value(json!({
            "bank": 9,
            "banks": [{"name": "Show", "pads": [{"file": "/a/intro.wav", "gain": 400.0}]}],
        }))
        .unwrap();
        board.tidy();
        assert_eq!(board.bank, BANKS - 1);
        assert_eq!(board.banks.len(), BANKS);
        assert_eq!(board.banks[0].name, "Show");
        assert_eq!(board.banks[1].name, "Commercials");
        assert_eq!(board.banks[0].pads.len(), PADS);
        assert_eq!(board.banks[0].pads[0].gain, 100.0);
        assert!(!board.solo, "sounds overlap unless the user chooses one at a time");
    }

    #[test]
    fn a_board_survives_saving_and_a_broken_file_gives_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("patch-carts-{}-{}", std::process::id(), patch_engine::now_ns()));
        let path = dir.join("carts.json");
        let mut board = Board::default();
        board.banks[2].pads[4] = Pad {
            label: "Sting".into(),
            file: "/s/sting.wav".into(),
            gain: 70.0,
            looped: true,
            fade: true,
            seconds: 3.5,
            hold: true,
            duck: true,
            colour: 3,
        };
        board.bank = 2;
        board.solo = true;
        board.save_to(&path).unwrap();
        assert_eq!(Board::load_from(&path), board);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Board::load_from(&path), Board::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_fill_the_free_pads_and_never_replace_another_sound() {
        let mut board = Board::default();
        board.banks[0].pads[1] = Pad { label: "Keep".into(), file: "/keep.wav".into(), ..Pad::default() };
        let files: Vec<(String, f64)> = (0..3).map(|i| (format!("/new/{i}.wav"), 10.0 + i as f64)).collect();
        assert_eq!(board.place(0, 0, &files), 3);
        let pads = &board.banks[0].pads;
        assert_eq!(pads[0].file, "/new/0.wav");
        assert_eq!(pads[1].file, "/keep.wav", "a pad with a sound is skipped");
        assert_eq!(pads[2].file, "/new/1.wav");
        assert_eq!(pads[3].file, "/new/2.wav");
        assert_eq!(pads[3].seconds, 12.0);
        // The pad chosen is replaced (that is what the user asked), its name with it.
        board.banks[0].pads[1].gain = 40.0;
        assert_eq!(board.place(0, 1, &[("/other.wav".into(), 1.0)]), 1);
        assert_eq!(board.banks[0].pads[1].file, "/other.wav");
        assert_eq!(board.banks[0].pads[1].label, "");
        assert_eq!(board.banks[0].pads[1].gain, 40.0, "its settings stay");
        // More files than pads: the rest are left out, not wrapped round.
        let many: Vec<(String, f64)> = (0..20).map(|i| (format!("/m/{i}.wav"), 0.0)).collect();
        assert_eq!(board.place(1, 6, &many), PADS - 6);
    }

    #[test]
    fn a_pad_is_named_by_its_label_or_its_file() {
        let mut pad = Pad { file: "/shows/ads/Spot 30s.final.wav".into(), ..Pad::default() };
        assert_eq!(pad.name(), "Spot 30s.final");
        pad.label = " Sponsor ".into();
        assert_eq!(pad.name(), "Sponsor");
        assert_eq!(title_of("https://example.test/live/stream"), "stream");
        assert_eq!(title_of("/a/.hidden"), ".hidden");
        assert!(!Pad::default().assigned());
    }

    #[test]
    fn a_ducking_voice_calls_the_player_at_its_start_and_its_end() {
        let script = duck_script("/opt/my \"patch\"/omarchy-patch", 3);
        assert!(script.contains(r#"{"/opt/my \"patch\"/omarchy-patch", "player", "duck", "on"}"#));
        assert!(script.contains(r#""player", "duck", "off", "3"}"#), "it names itself: the others still count");
        assert!(script.contains("user-data/patch-cart/duck"), "only a pad set to duck");
        assert!(script.contains(r#"register_event("shutdown""#), "however it ends");
    }

    #[test]
    fn an_old_board_reads_with_the_new_settings_off() {
        let pad: Pad = serde_json::from_value(json!({"label": "Old", "file": "/a.wav", "gain": 80.0})).unwrap();
        assert!(!pad.hold && !pad.duck && pad.colour == 0);
        let mut board = Board::default();
        board.banks[0].pads[0].colour = 200;
        board.tidy();
        assert_eq!(board.banks[0].pads[0].colour, COLOURS);
    }

    #[test]
    fn a_fade_falls_evenly_to_silence() {
        assert_eq!(fade_level(80.0, 0.0), 80.0);
        assert_eq!(fade_level(80.0, 0.5), 40.0);
        assert_eq!(fade_level(80.0, 1.0), 0.0);
        assert_eq!(fade_level(80.0, 3.0), 0.0);
    }

    #[test]
    fn a_loop_and_a_stream_have_no_time_left_to_show() {
        let mut p = Playing { position: 12.0, duration: 30.0, ..Playing::default() };
        assert_eq!(p.remaining(), 18.0);
        p.looped = true;
        assert_eq!(p.remaining(), 0.0);
        assert_eq!(Playing { position: 5.0, ..Playing::default() }.remaining(), 0.0);
    }

    /// A stand-in voice: answers a scripted run of mpv commands on a socket.
    fn fake_voice(path: PathBuf, replies: Vec<(Value, Value)>) -> JoinHandle<()> {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for (serial, (command, data)) in replies.into_iter().enumerate() {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["command"], command);
                writeln!(stream, "{}", json!({"request_id": serial + 1, "error": "success", "data": data})).unwrap();
            }
        })
    }

    #[test]
    fn a_voice_tells_which_pad_it_plays() {
        let dir = std::env::temp_dir().join(format!("patch-cart-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cart-3.sock");
        let voice = fake_voice(path.clone(), vec![
            (json!(["get_property", "idle-active"]), json!(false)),
            (json!(["get_property", "user-data/patch-cart"]), json!({"bank": 1, "pad": 7, "label": "Sponsor", "fading": true})),
            (json!(["get_property", "loop-file"]), json!("no")),
            (json!(["get_property", "time-pos"]), json!(4.5)),
            (json!(["get_property", "duration"]), json!(30.0)),
        ]);
        let Voice::Busy(_, p) = look_at(path.clone(), 3) else { panic!("the voice plays") };
        voice.join().unwrap();
        assert_eq!((p.bank, p.pad, p.label.as_str(), p.fading, p.looped, p.voice), (1, 7, "Sponsor", true, false, 3));
        assert_eq!(p.remaining(), 25.5);
        // The voice ended: its socket file is still there, and is cleared.
        assert!(path.exists());
        assert!(matches!(look_at(path.clone(), 3), Voice::Gone));
        assert!(!path.exists(), "a dead voice's socket is removed");
        // An idle voice is the spare.
        let voice = fake_voice(path.clone(), vec![(json!(["get_property", "idle-active"]), json!(true))]);
        assert!(matches!(look_at(path.clone(), 3), Voice::Spare(_)));
        voice.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
