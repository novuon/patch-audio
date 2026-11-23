//! The DJ library: every track Patch knows, by content identity.
//!
//! * **Identity** ([`identity`]): a track is its content, not its path:
//!   size plus hashes of the head, middle and tail of the file. Moving or
//!   renaming music keeps its analysis, cues and history.
//! * **Schema** ([`schema`]): SQLite, one row per track with the source
//!   facts; analysis and performance metadata are versioned JSON documents
//!   so new analysis kinds need no migration. Manual data (grid
//!   corrections, cues, tags, ratings) lives apart from automatic analysis,
//!   so re-analysis never overwrites what a DJ decided.
//! * **Search** ([`index`]): an in-memory index over the table, rebuilt on
//!   change; text and filters answer in well under a millisecond per
//!   thousand tracks.
//! * **History**: an append-only log of performance events (load, play,
//!   cue, loop, sync, transition), context for the DJ and later the AI DJ.
//!
//! Source files are never modified. Nothing here runs on an audio thread.

pub mod identity;
pub mod index;
pub mod interoperability;
pub mod package;
pub mod schema;
pub mod waveform;
pub mod edits;
pub use edits::TrackEdit;
pub mod genres;
pub mod years;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub use index::{Filter, Index};
pub use package::{Manifest, Package, PackageMode, ValidationReport};

/// Audio files the library imports.
pub const EXTENSIONS: [&str; 10] = [
    "wav", "wave", "aif", "aiff", "flac", "m4a", "alac", "mp3", "ogg", "opus",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: i64,
    pub track_uuid: String,
    pub content_id: String,
    pub path: String,
    pub size: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub codec: String,
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub duration: f64,
    pub added_at: i64,
    pub last_played: Option<i64>,
    pub plays: u32,
    pub missing: bool,
    /// Copied from the latest analysis for search and sorting.
    pub bpm: Option<f64>,
    /// Camelot, e.g. "8A".
    pub key: Option<String>,
    pub lufs: Option<f64>,
    pub energy: Option<f64>,
    /// Performance metadata the DJ set.
    pub rating: u8,
    pub tags: Vec<String>,
    pub color: Option<String>,
}

/// The tag that marks a favourite track.
pub const FAVOURITE_TAG: &str = "favourite";

