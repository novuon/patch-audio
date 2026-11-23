//! Track analysis for the DJ Booth, run once per file and cached:
//!
//! * **Tempo and beat grid** ([`tempo`]): spectral-flux onset envelope,
//!   comb-filtered autocorrelation over 60–200 BPM with a broad prior around
//!   120, folded into the DJ range 70–180; the grid phase is the offset
//!   whose beats line up with the most onset energy.
//! * **Key** ([`key`]): chromagram from spectral peaks (C2–C7), correlated
//!   with the Krumhansl–Kessler major/minor profiles; shown in musical and
//!   Camelot notation for harmonic mixing.
//! * **Loudness**: EBU R128 integrated loudness, loudness range and true
//!   peak (BS.1770-4, the assembly kernels of `patch-meter`), for gain
//!   matching between decks.
//! * **Waveform overview**: peak and RMS per bin.
//!
//! Decoding goes through `ffmpeg` (any format it reads). Nothing here runs
//! on a real-time thread.

pub mod cache;
mod cache_store;
pub mod detail;
pub mod evaluation;
pub mod grid;
pub mod harmony;
pub mod key;
pub mod region;
pub mod sections;
pub mod structure;
pub mod source;
pub mod tempo;
pub mod vocals;

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, LazyLock, Mutex, OnceLock};

const MAX_DECODE_JOBS: usize = 2;
/// One more decode turn that background work (the librarian) never takes:
/// a deck load or an AI DJ plan does not wait behind a library scan.
const INTERACTIVE_DECODE_JOBS: usize = 1;
static BACKGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process works in the background (the librarian): it leaves the
/// interactive decode turn to the others.
pub fn set_background() {
    BACKGROUND.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Machine-wide turns, held as `flock`s on files in the runtime directory:
/// the UI, the librarian and the engine all decode, and the limits below
/// are for the machine, not for each of them. The kernel drops a dead
/// process's locks. Without a runtime directory, each process counts alone.
struct Turns {
    files: Vec<PathBuf>,
    /// The last this many files are not for background work.
    reserved: usize,
    local: Mutex<usize>,
    freed: Condvar,
}

impl Turns {
    fn new(name: &str, count: usize) -> Turns {
        let dir = if cfg!(test) {
            // Tests never take turns from the Patch that is running.
            Some(std::env::temp_dir().join(format!("patch-turns-{}", std::process::id())))
        } else {
            std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("omarchy-patch"))
        };
        Turns::at(dir, name, count)
    }

    fn at(dir: Option<PathBuf>, name: &str, count: usize) -> Turns {
        Turns::reserving(dir, name, count, 0)
    }

    fn reserving(dir: Option<PathBuf>, name: &str, count: usize, reserved: usize) -> Turns {
        let files = dir
            .filter(|d| std::fs::create_dir_all(d).is_ok())
            .map(|d| (0..count).map(|i| d.join(format!("{name}-{i}.lock"))).collect())
            .unwrap_or_default();
        Turns {
            files,
            reserved,
            local: Mutex::new(count - reserved),
            freed: Condvar::new(),
        }
    }

    fn take(&'static self) -> Turn {
        if self.files.is_empty() {
            let mut free = self.local.lock().unwrap();
            while *free == 0 {
                free = self.freed.wait(free).unwrap();
            }
            *free -= 1;
            return Turn { turns: self, file: None };
        }
        use std::os::fd::AsRawFd;
        let usable = if BACKGROUND.load(std::sync::atomic::Ordering::Relaxed) {
            self.files.len() - self.reserved
        } else {
            self.files.len()
        };
        loop {
            // The reserved turns first, so background work keeps the rest.
            for path in self.files[..usable].iter().rev() {
                let Ok(file) = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(path)
                else {
                    continue;
                };
                // SAFETY: a valid descriptor owned by `file`.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    return Turn { turns: self, file: Some(file) };
                }
            }
            // Decodes take seconds: a short wait between looks costs nothing.
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

/// A turn; given back when dropped (closing the file drops its lock).
struct Turn {
    turns: &'static Turns,
    file: Option<std::fs::File>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        if self.file.take().is_none() {
            *self.turns.local.lock().unwrap() += 1;
            self.turns.freed.notify_one();
        }
    }
}

static DECODE_TURNS: LazyLock<Turns> = LazyLock::new(|| {
    let dir = if cfg!(test) {
        Some(std::env::temp_dir().join(format!("patch-turns-{}", std::process::id())))
    } else {
        std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("omarchy-patch"))
    };
    Turns::reserving(dir, "decode", MAX_DECODE_JOBS + INTERACTIVE_DECODE_JOBS, INTERACTIVE_DECODE_JOBS)
});

