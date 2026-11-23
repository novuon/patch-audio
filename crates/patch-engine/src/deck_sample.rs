//! A deck's sample maker: a part of the loaded track, the mix or one of its
//! stems, saved as a sound for the Tidal Unit (`s "name:n"`).
//!
//! What is cut is the deck's loop, or the beats that just played (whole
//! beats, and whole bars from a bar up, on the track's grid), from the
//! track's own file or its stem: as the track is, not as the deck plays it
//! (no tempo, key, EQ or effects), so Tidal's `loopAt` and `unit "c"` fit
//! it to any tempo. Sliced, each slice is a file of its own (`n "0 .. 7"`).
//! Each sample made is logged beside the folders (`.made.jsonl`), so a
//! deck can list what was made from its track.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// What to cut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum What {
    /// The deck's loop.
    Loop,
    /// The beats that just played.
    Beats { beats: f64 },
}

/// A deck's choices, as the panel sets them (and a controller's "sample"
/// button uses again).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub what: What,
    /// `mix`, or a stem (`drums`, `bass`, `vocals`, `other`).
    pub from: String,
    pub slices: u32,
    /// Empty: from the track's title.
    #[serde(default)]
    pub name: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { what: What::Loop, from: "mix".into(), slices: 1, name: String::new() }
    }
}

/// The grid as the deck knows it: BPM, first beat (s), the beat a bar
/// starts on (0 … 3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub bpm: f64,
    pub offset: f64,
    pub downbeat: u8,
}

/// The part of the track to cut: start and end (s), and how many beats.
pub fn region(
    what: What,
    grid: Option<Grid>,
    looped: Option<(f64, f64)>,
    position: f64,
) -> Result<(f64, f64, f64), String> {
    let beat = grid.filter(|g| g.bpm > 0.0).map(|g| 60.0 / g.bpm);
    match what {
        What::Loop => {
            let (a, b) = looped.filter(|(a, b)| b > a).ok_or("no loop on this deck: set one, or take the beats that just played")?;
            Ok((a, b, beat.map_or(0.0, |s| ((b - a) / s * 1000.0).round() / 1000.0)))
        }
        What::Beats { beats } => {
            let g = grid.ok_or("this track has no beat grid yet")?;
            let beat = beat.ok_or("this track has no beat grid yet")?;
            let beats = beats.clamp(0.25, 64.0);
            // Whole beats; from a bar up, whole bars.
            let (start, step) = if beats >= 4.0 {
                (g.offset + g.downbeat as f64 * beat, beat * 4.0)
            } else {
                (g.offset, beat)
            };
            let end = start + ((position - start) / step + 1e-6).floor() * step;
            let from = end - beats * beat;
            if from < -1e-6 {
                return Err(format!("not {beats} beats played yet"));
            }
            Ok((from.max(0.0), end, beats))
        }
    }
}

/// A sound name from a track's title: its first word, plain (`Midnight
/// Transit` → `midnight`).
pub fn default_name(title: &str, path: &str) -> String {
    let from = if title.trim().is_empty() {
        Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("")
    } else {
        title
    };
    let word: String = from
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| w.chars().any(char::is_alphabetic) && w.len() >= 2)
        .unwrap_or("deck")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect::<String>()
        .to_ascii_lowercase();
    if word.is_empty() || !patch_pattern::samples::valid_name(&word) { "deck".into() } else { word }
}

/// Tidal code that plays it: a loop fitted to its bars, slices in turn,
/// a hit as it is.
pub fn tidal_code(name: &str, first: usize, slices: usize, beats: f64) -> String {
    if slices > 1 {
        return format!("n \"{} .. {}\" # s \"{name}\"", first, first + slices - 1);
    }
    if beats >= 4.0 {
        let cycles = beats / 4.0;
        let chop = (beats * 2.0).round().clamp(2.0, 64.0);
        return format!("loopAt {} $ chop {chop} $ s \"{name}:{first}\"", fmt(cycles));
    }
    format!("s \"{name}:{first}\"")
}