/// Everything a DJ decides about a track; always wins over automation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Performance {
    /// Hot cues in seconds. Device/UI slot limits do not constrain the format.
    #[serde(default)]
    pub hot_cues: Vec<Option<f64>>,
    /// Memory cues (seconds, with an optional name).
    #[serde(default)]
    pub memory_cues: Vec<(f64, String)>,
    /// Saved loops (in, out seconds).
    #[serde(default)]
    pub loops: Vec<(f64, f64)>,
    /// Manual beat grid (bpm, first beat seconds): overrides analysis.
    #[serde(default)]
    pub grid: Option<(f64, f64)>,
    /// Preferred gain trim (dB), overrides auto-gain.
    #[serde(default)]
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// A rich native cue. Position is stored in source-sample coordinates;
/// `kind` is an open semantic value so future cue types do not require a
/// schema migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub id: String,
    pub kind: String,
    pub sample_position: u64,
    pub sample_rate: u32,
    pub beat_position: Option<f64>,
    pub label: String,
    pub color: Option<String>,
    pub comment: String,
    pub origin: String,
    pub confidence: Option<f64>,
    pub generated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedLoop {
    pub id: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
    pub beat_length: Option<f64>,
    pub label: String,
    pub quantized: bool,
    pub temporary: bool,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatSegment {
    pub start_sample: u64,
    pub end_sample: Option<u64>,
    pub bpm_start: f64,
    pub bpm_end: f64,
    pub first_beat_sample: u64,
    pub beats_per_bar: Option<u8>,
    pub downbeat_phase: Option<u8>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HumanGridOverride {
    pub bpm: f64,
    pub first_beat_s: f64,
    pub beats_per_bar: Option<u8>,
    pub downbeat_phase: Option<u8>,
    pub sample_rate: u32,
    pub segments: Vec<BeatSegment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionPlan {
    pub source_track: i64,
    pub destination_track: i64,
    pub source_mix_out_sample: u64,
    pub destination_mix_in_sample: u64,
    pub duration_samples: u64,
    pub sample_rate: u32,
    pub strategy: TransitionStrategy,
    pub confidence: Option<f64>,
    pub notes: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TransitionStrategy {
    pub phrase_alignment_beats: Option<u32>,
    pub tempo_ratio: Option<f64>,
    pub key_relation: Option<String>,
    pub energy_delta: Option<f64>,
    pub fader_curve: Vec<[f64; 2]>,
    pub eq_moves: Vec<ControlMove>,
    pub loops: Vec<PlannedLoop>,
    pub effects: Vec<ControlMove>,
    pub stems: Vec<ControlMove>,
    pub rationale: Option<AnalysisProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlMove {
    pub target: String,
    pub at_seconds: f64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedLoop {
    pub deck: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisProvenance {
    pub analyzer_id: String,
    pub analyzer_version: String,
    pub schema_version: u32,
    pub created_at: String,
    pub confidence: Option<f64>,
    pub input_fingerprint: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingVector {
    F32(Vec<f32>),
    I8 { scale: f32, values: Vec<i8> },
}

impl EmbeddingVector {
    pub fn dimension(&self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::I8 { values, .. } => values.len(),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let n = self.dimension();
        if n == 0 || n > 16_000_000 {
            return Err("embedding dimension outside supported range".into());
        }
        let (kind, mut payload) = match self {
            Self::F32(v) => {
                if v.iter().any(|x| !x.is_finite()) {
                    return Err("embedding contains non-finite values".into());
                }
                (
                    1,
                    v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
                )
            }
            Self::I8 { scale, values } => {
                if !scale.is_finite() || *scale <= 0.0 {
                    return Err("invalid embedding quantization scale".into());
                }
                (2, values.iter().map(|x| *x as u8).collect::<Vec<_>>())
            }
        };
        let mut out = Vec::with_capacity(20 + payload.len());
        out.extend_from_slice(b"PEV1");
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(kind as u16).to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        let scale = match self {
            Self::I8 { scale, .. } => *scale,
            _ => 1.0,
        };
        out.extend_from_slice(&scale.to_le_bytes());
        out.append(&mut payload);
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 20 || &bytes[..4] != b"PEV1" {
            return Err("bad PEV1 header".into());
        }
        let u16at = |p| u16::from_le_bytes(bytes[p..p + 2].try_into().unwrap());
        let n = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let scale = f32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if u16at(4) != 1 || n == 0 || n > 16_000_000 || !scale.is_finite() {
            return Err("unsupported or invalid embedding header".into());
        }
        let payload_end = bytes.len() - 4;
        let crc = u32::from_le_bytes(bytes[payload_end..].try_into().unwrap());
        if crc32(&bytes[..payload_end]) != crc {
            return Err("embedding CRC-32 mismatch".into());
        }
        match u16at(6) {
            1 if bytes.len() == 20 + n * 4 => {
                let v = bytes[16..payload_end]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect::<Vec<_>>();
                if v.iter().any(|x| !x.is_finite()) {
                    return Err("non-finite embedding".into());
                }
                Ok(Self::F32(v))
            }
            2 if bytes.len() == 20 + n && scale > 0.0 => Ok(Self::I8 {
                scale,
                values: bytes[16..payload_end].iter().map(|x| *x as i8).collect(),
            }),
            _ => Err("embedding type, dimension or length mismatch".into()),
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// An analysis failure in plain words (ffmpeg's text without its internals).
pub fn plain_error(e: &str) -> String {
    let line = e
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    // "ffmpeg: [in#0 @ 0x55…] Failed to find …" → "Failed to find …"
    let text = line
        .rsplit_once("] ")
        .map(|(_, t)| t)
        .unwrap_or(line)
        .trim_start_matches("ffmpeg: ");
    match text {
        t if t.contains("Invalid data") || t.contains("consecutive MPEG audio frames") => {
            "not playable audio (damaged, or not an audio file)".into()
        }
        t if t.contains("No such file") => "the file is gone".into(),
        t if t.contains("Permission denied") => "no permission to read it".into(),
        t => t.to_string(),
    }
}

/// A folder or drive in the library.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub added_at: i64,
    pub scanned_at: Option<i64>,
}

/// Where a track stands for DJing and the AI DJ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Readiness {
    /// Tempo with a steady beat grid, downbeat, key, loudness, energy.
    Ready,
    /// Analysed, but the beat is unsteady (under 50 % sure), or no key or
    /// downbeat was found: playable, check before mixing it.
    Check,
    /// Its tempo is confirmed by the file's own BPM (tag or filename), but
    /// the beat is too soft for a steady grid (ballads, live drums, no
    /// kick): nothing to check, it mixes short (the user, 2026-11-14).
    Soft,
    /// Not analysed yet.
    Waiting,
    /// The file couldn't be read (the reason is kept).
    Failed,
}

/// The SQL that says a track is ready (`t` tracks, `a` analysis): a steady
/// analysed grid, or a grid the DJ set by hand (the user, 2026-11-14).
const READY: &str = "(a.version IS NOT NULL AND ((t.bpm IS NOT NULL AND t.grid_confidence >= 0.5 AND t.key IS NOT NULL AND t.downbeat IS NOT NULL) OR EXISTS (SELECT 1 FROM beatgrids b WHERE b.track_id = t.id AND b.active = 1 AND b.layer = 'human') OR EXISTS (SELECT 1 FROM performance p WHERE p.track_id = t.id AND json_extract(p.data, '$.grid[0]') IS NOT NULL)))";
/// The SQL that says a not-ready track's tempo is confirmed by its file's
/// own BPM (`confirmed_tempo`, for the analysis it was checked against).
const SOFT: &str = "(a.version IS NOT NULL AND t.bpm IS NOT NULL AND EXISTS (SELECT 1 FROM confirmed_tempo c WHERE c.track_id = t.id AND c.bpm IS NOT NULL AND c.analysed_at = a.at))";
/// Before the first confirmation (an older library): none is soft.
const SOFT_NONE: &str = "0";

/// A BPM written in a file name, as DJ pools and rippers name files:
/// "… - 8B - 93.mp3", "… 124 BPM.mp3" (the last number from 60 to 200).
pub fn filename_bpm(path: &str) -> Option<f64> {
    let stem = Path::new(path).file_stem()?.to_string_lossy().into_owned();
    let lower = stem.to_ascii_lowercase();
    if let Some(i) = lower.find("bpm") {
        let before = lower[..i].trim_end();
        let n: String = before.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let n: String = n.chars().rev().collect();
        if let Ok(b) = n.parse::<f64>()
            && (60.0..=200.0).contains(&b)
        {
            return Some(b);
        }
    }
    // Only a number that ends the name after " - ", as pools write it
    // ("… - 8B - 93"): a title's own number ("Summer Of 69") is not one.
    let (_, tail) = stem.trim_end().rsplit_once(" - ")?;
    tail.trim().parse::<f64>().ok().filter(|b| (60.0..=200.0).contains(b))
}

/// Whether `bpm` agrees with `other` within 2 %, at the same, half or
/// double time.
pub fn tempo_agrees(bpm: f64, other: f64) -> bool {
    other > 0.0 && [0.5, 1.0, 2.0].iter().any(|m| (bpm * m / other - 1.0).abs() <= 0.02)
}

/// Which of the file's own BPMs confirms `bpm`: its tag first, then its
/// name.
pub fn tempo_confirmed_by(bpm: f64, tag_bpm: Option<f64>, path: &str) -> Option<(f64, &'static str)> {
    tag_bpm
        .filter(|t| tempo_agrees(bpm, *t))
        .map(|t| (t, "tag"))
        .or_else(|| filename_bpm(path).filter(|t| tempo_agrees(bpm, *t)).map(|t| (t, "file name")))
}

const CONFIRMED_TEMPO: &str = "CREATE TABLE IF NOT EXISTS confirmed_tempo (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    bpm REAL, source TEXT, analysed_at INTEGER NOT NULL)";

/// Record whether track `id`'s analysed tempo is confirmed by its file's
/// own BPM, for the analysis written at `at`.
fn store_confirmed_tempo(conn: &Connection, id: i64, at: i64, bpm: Option<f64>, tag_bpm: Option<f64>, path: &str) -> Result<(), String> {
    conn.execute(CONFIRMED_TEMPO, []).map_err(|e| e.to_string())?;
    let found = bpm.and_then(|b| tempo_confirmed_by(b, tag_bpm, path));
    conn.execute(
        "INSERT INTO confirmed_tempo (track_id, bpm, source, analysed_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(track_id) DO UPDATE SET bpm = excluded.bpm, source = excluded.source, analysed_at = excluded.analysed_at",
        params![id, found.map(|f| f.0), found.map(|f| f.1), at],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// How many tracks under a source stand where.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Counts {
    pub tracks: usize,
    pub ready: usize,
    /// Tempo confirmed by the file's own BPM, beat too soft for a grid.
    #[serde(default)]
    pub soft: usize,
    pub check: usize,
    pub waiting: usize,
    pub failed: usize,
}

/// A track as the AI DJ's first pass sees it (`Library::dj_facts`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DjFacts {
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub energy: Option<f64>,
    pub duration: f64,
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("omarchy-patch")
        .join("library.patchdj")
        .join("library.db")
}

pub fn default_package_path() -> PathBuf {
    default_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// The newest modification time among the files SQLite actually writes.
///
/// The library runs in WAL mode, so a write lands in `library.db-wal` and the
/// database file's own timestamp does not move until a checkpoint folds it back
/// in — minutes later, or not before the next clean close. A staleness check
/// keyed on `library.db` alone therefore sees nothing at all for exactly the
/// writes it exists to catch, and goes on serving its cached copy. Everything
/// that asks "has the library moved?" belongs here.
pub fn stamp() -> Option<std::time::SystemTime> {
    stamp_of_db(&default_path())
}

/// The same stamp for an explicit database path, so a test can drive it.
fn stamp_of_db(db: &Path) -> Option<std::time::SystemTime> {
    let sidecar = |suffix: &str| {
        let mut p = db.as_os_str().to_os_string();
        p.push(suffix);
        PathBuf::from(p)
    };
    [db.to_path_buf(), sidecar("-wal"), sidecar("-journal")]
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max()
}

/// Optional metadata probe. `ffprobe` is an open-source helper; packages and
/// playback remain usable when it is absent, in which case callers can retain
/// filename-derived titles and leave unavailable source facts unknown.
pub fn probe_audio_facts(path: &Path) -> Option<Facts> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() > 2 * 1024 * 1024 {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let stream = v["streams"]
        .as_array()?
        .iter()
        .find(|s| s["codec_type"] == "audio")?;
    let stream_tags = stream["tags"].as_object();
    let format_tags = v["format"]["tags"].as_object();
    let tag = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| {
                stream_tags
                    .into_iter()
                    .chain(format_tags)
                    .flat_map(|m| m.iter())
                    .find(|(n, _)| n.eq_ignore_ascii_case(key))
                    .and_then(|(_, v)| v.as_str())
            })
            .unwrap_or("")
            .to_string()
    };
    let duration = stream["duration"]
        .as_str()
        .and_then(|x| x.parse().ok())
        .or_else(|| {
            v["format"]["duration"]
                .as_str()
                .and_then(|x| x.parse().ok())
        })
        .filter(|x: &f64| x.is_finite() && *x >= 0.0)
        .unwrap_or(0.0);
    Some(Facts {
        title: tag(&["title"]),
        artist: tag(&["artist"]),
        album_artist: tag(&["album_artist", "album artist"]),
        album: tag(&["album"]),
        genre: tag(&["genre"]),
        label: tag(&["label"]),
        release: tag(&["release"]),
        release_date: tag(&["date", "year", "release_date"]),
        comment: tag(&["comment"]),
        container: v["format"]["format_name"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        original_tags: serde_json::json!({"stream":stream["tags"],"format":v["format"]["tags"]}),
        codec: stream["codec_name"].as_str().unwrap_or("").to_string(),
        rate: stream["sample_rate"]
            .as_str()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0),
        bits: stream["bits_per_raw_sample"]
            .as_str()
            .and_then(|x| x.parse().ok())
            .or_else(|| stream["bits_per_sample"].as_u64().map(|x| x as u32))
            .unwrap_or(0),
        channels: stream["channels"].as_u64().unwrap_or(0) as u32,
        duration,
    })
}

/// Open Patch's persistent native library package, creating it on first use.
/// The former standalone `library.db`, if present, is copied into the package
/// before migration; the original remains available as a recovery copy.
/// The library for a UI thread: a plain connection to the existing file,
/// waiting at most 250 ms for a lock (another process may be writing, or
/// upgrading it), never upgrading or repairing it itself. Background work
/// and the CLI use [`open_default`].
pub fn open_quick() -> Result<Library, String> {
    let path = default_path();
    if !path.is_file() {
        return open_default();
    }
    let db = Connection::open(&path).map_err(|e| e.to_string())?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(250));
    Ok(Library { db, path })
}

pub fn open_default() -> Result<Library, String> {
    let root = default_package_path();
    let manifest = root.join("manifest.json");
    if manifest.is_file() {
        return Package::open(&root).map(|p| p.library);
    }
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let db = root.join("library.db");
    if !db.exists() {
        let old = default_path()
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("library.db"));
        if let Some(old) = old.filter(|p| p.is_file()) {
            let _ = std::fs::copy(old, &db)
                .map_err(|e| format!("legacy Patch DJ database preserved; could not copy: {e}"))?;
            return Package::adopt(&root, "Patch Library", PackageMode::Library).map(|p| p.library);
        }
        return Package::create(&root, "Patch Library", PackageMode::Library).map(|p| p.library);
    }
    Package::adopt(&root, "Patch Library", PackageMode::Library).map(|p| p.library)
}

pub struct Library {
    pub(crate) db: Connection,
    path: PathBuf,
}

/// Result of a folder scan.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ScanReport {
    pub added: usize,
    pub moved: usize,
    pub unchanged: usize,
    /// Files that are copies of tracks the library has elsewhere.
    pub copies: usize,
    pub missing: usize,
}

impl Library {
    pub fn open(path: &Path) -> Result<Library, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        // The UI, the librarian and the CLI write to it: wait, don't fail.
        let _ = db.busy_timeout(std::time::Duration::from_secs(5));
        schema::migrate(&db)?;
        backfill_track_uuids(&db)?;
        Ok(Library {
            db,
            path: path.to_path_buf(),
        })
    }

    pub fn in_memory() -> Result<Library, String> {
        let db = Connection::open_in_memory().map_err(|e| e.to_string())?;
        schema::migrate(&db)?;
        backfill_track_uuids(&db)?;
        Ok(Library {
            db,
            path: PathBuf::from("."),
        })
    }

    pub fn store_waveform(
        &self,
        track: i64,
        waveform: &waveform::Waveform,
    ) -> Result<PathBuf, String> {
        let bytes = waveform.encode()?;
        let uuid: String = self
            .db
            .query_row("SELECT track_uuid FROM tracks WHERE id=?1", [track], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        let rel = PathBuf::from("analysis/waveforms").join(format!("{uuid}.pwf"));
        let root = self.path.parent().unwrap_or_else(|| Path::new("."));
        let path = root.join(&rel);
        package::safe_package_target(root, &rel)?;
        // Same directory as the sidecar, so rename does not cross filesystems.
        // The file and its directory are synced before this returns: a crash
        // leaves either the previous sidecar or the new one, never a torn file.
        publish_durable(&path, &bytes).map_err(|e| e.to_string())?;
        let size = bytes.len() as i64;
        let digest = sha256_bytes(&bytes);
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO assets(id,track_id,kind,relative_path,media_type,size,sha256) VALUES(?1,?2,'waveform',?3,'application/vnd.patch.waveform',?4,?5) ON CONFLICT(id) DO UPDATE SET relative_path=excluded.relative_path,size=excluded.size,sha256=excluded.sha256",params![format!("waveform-{track}"),track,rel.to_string_lossy(),size,digest]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO package_files(relative_path,size,sha256,role) VALUES(?1,?2,?3,'derived') ON CONFLICT(relative_path) DO UPDATE SET size=excluded.size,sha256=excluded.sha256",params![rel.to_string_lossy(),size,digest]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Import or refresh one file. Facts (codec, rate, …) come from
    /// `probe` only for files the library hasn't seen in this state.
    pub fn import(
        &self,
        path: &Path,
        probe: impl Fn(&Path) -> Option<Facts>,
    ) -> Result<(i64, Change), String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let p = path.to_string_lossy().to_string();
        // Known at this path in this state: nothing to do.
        let known: Option<(i64, i64, i64)> = self
            .db
            .query_row(
                "SELECT id, size, mtime FROM tracks WHERE path = ?1",
                params![p],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((id, s, m)) = known
            && s as u64 == size
            && m == mtime
        {
            self.db
                .execute("UPDATE tracks SET missing = 0 WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
            return Ok((id, Change::Unchanged));
        }
        let cid = identity::content_id(path).map_err(|e| e.to_string())?;
        // Same content elsewhere: it moved, or this is a copy.
        let same: Option<(i64, String)> = self
            .db
            .query_row(
                "SELECT id, path FROM tracks WHERE content_id = ?1",
                params![cid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // A copy while the original is still there: one track, it stays
        // where it is (else the two would take turns at every scan).
        if let Some((id, known)) = &same
            && known != &p
            && Path::new(known).is_file()
        {
            return Ok((*id, Change::Copy));
        }
        if let Some((id, _)) = same {
            self.db
                .execute(
                    "UPDATE tracks SET path = ?1, size = ?2, mtime = ?3, missing = 0 WHERE id = ?4",
                    params![p, size as i64, mtime, id],
                )
                .map_err(|e| e.to_string())?;
            return Ok((id, Change::Moved));
        }
        let f = probe(path).unwrap_or_default();
        let title = if f.title.is_empty() {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            f.title.clone()
        };
        self.db
            .execute(
                "INSERT INTO tracks (content_id, path, size, mtime, title, artist, album, genre, codec, rate, bits, channels, duration, added_at, track_uuid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![cid, p, size as i64, mtime, title, f.artist, f.album, f.genre, f.codec, f.rate, f.bits, f.channels, f.duration, now(), uuid::Uuid::new_v4().to_string()],
            )
            .map_err(|e| e.to_string())?;
        let id = self.db.last_insert_rowid();
        self.db
            .execute(
                "INSERT INTO source_metadata(track_id,data) VALUES(?1,?2)",
                params![id, serde_json::to_string(&f).map_err(|e| e.to_string())?],
            )
            .map_err(|e| e.to_string())?;
        Ok((id, Change::Added))
    }

    /// Import every audio file under `root`; tracks no longer found there
    /// are marked missing (never deleted: their metadata stays).
    pub fn scan(
        &self,
        root: &Path,
        probe: impl Fn(&Path) -> Option<Facts>,
    ) -> Result<ScanReport, String> {
        self.scan_with(root, probe, |_, _| {})
    }

    /// `scan`, telling `found` about each track as it is taken in, so a
    /// collection can fill while a big folder is still being scanned.
    pub fn scan_with(
        &self,
        root: &Path,
        probe: impl Fn(&Path) -> Option<Facts>,
        mut found: impl FnMut(i64, Change),
    ) -> Result<ScanReport, String> {
        let mut report = ScanReport::default();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                // Hidden files and folders are not music: macOS writes a
                // "._name.mp3" metadata twin next to every song on sticks it
                // has seen (no audio in it), and ".Trashes", ".Spotlight…".
                if p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
                    && let Ok((id, c)) = self.import(&p, &probe)
                {
                    seen.insert(id);
                    found(id, c);
                    match c {
                        Change::Added => report.added += 1,
                        Change::Moved => report.moved += 1,
                        Change::Unchanged => report.unchanged += 1,
                        Change::Copy => report.copies += 1,
                    }
                }
            }
        }
        let root_s = format!("{}/", root.to_string_lossy().trim_end_matches('/'));
        let under: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM tracks WHERE substr(path,1,length(?1))=?1 AND missing = 0")
                .map_err(|e| e.to_string())?;
            st.query_map(params![root_s], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect()
        };
        for id in under.into_iter().filter(|id| !seen.contains(id)) {
            self.db
                .execute("UPDATE tracks SET missing = 1 WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
            report.missing += 1;
        }
        // Hidden files taken in by an earlier scan leave their collections.
        self.db
            .execute(
                "DELETE FROM playlist_items WHERE track_id IN (SELECT id FROM tracks WHERE missing = 1 AND (path LIKE '%/.%'))",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(report)
    }

    /// Version of the analysis stored for a track, if any.
    pub fn analysis_version(&self, id: i64) -> Option<u32> {
        self.db
            .query_row(
                "SELECT version FROM analysis WHERE track_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// A track's content identity.
    pub fn content_id_of(&self, id: i64) -> Option<String> {
        self.db
            .query_row(
                "SELECT content_id FROM tracks WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// The stored analysis for a track, by path. The Library's rows are built
    /// from the analysis cache files, so a track whose cache file went missing
    /// (evicted, or a write that never landed) rendered blank although its
    /// library row was complete — measured on one imported track: the row had
    /// bpm, key, LUFS and a grid, the cache file was simply gone. This is what
    /// the row falls back to. One indexed lookup, and only for a track with no
    /// cache file, so the common path never touches it.
    pub fn analysis_doc(&self, path: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT a.data FROM tracks t JOIN analysis a ON a.track_id = t.id WHERE t.path = ?1",
                params![path],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn track_path(&self, id: i64) -> Option<String> {
        self.db
            .query_row("SELECT path FROM tracks WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    /// Saved overview bins, independent of the regenerable analysis cache.
    pub fn waveform_bins(&self, path: &str) -> Option<Vec<[u8; 2]>> {
        let rel: String = self.db.query_row(
            "SELECT a.relative_path FROM tracks t JOIN assets a ON a.track_id=t.id WHERE t.path=?1 AND a.kind='waveform'",
            params![path], |r| r.get(0),
        ).optional().ok()??;
        let root = self.path.parent()?;
        let relative = Path::new(&rel);
        package::safe_package_target(root, relative).ok()?;
        let wave = waveform::Waveform::decode(&std::fs::read(root.join(relative)).ok()?).ok()?;
        Some(
            wave.levels
                .first()?
                .bins
                .iter()
                .map(|b| {
                    let peak = b[0].abs().max(b[1].abs());
                    [(peak * 255.0).round() as u8, (b[2] * 255.0).round() as u8]
                })
                .collect(),
        )
    }

    pub fn track_by_path(&self, path: &str) -> Option<i64> {
        self.db
            .query_row(
                "SELECT id FROM tracks WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn track_by_uuid(&self, id: &str) -> Option<i64> {
        self.db
            .query_row(
                "SELECT id FROM tracks WHERE track_uuid = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Reattach a stable track UUID to the same content at a new location.
    /// The bounded fingerprint must match before the locator is changed.
    pub fn relink(&self, track_uuid: &str, path: &Path) -> Result<i64, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("relink target is not a regular file".into());
        }
        let id = self
            .track_by_uuid(track_uuid)
            .ok_or_else(|| format!("unknown track UUID {track_uuid}"))?;
        let expected: String = self
            .db
            .query_row(
                "SELECT content_id FROM tracks WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let actual = identity::content_id(path).map_err(|e| e.to_string())?;
        if actual != expected {
            return Err(
                "audio fingerprint does not match this track; import it as a new track instead"
                    .into(),
            );
        }
        let path_s = path.to_string_lossy().to_string();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE tracks SET path=?1,size=?2,mtime=?3,missing=0 WHERE id=?4",
            params![path_s, meta.len() as i64, mtime, id],
        )
        .map_err(|e| e.to_string())?;
        let mut doc: serde_json::Value = tx
            .query_row(
                "SELECT data FROM source_metadata WHERE track_id=?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let object = doc
            .as_object_mut()
            .ok_or_else(|| "source metadata must be a JSON object".to_string())?;
        object.insert(
            "source_uri".into(),
            serde_json::Value::String(path_s.clone()),
        );
        object
            .entry("original_path")
            .or_insert_with(|| serde_json::Value::String(path_s));
        object.insert("embedded".into(), serde_json::Value::Bool(false));
        object.insert(
            "relinked_at".into(),
            serde_json::Value::String(timestamp_iso()),
        );
        tx.execute("INSERT INTO source_metadata(track_id,data) VALUES(?1,?2) ON CONFLICT(track_id) DO UPDATE SET data=excluded.data",params![id,doc.to_string()]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// Save a human or generated cue without constraining cue count. Replacing
    /// a cue preserves its stable ID and creation time.
    pub fn put_cue(&self, track: i64, cue: &Cue) -> Result<(), String> {
        validate_cue(cue)?;
        self.db.execute(
            "INSERT INTO cues (id, track_id, kind, sample_position, sample_rate, beat_position, label, color, comment, origin, confidence, created_at, modified_at, generated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?12)
             ON CONFLICT(id) DO UPDATE SET track_id=excluded.track_id, kind=excluded.kind, sample_position=excluded.sample_position, sample_rate=excluded.sample_rate, beat_position=excluded.beat_position, label=excluded.label, color=excluded.color, comment=excluded.comment, origin=excluded.origin, confidence=excluded.confidence, modified_at=excluded.modified_at, generated=excluded.generated",
            params![cue.id, track, cue.kind, cue.sample_position as i64, cue.sample_rate, cue.beat_position, cue.label, cue.color, cue.comment, cue.origin, cue.confidence, cue.generated as i32],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn cues(&self, track: i64) -> Result<Vec<Cue>, String> {
        let mut st = self.db.prepare("SELECT id, kind, sample_position, sample_rate, beat_position, label, color, comment, origin, confidence, generated FROM cues WHERE track_id=?1 ORDER BY sample_position, id").map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![track], |r| {
                Ok(Cue {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    sample_position: r.get::<_, i64>(2)? as u64,
                    sample_rate: r.get(3)?,
                    beat_position: r.get(4)?,
                    label: r.get(5)?,
                    color: r.get(6)?,
                    comment: r.get(7)?,
                    origin: r.get(8)?,
                    confidence: r.get(9)?,
                    generated: r.get::<_, i64>(10)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn delete_cue(&self, id: &str) -> Result<bool, String> {
        self.db
            .execute("DELETE FROM cues WHERE id=?1", params![id])
            .map(|n| n > 0)
            .map_err(|e| e.to_string())
    }

    pub fn put_loop(&self, track: i64, lp: &SavedLoop) -> Result<(), String> {
        if lp.id.is_empty()
            || lp.id.len() > 128
            || lp.sample_rate == 0
            || lp.start_sample >= lp.end_sample
            || lp.end_sample > i64::MAX as u64
            || lp.beat_length.is_some_and(|x| !x.is_finite() || x <= 0.0)
        {
            return Err("invalid saved loop".into());
        }
        self.db.execute("INSERT INTO loops(id,track_id,start_sample,end_sample,sample_rate,beat_length,label,quantized,temporary,origin,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET start_sample=excluded.start_sample,end_sample=excluded.end_sample,sample_rate=excluded.sample_rate,beat_length=excluded.beat_length,label=excluded.label,quantized=excluded.quantized,temporary=excluded.temporary,origin=excluded.origin",params![lp.id,track,lp.start_sample as i64,lp.end_sample as i64,lp.sample_rate,lp.beat_length,lp.label,lp.quantized as i32,lp.temporary as i32,lp.origin,timestamp_iso()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub fn loops(&self, track: i64) -> Result<Vec<SavedLoop>, String> {
        let mut st=self.db.prepare("SELECT id,start_sample,end_sample,sample_rate,beat_length,label,quantized,temporary,origin FROM loops WHERE track_id=?1 ORDER BY start_sample,id").map_err(|e|e.to_string())?;
        let rows = st
            .query_map(params![track], |r| {
                Ok(SavedLoop {
                    id: r.get(0)?,
                    start_sample: r.get::<_, i64>(1)? as u64,
                    end_sample: r.get::<_, i64>(2)? as u64,
                    sample_rate: r.get(3)?,
                    beat_length: r.get(4)?,
                    label: r.get(5)?,
                    quantized: r.get::<_, i64>(6)? != 0,
                    temporary: r.get::<_, i64>(7)? != 0,
                    origin: r.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn delete_loop(&self, id: &str) -> Result<bool, String> {
        self.db
            .execute("DELETE FROM loops WHERE id=?1", params![id])
            .map(|n| n > 0)
            .map_err(|e| e.to_string())
    }

    pub fn put_beatgrid(
        &self,
        track: i64,
        id: &str,
        layer: &str,
        provenance: Option<&AnalysisProvenance>,
        time_signature: Option<(u8, u8)>,
        segments: &[BeatSegment],
        active: bool,
    ) -> Result<(), String> {
        if id.is_empty() || layer.is_empty() || segments.is_empty() || segments.len() > 1_000_000 {
            return Err("invalid beatgrid identity or segment count".into());
        }
        if time_signature.is_some_and(|(n, d)| n == 0 || d == 0 || n > 32 || d > 32) {
            return Err("invalid time signature".into());
        }
        for s in segments {
            if !s.bpm_start.is_finite()
                || !s.bpm_end.is_finite()
                || s.bpm_start <= 0.0
                || s.bpm_end <= 0.0
                || s.end_sample.is_some_and(|e| e <= s.start_sample)
                || s.beats_per_bar.is_some_and(|n| n == 0 || n > 32)
                || s.downbeat_phase.is_some_and(|phase| {
                    s.beats_per_bar
                        .or_else(|| time_signature.map(|(beats, _)| beats))
                        .is_none_or(|beats| phase >= beats)
                })
            {
                return Err("invalid beatgrid segment".into());
            }
        }
        if segments.windows(2).any(|pair| {
            pair[1].start_sample < pair[0].start_sample
                || pair[0]
                    .end_sample
                    .is_none_or(|end| pair[1].start_sample < end)
        }) {
            return Err("beatgrid segments must be ordered and non-overlapping".into());
        }
        let data = serde_json::to_string(segments).map_err(|e| e.to_string())?;
        let prov = serde_json::to_string(&provenance).map_err(|e| e.to_string())?;
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        if active {
            tx.execute(
                "UPDATE beatgrids SET active=0 WHERE track_id=?1",
                params![track],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute("INSERT INTO beatgrids (id,track_id,layer,analyzer_id,analyzer_version,created_at,confidence,time_signature_num,time_signature_den,segments,provenance,active) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(id) DO UPDATE SET segments=excluded.segments, provenance=excluded.provenance, active=excluded.active",
            params![id,track,layer,provenance.map(|p|p.analyzer_id.as_str()),provenance.map(|p|p.analyzer_version.as_str()),timestamp_iso(),provenance.and_then(|p|p.confidence),time_signature.map(|x|x.0),time_signature.map(|x|x.1),data,prov,active as i32]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// A hand-set beat 1 for the track at `path`: a human 4/4 grid on the
    /// tempo and first beat given, its bars starting on grid beat
    /// `downbeat` (0…3). It takes precedence over the analysis wherever the
    /// grid is read (decks, sync, the AI DJ); `clear_human_grid` undoes it.
    pub fn set_human_downbeat(
        &self,
        path: &str,
        bpm: f64,
        first_beat_s: f64,
        downbeat: u8,
    ) -> Result<(), String> {
        let track = self
            .track_by_path(path)
            .ok_or("the track isn't in the library")?;
        if !(bpm.is_finite() && (40.0..=300.0).contains(&bpm))
            || !(first_beat_s.is_finite() && first_beat_s >= 0.0)
        {
            return Err("no usable tempo or first beat".into());
        }
        let rate: u32 = self
            .db
            .query_row("SELECT rate FROM tracks WHERE id=?1", params![track], |r| {
                r.get::<_, Option<u32>>(0)
            })
            .map_err(|e| e.to_string())?
            .filter(|r| *r > 0)
            .unwrap_or(44_100);
        let segment = BeatSegment {
            start_sample: 0,
            end_sample: None,
            bpm_start: bpm,
            bpm_end: bpm,
            first_beat_sample: (first_beat_s * rate as f64).round() as u64,
            beats_per_bar: Some(4),
            downbeat_phase: Some(downbeat % 4),
            confidence: Some(1.0),
        };
        self.put_beatgrid(
            track,
            &format!("human-downbeat:{track}"),
            "human",
            None,
            Some((4, 4)),
            &[segment],
            true,
        )
    }

    /// Back to the analysis' own grid for the track at `path`: its human
    /// grids stop applying (they stay stored, inactive).
    pub fn clear_human_grid(&self, path: &str) -> Result<(), String> {
        let track = self
            .track_by_path(path)
            .ok_or("the track isn't in the library")?;
        self.db
            .execute(
                "UPDATE beatgrids SET active=0 WHERE track_id=?1 AND layer='human'",
                params![track],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Store the latest analyzer result as an immutable provenance record;
    /// previous rows remain available and can be marked superseded.
    pub fn store_analysis_result(
        &self,
        track: i64,
        kind: &str,
        p: &AnalysisProvenance,
        data: &Value,
    ) -> Result<String, String> {
        validate_provenance(kind, p)?;
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        let id = insert_current_result(&tx, track, kind, p, data)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// Provenance and the track's analysis generation, in one transaction.
    /// The generation is not visible unless the provenance row committed
    /// with it.
    pub fn store_provenance_and_analysis(
        &self,
        track: i64,
        version: u32,
        kind: &str,
        p: &AnalysisProvenance,
        summary: &Value,
        doc: &Value,
    ) -> Result<(), String> {
        validate_provenance(kind, p)?;
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        insert_current_result(&tx, track, kind, p, summary)?;
        write_analysis_doc(&tx, track, version, doc)?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// A current provenance row for this analyzer version.
    pub fn current_analysis_result(
        &self,
        track: i64,
        kind: &str,
        analyzer_id: &str,
        analyzer_version: &str,
    ) -> bool {
        self.db
            .query_row(
                "SELECT 1 FROM analysis_results WHERE track_id=?1 AND kind=?2 AND status='current' AND analyzer_id=?3 AND analyzer_version=?4 LIMIT 1",
                params![track, kind, analyzer_id, analyzer_version],
                |_| Ok(1i64),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// The waveform sidecar named by the asset row is a non-empty file.
    pub fn waveform_sidecar_present(&self, track: i64) -> bool {
        let Ok(rel) = self.db.query_row(
            "SELECT relative_path FROM assets WHERE id=?1 AND kind='waveform'",
            params![format!("waveform-{track}")],
            |r| r.get::<_, String>(0),
        ) else {
            return false;
        };
        std::fs::metadata(self.db_path_parent().join(rel)).is_ok_and(|m| m.is_file() && m.len() > 0)
    }

    pub fn save_set(&self, id: &str, name: &str, value: &Value) -> Result<(), String> {
        if id.is_empty() || name.trim().is_empty() || name.len() > 512 {
            return Err("invalid set identity or name".into());
        }
        let data = serde_json::to_string(value).map_err(|e| e.to_string())?;
        self.db.execute("INSERT INTO sets(id,name,data,created_at,modified_at) VALUES(?1,?2,?3,?4,?4) ON CONFLICT(id) DO UPDATE SET name=excluded.name,data=excluded.data,modified_at=excluded.modified_at",params![id,name,data,timestamp_iso()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    /// Prepared sets, including recoverable trash when requested.
    pub fn saved_sets(&self) -> Result<Vec<(String, String, Value)>, String> {
        let mut stmt = self
            .db
            .prepare("SELECT id,name,data FROM sets ORDER BY modified_at DESC,id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.map(|r| {
            let (id, name, data) = r.map_err(|e| e.to_string())?;
            Ok((
                id,
                name,
                serde_json::from_str(&data).map_err(|e| e.to_string())?,
            ))
        })
        .collect()
    }

    pub fn load_set(&self, id: &str) -> Result<Option<(String, Value)>, String> {
        let row: Option<(String, String)> = self
            .db
            .query_row("SELECT name,data FROM sets WHERE id=?1", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|e| e.to_string())?;
        row.map(|(n, d)| {
            serde_json::from_str(&d)
                .map(|v| (n, v))
                .map_err(|e| e.to_string())
        })
        .transpose()
    }

    pub fn save_transition(
        &self,
        id: &str,
        plan: &TransitionPlan,
        status: &str,
        provenance: &Value,
    ) -> Result<(), String> {
        validate_transition(plan)?;
        let data = serde_json::to_string(plan).map_err(|e| e.to_string())?;
        self.db.execute("INSERT INTO transitions(id,source_track,destination_track,kind,status,data,provenance,created_at) VALUES(?1,?2,?3,'plan',?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET status=excluded.status,data=excluded.data,provenance=excluded.provenance",params![id,plan.source_track,plan.destination_track,status,data,provenance.to_string(),timestamp_iso()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub fn log_session_event(
        &self,
        session_id: &str,
        at: &str,
        kind: &str,
        track: Option<i64>,
        deck: Option<&str>,
        sample: Option<u64>,
        data: &Value,
    ) -> Result<(), String> {
        if session_id.is_empty() || kind.is_empty() || kind.len() > 128 {
            return Err("invalid session event".into());
        }
        self.db
            .execute(
                "INSERT OR IGNORE INTO sessions(id,started_at,data) VALUES(?1,?2,'{}')",
                params![session_id, at],
            )
            .map_err(|e| e.to_string())?;
        self.db.execute("INSERT INTO performance_events(session_id,occurred_at,kind,track_id,deck,sample_position,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![session_id,at,kind,track,deck,sample.map(|x|x as i64),data.to_string()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub fn set_extension(
        &self,
        namespace: &str,
        version: u32,
        value: &Value,
    ) -> Result<(), String> {
        if !package::valid_namespace(namespace) {
            return Err("extension namespace must be a reverse-DNS lowercase name".into());
        }
        self.db.execute("INSERT INTO extensions(namespace,schema_version,data) VALUES(?1,?2,?3) ON CONFLICT(namespace) DO UPDATE SET schema_version=excluded.schema_version,data=excluded.data",params![namespace,version,value.to_string()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub fn store_embedding(
        &self,
        track: i64,
        model_id: &str,
        model_version: &str,
        capability: &str,
        vector: &EmbeddingVector,
        normalization: &str,
        source_input: &str,
    ) -> Result<String, String> {
        if model_id.is_empty()
            || model_id.len() > 256
            || model_version.len() > 128
            || capability.len() > 128
            || normalization.len() > 64
            || source_input.len() > 128
        {
            return Err("invalid embedding metadata".into());
        }
        let bytes = vector.encode()?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut sidecar = PathBuf::from("analysis/embeddings");
        sidecar.push(format!("{id}.pev"));
        let absolute = self.db_path_parent().join(&sidecar);
        std::fs::create_dir_all(absolute.parent().unwrap()).map_err(|e| e.to_string())?;
        let temp = absolute.with_extension("part");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        f.write_all(&bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &absolute).map_err(|e| e.to_string())?;
        let dtype = match vector {
            EmbeddingVector::F32(_) => "f32",
            EmbeddingVector::I8 { .. } => "i8",
        };
        let dim = vector.dimension();
        let checksum = sha256_bytes(&bytes);
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO models(id,version,capability,output_schema,metadata) VALUES(?1,?2,?3,1,'{}') ON CONFLICT(id,version) DO NOTHING",params![model_id,model_version,capability]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO embeddings(id,track_id,model_id,model_version,dimension,dtype,normalization,source_input,sidecar,byte_offset,byte_length,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,?11)",params![id,track,model_id,model_version,dim as i64,dtype,normalization,source_input,sidecar.to_string_lossy(),bytes.len() as i64,timestamp_iso()]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO assets(id,track_id,kind,relative_path,media_type,size,sha256) VALUES(?1,?2,'embedding',?3,'application/vnd.patch.embedding',?4,?5)",params![id,track,sidecar.to_string_lossy(),bytes.len() as i64,checksum]).map_err(|e|e.to_string())?;
        tx.execute(
            "INSERT INTO package_files(relative_path,size,sha256,role) VALUES(?1,?2,?3,'derived')",
            params![sidecar.to_string_lossy(), bytes.len() as i64, checksum],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn db_path_parent(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }

    /// Store an analysis document (versioned) and copy its headline
    /// figures onto the track for search.
    pub fn set_analysis(&self, id: i64, version: u32, doc: &Value) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        write_analysis_doc(&tx, id, version, doc)?;
        tx.commit().map_err(|e| e.to_string())
    }

    // ---- managing: names, collections, analysing again ----------------

    /// Rename a source (and its collection of the same name).
    pub fn rename_source(&self, id: i64, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a source needs a name".into());
        }
        let Some(s) = self.sources().into_iter().find(|s| s.id == id) else {
            return Err("no such source".into());
        };
        if self
            .sources()
            .iter()
            .any(|o| o.id != id && o.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("there is a source called '{name}' already"));
        }
        self.db
            .execute(
                "UPDATE sources SET name = ?1 WHERE id = ?2",
                params![name, id],
            )
            .map_err(|e| e.to_string())?;
        if let Some(c) = self.collection_by_name(&s.name)
            && self.collection_by_name(name).is_none()
        {
            let _ = self.rename_collection(c, name);
        }
        Ok(())
    }

    /// The source whose own collection this is (the librarian keeps one
    /// per source, by name), if any.
    pub fn collection_source(&self, id: i64) -> Option<Source> {
        let name: String = self
            .db
            .query_row(
                "SELECT name FROM playlists WHERE id = ?1 AND smart IS NULL",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()?;
        self.sources().into_iter().find(|s| s.name == name)
    }

    /// Rename a collection. A source's own collection renames the source
    /// with it: else the librarian makes one of the old name again.
    pub fn rename_collection(&self, id: i64, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 256 {
            return Err("a collection name is 1 to 256 characters".into());
        }
        if self.collection_by_name(name).is_some_and(|o| o != id) {
            return Err(format!("there is a collection called '{name}' already"));
        }
        let source = self.collection_source(id);
        if let Some(s) = &source
            && self
                .sources()
                .iter()
                .any(|o| o.id != s.id && o.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("there is a library folder called '{name}' already"));
        }
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE playlists SET name = ?1 WHERE id = ?2 AND smart IS NULL",
            params![name, id],
        )
        .map_err(|e| e.to_string())?;
        if let Some(s) = source {
            tx.execute(
                "UPDATE sources SET name = ?1 WHERE id = ?2",
                params![name, s.id],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Delete a collection (its tracks stay in the library). A source's
    /// own collection takes the source with it (its tracks leave the
    /// library, the files stay on disk): else the librarian makes it again
    /// at its next scan.
    pub fn delete_collection(&self, id: i64, confirm: bool) -> Result<(), String> {
        if !confirm {
            return Err("Deletion blocked: confirmation required (user-friendly safeguard). Use confirm=true to proceed.".to_string());
        }
        if let Some(s) = self.collection_source(id) {
            self.remove_source(&s.path)?;
        }
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        // No set_preps table until the first set prep: nothing to clear.
        let _ = tx.execute("DELETE FROM set_preps WHERE playlist_id = ?1", params![id]);
        let _ = tx.execute("DELETE FROM collection_play WHERE playlist_id = ?1", params![id]);
        tx.execute(
            "DELETE FROM playlists WHERE id = ?1 AND smart IS NULL",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Take tracks out of a collection (they stay in the library).
    pub fn remove_from_collection(
        &self,
        collection: i64,
        tracks: &[i64],
        confirm: bool,
    ) -> Result<(), String> {
        if !confirm {
            return Err("Track removal blocked: confirmation required (user-friendly safeguard). Use confirm=true to proceed.".to_string());
        }
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        for t in tracks {
            tx.execute(
                "DELETE FROM playlist_items WHERE playlist_id = ?1 AND track_id = ?2",
                params![collection, t],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Put tracks in a collection (those already in it stay once).
    pub fn add_many_to_collection(&self, collection: i64, tracks: &[i64]) -> Result<usize, String> {
        let have: std::collections::HashSet<i64> =
            self.collection_tracks(collection).into_iter().collect();
        let mut n = 0;
        for t in tracks.iter().filter(|t| !have.contains(t)) {
            self.add_to_playlist(collection, *t)?;
            n += 1;
        }
        Ok(n)
    }

    /// Import chosen files where they are (audio only; the rest are
    /// skipped) and, given one, put them in a collection. The librarian
    /// analyses them like any other track. Returns (tracks, skipped).
    pub fn import_files(
        &self,
        files: &[PathBuf],
        collection: Option<i64>,
    ) -> Result<(Vec<i64>, usize), String> {
        let mut ids = Vec::new();
        let mut skipped = 0;
        for f in files {
            let audio = f
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()));
            let path = f.canonicalize().unwrap_or_else(|_| f.clone());
            if !audio || !path.is_file() {
                skipped += 1;
                continue;
            }
            match self.import(&path, probe_audio_facts) {
                Ok((id, _)) => ids.push(id),
                Err(_) => skipped += 1,
            }
        }
        if let Some(c) = collection {
            self.add_many_to_collection(c, &ids)?;
        }
        Ok((ids, skipped))
    }

    /// Analyse these tracks again: their current analysis and failure are
    /// forgotten, so the librarian takes them next (history is kept).
    pub fn reanalyse(&self, tracks: &[i64]) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        for t in tracks {
            tx.execute("DELETE FROM analysis WHERE track_id = ?1", params![t])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM analysis_failures WHERE track_id = ?1",
                params![t],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("UPDATE tracks SET bpm = NULL, key = NULL, lufs = NULL, energy = NULL, grid_confidence = NULL, downbeat = NULL WHERE id = ?1", params![t]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    // ---- sources and analysis status ----------------------------------

    /// The folders and drives in the library, in the order they were added.
    /// Everything under `from` is now under `to`: a drive that mounts at
    /// another place (udisks adds a digit, `SAMSUNG_T5` → `SAMSUNG_T52`,
    /// when the usual mount point is still taken). Sources and tracks move
    /// in one transaction; a track's analysis, collections, cues and plays
    /// stay with it (they hang on its id, the analysis on its content).
    /// How many tracks moved.
    pub fn relocate(&self, from: &str, to: &str) -> Result<usize, String> {
        let (from, to) = (from.trim_end_matches('/'), to.trim_end_matches('/'));
        if from.is_empty() || to.is_empty() || from == to {
            return Ok(0);
        }
        // Prefix by length, not LIKE: '_' and '%' are common in drive names.
        let under = "(path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/')";
        let moved = "?2 || substr(path, length(?1) + 1)";
        let tx = rusqlite::Transaction::new_unchecked(&self.db, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        tx.execute(&format!("UPDATE sources SET path = {moved} WHERE {under}"), params![from, to])
            .map_err(|e| e.to_string())?;
        let n = tx
            .execute(&format!("UPDATE tracks SET path = {moved}, missing = 0 WHERE {under}"), params![from, to])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(n)
    }

    /// Up to `n` tracks under a folder, with their sizes: to check that a
    /// drive found elsewhere holds the same files.
    pub fn sample_under(&self, root: &str, n: usize) -> Vec<(String, u64)> {
        let root = root.trim_end_matches('/');
        let Ok(mut st) = self.db.prepare(
            "SELECT path, size FROM tracks WHERE substr(path, 1, length(?1) + 1) = ?1 || '/' ORDER BY id LIMIT ?2",
        ) else {
            return vec![];
        };
        st.query_map(params![root, n as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn sources(&self) -> Vec<Source> {
        let Ok(mut st) = self
            .db
            .prepare("SELECT id, path, name, added_at, scanned_at FROM sources ORDER BY id")
        else {
            return vec![];
        };
        st.query_map([], |r| {
            Ok(Source {
                id: r.get(0)?,
                path: r.get(1)?,
                name: r.get(2)?,
                added_at: r.get(3)?,
                scanned_at: r.get(4)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// A collection of everything in `dir` (found now, analysed by the
    /// librarian after): created as `name` or added to it. The folder is a
    /// source too unless one holds it already, so new files there are
    /// found later. Returns (tracks in it, the scan's report).
    pub fn collection_from_folder(
        &self,
        name: &str,
        dir: &Path,
    ) -> Result<(usize, ScanReport), String> {
        let root = dir
            .canonicalize()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        // The collection first, then the scan: its name shows at once (a big
        // folder scans for minutes, and its tracks are being analysed long
        // before a collection made at the end could show).
        let held = self.sources().iter().any(|s| root.starts_with(&s.path));
        if !held {
            let _ = self.add_source(&root, name);
        }
        let id = match self.collection_by_name(name) {
            Some(id) => id,
            None => self.create_collection(name)?,
        };
        // Fill as tracks come in, a batch at a time.
        let mut batch = Vec::new();
        let report = self.scan_with(&root, crate::probe_audio_facts, |t, _| {
            batch.push(t);
            if batch.len() >= 200 {
                let _ = self.add_all_to_collection(id, &batch);
                batch.clear();
            }
        })?;
        let all: Vec<i64> = self
            .tracks()?
            .iter()
            .filter(|t| !t.missing && Path::new(&t.path).starts_with(&root))
            .map(|t| t.id)
            .collect();
        self.add_all_to_collection(id, &all)?;
        Ok((all.len(), report))
    }

    /// What changes when tracks come, go or move: (tracks, newest id,
    /// missing ones). Cheap: for noticing another process's changes.
    /// Changes when tracks, collections (and what's in them) or sources
    /// change: a window watching it reloads. (Tracks alone missed a new
    /// collection over tracks already in the library: it never showed.)
    pub fn signature(&self) -> (i64, i64, i64) {
        let q = |sql: &str| {
            self.db
                .query_row(sql, [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                .unwrap_or((0, 0))
        };
        let (tracks, max_track) = q("SELECT count(*), coalesce(max(id), 0) FROM tracks");
        let (missing, _) = q("SELECT coalesce(sum(missing), 0), 0 FROM tracks");
        let (lists, max_list) = q("SELECT count(*), coalesce(max(id), 0) FROM playlists");
        // The names too: a renamed collection shows in every window.
        let names = self
            .db
            .query_row(
                "SELECT coalesce(group_concat(name, char(31)), '') FROM playlists",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default();
        let names = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            names.hash(&mut h);
            (h.finish() & 0xffff_ffff) as i64
        };
        let (plays, _) = q("SELECT coalesce(sum(plays), 0), 0 FROM tracks");
        // The order too: a track moved in a set prep shows in every window.
        let (items, order) = q("SELECT count(*), coalesce(sum(pos * (track_id % 1009)), 0) FROM playlist_items");
        let (sources, max_source) = q("SELECT count(*), coalesce(max(id), 0) FROM sources");
        // How each collection plays (in order or as the DJ decides).
        let (play, _) = q("SELECT coalesce(sum(playlist_id * 2 + in_order), 0), 0 FROM collection_play");
        // Favourites show in every window too: which tracks, not just how many.
        let (favourites, favourite_ids) = q(
            "SELECT count(*), coalesce(sum(id), 0) FROM tracks WHERE tags LIKE '%\"favourite\"%'",
        );
        // Folded into three numbers, as before (callers compare them).
        (
            tracks * 1_000_003 + max_track,
            (missing * 1_000_003 + items).wrapping_mul(31).wrapping_add(order),
            ((lists * 1_000_003 + max_list) * 1_000_003 + sources * 1_009 + max_source)
                .wrapping_mul(31)
                .wrapping_add(names)
                .wrapping_mul(31)
                .wrapping_add(plays)
                .wrapping_mul(31)
                .wrapping_add(favourites * 1_000_003 + favourite_ids)
                .wrapping_mul(31)
                .wrapping_add(play),
        )
    }

    /// Add a folder or drive (its tracks are found on the next scan).
    pub fn add_source(&self, path: &Path, name: &str) -> Result<i64, String> {
        let path = path.to_string_lossy().trim_end_matches('/').to_string();
        let name = name.trim();
        if path.is_empty() || name.is_empty() {
            return Err("a source needs a folder and a name".into());
        }
        if let Some(s) = self
            .sources()
            .into_iter()
            .find(|s| s.path == path || s.name.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "'{}' is already in the library ({})",
                s.name, s.path
            ));
        }
        self.db
            .execute(
                "INSERT INTO sources (path, name, added_at) VALUES (?1, ?2, ?3)",
                params![path, name, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(self.db.last_insert_rowid())
    }

    /// Take a source out of the library: its tracks are hidden (their cues,
    /// history and analyses are kept, should it come back).
    pub fn remove_source(&self, name_or_path: &str) -> Result<Source, String> {
        let want = name_or_path.trim_end_matches('/');
        let s = self
            .sources()
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(want) || s.path == want)
            .ok_or_else(|| format!("no source '{name_or_path}'"))?;
        self.db
            .execute("DELETE FROM sources WHERE id = ?1", params![s.id])
            .map_err(|e| e.to_string())?;
        self.db
            .execute(
                "UPDATE tracks SET missing = 1 WHERE substr(path, 1, length(?1)) = ?1",
                params![format!("{}/", s.path)],
            )
            .map_err(|e| e.to_string())?;
        Ok(s)
    }

    pub fn set_scanned(&self, source: i64) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE sources SET scanned_at = ?1 WHERE id = ?2",
                params![now(), source],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// This version of the analysis couldn't read the track: why.
    pub fn set_failure(&self, track: i64, version: u32, error: &str) -> Result<(), String> {
        self.db
            .execute(
                "INSERT INTO analysis_failures (track_id, version, error, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track_id) DO UPDATE SET version = excluded.version, error = excluded.error, at = excluded.at",
                params![track, version, error, now()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn clear_failure(&self, track: i64) -> Result<(), String> {
        self.db
            .execute(
                "DELETE FROM analysis_failures WHERE track_id = ?1",
                params![track],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Why a track's analysis failed (this version), if it did.
    pub fn failure(&self, track: i64, version: u32) -> Option<String> {
        self.db
            .query_row(
                "SELECT error FROM analysis_failures WHERE track_id = ?1 AND version >= ?2",
                params![track, version],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Tracks still to analyse (none that failed with this version), those
    /// under `first` ahead of the rest.
    pub fn queue(&self, version: u32) -> Result<Vec<(i64, String)>, String> {
        let mut st = self
            .db
            .prepare(
                "SELECT t.id, t.path FROM tracks t LEFT JOIN analysis a ON a.track_id = t.id LEFT JOIN analysis_failures f ON f.track_id = t.id
                 WHERE t.missing = 0 AND (a.version IS NULL OR a.version < ?1) AND (f.version IS NULL OR f.version < ?1) ORDER BY t.added_at, t.id",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![version], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        Ok(rows.flatten().collect())
    }

    /// Revalidate a bounded group selected from an available queue snapshot.
    /// Primary-key lookups avoid rescanning disconnected sources per page.
    pub fn analysis_batch(&self, version: u32, ids: &[i64]) -> Result<Vec<(i64, String)>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > 64 {
            return Err("analysis batch exceeds 64 tracks".into());
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT t.id,t.path FROM tracks t LEFT JOIN analysis a ON a.track_id=t.id LEFT JOIN analysis_failures f ON f.track_id=t.id WHERE t.missing=0 AND (a.version IS NULL OR a.version<?) AND (f.version IS NULL OR f.version<?) AND t.id IN ({placeholders}) ORDER BY t.added_at,t.id"
        );
        let values = [version as i64, version as i64]
            .into_iter()
            .chain(ids.iter().copied());
        let mut st = self.db.prepare(&sql).map_err(|e| e.to_string())?;
        st.query_map(rusqlite::params_from_iter(values), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
    }

    /// A queued item can be removed, moved or completed by another frontend.
    pub fn analysis_pending(&self, version: u32, id: i64, path: &str) -> Result<bool, String> {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM tracks t LEFT JOIN analysis a ON a.track_id=t.id LEFT JOIN analysis_failures f ON f.track_id=t.id WHERE t.id=?2 AND t.path=?3 AND t.missing=0 AND (a.version IS NULL OR a.version<?1) AND (f.version IS NULL OR f.version<?1))", params![version,id,path], |r| r.get(0)).map_err(|e| e.to_string())
    }

    /// The SQL for a soft-beat track, or none before the first
    /// confirmation made its table.
    fn soft_sql(&self) -> &'static str {
        let exists = self
            .db
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'confirmed_tempo'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if exists { SOFT } else { SOFT_NONE }
    }

    /// Check up to `limit` analysed tracks not checked against their
    /// current analysis yet: is their tempo confirmed by the file's own BPM
    /// (tag or name)? How many were checked. The librarian runs it until
    /// none is left; a fresh analysis checks its track as it is written.
    pub fn confirm_tempos(&self, limit: usize) -> Result<usize, String> {
        self.db.execute(CONFIRMED_TEMPO, []).map_err(|e| e.to_string())?;
        let rows: Vec<(i64, i64, Option<f64>, Option<f64>, String)> = {
            let mut st = self
                .db
                .prepare(
                    "SELECT t.id, a.at, t.bpm, json_extract(a.data, '$.tag_bpm'), t.path
                     FROM tracks t JOIN analysis a ON a.track_id = t.id
                     LEFT JOIN confirmed_tempo c ON c.track_id = t.id
                     WHERE t.missing = 0 AND (c.track_id IS NULL OR c.analysed_at != a.at)
                     LIMIT ?1",
                )
                .map_err(|e| e.to_string())?;
            st.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        };
        for (id, at, bpm, tag, path) in &rows {
            store_confirmed_tempo(&self.db, *id, *at, *bpm, *tag, path)?;
        }
        Ok(rows.len())
    }

    /// The file's own BPM that confirms `path`'s analysed tempo, and where
    /// it was written ("tag", "file name"), if one does.
    pub fn confirmed_tempo(&self, path: &str) -> Option<(f64, String)> {
        self.db
            .query_row(
                "SELECT c.bpm, c.source FROM confirmed_tempo c JOIN tracks t ON t.id = c.track_id
                 WHERE t.path = ?1 AND c.bpm IS NOT NULL",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    /// Where the tracks under `root` stand (see [`Readiness`]).
    pub fn counts_under(&self, root: &str, version: u32) -> Counts {
        let root = format!("{}/", root.trim_end_matches('/'));
        self.db
            .query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM({READY}), 0), COALESCE(SUM(a.version >= ?2), 0), COALESCE(SUM(f.version >= ?2 AND (a.version IS NULL OR a.version < ?2)), 0),
                            COALESCE(SUM(a.version >= ?2 AND NOT {READY} AND {soft}), 0)
                     FROM tracks t LEFT JOIN analysis a ON a.track_id = t.id LEFT JOIN analysis_failures f ON f.track_id = t.id
                     WHERE t.missing = 0 AND substr(t.path, 1, length(?1)) = ?1",
                    soft = self.soft_sql()
                ),
                params![root, version],
                |r| {
                    let (tracks, ready, analysed, failed, soft): (i64, i64, i64, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
                    Ok(Counts { tracks: tracks as usize, ready: ready as usize, soft: soft as usize, check: (analysed - ready - soft).max(0) as usize, failed: failed as usize, waiting: (tracks - analysed - failed).max(0) as usize })
                },
            )
            .unwrap_or_default()
    }

    /// Per track: its readiness, and why it failed if it did.
    pub fn readiness(
        &self,
        version: u32,
    ) -> std::collections::HashMap<String, (Readiness, Option<String>)> {
        let Ok(mut st) = self.db.prepare(&format!(
            "SELECT t.path, {READY}, a.version >= ?1, f.version >= ?1, f.error, {soft} FROM tracks t LEFT JOIN analysis a ON a.track_id = t.id LEFT JOIN analysis_failures f ON f.track_id = t.id WHERE t.missing = 0",
            soft = self.soft_sql()
        )) else {
            return Default::default();
        };
        st.query_map(params![version], |r| {
            let (ready, analysed, failed): (Option<bool>, Option<bool>, Option<bool>) =
                (r.get(1)?, r.get(2)?, r.get(3)?);
            let soft: Option<bool> = r.get(5)?;
            let state = if ready == Some(true) && analysed == Some(true) {
                Readiness::Ready
            } else if analysed == Some(true) && soft == Some(true) {
                Readiness::Soft
            } else if analysed == Some(true) {
                Readiness::Check
            } else if failed == Some(true) {
                Readiness::Failed
            } else {
                Readiness::Waiting
            };
            Ok((
                r.get::<_, String>(0)?,
                (state, r.get::<_, Option<String>>(4)?),
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Tracks whose stored analysis is older than `version` (or missing).
    pub fn needs_analysis(&self, version: u32) -> Result<Vec<(i64, String)>, String> {
        let mut st = self
            .db
            .prepare("SELECT t.id, t.path FROM tracks t LEFT JOIN analysis a ON a.track_id = t.id WHERE t.missing = 0 AND (a.version IS NULL OR a.version < ?1)")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![version], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        Ok(rows.flatten().collect())
    }

    pub fn performance(&self, id: i64) -> Performance {
        self.db
            .query_row(
                "SELECT data FROM performance WHERE track_id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Human-authored fixed beat grids keyed by source path. Kept separate
    /// from analyzer records so cache refreshes cannot replace them.
    pub fn manual_performance_grids(
        &self,
    ) -> Result<std::collections::HashMap<String, (f64, f64)>, String> {
        let mut statement = self
            .db
            .prepare("SELECT t.path, p.data FROM tracks t JOIN performance p ON p.track_id=t.id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut grids = std::collections::HashMap::new();
        for row in rows {
            let (path, json) = row.map_err(|e| e.to_string())?;
            let Ok(performance) = serde_json::from_str::<Performance>(&json) else {
                continue;
            };
            if let Some((bpm, offset)) = performance.grid
                && bpm.is_finite()
                && (40.0..=300.0).contains(&bpm)
                && offset.is_finite()
                && offset >= 0.0
            {
                grids.insert(path, (bpm, offset));
            }
        }
        Ok(grids)
    }

    /// Human-authored active beatgrids take precedence over the legacy fixed
    /// performance grid. The first segment supplies the current engine's
    /// scalar sync grid; the full segmented record remains stored unchanged.
    pub fn human_grid_overrides(
        &self,
    ) -> Result<std::collections::HashMap<String, HumanGridOverride>, String> {
        let mut overrides = self
            .manual_performance_grids()?
            .into_iter()
            .map(|(path, (bpm, first_beat_s))| {
                (
                    path,
                    HumanGridOverride {
                        bpm,
                        first_beat_s,
                        beats_per_bar: None,
                        downbeat_phase: None,
                        sample_rate: 44_100,
                        segments: Vec::new(),
                    },
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut statement = self
            .db
            .prepare("SELECT t.path, t.rate, b.time_signature_num, b.segments FROM beatgrids b JOIN tracks t ON t.id=b.track_id WHERE b.active=1 AND b.layer='human'")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, Option<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (path, sample_rate, meter, json) = row.map_err(|e| e.to_string())?;
            let Ok(segments) = serde_json::from_str::<Vec<BeatSegment>>(&json) else {
                continue;
            };
            let Some(first) = segments.first() else {
                continue;
            };
            let rate = if sample_rate == 0 {
                44_100
            } else {
                sample_rate
            };
            let grid = HumanGridOverride {
                bpm: first.bpm_start,
                first_beat_s: first.first_beat_sample as f64 / rate as f64,
                beats_per_bar: first.beats_per_bar.or(meter),
                downbeat_phase: first.downbeat_phase,
                sample_rate: rate,
                segments: segments.clone(),
            };
            if grid.bpm.is_finite()
                && (40.0..=300.0).contains(&grid.bpm)
                && grid.first_beat_s.is_finite()
                && grid.first_beat_s >= 0.0
                && grid
                    .beats_per_bar
                    .zip(grid.downbeat_phase)
                    .is_none_or(|(beats, phase)| beats > 0 && phase < beats)
            {
                overrides.insert(path, grid);
            }
        }
        Ok(overrides)
    }

    pub fn set_performance(&self, id: i64, p: &Performance) -> Result<(), String> {
        let data = serde_json::to_string(p).map_err(|e| e.to_string())?;
        self.db
            .execute(
                "INSERT INTO performance (track_id, data) VALUES (?1, ?2) ON CONFLICT(track_id) DO UPDATE SET data = excluded.data",
                params![id, data],
            )
            .map_err(|e| e.to_string())?;
        let tags = serde_json::to_string(&p.tags).unwrap_or_else(|_| "[]".into());
        self.db
            .execute(
                "UPDATE tracks SET rating = ?1, tags = ?2, color = ?3 WHERE id = ?4",
                params![p.rating, tags, p.color, id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Mark a track as a favourite or take the mark off. Kept with the track's
    /// other tags, so a rescan, a move or an older release leaves it alone.
    pub fn set_favourite(&self, id: i64, on: bool) -> Result<(), String> {
        let mut perf = self.performance(id);
        perf.tags.retain(|t| t != FAVOURITE_TAG);
        if on {
            perf.tags.push(FAVOURITE_TAG.into());
        }
        self.set_performance(id, &perf)
    }

    /// Append a performance event (load, play, cue, loop, sync, transition…).
    pub fn log(
        &self,
        event: &str,
        deck: Option<&str>,
        track: Option<i64>,
        data: &Value,
    ) -> Result<(), String> {
        self.db
            .execute(
                "INSERT INTO history (at, event, deck, track_id, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now(), event, deck, track, data.to_string()],
            )
            .map_err(|e| e.to_string())?;
        if event == "play"
            && let Some(t) = track
        {
            self.db
                .execute(
                    "UPDATE tracks SET plays = plays + 1, last_played = ?1 WHERE id = ?2",
                    params![now(), t],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Recent history, newest first: (unix seconds, event, deck, track id, data).
    pub fn history(&self, limit: usize) -> Vec<(i64, String, Option<String>, Option<i64>, Value)> {
        let Ok(mut st) = self.db.prepare(
            "SELECT at, event, deck, track_id, data FROM history ORDER BY id DESC LIMIT ?1",
        ) else {
            return vec![];
        };
        st.query_map(params![limit as i64], |r| {
            let data: String = r.get(4)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                serde_json::from_str(&data).unwrap_or(Value::Null),
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn create_playlist(&self, name: &str, smart: Option<&Filter>) -> Result<i64, String> {
        let q = smart.map(|f| serde_json::to_string(f).unwrap_or_default());
        self.db
            .execute(
                "INSERT INTO playlists (name, smart) VALUES (?1, ?2)",
                params![name, q],
            )
            .map_err(|e| e.to_string())?;
        Ok(self.db.last_insert_rowid())
    }

    /// Create a manual collection. Collections use the ordered membership
    /// table already used by static playlists, keeping the first feature
    /// small while preserving track identity and package portability.
    pub fn create_collection(&self, name: &str) -> Result<i64, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 256 {
            return Err("collection name must be between 1 and 256 bytes".into());
        }
        if self.collection_by_name(name).is_some() {
            return Err(format!("collection '{name}' already exists"));
        }
        self.create_playlist(name, None)
    }

    pub fn collection_by_name(&self, name: &str) -> Option<i64> {
        self.db
            .query_row(
                "SELECT id FROM playlists WHERE name=?1 AND smart IS NULL ORDER BY id LIMIT 1",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// The collections that are set preparations: a set being put
    /// together while pre-listening, in the order the tracks were added.
    /// Kept in a table of their own, made when the first one is, so a
    /// library older Patches read stays as it was.
    pub fn set_preps(&self) -> std::collections::HashSet<i64> {
        let Ok(mut st) = self.db.prepare(
            "SELECT s.playlist_id FROM set_preps s JOIN playlists p ON p.id = s.playlist_id",
        ) else {
            return Default::default();
        };
        st.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The set preps whose stems are all prepared ahead of the set (their
    /// own option, off by default: the user, 2026-11-14). A table of its
    /// own, made the first time one is switched on.
    pub fn set_preps_with_stems(&self) -> std::collections::HashSet<i64> {
        let Ok(mut st) = self.db.prepare(
            "SELECT s.playlist_id FROM set_prep_stems s JOIN set_preps p ON p.playlist_id = s.playlist_id",
        ) else {
            return Default::default();
        };
        st.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Switch a set prep's "prepare all stems" on or off. Only a set prep.
    pub fn set_set_prep_stems(&self, id: i64, on: bool) -> Result<(), String> {
        if !self.set_preps().contains(&id) {
            return Err("only a set prep has this option".into());
        }
        self.db
            .execute(
                "CREATE TABLE IF NOT EXISTS set_prep_stems (playlist_id INTEGER PRIMARY KEY REFERENCES playlists(id))",
                [],
            )
            .map_err(|e| e.to_string())?;
        let sql = if on {
            "INSERT OR IGNORE INTO set_prep_stems (playlist_id) VALUES (?1)"
        } else {
            "DELETE FROM set_prep_stems WHERE playlist_id = ?1"
        };
        self.db.execute(sql, params![id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Make a set prep (a collection marked as one).
    pub fn create_set_prep(&self, name: &str) -> Result<i64, String> {
        self.db
            .execute(
                "CREATE TABLE IF NOT EXISTS set_preps (playlist_id INTEGER PRIMARY KEY REFERENCES playlists(id))",
                [],
            )
            .map_err(|e| e.to_string())?;
        let id = self.create_collection(name)?;
        self.db
            .execute("INSERT OR IGNORE INTO set_preps (playlist_id) VALUES (?1)", params![id])
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// How the AI DJ plays each collection whose order was chosen: true
    /// in its listed order, false as the DJ decides. Every collection, a
    /// set prep too, has the DJ decide until told otherwise (the user,
    /// 2026-11-13).
    pub fn play_orders(&self) -> std::collections::HashMap<i64, bool> {
        let Ok(mut st) = self.db.prepare("SELECT playlist_id, in_order FROM collection_play") else {
            return Default::default();
        };
        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Whether the AI DJ plays this collection in its listed order.
    pub fn plays_in_order(&self, collection: i64) -> bool {
        self.play_orders().get(&collection).copied().unwrap_or(false)
    }

    /// Play a collection in its listed order, or as the DJ decides.
    pub fn set_play_in_order(&self, collection: i64, in_order: bool) -> Result<(), String> {
        self.db
            .execute(
                "CREATE TABLE IF NOT EXISTS collection_play (playlist_id INTEGER PRIMARY KEY REFERENCES playlists(id) ON DELETE CASCADE, in_order INTEGER NOT NULL)",
                [],
            )
            .map_err(|e| e.to_string())?;
        self.db
            .execute(
                "INSERT INTO collection_play (playlist_id, in_order) VALUES (?1, ?2) ON CONFLICT(playlist_id) DO UPDATE SET in_order = excluded.in_order",
                params![collection, in_order],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// A collection's track paths in its order.
    pub fn collection_paths(&self, collection: i64) -> Vec<String> {
        let Ok(mut st) = self.db.prepare(
            "SELECT t.path FROM playlist_items i JOIN tracks t ON t.id = i.track_id WHERE i.playlist_id = ?1 ORDER BY i.pos, i.rowid",
        ) else {
            return vec![];
        };
        st.query_map(params![collection], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Move a track one place earlier (`-1`) or later (`1`) in a collection.
    pub fn move_in_collection(&self, collection: i64, track: i64, by: i32) -> Result<(), String> {
        let order = self.collection_tracks(collection);
        let Some(at) = order.iter().position(|t| *t == track) else {
            return Err("the track isn't in it".into());
        };
        let to = at as i64 + by.signum() as i64;
        if to < 0 || to as usize >= order.len() {
            return Ok(());
        }
        let mut order = order;
        order.swap(at, to as usize);
        let tx = rusqlite::Transaction::new_unchecked(&self.db, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (pos, t) in order.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_items SET pos = ?1 WHERE playlist_id = ?2 AND track_id = ?3",
                params![pos as i64, collection, t],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// (id, name, member count) for manual collections.
    /// The folders right inside each source that hold tracks and aren't a
    /// collection yet (by name): what "+ Collection" offers, one click
    /// each. (Path, name, tracks), by name.
    pub fn folder_offers(&self) -> Vec<(String, String, usize)> {
        let taken: Vec<String> = self
            .collections()
            .into_iter()
            .map(|(_, n, _)| n.to_lowercase())
            .collect();
        let mut found: std::collections::BTreeMap<String, (String, usize)> = Default::default();
        let Ok(tracks) = self.tracks() else {
            return vec![];
        };
        // Only the places music lives in (Music, a drive) offer their
        // folders. A folder made a collection is one already: offering its
        // insides listed 60 year folders of "NL Top 40" (user, 2026-11-12).
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let user = std::env::var("USER").unwrap_or_default();
        for s in self.sources() {
            let root = Path::new(&s.path);
            if !music_place(root, home.as_deref(), &user) {
                continue;
            }
            for t in tracks.iter().filter(|t| !t.missing) {
                let Ok(rel) = Path::new(&t.path).strip_prefix(root) else {
                    continue;
                };
                let mut parts = rel.components();
                let (Some(first), Some(_)) = (parts.next(), parts.next()) else {
                    continue;
                };
                let name = first.as_os_str().to_string_lossy().to_string();
                if taken.contains(&name.to_lowercase()) {
                    continue;
                }
                let e = found
                    .entry(root.join(&name).to_string_lossy().to_string())
                    .or_insert((name, 0));
                e.1 += 1;
            }
        }
        let mut out: Vec<(String, String, usize)> =
            found.into_iter().map(|(p, (n, c))| (p, n, c)).collect();
        out.sort_by_key(|(_, n, _)| n.to_lowercase());
        out
    }

    pub fn collections(&self) -> Vec<(i64, String, usize)> {
        let Ok(mut st) = self.db.prepare(
            "SELECT p.id,p.name,COUNT(i.track_id) FROM playlists p LEFT JOIN playlist_items i ON i.playlist_id=p.id WHERE p.smart IS NULL GROUP BY p.id,p.name ORDER BY p.name COLLATE NOCASE"
        ) else { return vec![] };
        st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as usize))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn add_to_collection(&self, collection: i64, track: i64) -> Result<(), String> {
        let exists: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1 AND smart IS NULL)",
                params![collection],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !exists {
            return Err("collection not found".into());
        }
        // One statement, so two processes filling the same collection (the
        // librarian and an "add folder") cannot both add a track: a check
        // and then an insert let 3 913 tracks in twice (2026-11-12).
        self.db
            .execute(
                "INSERT INTO playlist_items (playlist_id, pos, track_id)
                 SELECT ?1, (SELECT COALESCE(MAX(pos), -1) + 1 FROM playlist_items WHERE playlist_id = ?1), ?2
                 WHERE NOT EXISTS (SELECT 1 FROM playlist_items WHERE playlist_id = ?1 AND track_id = ?2)",
                params![collection, track],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Add many tracks to a collection in one transaction (each once).
    pub fn add_all_to_collection(&self, collection: i64, tracks: &[i64]) -> Result<(), String> {
        // Immediate: a deferred one that reads first cannot take the write
        // lock once another process has written (SQLite answers busy at once).
        let tx = rusqlite::Transaction::new_unchecked(&self.db, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for &t in tracks {
            self.add_to_collection(collection, t)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn collection_tracks(&self, collection: i64) -> Vec<i64> {
        let Ok(mut st) = self
            .db
            .prepare("SELECT track_id FROM playlist_items WHERE playlist_id=?1 ORDER BY pos, rowid")
        else {
            return vec![];
        };
        st.query_map(params![collection], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Collection membership paths in one join; no full Track materialization.
    pub fn collection_memberships(
        &self,
    ) -> Result<std::collections::HashMap<i64, std::collections::HashSet<String>>, String> {
        let mut st = self.db.prepare("SELECT i.playlist_id,t.path FROM playlist_items i JOIN tracks t ON t.id=i.track_id JOIN playlists p ON p.id=i.playlist_id WHERE p.smart IS NULL").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = std::collections::HashMap::<i64, std::collections::HashSet<String>>::new();
        for row in rows {
            let (id, path) = row.map_err(|e| e.to_string())?;
            out.entry(id).or_default().insert(path);
        }
        Ok(out)
    }

    pub fn add_to_playlist(&self, playlist: i64, track: i64) -> Result<(), String> {
        let pos: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(MAX(pos), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
                params![playlist],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        self.db
            .execute(
                "INSERT INTO playlist_items (playlist_id, pos, track_id) VALUES (?1, ?2, ?3)",
                params![playlist, pos, track],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Tracks of a playlist (a smart playlist evaluates its filter).
    pub fn playlist(&self, playlist: i64, index: &Index) -> Vec<i64> {
        let smart: Option<String> = self
            .db
            .query_row(
                "SELECT smart FROM playlists WHERE id = ?1",
                params![playlist],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        if let Some(f) = smart.and_then(|s| serde_json::from_str::<Filter>(&s).ok()) {
            return index.search(&f).into_iter().map(|t| t.id).collect();
        }
        let Ok(mut st) = self
            .db
            .prepare("SELECT track_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY pos")
        else {
            return vec![];
        };
        st.query_map(params![playlist], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn playlists(&self) -> Vec<(i64, String, bool)> {
        let Ok(mut st) = self
            .db
            .prepare("SELECT id, name, smart IS NOT NULL FROM playlists ORDER BY name")
        else {
            return vec![];
        };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Changes committed by other connections, for a long-lived reader.
    /// This connection's own writes do not change the value.
    pub fn data_version(&self) -> Result<i64, String> {
        self.db
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    /// The AI's library pool, without copying format facts or parsing tags
    /// it never consumes. Ordering matches the tracks table scan.
    pub fn dj_pool(&self) -> Result<Vec<(String, String, String, u8)>, String> {
        let mut st = self
            .db
            .prepare("SELECT path,title,artist,rating FROM tracks WHERE missing=0 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let mut pool = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u8))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<(String, String, String, u8)>, _>>()
            .map_err(|e| e.to_string())?;
        // The DJ's own title and artist (the AI DJ's variety reads artists).
        let edits = self.track_edits();
        if !edits.is_empty() {
            for (path, title, artist, _) in &mut pool {
                if let Some(e) = edits.get(path.as_str()) {
                    if let Some(v) = &e.title { *title = v.clone(); }
                    if let Some(v) = &e.artist { *artist = v.clone(); }
                }
            }
        }
        Ok(pool)
    }

    /// What the AI DJ's first pass over a large pool reads, per path: BPM,
    /// Camelot key, energy (0–1) and length in seconds, from one query and
    /// no analysis files.
    pub fn dj_facts(&self) -> Result<std::collections::HashMap<String, DjFacts>, String> {
        let mut st = self
            .db
            .prepare("SELECT path, bpm, key, energy, duration FROM tracks WHERE missing=0")
            .map_err(|e| e.to_string())?;
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                DjFacts {
                    bpm: r.get(1)?,
                    key: r.get(2)?,
                    energy: r.get(3)?,
                    duration: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                },
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
    }

    /// All tracks (for the search index).
    pub fn tracks(&self) -> Result<Vec<Track>, String> {
        let mut st = self
            .db
            .prepare(
                "SELECT id, content_id, path, size, title, artist, album, genre, codec, rate, bits, channels, duration, added_at,
                        last_played, plays, missing, bpm, key, lufs, energy, rating, tags, color, track_uuid FROM tracks",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                let tags: Option<String> = r.get(22)?;
                Ok(Track {
                    id: r.get(0)?,
                    content_id: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    title: r.get(4)?,
                    artist: r.get(5)?,
                    album: r.get(6)?,
                    genre: r.get(7)?,
                    codec: r.get(8)?,
                    rate: r.get(9)?,
                    bits: r.get(10)?,
                    channels: r.get(11)?,
                    duration: r.get(12)?,
                    added_at: r.get(13)?,
                    last_played: r.get(14)?,
                    plays: r.get(15)?,
                    missing: r.get::<_, i64>(16)? != 0,
                    bpm: r.get(17)?,
                    key: r.get(18)?,
                    lufs: r.get(19)?,
                    energy: r.get(20)?,
                    rating: r.get::<_, i64>(21)? as u8,
                    tags: tags
                        .and_then(|t| serde_json::from_str(&t).ok())
                        .unwrap_or_default(),
                    color: r.get(23)?,
                    track_uuid: r.get::<_, Option<String>>(24)?.unwrap_or_else(|| {
                        format!("legacy-{}", r.get::<_, i64>(0).unwrap_or_default())
                    }),
                })
            })
            .map_err(|e| e.to_string())?;
        let mut tracks: Vec<Track> = rows.flatten().collect();
        // The DJ's own title, artist and genre win over the file's tags.
        let edits = self.track_edits();
        if !edits.is_empty() {
            for t in &mut tracks {
                if let Some(e) = edits.get(&t.path) {
                    if let Some(v) = &e.title { t.title = v.clone(); }
                    if let Some(v) = &e.artist { t.artist = v.clone(); }
                    if let Some(v) = &e.genre { t.genre = v.clone(); }
                }
            }
        }
        Ok(tracks)
    }

    /// Resolve a loaded source path to its stable track identity without
    /// materializing the entire library. Used by live deck status paths.
    pub fn track_uuid_for_path(&self, path: &str) -> Result<Option<String>, String> {
        self.db
            .query_row(
                "SELECT track_uuid FROM tracks WHERE path = ?1 LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }
}

fn backfill_track_uuids(db: &Connection) -> Result<(), String> {
    let mut st = db
        .prepare("SELECT id FROM tracks WHERE track_uuid IS NULL OR track_uuid LIKE 'legacy-%'")
        .map_err(|e| e.to_string())?;
    let ids = st
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    for id in ids {
        tx.execute(
            "UPDATE tracks SET track_uuid=?1 WHERE id=?2",
            params![uuid::Uuid::new_v4().to_string(), id],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn validate_cue(c: &Cue) -> Result<(), String> {
    if c.id.is_empty()
        || c.id.len() > 128
        || c.kind.is_empty()
        || c.kind.len() > 64
        || c.sample_rate == 0
        || c.sample_position > i64::MAX as u64
    {
        return Err("invalid cue identity or position".into());
    }
    if c.beat_position.is_some_and(|x| !x.is_finite() || x < 0.0)
        || c.confidence
            .is_some_and(|x| !x.is_finite() || !(0.0..=1.0).contains(&x))
    {
        return Err("invalid cue beat position or confidence".into());
    }
    Ok(())
}

fn validate_transition(t: &TransitionPlan) -> Result<(), String> {
    if t.source_track == t.destination_track || t.sample_rate == 0 || t.duration_samples == 0 {
        return Err("transition needs two tracks, a sample rate and positive duration".into());
    }
    if t.confidence
        .is_some_and(|x| !x.is_finite() || !(0.0..=1.0).contains(&x))
    {
        return Err("transition confidence must be in 0…1".into());
    }
    for [time, value] in &t.strategy.fader_curve {
        if !time.is_finite() || !value.is_finite() {
            return Err("transition controls must be finite".into());
        }
    }
    for m in t
        .strategy
        .eq_moves
        .iter()
        .chain(&t.strategy.effects)
        .chain(&t.strategy.stems)
    {
        if m.target.is_empty()
            || m.target.len() > 128
            || !m.at_seconds.is_finite()
            || m.at_seconds < 0.0
            || !m.value.is_finite()
        {
            return Err("invalid structured transition control".into());
        }
    }
    Ok(())
}

fn validate_provenance(kind: &str, p: &AnalysisProvenance) -> Result<(), String> {
    if kind.is_empty()
        || kind.len() > 128
        || p.analyzer_id.len() > 256
        || p.analyzer_version.len() > 128
    {
        Err("invalid analysis provenance".into())
    } else {
        Ok(())
    }
}

/// Write `bytes` via a temporary file in the same directory, fsync, then rename.
/// The directory entry is synced afterwards so the name survives a crash.
fn publish_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "{}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

fn insert_current_result(
    conn: &Connection,
    track: i64,
    kind: &str,
    p: &AnalysisProvenance,
    data: &Value,
) -> Result<String, String> {
    let text = serde_json::to_string(data).map_err(|e| e.to_string())?;
    // The same result again (the same analyzer, the same data): keep the
    // one there is. Storing each copy grew the library by 6 KB a time.
    let same: Option<String> = conn
        .query_row(
            "SELECT id FROM analysis_results WHERE track_id=?1 AND kind=?2 AND status='current' AND analyzer_id=?3 AND analyzer_version=?4 AND data=?5",
            params![track, kind, p.analyzer_id, p.analyzer_version, text],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(same) = same {
        return Ok(same);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let old: Option<String> = conn.query_row("SELECT id FROM analysis_results WHERE track_id=?1 AND kind=?2 AND status='current' ORDER BY created_at DESC LIMIT 1", params![track,kind], |r|r.get(0)).optional().map_err(|e|e.to_string())?;
    conn.execute("INSERT INTO models(id,version,capability,output_schema,metadata) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id,version) DO NOTHING",params![p.analyzer_id,p.analyzer_version,kind,p.schema_version,p.parameters.to_string()]).map_err(|e|e.to_string())?;
    conn.execute("INSERT INTO analysis_results (id,track_id,kind,analyzer_id,analyzer_version,schema_version,created_at,confidence,inputs,data,supersedes,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'current')",
        params![id,track,kind,p.analyzer_id,p.analyzer_version,p.schema_version,p.created_at,p.confidence,p.input_fingerprint,text,old]).map_err(|e|e.to_string())?;
    if let Some(old) = old {
        // Machine results are reproducible: the new record replaces the
        // old one instead of accumulating every past analysis.
        conn.execute(
            "UPDATE analysis_results SET supersedes=NULL WHERE id=?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE analysis_results SET supersedes=NULL WHERE supersedes=?1",
            params![old],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM analysis_results WHERE id=?1", params![old])
            .map_err(|e| e.to_string())?;
    }
    Ok(id)
}

fn write_analysis_doc(conn: &Connection, id: i64, version: u32, doc: &Value) -> Result<(), String> {
    let at = now();
    conn.execute(
            "INSERT INTO analysis (track_id, version, data, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(track_id) DO UPDATE SET version = excluded.version, data = excluded.data, at = excluded.at",
            params![id, version, doc.to_string(), at],
        )
        .map_err(|e| e.to_string())?;
    // Its tempo against the file's own BPM (tag, name): a soft-beat track
    // with a confirmed tempo is not one to check.
    if let Ok(path) = conn.query_row("SELECT path FROM tracks WHERE id = ?1", params![id], |r| r.get::<_, String>(0)) {
        store_confirmed_tempo(conn, id, at, doc["bpm"].as_f64(), doc["tag_bpm"].as_f64(), &path)?;
    }
    let key = doc["key"].as_object().map(|k| {
        let tonic = k.get("tonic").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let minor = k.get("minor").and_then(|v| v.as_bool()).unwrap_or(false);
        let major_tonic = if minor { (tonic + 3) % 12 } else { tonic };
        format!(
            "{}{}",
            (major_tonic * 7 % 12 + 7) % 12 + 1,
            if minor { "A" } else { "B" }
        )
    });
    conn.execute(
            "UPDATE tracks SET bpm = ?1, key = ?2, lufs = ?3, energy = ?4, grid_confidence = ?5, downbeat = ?6 WHERE id = ?7",
            params![
                doc["bpm"].as_f64(),
                key,
                doc["lufs"].as_f64(),
                // The energy curve's mean, 0…1.
                doc["energy"].as_array().filter(|e| !e.is_empty()).map(|e| e.iter().filter_map(|v| v.as_f64()).sum::<f64>() / e.len() as f64 / 255.0),
                doc["bpm_confidence"].as_f64(),
                doc["downbeat"].as_i64(),
                id
            ],
        )
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM analysis_failures WHERE track_id = ?1", [id])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn timestamp_iso() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", n.as_secs(), n.subsec_nanos())
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Moved,
    Unchanged,
    /// Another copy of a track the library has at a path that still exists.
    Copy,
}

/// A place music lives in, whose folders can each become a collection: a
/// folder in home (Music) or a drive's root. Not a folder that was itself
/// added as a collection (its folders are parts of it).
fn music_place(root: &Path, home: Option<&Path>, user: &str) -> bool {
    root.parent().is_some_and(|p| {
        Some(p) == home
            || p == Path::new(&format!("/run/media/{user}"))
            || p == Path::new(&format!("/media/{user}"))
    })
}

/// Facts about a source file (from ffprobe or tags).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facts {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub album_artist: String,
    pub label: String,
    pub release: String,
    pub release_date: String,
    pub comment: String,
    pub container: String,
    pub original_tags: Value,
    pub codec: String,
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub duration: f64,
}

/// Whether a track answers a "load <query>" request: the query appears in the
/// title, case-insensitively, after trimming.
///
/// This is the whole rule, and it lives here so the two entry points cannot drift
/// apart. The DJ Booth searches the tracks it is showing (its list is filtered by
/// whatever the user typed) and the engine searches the whole library, because it
/// has no view — the candidate list is the caller's, the rule is not. An empty
/// query matches nothing: `contains("")` is true for every title, so without this
/// a stray empty pad would quietly load whatever came first.
pub fn title_matches(title: &str, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    !needle.is_empty() && title.to_lowercase().contains(&needle)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_favourite_keeps_everything_else_and_comes_off_again() {
        let lib = Library::in_memory().unwrap();
        lib.db
            .execute("INSERT INTO tracks(content_id,path,size,title) VALUES('c','/a.flac',1,'A')", [])
            .unwrap();
        let id = lib.track_by_path("/a.flac").unwrap();
        let perf = Performance {
            hot_cues: vec![Some(12.5)],
            rating: 4,
            tags: vec!["peak".into()],
            ..Default::default()
        };
        lib.set_performance(id, &perf).unwrap();
        let shown = |lib: &Library| lib.tracks().unwrap()[0].tags.clone();

        let before = lib.signature();
        lib.set_favourite(id, true).unwrap();
        assert_ne!(lib.signature(), before, "a favourite shows in other windows");
        lib.set_favourite(id, true).unwrap();
        assert_eq!(shown(&lib), ["peak", FAVOURITE_TAG]);
        assert_eq!(lib.performance(id).hot_cues, [Some(12.5)]);
        assert_eq!(lib.tracks().unwrap()[0].rating, 4, "the AI's rating stays");

        let before = lib.signature();
        lib.set_favourite(id, false).unwrap();
        assert_eq!(shown(&lib), ["peak"]);
        assert_ne!(lib.signature(), before);
    }

    #[test]
    fn manual_grid_edits_on_two_tracks_keep_independent_identities() {
        let lib=Library::in_memory().unwrap();let d=tmpdir("two-human-grids");
        let first=d.join("one.flac");write(&first,1,1000);
        let second=d.join("two.flac");write(&second,2,1001);
        lib.import(&first,no_probe).unwrap();lib.import(&second,no_probe).unwrap();
        let first=first.to_str().unwrap();let second=second.to_str().unwrap();
        lib.set_human_downbeat(first,127.5,0.25,0).unwrap();
        lib.set_human_downbeat(second,132.5,0.75,2).unwrap();
        let grids=lib.human_grid_overrides().unwrap();
        assert_eq!(grids[first].bpm,127.5);assert_eq!(grids[second].bpm,132.5);
        assert_eq!(grids[first].first_beat_s,0.25);assert_eq!(grids[second].first_beat_s,0.75);
        lib.set_human_downbeat(first,128.0,0.5,1).unwrap();
        let grids=lib.human_grid_overrides().unwrap();
        assert_eq!(grids[first].bpm,128.0);assert_eq!(grids[second].bpm,132.5);
    }

    #[test]
    fn deleting_one_memory_loop_preserves_other_saved_loops() {
        let lib=Library::in_memory().unwrap();
        let d=tmpdir("memory-delete");
        let path=d.join("one.flac");write(&path,1,1000);
        let (track,_)=lib.import(&path,no_probe).unwrap();
        for id in ["first","second"] {
            lib.put_loop(track,&SavedLoop{id:id.into(),start_sample:100,end_sample:200,sample_rate:48000,beat_length:None,label:String::new(),quantized:false,temporary:false,origin:"performer".into()}).unwrap();
        }
        assert!(lib.delete_loop("first").unwrap());
        assert!(!lib.delete_loop("first").unwrap());
        let loops=lib.loops(track).unwrap();
        assert_eq!(loops.len(),1);
        assert_eq!(loops[0].id,"second");
    }

    #[test]
    fn a_write_that_only_lands_in_the_wal_still_moves_the_stamp() {
        use super::stamp_of_db;
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("patch-lib-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("library.db");
        std::fs::write(&db_path, b"db").unwrap();
        let before = stamp_of_db(&db_path).expect("a stamp for the database file");

        // What a WAL write does: the sidecar changes, the database file does not.
        let wal = dir.join("library.db-wal");
        std::fs::write(&wal, b"wal").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&wal)
            .unwrap()
            .set_modified(before + Duration::from_secs(5))
            .unwrap();

        let after = stamp_of_db(&db_path).expect("a stamp");
        assert!(
            after > before,
            "a write that only touches the wal must move the stamp, or every cache \
             keyed on it keeps serving a stale copy until the next checkpoint"
        );
        assert_eq!(
            std::fs::metadata(&db_path).unwrap().modified().unwrap(),
            before,
            "the database file itself must not have moved — that is the whole point"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_load_query_matches_a_title_by_substring_and_nothing_at_all() {
        use super::title_matches;
        assert!(title_matches("Midnight City", "midnight"));
        assert!(title_matches("Midnight City", "CITY"));
        assert!(title_matches("Midnight City", "  city  "), "trimmed");
        assert!(!title_matches("Midnight City", "daylight"));
        // An empty query is not a wildcard: it matches nothing, so a pad with no
        // query cannot load the first track in the library by accident.
        assert!(!title_matches("Midnight City", ""));
        assert!(!title_matches("Midnight City", "   "));
    }

    #[test]
    fn analysis_failure_rolls_back_generation_and_search_fields() {
        let db = Library::in_memory().unwrap();
        db.db
            .execute(
                "INSERT INTO tracks(content_id,path,size,added_at) VALUES('audit','audit.wav',0,0)",
                [],
            )
            .unwrap();
        let id = db.db.last_insert_rowid();
        let old = serde_json::json!({"bpm":120.0, "lufs":-12.0});
        db.set_analysis(id, 1, &old).unwrap();
        db.set_failure(id, 2, "retry pending").unwrap();
        for (name, statement) in [
            ("reject_track", "BEFORE UPDATE ON tracks"),
            ("reject_clear", "BEFORE DELETE ON analysis_failures"),
        ] {
            db.db.execute_batch(&format!("CREATE TEMP TRIGGER {name} {statement} BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;")).unwrap();
            let next = serde_json::json!({"bpm":130.0, "lufs":-8.0});
            assert!(db.set_analysis(id, 2, &next).is_err());
            assert_eq!(db.analysis_version(id), Some(1));
            let bpm: f64 = db
                .db
                .query_row("SELECT bpm FROM tracks WHERE id=?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(bpm, 120.0);
            let failures: i64 = db
                .db
                .query_row(
                    "SELECT count(*) FROM analysis_failures WHERE track_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(failures, 1);
            db.db
                .execute_batch(&format!("DROP TRIGGER {name};"))
                .unwrap();
        }
        db.set_analysis(id, 2, &serde_json::json!({"bpm":130.0}))
            .unwrap();
        assert_eq!(db.analysis_version(id), Some(2));
        let failures: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM analysis_failures WHERE track_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failures, 0);
    }

    #[test]
    fn provenance_and_analysis_commit_together() {
        let db = Library::in_memory().unwrap();
        db.db
            .execute(
                "INSERT INTO tracks(content_id,path,size,added_at) VALUES('a','a.wav',0,0)",
                [],
            )
            .unwrap();
        let id = db.db.last_insert_rowid();
        let provenance = AnalysisProvenance {
            analyzer_id: "org.patch.analysis".into(),
            analyzer_version: "9".into(),
            schema_version: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
            confidence: Some(0.5),
            input_fingerprint: String::new(),
            parameters: serde_json::json!({}),
        };
        let summary = serde_json::json!({"bpm": 120.0});
        let doc = serde_json::json!({"bpm": 120.0, "lufs": -10.0});
        db.db
            .execute_batch(
                "CREATE TEMP TRIGGER reject_analysis BEFORE INSERT ON analysis BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
        assert!(
            db.store_provenance_and_analysis(id, 9, "track_analysis", &provenance, &summary, &doc)
                .is_err()
        );
        assert_eq!(db.analysis_version(id), None);
        assert!(!db.current_analysis_result(id, "track_analysis", "org.patch.analysis", "9"));
        db.db
            .execute_batch("DROP TRIGGER reject_analysis;")
            .unwrap();
        db.store_provenance_and_analysis(id, 9, "track_analysis", &provenance, &summary, &doc)
            .unwrap();
        assert_eq!(db.analysis_version(id), Some(9));
        assert!(db.current_analysis_result(id, "track_analysis", "org.patch.analysis", "9"));
        db.store_provenance_and_analysis(id, 9, "track_analysis", &provenance, &summary, &doc)
            .unwrap();
        let rows: i64 = db
            .db
            .query_row(
                "SELECT count(*) FROM analysis_results WHERE track_id=?1 AND status='current'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn queue_pages_follow_original_order_and_skip_completed_items() {
        let lib = Library::in_memory().unwrap();
        for i in 0..140i64 {
            lib.db
                .execute(
                    "INSERT INTO tracks(content_id,path,size,added_at) VALUES(?1,?2,0,?3)",
                    params![format!("id{i}"), format!("path{i}"), i / 3],
                )
                .unwrap();
        }
        let expected = lib.queue(4).unwrap();
        let full: Vec<_> = lib
            .tracks()
            .unwrap()
            .into_iter()
            .filter(|t| !t.missing)
            .map(|t| (t.path, t.title, t.artist, t.rating))
            .collect();
        assert_eq!(lib.dj_pool().unwrap(), full);
        let mut got = Vec::new();
        for chunk in expected.chunks(17) {
            let ids: Vec<_> = chunk.iter().map(|(id, _)| *id).collect();
            let page = lib.analysis_batch(4, &ids).unwrap();
            assert!(page.len() <= 17);
            got.extend(page);
        }
        assert_eq!(got, expected);
        lib.set_failure(expected[0].0, 4, "unreadable").unwrap();
        let page = lib
            .analysis_batch(4, &[expected[0].0, expected[1].0])
            .unwrap();
        assert_eq!(page[0].0, expected[1].0);
        let batch = lib
            .analysis_batch(4, &[expected[2].0, expected[0].0, expected[1].0])
            .unwrap();
        assert_eq!(batch, expected[1..3]);
        assert!(
            lib.analysis_pending(4, expected[1].0, &expected[1].1)
                .unwrap()
        );
        lib.db
            .execute("UPDATE tracks SET missing=1 WHERE id=?1", [expected[1].0])
            .unwrap();
        assert!(
            !lib.analysis_pending(4, expected[1].0, &expected[1].1)
                .unwrap()
        );
        let collection = lib.create_collection("test").unwrap();
        lib.add_to_collection(collection, expected[2].0).unwrap();
        assert_eq!(
            lib.collection_memberships().unwrap()[&collection],
            std::collections::HashSet::from([expected[2].1.clone()])
        );
    }

    #[test]
    fn a_track_follows_its_file_and_a_copy_does_not_steal_it() {
        let dir = std::env::temp_dir().join(format!("patch-moves-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let orig = a.join("song.mp3");
        std::fs::write(&orig, vec![7u8; 300_000]).unwrap();
        let db = Library::in_memory().unwrap();
        let (id, _) = db.import(&orig, |_| None).unwrap();
        assert_eq!(
            db.track_uuid_for_path(&orig.to_string_lossy()).unwrap(),
            Some(db.tracks().unwrap()[0].track_uuid.clone())
        );
        db.set_analysis(id, 5, &serde_json::json!({"bpm": 124.0, "bpm_confidence": 0.9, "downbeat": 0, "lufs": -8.0, "key": null, "energy": []})).unwrap();
        // A copy elsewhere, original still there: the same track, not moved.
        let copy = b.join("song (copy).mp3");
        std::fs::copy(&orig, &copy).unwrap();
        assert_eq!(db.import(&copy, |_| None).unwrap(), (id, Change::Copy));
        assert_eq!(db.tracks().unwrap()[0].path, orig.to_string_lossy());
        // The original goes (moved, deleted, its stick unplugged): the copy
        // is the track now, analysis and all.
        std::fs::remove_file(&orig).unwrap();
        assert_eq!(db.import(&copy, |_| None).unwrap(), (id, Change::Moved));
        assert_eq!(db.tracks().unwrap()[0].path, copy.to_string_lossy());
        assert_eq!(db.analysis_version(id), Some(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bpm_in_a_file_name_is_read_only_where_it_ends_the_name() {
        assert_eq!(filename_bpm("/m/Bee Gees - Holiday - 8A - 106.mp3"), Some(106.0));
        assert_eq!(filename_bpm("/m/Track 124 BPM.mp3"), Some(124.0));
        assert_eq!(filename_bpm("/m/Track (128bpm).flac"), Some(128.0));
        assert_eq!(filename_bpm("/m/[068] Lil Wayne - Mirror - 4A - 135.mp3"), Some(135.0));
        assert_eq!(filename_bpm("/m/71_207 - Bread - If - 11B - 98.mp3"), Some(98.0));
        assert_eq!(filename_bpm("/m/Prince - 1999.mp3"), None, "a year, not a tempo");
        assert_eq!(filename_bpm("/m/Blink 182 - Adam's Song.mp3"), None, "not at the end");
        assert_eq!(filename_bpm("/m/Track 03.mp3"), None);
        assert_eq!(filename_bpm("/m/Bryan Adams - Summer Of 69.mp3"), None, "a title's own number");
        assert!(tempo_agrees(73.4, 146.0) && tempo_agrees(134.8, 135.0) && !tempo_agrees(120.0, 126.0));
        assert_eq!(tempo_confirmed_by(105.0, Some(106.0), "/m/x.mp3"), Some((106.0, "tag")));
        assert_eq!(tempo_confirmed_by(105.0, Some(90.0), "/m/x - 105.mp3"), Some((105.0, "file name")));
        assert_eq!(tempo_confirmed_by(105.0, None, "/m/x.mp3"), None);
    }

    #[test]
    fn a_confirmed_tempo_is_soft_not_check_and_a_hand_set_grid_is_ready() {
        let dir = std::env::temp_dir().join(format!("patch-soft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["Ballad - 8A - 98.mp3", "Live - 8A.mp3", "Drift.mp3", "Fixed.mp3"];
        for (i, n) in names.iter().enumerate() {
            std::fs::write(dir.join(n), vec![i as u8 + 1; 70 + i]).unwrap();
        }
        let db = Library::in_memory().unwrap();
        db.add_source(&dir, "Music").unwrap();
        db.scan(&dir, |_| None).unwrap();
        let id = |n: &str| db.track_by_path(&dir.join(n).to_string_lossy()).unwrap();
        let v = 5;
        let unsteady = |bpm: f64, tag: Option<f64>| serde_json::json!({"bpm": bpm, "bpm_confidence": 0.3, "downbeat": 0, "lufs": -14.0, "key": {"tonic": 2, "minor": true}, "energy": [], "tag_bpm": tag});
        // Confirmed by its name; by its tag (half time); by nothing; set by hand.
        db.set_analysis(id(names[0]), v, &unsteady(97.6, None)).unwrap();
        db.set_analysis(id(names[1]), v, &unsteady(73.0, Some(146.0))).unwrap();
        db.set_analysis(id(names[2]), v, &unsteady(110.0, Some(126.0))).unwrap();
        db.set_analysis(id(names[3]), v, &unsteady(101.0, None)).unwrap();
        db.set_performance(id(names[3]), &Performance { grid: Some((101.2, 0.31)), ..Default::default() }).unwrap();
        let r = db.readiness(v);
        let at = |n: &str| r[&dir.join(n).to_string_lossy().to_string()].0;
        assert_eq!(at(names[0]), Readiness::Soft);
        assert_eq!(at(names[1]), Readiness::Soft);
        assert_eq!(at(names[2]), Readiness::Check, "nothing confirms 110 against 126");
        assert_eq!(at(names[3]), Readiness::Ready, "the DJ's own grid");
        let c = db.counts_under(&dir.to_string_lossy(), v);
        assert_eq!((c.tracks, c.ready, c.soft, c.check), (4, 1, 2, 1));
        assert_eq!(db.confirmed_tempo(&dir.join(names[1]).to_string_lossy()), Some((146.0, "tag".into())));
        // An older library: analysed before the table existed. The
        // librarian's pass checks them; nothing is left after it.
        db.db.execute("DROP TABLE confirmed_tempo", []).unwrap();
        assert_eq!(db.readiness(v)[&dir.join(names[0]).to_string_lossy().to_string()].0, Readiness::Check);
        assert_eq!(db.confirm_tempos(500).unwrap(), 4);
        assert_eq!(db.confirm_tempos(500).unwrap(), 0, "each analysis once");
        assert_eq!(db.readiness(v)[&dir.join(names[0]).to_string_lossy().to_string()].0, Readiness::Soft);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_and_where_their_tracks_stand() {
        let dir = std::env::temp_dir().join(format!("patch-sources-{}", std::process::id()));
        let usb = dir.join("usb");
        std::fs::create_dir_all(&usb).unwrap();
        for (i, name) in ["a.mp3", "b.mp3", "c.mp3", "d.mp3"].iter().enumerate() {
            std::fs::write(usb.join(name), vec![i as u8; 64 + i]).unwrap();
        }
        let db = Library::in_memory().unwrap();
        db.add_source(&usb, "USB").unwrap();
        assert!(db.add_source(&usb, "Again").is_err(), "one folder once");
        db.scan(&usb, |_| None).unwrap();
        let id = |n: &str| db.track_by_path(&usb.join(n).to_string_lossy()).unwrap();
        let v = 5;
        // a: ready · b: unsteady beat · c: failed · d: not yet.
        db.set_analysis(id("a.mp3"), v, &serde_json::json!({"bpm": 124.0, "bpm_confidence": 0.9, "downbeat": 1, "lufs": -8.0, "key": {"tonic": 9, "minor": true}, "energy": [100, 200]})).unwrap();
        db.set_analysis(id("b.mp3"), v, &serde_json::json!({"bpm": 90.0, "bpm_confidence": 0.2, "downbeat": 0, "lufs": -20.0, "key": {"tonic": 2, "minor": false}, "energy": []})).unwrap();
        db.set_failure(id("c.mp3"), v, "ffmpeg: invalid data")
            .unwrap();
        let c = db.counts_under(&usb.to_string_lossy(), v);
        assert_eq!(
            (c.tracks, c.ready, c.check, c.failed, c.waiting),
            (4, 1, 1, 1, 1)
        );
        let r = db.readiness(v);
        let at = |n: &str| r[&usb.join(n).to_string_lossy().to_string()].clone();
        assert_eq!(at("a.mp3").0, Readiness::Ready);
        assert_eq!(at("b.mp3").0, Readiness::Check);
        assert_eq!(
            at("c.mp3"),
            (Readiness::Failed, Some("ffmpeg: invalid data".into()))
        );
        assert_eq!(at("d.mp3").0, Readiness::Waiting);
        // The failed track isn't retried; the waiting one is next.
        let q: Vec<i64> = db.queue(v).unwrap().into_iter().map(|(i, _)| i).collect();
        assert_eq!(q, vec![id("d.mp3")]);
        // A newer analysis version retries everything but what it has.
        assert_eq!(db.queue(v + 1).unwrap().len(), 4);
        // Out of the library: hidden, not deleted.
        db.remove_source("usb").unwrap();
        assert_eq!(db.counts_under(&usb.to_string_lossy(), v).tracks, 0);
        assert!(db.sources().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn play_history_invalidates_library_snapshot() {
        let db = Library::in_memory().unwrap();
        let before = db.signature();
        db.db.execute("INSERT INTO tracks (path, title, content_id, size) VALUES ('/test.wav', 'Test', 'test', 1)", []).unwrap();
        let id = db.track_by_path("/test.wav").unwrap();
        let inserted = db.signature();
        assert_ne!(before, inserted);
        db.log("play", Some("a"), Some(id), &serde_json::json!({}))
            .unwrap();
        assert_ne!(inserted, db.signature());
        let track = db.tracks().unwrap().remove(0);
        assert_eq!(track.plays, 1);
        assert!(track.last_played.is_some());
    }

    #[test]
    fn only_music_and_drives_offer_their_folders() {
        let home = Some(Path::new("/home/u"));
        assert!(music_place(Path::new("/home/u/Music"), home, "u"));
        assert!(music_place(Path::new("/run/media/u/SAMSUNG_T52"), home, "u"));
        assert!(music_place(Path::new("/media/u/STICK"), home, "u"));
        assert!(!music_place(Path::new("/run/media/u/SAMSUNG_T52/NL Top 40"), home, "u"));
        assert!(!music_place(Path::new("/home/u/Music/Reference"), home, "u"));
    }

    #[test]
    fn a_drive_that_mounts_elsewhere_takes_its_tracks_along() {
        let base = std::env::temp_dir().join(format!("patch-relocate-{}", std::process::id()));
        let old = base.join("SAMSUNG_T52/NL Top 40");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.mp3"), vec![1u8; 70]).unwrap();
        let db = Library::in_memory().unwrap();
        let src = db.add_source(&old, "NL Top 40").unwrap();
        let mut ids = vec![];
        db.scan_with(&old, |_| None, |id, _| ids.push(id)).unwrap();
        // Another track elsewhere whose name only starts the same.
        let other = base.join("SAMSUNG_T52 backup/b.mp3");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, vec![2u8; 71]).unwrap();
        db.import(&other, |_| None).unwrap();
        let from = base.join("SAMSUNG_T52").to_string_lossy().into_owned();
        let to = base.join("SAMSUNG_T5").to_string_lossy().into_owned();
        assert_eq!(db.sample_under(&from, 5).len(), 1, "the other folder is not under it");
        assert_eq!(db.relocate(&from, &to).unwrap(), 1);
        let s = db.sources().into_iter().find(|s| s.id == src).unwrap();
        assert_eq!(s.path, format!("{to}/NL Top 40"));
        let paths: Vec<String> = db.tracks().unwrap().into_iter().map(|t| t.path).collect();
        assert!(paths.contains(&format!("{to}/NL Top 40/a.mp3")));
        assert!(paths.iter().any(|p| p.contains("SAMSUNG_T52 backup")), "only what was under it moves");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_set_preps_prepare_all_stems_option_is_off_until_switched_on() {
        let db = Library::in_memory().unwrap();
        assert!(db.set_preps_with_stems().is_empty(), "no table yet: none");
        let set = db.create_set_prep("Friday").unwrap();
        let plain = db.create_collection("Plain").unwrap();
        assert!(db.set_preps_with_stems().is_empty(), "off by default");
        assert!(db.set_set_prep_stems(plain, true).is_err(), "only a set prep");
        db.set_set_prep_stems(set, true).unwrap();
        db.set_set_prep_stems(set, true).unwrap();
        assert_eq!(db.set_preps_with_stems(), [set].into());
        db.set_set_prep_stems(set, false).unwrap();
        assert!(db.set_preps_with_stems().is_empty());
    }

    #[test]
    fn a_set_prep_keeps_its_order_and_moves_tracks() {
        let dir = std::env::temp_dir().join(format!("patch-setprep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, name) in ["c.mp3", "a.mp3", "b.mp3"].iter().enumerate() {
            std::fs::write(dir.join(name), vec![i as u8 + 7; 80 + i]).unwrap();
        }
        let db = Library::in_memory().unwrap();
        let mut found = Vec::new();
        db.scan_with(&dir, |_| None, |id, _| found.push(id)).unwrap();
        assert!(db.set_preps().is_empty(), "no table yet: none");
        let plain = db.create_collection("Plain").unwrap();
        let set = db.create_set_prep("Friday").unwrap();
        assert_eq!(db.set_preps(), [set].into_iter().collect());
        // In the order added, not by name.
        for t in [found[2], found[0], found[1]] {
            db.add_to_collection(set, t).unwrap();
        }
        assert_eq!(db.collection_tracks(set), vec![found[2], found[0], found[1]]);
        let before = db.signature();
        db.move_in_collection(set, found[1], -1).unwrap();
        assert_eq!(db.collection_tracks(set), vec![found[2], found[1], found[0]]);
        assert_ne!(db.signature(), before, "a move shows in other windows");
        // At the ends nothing moves.
        db.move_in_collection(set, found[2], -1).unwrap();
        assert_eq!(db.collection_tracks(set)[0], found[2]);
        assert_eq!(db.collection_paths(set).len(), 3);
        // The DJ decides unless told otherwise, a set prep too.
        assert!(!db.plays_in_order(set) && !db.plays_in_order(plain));
        db.set_play_in_order(plain, true).unwrap();
        db.set_play_in_order(set, false).unwrap();
        assert!(db.plays_in_order(plain) && !db.plays_in_order(set));
        db.delete_collection(set, true).unwrap();
        assert_eq!(db.play_orders().get(&set), None, "deleted with it");
        assert!(db.set_preps().is_empty());
        assert_eq!(db.collections().len(), 1);
        let _ = plain;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_collection_fills_once_while_it_is_scanned() {
        let dir = std::env::temp_dir().join(format!("patch-fill-{}", std::process::id()));
        let sub = dir.join("NL Top 40/1987");
        std::fs::create_dir_all(&sub).unwrap();
        for (i, name) in ["a.mp3", "b.flac", "c.mp3"].iter().enumerate() {
            std::fs::write(sub.join(name), vec![i as u8 + 1; 64 + i]).unwrap();
        }
        let db = Library::in_memory().unwrap();
        // The scan tells about each track as it is taken in.
        let mut found = Vec::new();
        db.scan_with(&dir, |_| None, |id, _| found.push(id)).unwrap();
        assert_eq!(found.len(), 3);
        // Two fillers adding the same tracks (the librarian and "add
        // folder") leave each once.
        let c = db.create_collection("NL Top 40").unwrap();
        db.add_all_to_collection(c, &found).unwrap();
        db.add_all_to_collection(c, &found).unwrap();
        db.add_to_collection(c, found[0]).unwrap();
        assert_eq!(db.collection_tracks(c), found);
        // From a folder: the collection holds the folder's tracks, once.
        let (n, _) = db.collection_from_folder("NL Top 40", &dir.join("NL Top 40")).unwrap();
        assert_eq!(n, 3);
        assert_eq!(db.collection_tracks(c).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collections_are_managed_and_a_folders_own_follows_its_source() {
        let dir = std::env::temp_dir().join(format!("patch-manage-{}", std::process::id()));
        let usb = dir.join("usb");
        std::fs::create_dir_all(&usb).unwrap();
        for (i, name) in ["a.mp3", "b.flac", "notes.txt"].iter().enumerate() {
            std::fs::write(usb.join(name), vec![i as u8; 64 + i]).unwrap();
        }
        let db = Library::in_memory().unwrap();
        // Chosen files: audio comes in (into the collection), the rest not.
        let mix = db.create_collection("Mix").unwrap();
        let files: Vec<PathBuf> = ["a.mp3", "b.flac", "notes.txt", "gone.mp3"]
            .iter()
            .map(|n| usb.join(n))
            .collect();
        let (ids, skipped) = db.import_files(&files, Some(mix)).unwrap();
        assert_eq!((ids.len(), skipped), (2, 2));
        assert_eq!(db.collection_tracks(mix), ids);
        // Out of the collection, still in the library.
        db.remove_from_collection(mix, &ids[..1], true).unwrap();
        assert_eq!(db.collection_tracks(mix), ids[1..]);
        assert_eq!(db.tracks().unwrap().len(), 2);
        // A plain collection: renamed, deleted; tracks stay.
        let before = db.signature();
        db.rename_collection(mix, "Warm up").unwrap();
        assert_ne!(db.signature(), before, "a rename shows in other windows");
        assert_eq!(db.collection_by_name("Warm up"), Some(mix));
        assert!(db.collection_source(mix).is_none());
        db.delete_collection(mix, true).unwrap();
        assert!(db.collections().is_empty());
        assert_eq!(db.tracks().unwrap().len(), 2);
        // A source's own collection renames the source with it, and
        // deleting it takes the source out (else the librarian brings it back).
        db.add_source(&usb, "USB").unwrap();
        let own = db.create_collection("USB").unwrap();
        db.add_many_to_collection(own, &ids).unwrap();
        assert_eq!(
            db.collection_source(own).map(|s| s.name),
            Some("USB".into())
        );
        db.rename_collection(own, "Stick").unwrap();
        assert_eq!(db.sources()[0].name, "Stick");
        assert_eq!(
            db.collection_source(own).map(|s| s.name),
            Some("Stick".into())
        );
        db.delete_collection(own, true).unwrap();
        assert!(db.sources().is_empty());
        assert!(db.collections().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("patch-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, seed: u8, len: usize) {
        let data: Vec<u8> = (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect();
        std::fs::write(p, data).unwrap();
    }

    fn no_probe(_: &Path) -> Option<Facts> {
        None
    }

    #[test]
    fn moving_a_file_keeps_its_metadata() {
        let d = tmpdir("move");
        let lib = Library::in_memory().unwrap();
        let a = d.join("one.flac");
        write(&a, 7, 300_000);
        let (id, c) = lib.import(&a, no_probe).unwrap();
        assert_eq!(c, Change::Added);
        let perf = Performance {
            hot_cues: vec![Some(12.5), None, None, None],
            rating: 4,
            tags: vec!["peak".into()],
            ..Default::default()
        };
        lib.set_performance(id, &perf).unwrap();
        // Move it into a subfolder and rescan.
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let b = d.join("sub/renamed.flac");
        std::fs::rename(&a, &b).unwrap();
        let r = lib.scan(&d, no_probe).unwrap();
        assert_eq!(
            r,
            ScanReport {
                added: 0,
                moved: 1,
                unchanged: 0,
                copies: 0,
                missing: 0
            }
        );
        assert_eq!(
            lib.track_by_path(&b.to_string_lossy()),
            Some(id),
            "same track"
        );
        assert_eq!(lib.performance(id), perf, "cues, rating and tags kept");
    }

    #[test]
    fn human_grid_catalogue_reads_manual_values_outside_analysis_records() {
        let d = tmpdir("manual-grid");
        let lib = Library::in_memory().unwrap();
        let path = d.join("track.flac");
        write(&path, 7, 300_000);
        let (id, _) = lib.import(&path, no_probe).unwrap();
        let grid = (127.5, 0.33);
        lib.set_performance(
            id,
            &Performance {
                grid: Some(grid),
                ..Default::default()
            },
        )
        .unwrap();

        let catalogue = lib.manual_performance_grids().unwrap();
        assert_eq!(catalogue.get(path.to_str().unwrap()), Some(&grid));
        let analysis_rows: i64 = lib
            .db
            .query_row(
                "SELECT COUNT(*) FROM analysis WHERE track_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(analysis_rows, 0, "manual work does not fake analysis");
    }

    #[test]
    fn a_hand_set_downbeat_overrides_and_clears() {
        let d = tmpdir("human-downbeat");
        let lib = Library::in_memory().unwrap();
        let path = d.join("track.flac");
        write(&path, 7, 300_000);
        let (id, _) = lib.import(&path, no_probe).unwrap();
        lib.db
            .execute("UPDATE tracks SET rate=48000 WHERE id=?1", [id])
            .unwrap();
        let p = path.to_str().unwrap();
        lib.set_human_downbeat(p, 123.0, 0.245, 0).unwrap();
        let g = lib.human_grid_overrides().unwrap().remove(p).unwrap();
        assert_eq!(
            (g.bpm, g.downbeat_phase, g.beats_per_bar),
            (123.0, Some(0), Some(4))
        );
        assert!((g.first_beat_s - 0.245).abs() < 1e-4);
        lib.set_human_downbeat(p, 123.0, 0.245, 5).unwrap();
        let g = lib.human_grid_overrides().unwrap().remove(p).unwrap();
        assert_eq!(
            g.downbeat_phase,
            Some(1),
            "a second setting replaces the first"
        );
        lib.clear_human_grid(p).unwrap();
        assert!(!lib.human_grid_overrides().unwrap().contains_key(p));
        assert!(
            lib.set_human_downbeat("/nowhere.flac", 123.0, 0.2, 0)
                .is_err()
        );
    }

    #[test]
    fn active_human_beatgrid_supplies_bar_phase_and_overrides_legacy_grid() {
        let d = tmpdir("human-beatgrid");
        let lib = Library::in_memory().unwrap();
        let path = d.join("track.flac");
        write(&path, 7, 300_000);
        let (id, _) = lib.import(&path, no_probe).unwrap();
        lib.set_performance(
            id,
            &Performance {
                grid: Some((112.0, 0.1)),
                ..Default::default()
            },
        )
        .unwrap();
        lib.db
            .execute("UPDATE tracks SET rate=48000 WHERE id=?1", [id])
            .unwrap();
        let segment = BeatSegment {
            start_sample: 0,
            end_sample: None,
            bpm_start: 123.0,
            bpm_end: 123.0,
            first_beat_sample: 24_000,
            beats_per_bar: Some(4),
            downbeat_phase: Some(2),
            confidence: Some(1.0),
        };
        lib.put_beatgrid(
            id,
            "human-grid",
            "human",
            None,
            Some((4, 4)),
            std::slice::from_ref(&segment),
            true,
        )
        .unwrap();
        let mut invalid_phase = segment.clone();
        invalid_phase.downbeat_phase = Some(4);
        assert!(
            lib.put_beatgrid(
                id,
                "invalid-human-grid",
                "human",
                None,
                Some((4, 4)),
                &[invalid_phase],
                false,
            )
            .is_err()
        );

        let grid = lib
            .human_grid_overrides()
            .unwrap()
            .remove(path.to_str().unwrap())
            .unwrap();
        assert_eq!(grid.bpm, 123.0);
        assert!((grid.first_beat_s - 0.5).abs() < 1e-9);
        assert_eq!(grid.beats_per_bar, Some(4));
        assert_eq!(grid.downbeat_phase, Some(2));
        assert_eq!(grid.sample_rate, 48_000);
        assert_eq!(grid.segments, vec![segment]);
    }

    #[test]
    fn missing_files_keep_their_rows() {
        let d = tmpdir("missing");
        let lib = Library::in_memory().unwrap();
        let a = d.join("gone.mp3");
        write(&a, 3, 50_000);
        lib.scan(&d, no_probe).unwrap();
        std::fs::remove_file(&a).unwrap();
        let r = lib.scan(&d, no_probe).unwrap();
        assert_eq!(r.missing, 1);
        let t = lib.tracks().unwrap();
        assert_eq!(t.len(), 1);
        assert!(t[0].missing);
    }

    #[test]
    fn analysis_is_versioned() {
        let d = tmpdir("ver");
        let lib = Library::in_memory().unwrap();
        let a = d.join("x.wav");
        write(&a, 9, 10_000);
        let (id, _) = lib.import(&a, no_probe).unwrap();
        assert_eq!(lib.needs_analysis(1).unwrap().len(), 1);
        lib.set_analysis(
            id,
            1,
            &serde_json::json!({"bpm": 124.0, "key": {"tonic": 9, "minor": true}, "lufs": -9.5}),
        )
        .unwrap();
        assert!(lib.needs_analysis(1).unwrap().is_empty());
        assert_eq!(
            lib.needs_analysis(2).unwrap().len(),
            1,
            "a newer analysis version re-analyses"
        );
        let t = &lib.tracks().unwrap()[0];
        assert_eq!((t.bpm, t.key.as_deref()), (Some(124.0), Some("8A")));
    }

    #[test]
    fn history_and_plays() {
        let d = tmpdir("hist");
        let lib = Library::in_memory().unwrap();
        let a = d.join("h.flac");
        write(&a, 1, 5_000);
        let (id, _) = lib.import(&a, no_probe).unwrap();
        lib.log(
            "load",
            Some("a"),
            Some(id),
            &serde_json::json!({"bpm": 124}),
        )
        .unwrap();
        lib.log("play", Some("a"), Some(id), &Value::Null).unwrap();
        let h = lib.history(10);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].1, "play");
        assert_eq!(lib.tracks().unwrap()[0].plays, 1);
    }

    #[test]
    fn human_records_sets_transitions_embeddings_and_sessions_roundtrip() {
        let d = tmpdir("native-data");
        let db = Library::open(&d.join("library.db")).unwrap();
        let a = d.join("a.flac");
        let b = d.join("b.flac");
        write(&a, 11, 90_000);
        write(&b, 19, 100_000);
        let (ta, _) = db.import(&a, no_probe).unwrap();
        let (tb, _) = db.import(&b, no_probe).unwrap();
        let cue = Cue {
            id: "human-hot-1".into(),
            kind: "hot".into(),
            sample_position: 441_000,
            sample_rate: 44_100,
            beat_position: Some(16.0),
            label: "drop".into(),
            color: Some("#ffcc00".into()),
            comment: "manual".into(),
            origin: "performer".into(),
            confidence: None,
            generated: false,
        };
        db.put_cue(ta, &cue).unwrap();
        assert_eq!(db.cues(ta).unwrap(), std::slice::from_ref(&cue));
        let lp = SavedLoop {
            id: "loop-a".into(),
            start_sample: 441_000,
            end_sample: 617_400,
            sample_rate: 44_100,
            beat_length: Some(4.0),
            label: "four beats".into(),
            quantized: true,
            temporary: false,
            origin: "performer".into(),
        };
        db.put_loop(ta, &lp).unwrap();
        assert_eq!(db.loops(ta).unwrap(), [lp]);
        let seg = BeatSegment {
            start_sample: 0,
            end_sample: Some(441_000),
            bpm_start: 124.0,
            bpm_end: 124.0,
            first_beat_sample: 200,
            beats_per_bar: Some(4),
            downbeat_phase: Some(0),
            confidence: Some(0.9),
        };
        let prov = AnalysisProvenance {
            analyzer_id: "org.patch.test".into(),
            analyzer_version: "3".into(),
            schema_version: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
            confidence: Some(0.9),
            input_fingerprint: "sampled:ab12".into(),
            parameters: Value::Null,
        };
        db.put_beatgrid(
            ta,
            "auto-grid",
            "automatic",
            Some(&prov),
            Some((4, 4)),
            std::slice::from_ref(&seg),
            false,
        )
        .unwrap();
        db.put_beatgrid(ta, "dj-grid", "human", None, Some((7, 8)), &[seg], true)
            .unwrap();
        let automatic: i64 = db
            .db
            .query_row(
                "SELECT COUNT(*) FROM beatgrids WHERE track_id=?1 AND layer='automatic'",
                [ta],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(automatic, 1);
        let set = serde_json::json!({"tracks":[ta,tb],"alternates":[tb],"notes":"open format"});
        db.save_set("set-1", "Friday", &set).unwrap();
        assert_eq!(db.load_set("set-1").unwrap().unwrap().1, set);
        let plan = TransitionPlan {
            source_track: ta,
            destination_track: tb,
            source_mix_out_sample: 900_000,
            destination_mix_in_sample: 0,
            duration_samples: 352_800,
            sample_rate: 44_100,
            strategy: TransitionStrategy {
                phrase_alignment_beats: Some(32),
                tempo_ratio: Some(1.01),
                key_relation: Some("compatible".into()),
                energy_delta: Some(0.1),
                fader_curve: vec![[0.0, 0.0], [8.0, 1.0]],
                ..Default::default()
            },
            confidence: Some(0.8),
            notes: "approved".into(),
        };
        db.save_transition(
            "tr-1",
            &plan,
            "planned",
            &serde_json::json!({"source":"human"}),
        )
        .unwrap();
        let embedding = EmbeddingVector::F32(vec![0.1, -0.25, 0.5]);
        let bytes = embedding.encode().unwrap();
        assert_eq!(EmbeddingVector::decode(&bytes).unwrap(), embedding);
        db.store_embedding(
            ta,
            "org.patch.test-embed",
            "1",
            "semantic",
            &embedding,
            "l2",
            "audio-pcm",
        )
        .unwrap();
        db.set_analysis(
            ta,
            1,
            &serde_json::json!({"bpm":124.0,"lufs":-11.0,"energy":0.7}),
        )
        .unwrap();
        db.set_analysis(
            tb,
            1,
            &serde_json::json!({"bpm":126.0,"lufs":-12.0,"energy":0.5}),
        )
        .unwrap();
        let index = Index::new(db.tracks().unwrap());
        let fixed = db.create_playlist("Prepared", None).unwrap();
        db.add_to_playlist(fixed, ta).unwrap();
        db.add_to_playlist(fixed, tb).unwrap();
        assert_eq!(db.playlist(fixed, &index), [ta, tb]);
        let smart = db
            .create_playlist(
                "Tempo 120",
                Some(&Filter {
                    bpm: Some((120.0, 130.0)),
                    ..Default::default()
                }),
            )
            .unwrap();
        assert_eq!(
            db.playlist(smart, &index),
            vec![ta, tb],
            "smart rules evaluate against current index"
        );
        db.log_session_event(
            "session-1",
            "2026-01-01T00:00:00Z",
            "transition_completed",
            Some(ta),
            Some("a"),
            Some(1234),
            &serde_json::json!({"to":tb}),
        )
        .unwrap();
        let ev: i64 = db
            .db
            .query_row(
                "SELECT COUNT(*) FROM performance_events WHERE session_id='session-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ev, 1);
        let vecs: Vec<String> = db
            .db
            .prepare("SELECT sidecar FROM embeddings WHERE track_id=?1")
            .unwrap()
            .query_map([ta], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            EmbeddingVector::decode(&std::fs::read(d.join(&vecs[0])).unwrap()).unwrap(),
            embedding
        );
    }
}