/// One of the machine's [`MAX_DECODE_JOBS`] decode turns.
struct DecodePermit(#[allow(dead_code)] Turn);

impl DecodePermit {
    fn acquire() -> Self {
        DecodePermit(DECODE_TURNS.take())
    }
}

/// Audio longer than this (a recorded mix, not a track) is decoded and
/// analysed one file at a time on the machine: its samples alone are 400 MB
/// and up, and the librarian analyses several files at once.
const LONG_AUDIO_SECONDS: usize = if cfg!(test) { 5 } else { 20 * 60 };
static LONG_TURN: LazyLock<Turns> = LazyLock::new(|| Turns::new("long-audio", 1));

/// The one turn for long audio, held until its analysis is done.
pub(crate) struct LongAudioPermit(#[allow(dead_code)] Turn);

impl LongAudioPermit {
    fn acquire() -> Self {
        LongAudioPermit(LONG_TURN.take())
    }
}

/// Bump only for incompatible cache/data-model changes. Algorithm versions
/// below independently invalidate their own cached outputs.
pub const VERSION: u32 = 7;
pub const BEAT_ANALYZER_VERSION: u32 = 11;
pub const GRID_ANALYZER_VERSION: u32 = 6;
pub const KEY_ANALYZER_VERSION: u32 = 2;
pub const HARMONY_ANALYZER_VERSION: u32 = harmony::ANALYZER_VERSION;
pub const LEVELS_ANALYZER_VERSION: u32 = 1;
pub const STRUCTURE_ANALYZER_VERSION: u32 = structure::ANALYZER_VERSION;
/// Analysis runs at this rate (stereo for loudness, mono half-rate for the rest).
pub const RATE: u32 = 44_100;
pub const WAVEFORM_BINS: usize = 600;

/// Independent provenance for the costly analysis groups. Bumping one field
/// refreshes only its outputs, not unrelated analyzers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzerVersions {
    pub beat: u32,
    pub grid: u32,
    pub key: u32,
    #[serde(default)]
    pub harmony: u32,
    pub levels: u32,
    #[serde(default)]
    pub structure: u32,
}

/// The library's analysis generation: the cache schema and every analyzer
/// version in one number that only grows. The library queues a track
/// whenever its stored generation is older, so bumping any one analyzer
/// refreshes exactly the stale groups (see `refresh_stale`) instead of
/// leaving cache reads to reject them with nothing queued to refresh them.
pub const GENERATION: u32 = generation(VERSION, AnalyzerVersions::CURRENT);

const fn generation(schema: u32, a: AnalyzerVersions) -> u32 {
    schema * 10_000 + a.beat + a.grid + a.key + a.harmony + a.levels + a.structure
}

impl AnalyzerVersions {
    pub const CURRENT: Self = Self {
        beat: BEAT_ANALYZER_VERSION,
        grid: GRID_ANALYZER_VERSION,
        key: KEY_ANALYZER_VERSION,
        harmony: HARMONY_ANALYZER_VERSION,
        levels: LEVELS_ANALYZER_VERSION,
        structure: STRUCTURE_ANALYZER_VERSION,
    };

    fn current(self) -> bool {
        self == Self::CURRENT
    }

    /// What decks, the planner and the views need is current: beats, key
    /// and levels. A grid, harmony or structure from an older analyzer stays
    /// well-formed and is used until the background refresh replaces it (an
    /// improvement there hid every track's analysis until it was redone:
    /// no steps waveform, "analysing…", an empty AI candidate pool).
    fn usable(self) -> bool {
        let c = Self::CURRENT;
        self.beat == c.beat && self.key == c.key && self.levels == c.levels
    }
}

/// Refine a DJ grid against the kick across the track. Cache by file and
/// approximate BPM so live decks and auto-mix do not decode on every poll.
/// If the low band has no clear pulse, keep the analysed grid.
#[derive(Clone, PartialEq, Eq, Hash)]
struct KickKey {
    path: PathBuf,
    source: source::Revision,
    bpm: u64,
    epoch: u64,
}

// Explicit reanalysis of a track invalidates what was derived from it in
// memory, including jobs already running: each track counts its reanalyses,
// and a result is kept only under the count it started with. Other tracks'
// work is untouched.
static MEMO_EPOCHS: OnceLock<Mutex<std::collections::HashMap<PathBuf, u64>>> = OnceLock::new();

fn memo_epoch(path: &Path) -> u64 {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let epochs = MEMO_EPOCHS.get_or_init(Default::default).lock().unwrap();
    epochs.get(&path).copied().unwrap_or(0)
}

fn bump_memo_epoch(path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    *MEMO_EPOCHS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(path)
        .or_default() += 1;
}

impl KickKey {
    fn new(path: &Path, bpm: f64) -> Option<Self> {
        let path = std::fs::canonicalize(path).ok()?;
        Some(Self {
            source: source::Revision::of(&path).ok()?,
            epoch: memo_epoch(&path),
            path,
            bpm: bpm.to_bits(),
        })
    }
}

type KickCache = Mutex<cache::Cache<KickKey, Option<tempo::Tempo>>>;
static KICK_CACHE: OnceLock<KickCache> = OnceLock::new();
static KICK_PENDING: OnceLock<Mutex<std::collections::HashSet<KickKey>>> = OnceLock::new();

/// The kick-refined grid if it is known already; None: not worked out
/// yet (it then starts in the background). For a UI thread: it never
/// decodes (the first refinement of a track took ~0.6 s).
pub fn kick_grid_soon(path: &Path, bpm: f64) -> Option<Option<tempo::Tempo>> {
    if !bpm.is_finite() || !(40.0..=300.0).contains(&bpm) {
        return Some(None);
    }
    let key = KickKey::new(path, bpm)?;
    if let Some(v) = KICK_CACHE
        .get_or_init(|| Mutex::new(cache::Cache::new(2 * 1024 * 1024)))
        .lock()
        .unwrap()
        .get(&key)
        .copied()
    {
        return Some(v);
    }
    if KICK_PENDING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(key.clone())
    {
        let path = path.to_path_buf();
        let pending_key = key.clone();
        let started = std::thread::Builder::new()
            .name("patch-kick-grid".into())
            .spawn(move || {
                let _ = kick_grid_for(&path, bpm);
                KICK_PENDING
                    .get_or_init(Default::default)
                    .lock()
                    .unwrap()
                    .remove(&pending_key);
            });
        if started.is_err() {
            KICK_PENDING.get_or_init(Default::default).lock().unwrap().remove(&key);
        }
    }
    None
}

pub fn kick_grid_for(path: &Path, bpm: f64) -> Option<tempo::Tempo> {
    if !bpm.is_finite() || !(40.0..=300.0).contains(&bpm) {
        return None;
    }
    let key = KickKey::new(path, bpm)?;
    let cache = KICK_CACHE.get_or_init(|| Mutex::new(cache::Cache::new(2 * 1024 * 1024)));
    if let Some(v) = cache.lock().unwrap().get(&key).copied() {
        return v;
    }
    let _permit = DecodePermit::acquire();
    // Transport/decode errors are retryable; only a successfully decoded
    // track may be cached as lacking a coherent kick grid.
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "22050", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .watched_output()
        .ok()
        .filter(|o| o.status.success())?;
    let mono: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let result = tempo::kick_grid_fit(&mono, 22_050, bpm).map(|mut fit| {
        if let Some(tag) = read_tags(path).0.filter(|tag| (fit.bpm - tag).abs() < 0.04) {
            fit.bpm = tag;
        }
        fit
    });
    if source::Revision::of(&key.path).ok()? != key.source
        || memo_epoch(&key.path) != key.epoch
    {
        return None;
    }
    let bytes = std::mem::size_of::<KickKey>()
        + key.path.as_os_str().len() * 2
        + std::mem::size_of_val(&result);
    cache.lock().unwrap().insert(key, result, bytes);
    result
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub version: u32,
    /// Per-analyzer provenance so a refresh can preserve unrelated results.
    #[serde(default)]
    pub analyzers: AnalyzerVersions,
    /// Human-authored override loaded from the Patch Library at runtime.
    /// The machine-analysis cache never serializes this value.
    #[serde(skip)]
    pub human_grid: Option<HumanGridOverride>,
    pub duration: f64,
    /// Beats per minute, when the track has a steady beat.
    pub bpm: Option<f64>,
    /// Time of the first beat of the grid, seconds.
    pub beat_offset: f64,
    /// Raw detector support score (legacy field name); not calibrated probability.
    pub bpm_confidence: f32,
    pub key: Option<key::Key>,
    /// Raw profile-margin score (legacy field name); not calibrated probability.
    pub key_confidence: f32,
    /// EBU R128 integrated loudness, LUFS.
    pub lufs: f64,
    /// Loudness range, LU.
    pub lra: f64,
    /// Maximum true peak, dBTP.
    pub true_peak: f64,
    /// Peak and RMS per bin, 0–255 on a linear amplitude scale.
    pub waveform: Vec<[u8; 2]>,
    /// Which grid beat (0…3) starts a bar: downbeats are at
    /// `beat_offset + (downbeat + 4k) · 60 / bpm` (4/4 assumed).
    #[serde(default)]
    pub downbeat: Option<u8>,
    /// Raw downbeat score (legacy field name); not calibrated probability.
    #[serde(default)]
    pub downbeat_confidence: f32,
    /// Energy per second (RMS, 0–255), for structure and energy flow.
    #[serde(default)]
    pub energy: Vec<u8>,
    /// Coarse Level 1 section evidence; raw scores are not probabilities.
    #[serde(default)]
    pub structure: Option<structure::StructureEvidence>,
    /// Where `key` comes from: the audio (Patch's detector) or the file's
    /// own tag (e.g. Beatport's analysis). The detector's answer is kept
    /// in `key_detected` either way.
    #[serde(default)]
    pub key_source: Source,
    #[serde(default)]
    pub key_detected: Option<key::Key>,
    /// The detector's own raw confidence in `key_detected` (`key_confidence`
    /// is that of `key`, the tag's when tagged).
    #[serde(default)]
    pub key_detected_confidence: f32,
    /// The file's BPM tag, when it has one (a check, not the grid).
    #[serde(default)]
    pub tag_bpm: Option<f64>,
    /// The kick grid and how straight the beat runs on it (see [`grid`]):
    /// coarse with every analysis, precise once the AI DJ asked for it.
    /// Analyses from before this existed have none until measured.
    #[serde(default)]
    pub grid: Option<grid::GridQuality>,
    /// Compact time-indexed Level 1 rhythm evidence, independent from the
    /// headline BPM and raw detector-score compatibility fields.
    #[serde(default)]
    pub rhythm: Option<tempo::RhythmEvidence>,
    /// Selectively cached Level 2 analysis for candidate performance regions.
    #[serde(default)]
    pub deep_regions: Vec<region::DeepRegion>,
    /// Local C-first chroma evidence from the key-analysis STFT pass.
    #[serde(default)]
    pub harmony: Option<harmony::HarmonicProfile>,
    /// Where the track sings (see [`vocals`]): measured on the separated
    /// vocals stem in the background, so None until then, or for good
    /// without the stem separator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocals: Option<vocals::VocalEvidence>,
    /// The file this was made from (see [`SourceId`]). None in records from
    /// before it was kept: those are trusted as before until analysed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

/// The source an analysis was made from: the full SHA-256 of the file, and
/// the file revision (device, inode, size, times) it was last checked
/// against. The cache is found by a quick sampled key; this makes sure the
/// record found belongs to these exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceId {
    pub sha256: String,
    pub revision: source::Revision,
}

/// A runtime-only, human-authored grid loaded from Patch Library metadata.
/// It is deliberately absent from cached machine-analysis JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanGridOverride {
    pub bpm: f64,
    pub offset: f64,
    pub beats_per_bar: Option<u8>,
    pub downbeat_phase: Option<u8>,
    /// Optional active human tempo map. Its first segment defines the initial
    /// scalar sync grid; planners can use later segments for local phrasing.
    pub segments: Vec<HumanTempoSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HumanTempoSegment {
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub bpm_start: f64,
    pub bpm_end: f64,
}

/// Provenance of an analysed value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Audio,
    Tag,
}

/// BPM and key from a file's tags (TBPM/bpm; TKEY/initialkey/key, in
/// Camelot or musical notation).
pub fn read_tags(path: &Path) -> (Option<f64>, Option<key::Key>) {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "format_tags:stream_tags",
            "-of",
            "json",
        ])
        .arg(path)
        .stderr(Stdio::null())
        .watched_output();
    let Ok(out) = out else { return (None, None) };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut tags: Vec<(String, String)> = vec![];
    let mut collect = |t: &serde_json::Value| {
        if let Some(m) = t.as_object() {
            for (k, val) in m {
                if let Some(s) = val.as_str() {
                    tags.push((k.to_lowercase(), s.to_string()));
                }
            }
        }
    };
    collect(&v["format"]["tags"]);
    for st in v["streams"].as_array().into_iter().flatten() {
        collect(&st["tags"]);
    }
    let get = |names: &[&str]| {
        tags.iter()
            .find(|(k, _)| names.contains(&k.as_str()))
            .map(|(_, v)| v.trim().to_string())
    };
    let bpm = get(&["tbpm", "bpm"])
        .and_then(|b| b.replace(',', ".").parse::<f64>().ok())
        .filter(|b| (40.0..=300.0).contains(b));
    let key = get(&["tkey", "initialkey", "initial_key", "key"])
        .and_then(|k| key::from_camelot(&k).or_else(|| key::from_name(&k)));
    (bpm, key)
}

impl Analysis {
    /// Gain that brings the track to `target` LUFS without letting its true
    /// peak exceed `ceiling` dBTP, and never more than `max_boost` dB up.
    pub fn match_gain(&self, target: f64, ceiling: f64, max_boost: f64) -> f64 {
        if !self.lufs.is_finite() || self.lufs < -70.0 {
            return 0.0;
        }
        let want = target - self.lufs;
        let headroom = ceiling - self.true_peak;
        want.min(headroom).min(max_boost)
    }
}

/// The longest audio a decode reads (about 3.8 GB of samples at most).
const MAX_DECODE_SECONDS: usize = 3 * 3600;
/// How long a decoder may produce nothing before it is stopped. Tests keep
/// the real limit (a busy CI runner starves ffmpeg for over a second); the
/// stall test gives its own child process a short one.
fn decode_stall() -> std::time::Duration {
    let short = cfg!(test)
        .then(|| std::env::var("PATCH_DECODE_STALL_S").ok()?.parse().ok())
        .flatten();
    std::time::Duration::from_secs(short.unwrap_or(60))
}

/// Kills a decoder child that stops producing output. The kill and
/// `finish` share a lock, so it never signals a pid after the caller has
/// moved on to reaping it (when the pid could be reused).
struct Watchdog {
    state: std::sync::Arc<(Mutex<WatchState>, Condvar)>,
}

struct WatchState {
    last: std::time::Instant,
    done: bool,
    killed: bool,
}