fn fmt(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// What a sample was made of, as listed.
pub fn label(what: What, beats: f64, from: &str, slices: u32) -> String {
    let len = if beats <= 0.0 {
        "the loop".to_string()
    } else if beats >= 4.0 && (beats / 4.0).fract() == 0.0 {
        let bars = beats / 4.0;
        if bars == 1.0 { "1 bar".into() } else { format!("{} bars", fmt(bars)) }
    } else if beats == 1.0 {
        "1 beat".into()
    } else {
        format!("{} beats", fmt(beats))
    };
    let what = match what {
        What::Loop => format!("loop · {len}"),
        What::Beats { .. } => len,
    };
    let sliced = if slices > 1 { format!(" · {slices} slices") } else { String::new() };
    format!("{what} · {from}{sliced}")
}

/// One sample made, as logged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Made {
    pub name: String,
    /// The first file's `n`, and how many files (slices).
    pub n: usize,
    pub files: Vec<PathBuf>,
    pub track: String,
    pub label: String,
    pub code: String,
    pub seconds: f64,
    pub made: u64,
}

fn log_path() -> PathBuf {
    patch_pattern::samples::own_root().join(".made.jsonl")
}

pub fn log(m: &Made) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(patch_pattern::samples::own_root());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path())
        && let Ok(line) = serde_json::to_string(m)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Samples made from `track` whose files are still there, newest first.
pub fn made_from(track: &str, limit: usize) -> Vec<Made> {
    let text = std::fs::read_to_string(log_path()).unwrap_or_default();
    let mut out: Vec<Made> = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Made>(l).ok())
        .filter(|m| m.track == track && m.files.iter().all(|f| f.is_file()))
        .take(limit)
        .collect();
    out.dedup_by(|a, b| a.files == b.files);
    out
}

/// Where a cut comes from: the track's file, or a stem's (and its gain).
pub struct Source {
    pub path: PathBuf,
    pub gain: f32,
    /// Holds the stem cache entry while it is read.
    pub _lease: Option<patch_stems::cache::Lease>,
}

/// The stems ready for a track (`drums`, `bass` …), in their order.
pub fn stems_ready(track: &str) -> Vec<String> {
    let c = crate::stem_service::context(track);
    if c.set_id.is_none() { vec![] } else { c.available }
}

pub fn source(track: &str, from: &str) -> Result<Source, String> {
    if from == "mix" {
        return Ok(Source { path: track.into(), gain: 1.0, _lease: None });
    }
    let c = crate::stem_service::context(track);
    let key = c.set_id.ok_or("this track has no stems yet: prepare them first")?;
    let lease = patch_stems::cache::Cache::default_cache()?.acquire(&key)?;
    if !lease.set.playback_ready() {
        return Err("this track's stems aren't ready".into());
    }
    let a = lease.set.stems.get(from).ok_or_else(|| format!("this track has no {from} stem"))?;
    Ok(Source {
        path: lease.directory.join(&a.cache_ref),
        gain: patch_stems::jobs::stored_gain(&a.codec),
        _lease: Some(lease),
    })
}

/// Cut and save: one file, or `slices` files side by side.
pub fn make(
    track: &str,
    settings: &Settings,
    name: &str,
    (from, to, beats): (f64, f64, f64),
) -> Result<Made, String> {
    let src = source(track, &settings.from)?;
    let slices = settings.slices.clamp(1, 64) as usize;
    let len = (to - from) / slices as f64;
    let (dir, _lock) = crate::sample_bank::lock_sample_folder(name)?;
    let mut files = vec![];
    let mut first = None;
    for i in 0..slices {
        let a = from + len * i as f64;
        let label = if settings.from == "mix" { None } else { Some(settings.from.as_str()) };
        let saved = match crate::sample_bank::save_part(&dir, &src.path, Some(a), Some(a + len), src.gain, label) {
            Ok(saved) if first.is_none_or(|n| saved.n == n + i) => saved,
            result => {
                let error = match result {
                    Ok(saved) => {
                        let _ = std::fs::remove_file(saved.path);
                        "the slices did not land in consecutive sample slots".into()
                    }
                    Err(error) => error,
                };
                for file in files {
                    let _ = std::fs::remove_file(file);
                }
                return Err(error);
            }
        };
        first.get_or_insert(saved.n);
        files.push(saved.path);
    }
    let n = first.unwrap_or(0);
    let made = Made {
        name: name.into(),
        n,
        files,
        track: track.into(),
        label: label(settings.what, beats, &settings.from, settings.slices),
        code: tidal_code(name, n, slices, beats),
        seconds: to - from,
        made: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    };
    log(&made);
    Ok(made)
}

