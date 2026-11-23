//! The DJ's own title, artist, year and genre for a track, kept in the
//! library and never written into the audio file (the user, 2026-11-14:
//! "keep the edits of files in patch, never the files themselves").
//!
//! An edit wins over the file's tags and over what was looked up online,
//! wherever Patch reads them ([`Library::tracks`], [`Library::dj_pool`],
//! [`Library::years`], [`Library::genres`]). The `tracks` row keeps the
//! file's own tags, so clearing a field brings the file's value back. A
//! table of its own, made with the first edit: a library older Patches read
//! stays as it was.

use crate::Library;
use rusqlite::params;
use std::collections::HashMap;

/// What the DJ changed; `None` is the file's own value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackEdit {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub year: Option<u16>,
    pub genre: Option<String>,
}

impl TrackEdit {
    pub fn is_empty(&self) -> bool {
        *self == TrackEdit::default()
    }
}

/// A field as typed: trimmed, empty = no edit.
fn field(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Library {
    fn has_edits(&self) -> bool {
        self.db
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'track_edits'",
                [],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Every edit, by path (none before the first).
    pub fn track_edits(&self) -> HashMap<String, TrackEdit> {
        if !self.has_edits() {
            return HashMap::new();
        }
        let Ok(mut st) = self.db.prepare(
            "SELECT t.path, e.title, e.artist, e.year, e.genre FROM track_edits e JOIN tracks t ON t.id = e.track_id",
        ) else {
            return HashMap::new();
        };
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TrackEdit {
                    title: r.get(1)?,
                    artist: r.get(2)?,
                    year: r
                        .get::<_, Option<i64>>(3)?
                        .and_then(|y| u16::try_from(y).ok()),
                    genre: r.get(4)?,
                },
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// `path`'s edit (all `None` when there is none).
    pub fn track_edit(&self, path: &str) -> TrackEdit {
        self.track_edits().remove(path).unwrap_or_default()
    }

    /// Keep `edit` for `path`: blank fields are the file's own value; all
    /// blank removes the edit. Only the library changes, never the file.
    pub fn set_track_edit(&self, path: &str, edit: &TrackEdit) -> Result<(), String> {
        let id = self
            .track_by_path(path)
            .ok_or("not a track in the library")?;
        let edit = TrackEdit {
            title: field(&edit.title),
            artist: field(&edit.artist),
            year: edit.year.filter(|y| (1000..=2999).contains(y)),
            genre: field(&edit.genre),
        };
        self.db
            .execute(
                "CREATE TABLE IF NOT EXISTS track_edits (
                    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    title TEXT, artist TEXT, year INTEGER, genre TEXT, at INTEGER NOT NULL)",
                [],
            )
            .map_err(|e| e.to_string())?;
        if edit.is_empty() {
            self.db
                .execute("DELETE FROM track_edits WHERE track_id = ?1", params![id])
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        self.db
            .execute(
                "INSERT INTO track_edits (track_id, title, artist, year, genre, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(track_id) DO UPDATE SET title = excluded.title, artist = excluded.artist,
                     year = excluded.year, genre = excluded.genre, at = excluded.at",
                params![id, edit.title, edit.artist, edit.year.map(i64::from), edit.genre, crate::now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_wins_everywhere_and_clearing_it_brings_the_file_back() {
        let dir = std::env::temp_dir().join(format!("patch-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("track.mp3");
        std::fs::write(&file, vec![7u8; 96]).unwrap();
        let before = std::fs::read(&file).unwrap();
        let db = Library::in_memory().unwrap();
        db.add_source(&dir, "Music").unwrap();
        db.scan(&dir, |_| None).unwrap();
        let path = file.to_string_lossy().to_string();
        assert!(db.track_edits().is_empty(), "no table yet: none");
        let tagged = db.tracks().unwrap()[0].title.clone();

        let edit = TrackEdit {
            title: Some("  Holiday ".into()),
            artist: Some("Bee Gees".into()),
            year: Some(1967),
            genre: Some("Pop".into()),
        };
        db.set_track_edit(&path, &edit).unwrap();
        let t = &db.tracks().unwrap()[0];
        assert_eq!(
            (t.title.as_str(), t.artist.as_str(), t.genre.as_str()),
            ("Holiday", "Bee Gees", "Pop")
        );
        let pool = db.dj_pool().unwrap();
        assert_eq!(
            (pool[0].1.as_str(), pool[0].2.as_str()),
            ("Holiday", "Bee Gees")
        );
        assert_eq!(db.years()[&path], (Some(1967), None));
        assert_eq!(db.genres()[&path], "Pop");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "the file is never written"
        );

        // A blank field is the file's own value; all blank: no edit.
        db.set_track_edit(
            &path,
            &TrackEdit {
                title: Some(" ".into()),
                year: Some(1967),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.tracks().unwrap()[0].title, tagged);
        assert_eq!(db.track_edit(&path).year, Some(1967));
        db.set_track_edit(&path, &TrackEdit::default()).unwrap();
        assert!(db.track_edits().is_empty());
        assert!(!db.years().contains_key(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