impl Watchdog {
    fn start(pid: u32) -> Watchdog {
        let state = std::sync::Arc::new((
            Mutex::new(WatchState {
                last: std::time::Instant::now(),
                done: false,
                killed: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let stall = decode_stall();
        let _ = std::thread::Builder::new()
            .name("patch-decode-watch".into())
            .spawn(move || {
                let (lock, wake) = &*shared;
                let mut s = lock.lock().unwrap();
                while !s.done {
                    let idle = s.last.elapsed();
                    if idle >= stall {
                        // SAFETY: plain syscall; the child is not reaped
                        // while `done` is false (see `finish`).
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                        s.killed = true;
                        break;
                    }
                    s = wake.wait_timeout(s, stall - idle).unwrap().0;
                }
            });
        Watchdog { state }
    }

    fn progress(&self) {
        self.state.0.lock().unwrap().last = std::time::Instant::now();
    }

    /// Stop watching (before the child is reaped). True if it was killed.
    fn finish(&self) -> bool {
        let mut s = self.state.0.lock().unwrap();
        s.done = true;
        self.state.1.notify_one();
        s.killed
    }
}

/// `Command::output` with the decode watchdog: a child that stops
/// producing for [`decode_stall`] is killed (an error), and its output is
/// capped like [`decode`]'s. stderr is kept only when piped (16 KB).
pub(crate) trait WatchedOutput {
    fn watched_output(&mut self) -> std::io::Result<std::process::Output>;
}

impl WatchedOutput for Command {
    fn watched_output(&mut self) -> std::io::Result<std::process::Output> {
        let mut child = self.stdout(Stdio::piped()).spawn()?;
        let mut stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no output"))?;
        let errors = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut kept = Vec::new();
                let _ = (&mut stderr).take(16 * 1024).read_to_end(&mut kept);
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
                kept
            })
        });
        let watchdog = Watchdog::start(child.id());
        let limit = MAX_DECODE_SECONDS * RATE as usize * 2 * 4;
        let mut out = Vec::new();
        let mut bytes = [0u8; 64 * 1024];
        let read = loop {
            match stdout.read(&mut bytes) {
                Ok(0) => break Ok(()),
                Ok(n) if out.len() + n > limit => {
                    break Err(std::io::Error::other("decoder output too long"));
                }
                Ok(n) => {
                    watchdog.progress();
                    out.extend_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        let stalled = watchdog.finish();
        if read.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        let stderr = errors.and_then(|e| e.join().ok()).unwrap_or_default();
        read?;
        if stalled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "decoder stalled: stopped",
            ));
        }
        Ok(std::process::Output {
            status,
            stdout: out,
            stderr,
        })
    }
}

/// Decode any file ffmpeg reads to interleaved stereo f32 at [`RATE`].
pub fn decode(path: &Path) -> Result<Vec<f32>, String> {
    decode_kept(path).map(|(pcm, _)| pcm)
}

/// [`decode`], with the long-audio turn when the file needed it: the caller
/// keeps it while it holds the samples.
fn decode_kept(path: &Path) -> Result<(Vec<f32>, Option<LongAudioPermit>), String> {
    let mut permit = Some(DecodePermit::acquire());
    let mut long = None;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "2",
            "-ar",
            &RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("ffmpeg: no output".into());
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("ffmpeg: no error stream".into());
    };
    let stderr_reader = match std::thread::Builder::new()
        .name("patch-analysis-errors".into())
        .spawn(move || {
            let mut kept = Vec::with_capacity(4096);
            let mut buf = [0u8; 4096];
            loop {
                let n = match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let keep = n.min(16 * 1024usize.saturating_sub(kept.len()));
                kept.extend_from_slice(&buf[..keep]);
            }
            kept
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ffmpeg stderr reader: {error}"));
        }
    };

    // A decoder that stops producing (a stalled mount, a file ffmpeg loops
    // on) is killed, so it cannot hold its decode permit for good; and no
    // file is read past `MAX_DECODE_SECONDS` (a runaway stream).
    let mut watchdog = Watchdog::start(child.id());
    let mut pcm = Vec::new();
    let mut bytes = [0u8; 64 * 1024];
    let mut carry = [0u8; 4];
    let mut carried = 0;
    loop {
        let n = match stdout.read(&mut bytes) {
            Ok(n) => n,
            Err(error) => {
                watchdog.finish();
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(format!("ffmpeg output: {error}"));
            }
        };
        if n == 0 {
            break;
        }
        watchdog.progress();
        if long.is_none() && pcm.len() > LONG_AUDIO_SECONDS * RATE as usize * 2 {
            // Waiting for the turn without the decode permit: the file
            // holding the turn may need a permit to finish its analysis.
            // ffmpeg simply blocks on its full pipe meanwhile.
            drop(permit.take());
            watchdog.finish();
            long = Some(LongAudioPermit::acquire());
            permit = Some(DecodePermit::acquire());
            watchdog = Watchdog::start(child.id());
        }
        if pcm.len() > MAX_DECODE_SECONDS * RATE as usize * 2 {
            watchdog.finish();
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(format!(
                "longer than {} hours: not analysed",
                MAX_DECODE_SECONDS / 3600
            ));
        }
        let mut offset = 0;
        if carried > 0 {
            let take = (4 - carried).min(n);
            carry[carried..carried + take].copy_from_slice(&bytes[..take]);
            carried += take;
            offset = take;
            if carried == 4 {
                pcm.push(f32::from_le_bytes(carry));
            } else {
                // Keep a partial sample for the next read.
                continue;
            }
        }
        let whole = (n - offset) / 4 * 4;
        pcm.extend(
            bytes[offset..offset + whole]
                .chunks_exact(4)
                .map(|sample| f32::from_le_bytes(sample.try_into().unwrap())),
        );
        let remainder = n - offset - whole;
        carry[..remainder].copy_from_slice(&bytes[offset + whole..n]);
        carried = remainder;
    }
    let stalled = watchdog.finish();
    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    if stalled {
        let _ = stderr_reader.join();
        return Err(format!(
            "ffmpeg stalled for {} s: stopped",
            decode_stall().as_secs()
        ));
    }
    let stderr = stderr_reader
        .join()
        .unwrap_or_else(|_| b"error reader failed".to_vec());
    if !status.success() {
        return Err(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&stderr)
                .lines()
                .next()
                .unwrap_or("failed")
        ));
    }
    if carried != 0 {
        return Err("ffmpeg returned a partial float sample".into());
    }
    drop(permit);
    Ok((pcm, long))
}

/// Analyse interleaved stereo samples at [`RATE`].
pub fn analyse(stereo: &[f32]) -> Analysis {
    analyse_with_scores(stereo).0
}