/// A request's settings over the remembered ones.
pub fn settings_from(req: &Value, was: &Settings) -> Result<Settings, String> {
    let mut s = was.clone();
    match req["what"].as_str() {
        Some("loop") => s.what = What::Loop,
        Some("beats") => {
            let b = req["beats"].as_f64().ok_or("beats needs a number")?;
            s.what = What::Beats { beats: b };
        }
        Some(other) => return Err(format!("what is loop or beats, not '{other}'")),
        None => {}
    }
    if let Some(f) = req["from"].as_str() {
        if f != "mix" && !patch_core::stems::CANONICAL.contains(&f) {
            return Err(format!("from is mix or a stem ({}), not '{f}'", patch_core::stems::CANONICAL.join(", ")));
        }
        s.from = f.into();
    }
    if let Some(n) = req["slices"].as_u64() {
        s.slices = n.clamp(1, 64) as u32;
    }
    if let Some(n) = req["name"].as_str() {
        let n = n.trim();
        if !n.is_empty() && !patch_pattern::samples::valid_name(n) {
            return Err(format!("'{n}' can't name a sound: letters, digits, _ - . only"));
        }
        s.name = n.into();
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Grid = Grid { bpm: 120.0, offset: 0.1, downbeat: 1 };

    #[test]
    fn the_part_cut() {
        // The loop, as it is.
        assert_eq!(region(What::Loop, Some(G), Some((10.1, 12.1)), 30.0).unwrap(), (10.1, 12.1, 4.0));
        assert!(region(What::Loop, Some(G), None, 30.0).unwrap_err().contains("no loop"));
        // Beats just played end on the last beat before the playhead.
        let (a, b, n) = region(What::Beats { beats: 1.0 }, Some(G), None, 10.37).unwrap();
        assert!((b - 10.1).abs() < 1e-9 && (a - 9.6).abs() < 1e-9 && n == 1.0, "{a} {b}");
        // A bar and more: on bar lines (bars start at 0.1 + 0.5 = 0.6, every 2 s).
        let (a, b, _) = region(What::Beats { beats: 4.0 }, Some(G), None, 11.0).unwrap();
        assert!((b - 10.6).abs() < 1e-9 && (a - 8.6).abs() < 1e-9, "{a} {b}");
        assert!(region(What::Beats { beats: 16.0 }, Some(G), None, 3.0).unwrap_err().contains("not 16 beats"));
        assert!(region(What::Beats { beats: 4.0 }, None, None, 30.0).is_err());
    }

    #[test]
    fn names_labels_and_code() {
        assert_eq!(default_name("Midnight Transit", ""), "midnight");
        assert_eq!(default_name("", "/m/03 - Rumble (Club Mix).flac"), "rumble");
        assert_eq!(default_name("1997", ""), "deck");
        assert_eq!(default_name("Ünïcode Größe", ""), "ncode");
        assert_eq!(tidal_code("midnight", 2, 1, 4.0), "loopAt 1 $ chop 8 $ s \"midnight:2\"");
        assert_eq!(tidal_code("midnight", 0, 1, 8.0), "loopAt 2 $ chop 16 $ s \"midnight:0\"");
        assert_eq!(tidal_code("midnight", 3, 1, 1.0), "s \"midnight:3\"");
        assert_eq!(tidal_code("kit", 4, 8, 4.0), "n \"4 .. 11\" # s \"kit\"");
        assert_eq!(label(What::Loop, 4.0, "drums", 1), "loop · 1 bar · drums");
        assert_eq!(label(What::Beats { beats: 8.0 }, 8.0, "mix", 8), "2 bars · mix · 8 slices");
        assert_eq!(label(What::Beats { beats: 0.5 }, 0.5, "vocals", 1), "0.5 beats · vocals");
    }

    #[test]
    fn requests_set_over_what_was() {
        let was = Settings::default();
        let s = settings_from(&serde_json::json!({"what": "beats", "beats": 8, "from": "drums", "slices": 4, "name": "kit"}), &was).unwrap();
        assert_eq!(s, Settings { what: What::Beats { beats: 8.0 }, from: "drums".into(), slices: 4, name: "kit".into() });
        let again = settings_from(&serde_json::json!({}), &s).unwrap();
        assert_eq!(again, s, "nothing given: as before");
        assert!(settings_from(&serde_json::json!({"from": "guitar"}), &was).is_err());
        assert!(settings_from(&serde_json::json!({"name": "two words"}), &was).is_err());
    }
}
