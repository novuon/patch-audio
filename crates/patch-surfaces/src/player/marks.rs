//! Marks: named places in a file, set while listening (M) and kept per
//! file. Their list copies out as timestamps, the form show notes and video
//! chapters use ("12:41 Sponsor read"). One small JSON file in the state
//! folder; the files themselves are never written to.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Two marks closer than this are one.
const SAME: f64 = 0.5;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mark {
    /// Seconds into the file.
    pub at: f64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Marks {
    /// Each file's marks, in time order.
    pub files: BTreeMap<String, Vec<Mark>>,
}

fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("omarchy-patch/player-marks.json"))
}

/// The marks file's time (None: none yet).
pub fn stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(file()?).and_then(|m| m.modified()).ok()
}

impl Marks {
    pub fn load() -> Self {
        file().and_then(|f| std::fs::read(f).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(file) = file() else { return };
        if let Some(dir) = file.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return;
        }
        let Ok(text) = serde_json::to_string(self) else { return };
        let tmp = file.with_extension("json.new");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }

    pub fn of(&self, path: &str) -> &[Mark] {
        self.files.get(path).map_or(&[], Vec::as_slice)
    }

    /// Mark a place; its index in the file's list. A mark already there
    /// (within half a second) is that one, not a second.
    pub fn add(&mut self, path: &str, at: f64) -> usize {
        let marks = self.files.entry(path.to_owned()).or_default();
        if let Some(i) = marks.iter().position(|m| (m.at - at).abs() < SAME) {
            return i;
        }
        let i = marks.iter().position(|m| m.at > at).unwrap_or(marks.len());
        marks.insert(i, Mark { at: at.max(0.0), name: String::new() });
        i
    }

    pub fn remove(&mut self, path: &str, index: usize) {
        if let Some(marks) = self.files.get_mut(path) {
            if index < marks.len() {
                marks.remove(index);
            }
            if marks.is_empty() {
                self.files.remove(path);
            }
        }
    }

    pub fn rename(&mut self, path: &str, index: usize, name: &str) {
        if let Some(mark) = self.files.get_mut(path).and_then(|marks| marks.get_mut(index)) {
            mark.name = name.trim().to_owned();
        }
    }
}

/// A time as show notes write it: 12:41, and 1:02:41 from an hour on.
pub fn stamp_of(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

/// Places as lines of "time name", in time order: what Copy puts on the
/// clipboard. With any place from an hour on, every line has the hours, so
/// the column lines up (and a video site reads them as chapters).
pub fn notes(places: &[(f64, String)]) -> String {
    let hours = places.iter().any(|(at, _)| *at >= 3600.0);
    let mut places: Vec<&(f64, String)> = places.iter().collect();
    places.sort_by(|a, b| a.0.total_cmp(&b.0));
    places
        .iter()
        .map(|(at, name)| {
            let s = at.max(0.0) as u64;
            let time = if hours { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { stamp_of(*at) };
            if name.trim().is_empty() { time } else { format!("{time} {}", name.trim()) }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The place to jump to from `position`: the next one on (step 1) or the
/// one before (step -1; from just after a place, the one before that, as a
/// "previous" key does).
pub fn step(places: &[f64], position: f64, step: i32) -> Option<f64> {
    let mut places: Vec<f64> = places.to_vec();
    places.sort_by(f64::total_cmp);
    if step > 0 {
        places.into_iter().find(|p| *p > position + SAME)
    } else {
        places.into_iter().rev().find(|p| *p < position - 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_stay_in_time_order_and_one_place_is_one_mark() {
        let mut marks = Marks::default();
        assert_eq!(marks.add("/ep.mp3", 300.0), 0);
        assert_eq!(marks.add("/ep.mp3", 60.0), 0, "an earlier place goes before");
        assert_eq!(marks.add("/ep.mp3", 900.0), 2);
        assert_eq!(marks.add("/ep.mp3", 300.3), 1, "the same place again is the same mark");
        assert_eq!(marks.of("/ep.mp3").iter().map(|m| m.at).collect::<Vec<_>>(), [60.0, 300.0, 900.0]);
        marks.rename("/ep.mp3", 1, "  Sponsor read ");
        assert_eq!(marks.of("/ep.mp3")[1].name, "Sponsor read");
        marks.remove("/ep.mp3", 0);
        marks.remove("/ep.mp3", 0);
        marks.remove("/ep.mp3", 0);
        assert!(marks.files.is_empty(), "a file with no marks left is not kept");
        assert!(marks.of("/other.mp3").is_empty());
    }

    #[test]
    fn the_list_copies_as_show_notes() {
        let places = vec![(761.0, "Sponsor read".to_owned()), (0.0, "Intro".to_owned()), (95.5, String::new())];
        assert_eq!(notes(&places), "00:00 Intro\n01:35\n12:41 Sponsor read");
        // From an hour on, every line carries the hours.
        let long = vec![(0.0, "Intro".to_owned()), (3761.0, "Part two".to_owned())];
        assert_eq!(notes(&long), "0:00:00 Intro\n1:02:41 Part two");
        assert_eq!(stamp_of(3761.0), "1:02:41");
    }

    #[test]
    fn next_and_previous_step_through_the_places() {
        let places = [300.0, 60.0, 900.0];
        assert_eq!(step(&places, 0.0, 1), Some(60.0));
        assert_eq!(step(&places, 60.0, 1), Some(300.0), "from a place, the next one");
        assert_eq!(step(&places, 950.0, 1), None);
        assert_eq!(step(&places, 400.0, -1), Some(300.0));
        assert_eq!(step(&places, 301.0, -1), Some(60.0), "just after a place: the one before it");
        assert_eq!(step(&places, 30.0, -1), None);
    }
}