fn analyse_with_scores(stereo: &[f32]) -> (Analysis, Vec<(f64, f64)>) {
    let frames = stereo.len() / 2;
    // Mono at half rate for tempo and key: plenty of bandwidth (to 11 kHz).
    let mono: Vec<f32> = stereo
        .chunks_exact(4)
        .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
        .collect();
    let half = RATE / 2;

    let (onsets, onset_rate) = tempo::onset_envelope(&mono, half);
    let low_onsets = tempo::low_onset_envelope(&mono, half, 150.0);
    let scan = tempo::scan_from_envelopes(&onsets, &low_onsets, onset_rate, half);
    let t = scan.selected;
    let grid = t
        .as_ref()
        .filter(|t| t.confidence >= 0.3)
        .and_then(|t| grid::measure(&mono, half, t.bpm, t.offset));
    let phased = t.map(|t| kick_phased(t, grid.as_ref()));
    let phases = phased
        .map(|t| tempo::downbeat_hypotheses(&mono, half, &t))
        .unwrap_or_default();
    let downbeat = tempo::downbeat_from_hypotheses(&phases);
    let rhythm = tempo::rhythm_evidence(
        &onsets,
        onset_rate,
        half,
        frames as f64 / RATE as f64,
        phased,
        // Bars as the grid counts them (its downbeat is measured on the
        // kicks), else the detector's.
        grid.as_ref().and_then(|g| g.downbeat).or(downbeat.map(|d| d.0)),
        phases,
        &scan.raw_scores,
    );
    let (k, harmony) = key::detect_with_harmony(&mono, half);

    let mut loud = patch_meter::Loudness::new(RATE);
    let mut tp = [0.0f32; 2];
    for block in stereo.chunks(RATE as usize) {
        loud.push(&block[..block.len() / 2 * 2]);
        let p = loud.true_peak.take();
        tp = [tp[0].max(p[0]), tp[1].max(p[1])];
    }
    let tp = tp[0].max(tp[1]);

    let bin = frames.div_ceil(WAVEFORM_BINS).max(1);
    let waveform = stereo
        .chunks(bin * 2)
        .map(|c| {
            let peak = c.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            let rms = (c.iter().map(|x| (x * x) as f64).sum::<f64>() / c.len().max(1) as f64).sqrt()
                as f32;
            [
                (peak.min(1.0) * 255.0).round() as u8,
                (rms.min(1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    let energy: Vec<u8> = stereo
        .chunks(RATE as usize * 2)
        .map(|c| {
            let rms =
                (c.iter().map(|x| (x * x) as f64).sum::<f64>() / c.len().max(1) as f64).sqrt();
            (rms.min(1.0) * 255.0 * 2.0).min(255.0) as u8
        })
        .collect();
    let structure = structure::analyze(
        &energy,
        &structure::band_energy(stereo, RATE),
        &rhythm.beats,
        frames as f64 / RATE as f64,
    );
    let analysis = Analysis {
        version: VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        human_grid: None,
        source: None,
        duration: frames as f64 / RATE as f64,
        // The kick-phased reading: the same one `beats`, bars, downbeats and
        // `grid` are counted from. The detector's first-pass phase can sit on
        // the off-beat (in club music the open hat is the loudest attack
        // there), which left the stored tempo and first beat half a beat from
        // the beats they describe, and every reader that did not call
        // `use_grid()` on the wrong beat.
        bpm: phased.as_ref().map(|t| t.bpm),
        beat_offset: phased.as_ref().map(|t| t.offset).unwrap_or(0.0),
        bpm_confidence: t.as_ref().map(|t| t.confidence).unwrap_or(0.0),
        key: k.as_ref().map(|k| k.0),
        key_confidence: k.map(|k| k.1).unwrap_or(0.0),
        lufs: loud.integrated(),
        lra: loud.loudness_range(),
        true_peak: if tp > 0.0 {
            20.0 * (tp as f64).log10()
        } else {
            -120.0
        },
        waveform,
        downbeat: downbeat.map(|d| d.0),
        downbeat_confidence: downbeat.map(|d| d.1).unwrap_or(0.0),
        key_source: Source::Audio,
        key_detected: k.as_ref().map(|k| k.0),
        key_detected_confidence: k.as_ref().map_or(0.0, |k| k.1),
        tag_bpm: None,
        grid,
        rhythm: Some(rhythm),
        deep_regions: Vec::new(),
        harmony: Some(harmony),
        vocals: None,
        energy,
        structure: Some(structure),
    };
    (analysis, scan.raw_scores)
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
        });
    base.join("omarchy-patch").join("analysis")
}

/// Cache file for `path` in its current state (path, size, mtime, version).
/// A file's content, as a number: its size and three 64 KB pieces (start,
/// middle, end), FNV-1a. Moved, renamed or copied, a file keeps it; a file
/// whose audio or tags change gets a new one.
fn content_key(path: &Path, len: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    const BLOCK: u64 = 64 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(&len.to_le_bytes());
    let mut buf = vec![0u8; BLOCK as usize];
    for start in [0, len.saturating_sub(BLOCK) / 2, len.saturating_sub(BLOCK)] {
        f.seek(SeekFrom::Start(start)).ok()?;
        let n = f.read(&mut buf).ok()?;
        eat(&buf[..n]);
    }
    Some(h)
}

/// Where the analysis of `path` is cached: by content and data-schema version,
/// so it follows a file wherever moved and analyzer versions can refresh in place.
fn cache_path(path: &Path) -> Option<PathBuf> {
    // One file, one entry: relative paths and symlinks resolve first.
    let path = std::fs::canonicalize(path).ok()?;
    let m = std::fs::metadata(&path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    // The content is read once per file version (cached() runs many times
    // a second for a playing deck).
    type Keys = std::collections::HashMap<(PathBuf, u64, u128), u64>;
    static KEYS: std::sync::LazyLock<std::sync::Mutex<Keys>> =
        std::sync::LazyLock::new(Default::default);
    let id = (path.clone(), m.len(), mtime);
    let known = KEYS.lock().unwrap().get(&id).copied();
    let content = match known {
        Some(k) => k,
        None => {
            let k = content_key(&path, m.len())?;
            let mut keys = KEYS.lock().unwrap();
            if keys.len() >= 8192 {
                keys.clear();
            }
            keys.insert(id, k);
            k
        }
    };
    let mut h = content;
    for b in VERSION.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let file = cache_dir().join(format!("c{h:016x}.json"));
    // Cached before, by path: take it over (no second analysis).
    if !file.exists()
        && let Some(old) = legacy_cache_path(&path, m.len(), mtime)
        && old.exists()
    {
        let _ = std::fs::rename(old.with_extension("wfd"), file.with_extension("wfd"));
        let _ = std::fs::rename(&old, &file);
    }
    Some(file)
}

/// The cache entry's name before it followed content (path, size, mtime).
fn legacy_cache_path(path: &Path, len: u64, mtime: u128) -> Option<PathBuf> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path
        .to_string_lossy()
        .bytes()
        .chain(len.to_le_bytes())
        .chain(mtime.to_le_bytes())
        .chain(VERSION.to_le_bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Some(cache_dir().join(format!("{h:016x}.json")))
}

/// Forget the cached analysis of `path` (it is analysed again next time).
pub fn forget(path: &Path) {
    bump_memo_epoch(path);
    if let Some(c) = cache_path(path) {
        let _ = cache_store::remove(&c, None);
    }
}

/// Remove obsolete, expired or over-budget complete cache entries. Newer
/// schemas are kept. A detail sidecar without its record, or an unfinished
/// write, may belong to a writer still at work: only after a day is it
/// taken for a leftover. Returns (files, bytes) freed.
pub fn prune_cache() -> (usize, u64) {
    prune_cache_dir(&cache_dir())
}

fn prune_cache_dir(dir_path: &Path) -> (usize, u64) {
    #[derive(serde::Deserialize)]
    struct V { version: u32 }
    const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(365 * 24 * 60 * 60);
    let Ok(dir) = std::fs::read_dir(dir_path) else { return (0, 0) };
    let mut removed = (0, 0);
    let mut entries = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        let extension = path.extension().and_then(|s| s.to_str());
        let leftover = match extension {
            Some("pending" | "tmp") => true,
            Some("wfd") => !path.with_extension("json").exists(),
            _ => false,
        };
        if leftover {
            const SETTLED: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
            if let Ok(metadata) = std::fs::metadata(&path)
                && metadata.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age > SETTLED)
                && std::fs::remove_file(&path).is_ok()
            {
                removed.0 += 1;
                removed.1 += metadata.len();
            }
            continue;
        }
        if extension != Some("json") { continue; }
        let Ok(revision) = source::Revision::of(&path) else { continue };
        let version = std::fs::read(&path).ok()
            .and_then(|text| serde_json::from_slice::<V>(&text).ok()).map(|v| v.version);
        if version.is_none_or(|v| v < VERSION) {
            let gone = cache_store::remove(&path, Some(revision)).unwrap_or_default();
            removed.0 += gone.0; removed.1 += gone.1;
        } else if version == Some(VERSION) && let Ok(metadata) = std::fs::metadata(&path) {
            let detail_bytes = std::fs::metadata(path.with_extension("wfd")).map_or(0, |m| m.len());
            let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((modified, path, revision, metadata.len().saturating_add(detail_bytes)));
        }
    }
    let now = std::time::SystemTime::now();
    let mut used = entries.iter().map(|(_, _, _, n)| *n).sum::<u64>();
    entries.sort_by_key(|(modified, _, _, _)| *modified);
    for (modified, path, revision, _) in entries {
        let expired = now.duration_since(modified).unwrap_or_default() > MAX_AGE;
        if expired || used > MAX_BYTES {
            let gone = cache_store::remove(&path, Some(revision)).unwrap_or_default();
            removed.0 += gone.0; removed.1 += gone.1;
            used = used.saturating_sub(gone.1);
        }
    }
    removed
}

/// The cached analysis of `path`, if it is up to date.
pub fn cached(path: &Path) -> Option<Analysis> {
    cached_shared(path).map(|a| (*a).clone())
}

/// Shared parsed analysis; cache hits need not copy waveform buffers.
pub fn cached_shared(path: &Path) -> Option<std::sync::Arc<Analysis>> {
    cached_file(&analysis_cache_path(path)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trust {
    /// The record was made from these bytes (or predates source checks).
    Yes,
    /// It was made from other bytes that happen to share the sampled key.
    No,
    /// The file changed revision since the record was checked; its hash is
    /// being worked out in the background.
    Pending,
}

/// Full digests worked out in this process, by file revision.
static DIGESTS: LazyLock<Mutex<cache::Cache<source::Revision, String>>> =
    LazyLock::new(|| Mutex::new(cache::Cache::new(1024 * 1024)));
static HASHING: LazyLock<Mutex<std::collections::HashSet<PathBuf>>> =
    LazyLock::new(Default::default);

/// The file's full digest now: from the memo, or read here (a worker).
fn digest(path: &Path) -> Option<source::VerifiedSource> {
    let v = source::verify(path, || false).ok()?;
    DIGESTS.lock().unwrap().insert(v.revision, v.sha256(), 160);
    Some(v)
}

/// Whether the record at `file` belongs to the file at `path`: free when the
/// file has the revision the record was last checked at; otherwise its hash
/// decides, worked out off this thread (a UI may be asking).
fn trust(path: &Path, file: &Path) -> Trust {
    let Some(made_from) = record_source(file) else {
        return Trust::Yes;
    };
    let Ok(revision) = source::Revision::of(path) else {
        return Trust::No;
    };
    if revision.same_file(&made_from.revision) {
        return Trust::Yes;
    }
    if let Some(sha) = DIGESTS.lock().unwrap().get(&revision) {
        return if *sha == made_from.sha256 { Trust::Yes } else { Trust::No };
    }
    let path = path.to_path_buf();
    if HASHING.lock().unwrap().insert(path.clone()) {
        let file = file.to_path_buf();
        let job = path.clone();
        let started = std::thread::Builder::new()
            .name("patch-source-hash".into())
            .spawn(move || {
                if let Some(v) = digest(&job) {
                    recheck_source(&file, &v);
                }
                HASHING.lock().unwrap().remove(&job);
            });
        if started.is_err() {
            HASHING.lock().unwrap().remove(&path);
        }
    }
    Trust::Pending
}

/// A record whose file was moved or copied (same bytes, new revision) is
/// stamped with the new revision, so its next lookups are free again.
fn recheck_source(file: &Path, v: &source::VerifiedSource) {
    let _ = cache_store::update(file, |latest| {
        let mut record = latest.ok_or("analysis missing")?;
        if record["source"]["sha256"].as_str() == Some(&v.sha256()) {
            record["source"]["revision"] =
                serde_json::to_value(v.revision).map_err(|e| e.to_string())?;
        }
        Ok(record)
    });
}

/// The `source` of the record at `file`, memoised by the record's own
/// stamp (a library pass asks this for every track).
fn record_source(file: &Path) -> Option<SourceId> {
    #[derive(Deserialize)]
    struct Head {
        #[serde(default)]
        source: Option<SourceId>,
    }
    type Memo = cache::Cache<PathBuf, (CacheStamp, Option<SourceId>)>;
    // ~500 bytes an entry: 32 MB holds 60 000 records. 4 MB held 8 000, and
    // a 19 000-track library then read and parsed every record again on
    // each pass (the Library window's reader at 100 % for minutes).
    static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(|| Mutex::new(cache::Cache::new(32 * 1024 * 1024)));
    let stamp = file_stamp(file)?;
    if let Some((t, s)) = MEMO.lock().unwrap().get(&file.to_path_buf())
        && *t == stamp
    {
        return s.clone();
    }
    let head: Head = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    MEMO.lock().unwrap().insert(
        file.to_path_buf(),
        (stamp, head.source.clone()),
        256 + file.as_os_str().len() * 2,
    );
    head.source
}

/// The library should analyse `path` again: its cache record is gone, or it
/// belongs to other bytes.
pub fn cache_needs_analysis(path: &Path) -> bool {
    cache_path(path).is_none_or(|file| !file.exists() || trust(path, &file) == Trust::No)
}

/// File identity includes size and nanosecond mtime, and is checked on every
/// lookup so background analysis updates become visible immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStamp {
    pub len: u64,
    pub modified: std::time::SystemTime,
}

pub fn file_stamp(path: &Path) -> Option<CacheStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(CacheStamp {
        len: m.len(),
        modified: m.modified().ok()?,
    })
}

/// Where `path`'s analysis record lives, whether or not it is there or
/// trusted yet: for a caller that watches the file and only checks it
/// (`analysis_cache_path`) when it changes.
pub fn analysis_cache_location(path: &Path) -> Option<PathBuf> {
    cache_path(path)
}

/// Resolve once when loading both analysis and detailed waveform sidecars.
/// None while the record there is not known to be this file's.
pub fn analysis_cache_path(path: &Path) -> Option<PathBuf> {
    let file = cache_path(path)?;
    (!file.exists() || trust(path, &file) == Trust::Yes).then_some(file)
}

/// An analysis file read as it is now, not kept in the shared cache (for a
/// pass over the whole library that needs a few numbers per track).
pub fn read_file(file: &Path) -> Option<Analysis> {
    let a: Analysis = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    (a.version == VERSION && a.analyzers.usable()).then_some(a)
}

pub fn cached_file(file: &Path) -> Option<std::sync::Arc<Analysis>> {
    let analysis = cached_file_any(file)?;
    analysis.analyzers.usable().then_some(analysis)
}

/// Read a schema-current record even if an individual analyzer is stale.
/// Only `analyse_file` uses this path to preserve unaffected output while
/// selectively refreshing the outdated analyzer group.
fn cached_file_any(file: &Path) -> Option<std::sync::Arc<Analysis>> {
    use std::sync::{Arc, LazyLock};
    type Memo = cache::Cache<PathBuf, (CacheStamp, Arc<Analysis>)>;
    static MEMO: LazyLock<Mutex<Memo>> =
        LazyLock::new(|| Mutex::new(cache::Cache::new(16 * 1024 * 1024)));
    let stamp = file_stamp(file)?;
    if let Some((t, a)) = MEMO.lock().unwrap().get(&file.to_path_buf())
        && *t == stamp
    {
        return Some(a.clone());
    }
    let text = std::fs::read_to_string(file).ok()?;
    let a: Analysis = serde_json::from_str(&text).ok()?;
    if a.version != VERSION {
        return None;
    }
    // If replaced during the read, retry on the next lookup rather than
    // publishing a value with an identity it never had.
    if file_stamp(file)? != stamp {
        return None;
    }
    let bytes = a.heap_bytes() + std::mem::size_of::<Analysis>() + file.as_os_str().len() * 2 + 256;
    let a = Arc::new(a);
    MEMO.lock()
        .unwrap()
        .insert(file.to_path_buf(), (stamp, a.clone()), bytes);
    Some(a)
}

impl Analysis {
    pub fn heap_bytes(&self) -> usize {
        self.waveform.capacity() * 2
            + self.energy.capacity()
            + self.grid.as_ref().map_or(0, |g| {
                g.beatless.capacity() * std::mem::size_of::<[f32; 2]>()
            })
            + self.rhythm.as_ref().map_or(0, |r| {
                r.beats.capacity() * std::mem::size_of::<tempo::BeatEvent>()
                    + r.regions.capacity() * std::mem::size_of::<tempo::RhythmRegion>()
                    + r.tempo_hypotheses.capacity() * std::mem::size_of::<tempo::TempoHypothesis>()
                    + r.meter_hypotheses.capacity() * std::mem::size_of::<tempo::MeterHypothesis>()
            })
            + self.structure.as_ref().map_or(0, |s| {
                s.boundaries.capacity() * std::mem::size_of::<structure::BoundaryEvidence>()
                    + s.sections.capacity() * std::mem::size_of::<structure::CoarseSection>()
            })
            + self.harmony.as_ref().map_or(0, |profile| {
                profile.frames.capacity() * std::mem::size_of::<harmony::HarmonicFrame>()
            })
            + self.deep_regions.capacity() * std::mem::size_of::<region::DeepRegion>()
            + self
                .vocals
                .as_ref()
                .map_or(0, |v| v.level.capacity() + v.model.capacity())
            + self
                .deep_regions
                .iter()
                .map(|r| {
                    r.source_identity.capacity()
                        + r.key_hypotheses.capacity() * std::mem::size_of::<region::KeyHypothesis>()
                        + r.harmony_frames.capacity() * std::mem::size_of::<region::HarmonyFrame>()
                        + r.harmony_frames
                            .iter()
                            .map(|frame| {
                                frame.chords.capacity()
                                    * std::mem::size_of::<region::ChordHypothesis>()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }

    /// All decision inputs, without the waveform used only for drawing.
    pub fn for_dj(&self) -> Analysis {
        Analysis {
            version: self.version,
            analyzers: self.analyzers,
            human_grid: self.human_grid.clone(),
            source: self.source.clone(),
            duration: self.duration,
            bpm: self.bpm,
            beat_offset: self.beat_offset,
            bpm_confidence: self.bpm_confidence,
            key: self.key,
            key_confidence: self.key_confidence,
            lufs: self.lufs,
            lra: self.lra,
            true_peak: self.true_peak,
            waveform: Vec::new(),
            downbeat: self.downbeat,
            downbeat_confidence: self.downbeat_confidence,
            energy: self.energy.clone(),
            key_source: self.key_source,
            key_detected: self.key_detected,
            key_detected_confidence: self.key_detected_confidence,
            tag_bpm: self.tag_bpm,
            grid: self.grid.clone(),
            rhythm: self.rhythm.clone(),
            deep_regions: self.deep_regions.clone(),
            harmony: self.harmony.clone(),
            structure: self.structure.clone(),
            vocals: self.vocals.clone(),
        }
    }
}

/// Analyse `path` (or return the cached result) and cache it.
pub fn analyse_file(path: &Path) -> Result<Analysis, String> {
    let source_revision = source::Revision::of(path).map_err(|e| e.to_string())?;
    let epoch = memo_epoch(path);
    let cache_file = cache_path(path);
    // Whose record the sampled key found: free when the file still has the
    // revision it was checked at, else (or for an older record without a
    // source) the file is hashed here, on this worker.
    let made_from = cache_file.as_deref().and_then(record_source);
    let sha = match &made_from {
        Some(m) if m.revision.same_file(&source_revision) => Some(m.sha256.clone()),
        _ => digest(path).filter(|v| v.revision == source_revision).map(|v| v.sha256()),
    };
    let ours = made_from
        .as_ref()
        .is_none_or(|m| sha.as_ref() == Some(&m.sha256));
    let stamp = sha.clone().map(|sha256| SourceId {
        sha256,
        revision: source_revision,
    });
    let cached = cache_file
        .as_ref()
        .filter(|_| ours)
        .and_then(|file| cached_file_any(file))
        .map(|analysis| (*analysis).clone());
    if let Some(a) = cached.as_ref().filter(|a| a.analyzers.current()) {
        // Moved, copied or from before sources were kept: stamp it now.
        if let (Some(file), Some(stamp)) = (&cache_file, &stamp)
            && made_from.as_ref() != Some(stamp)
        {
            let _ = cache_store::update(file, |latest| {
                let mut record = latest.ok_or("analysis missing")?;
                if record.get("source").is_none_or(|s| s["sha256"].as_str() == Some(&stamp.sha256)) {
                    record["source"] = serde_json::to_value(stamp).map_err(|e| e.to_string())?;
                }
                Ok(record)
            });
        }
        return Ok(a.clone());
    }
    // A source BPM tag only needs to correct a newly computed beat analysis.
    // Reapplying it during a key/levels-only refresh would have no current raw
    // tempo alternatives to rebuild the corrected rhythm evidence from.
    let beat_analysis_stale = cached
        .as_ref()
        .is_none_or(|a| a.analyzers.beat != BEAT_ANALYZER_VERSION);
    let baseline = cached
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(serde_json::Value::Null);
    // A long file keeps its turn until analysed (see `LONG_AUDIO_SECONDS`).
    let (pcm, _long) = decode_kept(path)?;
    let (mut a, raw_tempo_scores) = match cached {
        Some(mut a) => {
            let scores = refresh_stale(&mut a, &pcm);
            (a, scores)
        }
        None => analyse_with_scores(&pcm),
    };
    // Tags are source facts: a tagged key is what the track is sold as
    // (usually a professional analysis) and is used for mixing; the tagged
    // BPM corrects a 3:2, 2:3 or octave error of the detector (never its
    // precision): a 150 BPM track tagged so is not mixed as 75.
    let (tag_bpm, tag_key) = read_tags(path);
    a.tag_bpm = tag_bpm;
    // Records from before `key_detected`: their key is the detector's. A
    // tagged key never overwrites the detector's answer (provenance).
    if a.key_detected.is_none() && a.key_source == Source::Audio {
        a.key_detected = a.key;
        a.key_detected_confidence = a.key_confidence;
    }
    if let Some(k) = tag_key {
        a.key = Some(k);
        a.key_source = Source::Tag;
        a.key_confidence = 0.9;
    }
    if beat_analysis_stale && let (Some(t), Some(b)) = (tag_bpm, a.bpm) {
        for ratio in [1.5, 2.0 / 3.0, 2.0, 0.5] {
            if ((b * ratio) / t - 1.0).abs() < 0.02 && (60.0..=200.0).contains(&(b * ratio)) {
                let corrected = (b * ratio * 100.0).round() / 100.0;
                a.bpm = Some(corrected);
                // Refit phase and bar origin after correcting a half/double
                // time BPM hypothesis using the file's trusted tempo tag.
                let mono: Vec<f32> = pcm
                    .chunks_exact(4)
                    .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
                    .collect();
                if let Some(offset) = tempo::grid_offset(&mono, RATE / 2, corrected) {
                    a.beat_offset = offset;
                    let tempo = tempo::Tempo {
                        bpm: corrected,
                        offset,
                        confidence: a.bpm_confidence,
                    };
                    // As in a full analysis: the grid first, then beats,
                    // bars and downbeats from its kick phase.
                    a.grid = grid::measure(&mono, RATE / 2, corrected, offset);
                    let phased = kick_phased(tempo, a.grid.as_ref());
                    let phases = tempo::downbeat_hypotheses(&mono, RATE / 2, &phased);
                    let downbeat = tempo::downbeat_from_hypotheses(&phases);
                    a.downbeat = downbeat.map(|d| d.0);
                    a.downbeat_confidence = downbeat.map(|d| d.1).unwrap_or(0.0);
                    let (onsets, fr) = tempo::onset_envelope(&mono, RATE / 2);
                    a.rhythm = Some(tempo::rhythm_evidence(
                        &onsets,
                        fr,
                        RATE / 2,
                        a.duration,
                        Some(phased),
                        a.grid.as_ref().and_then(|g| g.downbeat).or(a.downbeat),
                        phases,
                        &raw_tempo_scores,
                    ));
                }
                break;
            }
        }
    }
    if source::Revision::of(path).map_err(|e| e.to_string())? != source_revision {
        return Err("analysis source changed during analysis".into());
    }
    // The analysis stands on its own: a cache that cannot take it (a full
    // disk, a busy entry, a newer result from another process) costs a
    // later recomputation, never this result.
    if let Some(c) = cache_file {
        let detail_path = c.with_extension("wfd");
        if (!ours || !detail_path.exists())
            && let Err(e) = detail::write(&detail_path, &detail::build(&pcm))
        {
            eprintln!("analysis: detail of {} not cached: {e}", path.display());
        }
        a.source = stamp.clone();
        let computed = serde_json::to_value(&a).map_err(|e| e.to_string())?;
        let committed = cache_store::update(&c, |latest| {
            // Another file's record under the same sampled key: replaced.
            if let (Some(latest), Some(stamp)) = (&latest, &stamp)
                && latest["source"]["sha256"].as_str().is_some_and(|sha| sha != stamp.sha256)
            {
                return Ok(computed.clone());
            }
            if source::Revision::of(path).map_err(|e| e.to_string())? != source_revision
                || memo_epoch(path) != epoch
            {
                return Err("source changed or reanalysis requested before cache commit".into());
            }
            if latest
                .as_ref()
                .is_some_and(|v| v["version"].as_u64() != Some(VERSION as u64))
            {
                return Err("analysis schema changed before cache commit".into());
            }
            cache_store::merge(
                &baseline,
                &computed,
                &latest.unwrap_or(serde_json::Value::Null),
            )
        })
        .and_then(|merged| serde_json::from_value(merged).map_err(|e| e.to_string()));
        match committed {
            Ok(merged) => a = merged,
            Err(e) => eprintln!("analysis: {} not cached: {e}", path.display()),
        }
    }
    Ok(a)
}

/// The track's analysis has no current vocal evidence yet (and has an
/// analysis to keep it with).
pub fn vocals_due(path: &Path) -> bool {
    cached_shared(path).is_some_and(|a| a.vocals.as_ref().is_none_or(|v| !v.current()))
}

/// Keep measured vocal evidence with the cached analysis (as a grid is
/// kept: the record is read again under the lock, so a refresh in between
/// is not undone). False if the track has no analysis to keep it with.
pub fn store_vocals(path: &Path, v: vocals::VocalEvidence) -> bool {
    let Some(file) = cache_path(path) else {
        return false;
    };
    cache_store::update(&file, |latest| {
        let mut a: Analysis =
            serde_json::from_value(latest.ok_or("analysis missing")?).map_err(|e| e.to_string())?;
        if a.version != VERSION {
            return Err("analysis schema changed".into());
        }
        a.vocals = Some(v);
        serde_json::to_value(a).map_err(|e| e.to_string())
    })
    .is_ok()
}

/// The detected tempo, phased (and timed) as the grid measured it on the
/// kicks. The detector's first-pass offset can sit on the off-beat (in club
/// music the open hat is the loudest attack there); counting the stored
/// beats, bars and downbeats from it put them between the kicks on about
/// half of a tech-house crate, while sync used the grid. A grid at another
/// tempo (beyond 1 %) is a different reading and leaves the tempo as is.
fn kick_phased(t: tempo::Tempo, grid: Option<&grid::GridQuality>) -> tempo::Tempo {
    match grid {
        Some(g) if g.bpm.is_finite() && g.offset.is_finite() && (g.bpm / t.bpm - 1.0).abs() < 0.01 => {
            tempo::Tempo {
                bpm: g.bpm,
                offset: g.offset,
                ..t
            }
        }
        _ => t,
    }
}

/// Refresh only analyzer groups whose independent provenance is stale.
fn refresh_stale(a: &mut Analysis, stereo: &[f32]) -> Vec<(f64, f64)> {
    let frames = stereo.len() / 2;
    let duration = frames as f64 / RATE as f64;
    let half = RATE / 2;
    let mono: Vec<f32> = stereo
        .chunks_exact(4)
        .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
        .collect();
    let mut raw_tempo_scores = Vec::new();

    if a.analyzers.beat != BEAT_ANALYZER_VERSION {
        let (onsets, onset_rate) = tempo::onset_envelope(&mono, half);
        let low_onsets = tempo::low_onset_envelope(&mono, half, 150.0);
        let scan = tempo::scan_from_envelopes(&onsets, &low_onsets, onset_rate, half);
        let selected = scan.selected;
        // The grid first: beats, bars and downbeats count from its phase.
        a.grid = selected
            .filter(|t| t.confidence >= 0.3)
            .and_then(|t| grid::measure(&mono, half, t.bpm, t.offset));
        a.analyzers.grid = GRID_ANALYZER_VERSION;
        let phased = selected.map(|t| kick_phased(t, a.grid.as_ref()));
        a.bpm = phased.map(|t| t.bpm);
        a.beat_offset = phased.map(|t| t.offset).unwrap_or(0.0);
        a.bpm_confidence = selected.map(|t| t.confidence).unwrap_or(0.0);
        let phases = phased
            .map(|t| tempo::downbeat_hypotheses(&mono, half, &t))
            .unwrap_or_default();
        let downbeat = tempo::downbeat_from_hypotheses(&phases);
        a.downbeat = downbeat.map(|d| d.0);
        a.downbeat_confidence = downbeat.map(|d| d.1).unwrap_or(0.0);
        a.rhythm = Some(tempo::rhythm_evidence(
            &onsets,
            onset_rate,
            half,
            duration,
            phased,
            a.grid.as_ref().and_then(|g| g.downbeat).or(a.downbeat),
            phases,
            &scan.raw_scores,
        ));
        raw_tempo_scores = scan.raw_scores;
        a.analyzers.beat = BEAT_ANALYZER_VERSION;
    }

    if a.analyzers.grid != GRID_ANALYZER_VERSION {
        a.grid = a
            .bpm
            .filter(|_| a.bpm_confidence >= 0.3)
            .and_then(|bpm| grid::measure(&mono, half, bpm, a.beat_offset));
        a.analyzers.grid = GRID_ANALYZER_VERSION;
    }
    // Store the phase `use_grid()` applies anyway. Decks sync on the measured
    // grid, so a cached tempo and first beat that disagree with it are simply
    // wrong: a detector locked to the off-beat left them half a beat out.
    if let Some(bpm) = a.bpm {
        let phased = kick_phased(
            tempo::Tempo {
                bpm,
                offset: a.beat_offset,
                confidence: a.bpm_confidence,
            },
            a.grid.as_ref(),
        );
        a.bpm = Some(phased.bpm);
        a.beat_offset = phased.offset;
    }

    if a.analyzers.key != KEY_ANALYZER_VERSION || a.analyzers.harmony != HARMONY_ANALYZER_VERSION {
        let (detected, harmony) = key::detect_with_harmony(&mono, half);
        a.key_detected = detected.map(|k| k.0);
        a.key_detected_confidence = detected.map(|k| k.1).unwrap_or(0.0);
        a.harmony = Some(harmony);
        if a.key_source == Source::Audio {
            a.key = a.key_detected;
            a.key_confidence = a.key_detected_confidence;
        }
        a.analyzers.key = KEY_ANALYZER_VERSION;
        a.analyzers.harmony = HARMONY_ANALYZER_VERSION;
    }

    if a.analyzers.levels != LEVELS_ANALYZER_VERSION {
        let mut loud = patch_meter::Loudness::new(RATE);
        let mut true_peak = [0.0f32; 2];
        for block in stereo.chunks(RATE as usize) {
            loud.push(&block[..block.len() / 2 * 2]);
            let peak = loud.true_peak.take();
            true_peak = [true_peak[0].max(peak[0]), true_peak[1].max(peak[1])];
        }
        let peak = true_peak[0].max(true_peak[1]);
        a.lufs = loud.integrated();
        a.lra = loud.loudness_range();
        a.true_peak = if peak > 0.0 {
            20.0 * (peak as f64).log10()
        } else {
            -120.0
        };
        let bin = frames.div_ceil(WAVEFORM_BINS).max(1);
        a.waveform = stereo
            .chunks(bin * 2)
            .map(|c| {
                let peak = c.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                let rms = (c.iter().map(|x| (x * x) as f64).sum::<f64>() / c.len().max(1) as f64)
                    .sqrt() as f32;
                [
                    (peak.min(1.0) * 255.0).round() as u8,
                    (rms.min(1.0) * 255.0).round() as u8,
                ]
            })
            .collect();
        a.energy = stereo
            .chunks(RATE as usize * 2)
            .map(|c| {
                let rms =
                    (c.iter().map(|x| (x * x) as f64).sum::<f64>() / c.len().max(1) as f64).sqrt();
                (rms.min(1.0) * 255.0 * 2.0).min(255.0) as u8
            })
            .collect();
        a.analyzers.levels = LEVELS_ANALYZER_VERSION;
    }

    if a.analyzers.structure != STRUCTURE_ANALYZER_VERSION {
        a.structure = Some(structure::analyze(
            &a.energy,
            &structure::band_energy(stereo, RATE),
            a.rhythm.as_ref().map(|r| r.beats.as_slice()).unwrap_or(&[]),
            duration,
        ));
        a.analyzers.structure = STRUCTURE_ANALYZER_VERSION;
    }

    a.duration = duration;
    a.version = VERSION;
    a.analyzers = AnalyzerVersions::CURRENT;
    raw_tempo_scores
}

/// Short-time Fourier magnitudes: `hop`-spaced Hann-windowed frames of
/// `win` samples (a power of two). Returns one magnitude row per frame.
pub(crate) fn stft(x: &[f32], win: usize, hop: usize, mut each: impl FnMut(usize, &[f64])) {
    let window: Vec<f64> = (0..win)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / win as f64).cos())
        .collect();
    let (mut re, mut im) = (vec![0.0; win], vec![0.0; win]);
    let mut mag = vec![0.0; win / 2 + 1];
    let mut frame = 0;
    let mut start = 0;
    while start + win <= x.len() {
        for i in 0..win {
            re[i] = x[start + i] as f64 * window[i];
            im[i] = 0.0;
        }
        patch_meter::spectrum::fft(&mut re, &mut im);
        for (k, m) in mag.iter_mut().enumerate() {
            *m = (re[k] * re[k] + im[k] * im[k]).sqrt();
        }
        each(frame, &mag);
        frame += 1;
        start += hop;
    }
}

/// The detailed waveform of `path` (if analysed).
pub fn detail_of(path: &Path) -> Option<detail::Detail> {
    detail::read(&analysis_cache_path(path)?.with_extension("wfd"))
}

/// Rebuild the scrolling display asset without remeasuring musical facts.
/// Intended for a background worker, never for a frame or audio callback.
pub fn ensure_detail(path: &Path) -> Result<(), String> {
    if detail_of(path).is_some_and(|d| {
        d.columns_per_second == detail::COLUMNS_PER_SECOND
    }) {
        return Ok(());
    }
    let pcm = decode(path)?;
    let file = analysis_cache_path(path).ok_or("cannot identify waveform source")?;
    let rebuilt = detail::build(&pcm);
    std::fs::create_dir_all(file.parent().ok_or("analysis cache has no parent")?)
        .map_err(|e| e.to_string())?;
    detail::write(&file.with_extension("wfd"), &rebuilt).map_err(|e| e.to_string())
}

/// Recover planner-only arrays from the audio while retaining the saved
/// tempo, key, grid, structure and level measurements.
pub fn recover_dj(path: &Path, mut saved: Analysis) -> Result<Analysis, String> {
    let pcm = decode(path)?;
    restore_decision_arrays(&mut saved, &pcm);
    let file = analysis_cache_path(path).ok_or("cannot identify analysis source")?;
    std::fs::create_dir_all(file.parent().ok_or("analysis cache has no parent")?)
        .map_err(|e| e.to_string())?;
    let computed = serde_json::to_value(&saved).map_err(|e| e.to_string())?;
    let stored = cache_store::update(&file, |latest| {
        let mut latest = latest.unwrap_or_else(|| computed.clone());
        if latest["version"] != computed["version"]
            || latest["analyzers"] != computed["analyzers"]
        {
            return Ok(latest);
        }
        for key in ["waveform", "harmony"] {
            let missing = latest.get(key).is_none_or(|v| {
                v.is_null() || v.as_array().is_some_and(Vec::is_empty)
            });
            if missing && let Some(value) = computed.get(key) {
                latest[key] = value.clone();
            }
        }
        let beats_empty = latest["rhythm"].get("beats").is_none_or(|beats| {
            beats.as_array().is_some_and(Vec::is_empty)
                || beats.get("offsets").and_then(serde_json::Value::as_array).is_some_and(Vec::is_empty)
        });
        if beats_empty && let Some(beats) = computed["rhythm"].get("beats") {
            latest["rhythm"]["beats"] = beats.clone();
        }
        Ok(latest)
    })?;
    let recovered = serde_json::from_value(stored).map_err(|e| e.to_string())?;
    detail::write(&file.with_extension("wfd"), &detail::build(&pcm))
        .map_err(|e| e.to_string())?;
    Ok(recovered)
}

fn restore_decision_arrays(saved: &mut Analysis, pcm: &[f32]) {
    if saved.waveform.is_empty() {
        let bin = (pcm.len() / 2).div_ceil(WAVEFORM_BINS).max(1);
        saved.waveform = pcm
            .chunks(bin * 2)
            .map(|chunk| {
                let peak = chunk.iter().fold(0.0f32, |max, sample| max.max(sample.abs()));
                let rms = (chunk
                    .iter()
                    .map(|sample| (*sample as f64).powi(2))
                    .sum::<f64>()
                    / chunk.len().max(1) as f64)
                .sqrt();
                [
                    (peak.min(1.0) * 255.0).round() as u8,
                    (rms.min(1.0) * 255.0).round() as u8,
                ]
            })
            .collect();
    }
    let mono: Vec<f32> = pcm
        .chunks_exact(4)
        .map(|frame| (frame[0] + frame[1] + frame[2] + frame[3]) * 0.25)
        .collect();
    if saved.harmony.is_none() {
        saved.harmony = Some(key::detect_with_harmony(&mono, RATE / 2).1);
    }
    if saved.rhythm.as_ref().is_none_or(|r| r.beats.is_empty()) {
        let (onsets, onset_rate) = tempo::onset_envelope(&mono, RATE / 2);
        let selected = saved.bpm.map(|bpm| tempo::Tempo {
            bpm,
            offset: saved.beat_offset,
            confidence: saved.bpm_confidence,
        });
        let recovered = tempo::rhythm_evidence(
            &onsets,
            onset_rate,
            RATE / 2,
            saved.duration,
            selected,
            saved.downbeat,
            Vec::new(),
            &[],
        );
        if let Some(old) = &mut saved.rhythm {
            old.beats = recovered.beats;
        } else {
            saved.rhythm = Some(recovered);
        }
    }
}

/// The grid of `path` as measured (precise if it was), when there is one.
pub fn grid_of(path: &Path) -> Option<grid::GridQuality> {
    cached(path)?.grid
}

/// Tempo seed for grid verification. A source tag may rescue an uncertain
/// global estimate only when the two agree closely; the measured grid still
/// has to pass the normal quality checks before DJ planning uses it.
fn grid_measure_bpm(a: &Analysis) -> Option<f64> {
    a.bpm.filter(|_| a.bpm_confidence >= 0.3).or_else(|| {
        a.bpm.zip(a.tag_bpm).and_then(|(detected, tagged)| {
            (tagged.is_finite()
                && (40.0..=300.0).contains(&tagged)
                && (detected - tagged).abs() / tagged <= 0.02)
                .then_some(tagged)
        })
    })
}

/// Measure `path`'s grid now (coarse on the half-rate mono, or precise at
/// the full rate) and keep it with its analysis. Blocking: seconds for a
/// precise pass. None when it has no steady beat or isn't analysed.
pub fn measure_grid(path: &Path, precise: bool) -> Option<grid::GridQuality> {
    let a = cached(path)?;
    // A low-confidence detector can still have the correct tempo. On
    // professionally tagged tracks, a matching source BPM is useful as a
    // *measurement seed*: the kick/grid pass below must still find a
    // coherent local grid before `use_grid` promotes it for DJ planning.
    // Never let a disagreeing tag override the audio estimate.
    let bpm = grid_measure_bpm(&a)?;
    let rate = if precise { RATE } else { RATE / 2 };
    let _permit = DecodePermit::acquire();
    let out = Command::new("nice")
        .args(["-n", "19", "ffmpeg", "-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .watched_output()
        .ok()
        .filter(|o| o.status.success())?;
    let mono: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let coarse = match (&a.grid, precise) {
        (Some(g), true) => Some(g.clone()),
        _ => {
            let half: Vec<f32> = if precise {
                mono.chunks_exact(2).map(|p| (p[0] + p[1]) * 0.5).collect()
            } else {
                mono.clone()
            };
            grid::measure(&half, RATE / 2, bpm, a.beat_offset)
        }
    }?;
    let q = if precise {
        grid::precise(&mono, rate, &coarse)?
    } else {
        coarse
    };
    // The kicks decide the tempo, not the file's BPM tag: tags are rounded
    // (a track measured at 126.003 drifts 16 ms from a 126.000 grid over
    // five minutes).
    store_grid(path, &q);
    Some(q)
}

/// How the beats hold over one window of a track — the stretch a mix lands
/// in, measured part by part (4-bar parts) against the grid the track's own
/// analysis stores (`bpm`, `offset`). None when the file is not analysed,
/// the window is under four seconds or nothing in it has a clear attack.
///
/// The whole-track grid is one verdict for a whole record; a mix lays 8-32
/// bars of the outgoing over the incoming's first bars, and those parts can
/// read very differently from the aggregate (measured: 15 % whole-track,
/// 0.83 over the window). Memoised by file and window, and mounted at the
/// lowest priority — a decode, so never on a real-time path.
pub fn window_grid(
    path: &Path,
    bpm: f64,
    offset: f64,
    from_s: f64,
    to_s: f64,
) -> Option<grid::WindowGrid> {
    if !bpm.is_finite() || !(40.0..=300.0).contains(&bpm) || !offset.is_finite() {
        return None;
    }
    if !from_s.is_finite() || !to_s.is_finite() || to_s - from_s < 4.0 {
        return None;
    }
    let stamp = source::Revision::of(path).ok()?;
    // All calculation inputs participate: correcting a grid must not reuse
    // a measurement made against its previous BPM or phase. Keep the source
    // stamp intact rather than folding its size and timestamp with XOR.
    #[derive(Clone, PartialEq, Eq, Hash)]
    struct Key {
        path: PathBuf,
        source: source::Revision,
        epoch: u64,
        bpm: u64,
        offset: u64,
        from: u64,
        to: u64,
    }
    static MEMO: OnceLock<Mutex<cache::Cache<Key, Option<grid::WindowGrid>>>> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(cache::Cache::new(2 * 1024 * 1024)));
    let key = Key {
        path: path.to_path_buf(),
        source: stamp,
        epoch: memo_epoch(path),
        bpm: bpm.to_bits(),
        offset: offset.to_bits(),
        from: from_s.to_bits(),
        to: to_s.to_bits(),
    };
    if let Some(v) = memo.lock().unwrap().get(&key).copied() {
        return v;
    }
    let rate = RATE / 2;
    let span = to_s - from_s;
    // `-ss` before `-i` seeks accurately here (ffmpeg decodes from the
    // preceding keyframe and discards); a whole-file decode would cost
    // seconds per window. A window is at most 32 bars of mono audio, so it
    // takes no decode permit: planning never waits behind a whole track.
    let output = Command::new("nice")
        .args([
            "-n",
            "19",
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &format!("{from_s:.3}"),
            "-t",
            &format!("{span:.3}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .watched_output()
        .ok()
        .filter(|o| o.status.success())?;
    let mono: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let v = grid::window_grid(&mono, rate, bpm, offset, 16.0 * 60.0 / bpm, from_s);
    if source::Revision::of(path).ok()? != stamp || memo_epoch(path) != key.epoch {
        return None; // The decoded source changed during the measurement.
    }
    let bytes = 512 + key.path.as_os_str().len() * 2;
    memo.lock().unwrap().insert(key, v, bytes);
    v
}

/// Keep a measured grid with the cached analysis.
fn store_grid(path: &Path, q: &grid::GridQuality) {
    let Some(file) = cache_path(path) else { return };
    let _ = cache_store::update(&file, |latest| {
        let mut a: Analysis =
            serde_json::from_value(latest.ok_or("analysis missing")?).map_err(|e| e.to_string())?;
        if a.version != VERSION {
            return Err("analysis schema changed".into());
        }
        if !a
            .grid
            .as_ref()
            .is_some_and(|g| g.precise && !q.precise && g.method >= q.method)
        {
            a.grid = Some(q.clone());
            a.analyzers.grid = GRID_ANALYZER_VERSION;
        }
        serde_json::to_value(a).map_err(|e| e.to_string())
    });
}

/// The grid of `path` if measured well enough already (precise when
/// `precise`); else None, and the measurement starts in the background
/// (one track at a time, at the lowest priority). For callers that must
/// not wait: the AI DJ planning, a UI.
pub fn grid_soon(path: &Path, precise: bool) -> Option<grid::GridQuality> {
    if let Some(g) = grid_of(path).filter(|g| (g.precise || !precise) && g.method >= grid::METHOD) {
        return Some(g);
    }
    type Job = (PathBuf, bool);
    static QUEUE: OnceLock<Mutex<(std::collections::VecDeque<Job>, bool)>> = OnceLock::new();
    let q = QUEUE.get_or_init(Default::default);
    let mut g = q.lock().unwrap();
    let job = (path.to_path_buf(), precise);
    if !g.0.contains(&job) {
        // Precise asks come first: the AI waits on them for a transition.
        if precise {
            g.0.push_front(job)
        } else {
            g.0.push_back(job)
        }
    }
    if !g.1 {
        g.1 = true;
        let spawn = std::thread::Builder::new()
            .name("patch-grid".into())
            .spawn(move || {
                loop {
                    let next = {
                        let mut g = q.lock().unwrap();
                        match g.0.pop_front() {
                            Some(j) => j,
                            None => {
                                g.1 = false;
                                return;
                            }
                        }
                    };
                    let _ = measure_grid(&next.0, next.1);
                }
            });
        if spawn.is_err() {
            // Keep queued work retryable on the next request. Leaving this
            // true after an EAGAIN permanently stranded the grid queue.
            g.1 = false;
        }
    }
    None
}

impl Analysis {
    /// Use a human override first, then the measured grid, as the track's
    /// tempo and first beat: what decks sync on and the AI plans with.
    /// True if it was applied.
    /// Probability that this track's tempo is right (operational, not a
    /// raw score). A human grid is authoritative. Otherwise a logistic fit of
    /// the raw tempo confidence and the grid's phase confidence against 956
    /// BPM-tagged tracks of a DJ library (2026-10-06; tempo right 96.0 %):
    /// held-out Brier 0.031 against 0.036 for a constant, and reliable per
    /// bin (≈0.66 predicted → 0.70 right, ≈0.97 → 0.98). Refit with
    /// `patch-bench --calibrate` when the tempo analyzer changes.
    pub fn tempo_probability(&self) -> f32 {
        if self.human_grid.is_some() {
            return 1.0;
        }
        if self.bpm.is_none() {
            return 0.0;
        }
        let phase = self.grid.as_ref().map_or(0.0, |g| g.phase_confidence);
        let z = 1.0796
            + 1.2659 * (self.bpm_confidence - 0.85) * 10.0
            + 1.9655 * phase.clamp(0.0, 0.2) / 0.2;
        1.0 / (1.0 + (-z).exp())
    }

    pub fn use_grid(&mut self) -> bool {
        if let Some(human) = self.human_grid.as_ref()
            && let (bpm, offset) = (human.bpm, human.offset)
            && bpm.is_finite()
            && (40.0..=300.0).contains(&bpm)
            && offset.is_finite()
            && offset >= 0.0
            && offset < self.duration
            && human
                .beats_per_bar
                .zip(human.downbeat_phase)
                .is_none_or(|(beats, phase)| beats > 0 && phase < beats)
        {
            self.bpm = Some(bpm);
            self.beat_offset = offset;
            self.bpm_confidence = 1.0;
            // Only the planner's supported 4/4 meter can supply its bar phase.
            // Preserve other human meter metadata in `human_grid`, but never
            // reinterpret it as four-beat bars.
            if human.beats_per_bar == Some(4) {
                self.downbeat = human.downbeat_phase;
                self.downbeat_confidence = if human.downbeat_phase.is_some() {
                    1.0
                } else {
                    0.0
                };
            } else {
                self.downbeat = None;
                self.downbeat_confidence = 0.0;
            }
            self.grid = None;
            return true;
        }
        // An invalid runtime human record must not leak its tempo map into a
        // fallback machine grid. It is only a transient copy, never persisted.
        self.human_grid = None;
        let Some(g) = self.grid.clone() else {
            return false;
        };
        // Where the analysis' own downbeat was, on its own grid.
        let old = match (self.downbeat, self.bpm) {
            (Some(d), Some(b)) => Some(self.beat_offset + d as f64 * 60.0 / b),
            _ => None,
        };
        self.bpm = Some(g.bpm);
        // Measured on the kicks: as sure as a beat gets, when it holds.
        self.bpm_confidence = self.bpm_confidence.max(0.5 + 0.5 * g.mixability);
        self.beat_offset = g.offset;

        // The analysis' own downbeat is on its old grid: keep it only if it
        // lands on the same beat and the grid has none.
        let beat = 60.0 / g.bpm;
        match (g.downbeat, old) {
            (Some(d), _) => {
                self.downbeat = Some(d);
                self.downbeat_confidence = g.downbeat_confidence;
            }
            (None, Some(t)) => {
                let k = ((t - g.offset) / beat).round().rem_euclid(4.0) as u8;
                self.downbeat = Some(k);
            }
            (None, None) => {}
        }
        true
    }

    /// This analysis's generation (see [`GENERATION`]).
    pub fn generation(&self) -> u32 {
        generation(self.version, self.analyzers)
    }

    /// Local tempo from a runtime human map, falling back to the selected
    /// track tempo where the map does not cover the requested source time.
    pub fn bpm_at(&self, source_seconds: f64) -> Option<f64> {
        if !source_seconds.is_finite() || source_seconds < 0.0 {
            return None;
        }
        if let Some(human) = self.human_grid.as_ref() {
            if let Some(segment) = human.segments.iter().find(|segment| {
                source_seconds >= segment.start_s
                    && segment.end_s.is_none_or(|end| source_seconds < end)
            }) {
                let end = segment.end_s.unwrap_or(self.duration);
                let span = end - segment.start_s;
                let ratio = if span > 0.0 {
                    ((source_seconds - segment.start_s) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let bpm = segment.bpm_start + (segment.bpm_end - segment.bpm_start) * ratio;
                if bpm.is_finite() && (40.0..=300.0).contains(&bpm) {
                    return Some(bpm);
                }
            }
            // A fixed human grid remains authoritative outside any saved
            // tempo-map coverage; never substitute machine regional guesses.
            return self.bpm;
        }
        if let Some(evidence) = self
            .rhythm
            .as_ref()
            .filter(|evidence| evidence.analyzer_version == tempo::RHYTHM_EVIDENCE_VERSION)
            && let Some(region) = evidence
                .regions
                .iter()
                .find(|region| source_seconds >= region.start_s && source_seconds < region.end_s)
                && let Some(local_bpm) = region
                    .local_bpm
                    .filter(|bpm| bpm.is_finite() && (40.0..=300.0).contains(&f64::from(*bpm)))
                {
                    return Some(f64::from(local_bpm));
                }
        self.bpm
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

pub mod mic;

#[cfg(test)]
mod audit_regressions;
