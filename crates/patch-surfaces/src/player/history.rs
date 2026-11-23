//! What the Player remembers between runs: the sources played lately, where
//! a long one was left (a podcast or an audiobook goes on from there), and
//! the queue as it last stood. Two small JSON files in the state folder;
//! the windows and the CLI share them.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Sources remembered, the latest first.
pub const KEPT: usize = 30;
/// A file this long or longer goes on where it was left.
pub const LONG: f64 = 600.0;
/// Not resumed this close to either end: it starts over.
const EDGE: f64 = 30.0;
/// Resuming steps back a little, to pick up the sentence.
const BACK: f64 = 3.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub path: String,
    pub title: String,
    pub duration: f64,
    /// Where it was left (0: from the start).
    pub position: f64,
    /// When it last played (seconds since 1970): the order.
    pub at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub items: Vec<String>,
    /// The one that was playing.
    pub current: usize,
}

fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("omarchy-patch"))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn load<T: Default + for<'a> Deserialize<'a>>(name: &str) -> T {
    state_dir()
        .and_then(|dir| std::fs::read(dir.join(name)).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Written beside and renamed: a reader never sees half a file.
fn save<T: Serialize>(name: &str, value: &T) {
    let Some(dir) = state_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(value) else { return };
    let tmp = dir.join(format!("{name}.new"));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(name));
    }
}

const HISTORY: &str = "player-history.json";
const QUEUE: &str = "player-queue.json";

/// The history file's time (None: none yet): a window reads it again when
/// this changes.
pub fn stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(state_dir()?.join(HISTORY)).and_then(|m| m.modified()).ok()
}

impl History {
    pub fn load() -> Self {
        load(HISTORY)
    }

    pub fn save(&self) {
        save(HISTORY, self);
    }

    /// A source was opened: it moves to the front (its place kept).
    pub fn opened(&mut self, path: &str) {
        let mut entry = match self.entries.iter().position(|e| e.path == path) {
            Some(i) => self.entries.remove(i),
            None => Entry { path: path.to_owned(), ..Entry::default() },
        };
        entry.at = now();
        self.entries.insert(0, entry);
        self.entries.truncate(KEPT);
    }

    /// What plays, where: its title and length become known, and a long
    /// file's place is kept (not within `EDGE` of either end, where it
    /// starts over). True when something changed.
    pub fn note(&mut self, path: &str, title: &str, position: f64, duration: f64) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) else { return false };
        let place = if duration >= LONG && position >= EDGE && position <= duration - EDGE { position } else { 0.0 };
        let mut changed = false;
        if !title.is_empty() && entry.title != title {
            entry.title = title.to_owned();
            changed = true;
        }
        if duration > 0.0 && (entry.duration - duration).abs() > 0.5 {
            entry.duration = duration;
            changed = true;
        }
        if (entry.position - place).abs() > 0.5 {
            entry.position = place;
            changed = true;
        }
        changed
    }

    /// Where to go on in a file (None: from the start).
    pub fn resume(&self, path: &str) -> Option<f64> {
        let entry = self.entries.iter().find(|e| e.path == path)?;
        (entry.position > 0.0).then(|| (entry.position - BACK).max(0.0))
    }

    /// Start this one over next time.
    pub fn forget_place(&mut self, path: &str) -> bool {
        match self.entries.iter_mut().find(|e| e.path == path) {
            Some(entry) if entry.position > 0.0 => {
                entry.position = 0.0;
                true
            }
            _ => false,
        }
    }
}

impl Queue {
    pub fn load() -> Self {
        let mut queue: Self = load(QUEUE);
        queue.current = queue.current.min(queue.items.len().saturating_sub(1));
        queue
    }

    pub fn save(&self) {
        save(QUEUE, self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_source_is_first_and_the_list_is_bounded() {
        let mut h = History::default();
        for i in 0..KEPT + 5 {
            h.opened(&format!("/a/{i}.mp3"));
        }
        assert_eq!(h.entries.len(), KEPT);
        assert_eq!(h.entries[0].path, format!("/a/{}.mp3", KEPT + 4));
        // Opened again: to the front, once, with what was known of it.
        h.note("/a/20.mp3", "Twenty", 700.0, 3600.0);
        h.opened("/a/20.mp3");
        assert_eq!(h.entries[0].title, "Twenty");
        assert_eq!(h.entries.iter().filter(|e| e.path == "/a/20.mp3").count(), 1);
        assert_eq!(h.entries.len(), KEPT);
    }

    #[test]
    fn a_long_file_goes_on_where_it_was_left_and_a_short_one_starts_over() {
        let mut h = History::default();
        h.opened("/pod/ep42.mp3");
        h.opened("/jingle.wav");
        assert!(h.note("/pod/ep42.mp3", "Episode 42", 761.0, 2886.0));
        assert_eq!(h.resume("/pod/ep42.mp3"), Some(758.0), "a little before, to pick up the sentence");
        assert!(!h.note("/pod/ep42.mp3", "Episode 42", 761.2, 2886.0), "no write for a fraction of a second");
        // A jingle, or a track: never resumed.
        h.note("/jingle.wav", "Jingle", 4.0, 9.0);
        h.note("/jingle.wav", "Jingle", 200.0, 400.0);
        assert_eq!(h.resume("/jingle.wav"), None);
        // Near the start or the end: from the start.
        h.note("/pod/ep42.mp3", "Episode 42", 12.0, 2886.0);
        assert_eq!(h.resume("/pod/ep42.mp3"), None);
        h.note("/pod/ep42.mp3", "Episode 42", 2870.0, 2886.0);
        assert_eq!(h.resume("/pod/ep42.mp3"), None);
        // Stop: over from the start.
        h.note("/pod/ep42.mp3", "Episode 42", 900.0, 2886.0);
        assert!(h.forget_place("/pod/ep42.mp3"));
        assert_eq!(h.resume("/pod/ep42.mp3"), None);
        assert!(!h.note("/not/opened.mp3", "x", 900.0, 2886.0), "only what was opened is remembered");
    }
}
