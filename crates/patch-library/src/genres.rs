//! Genres: the file's own genre tag when it names one ("Tech House"), else
//! what people tagged the song with at MusicBrainz, else its artist's
//! genres there — only names on MusicBrainz's genre list, so "seen live"
//! or "top 40" never count. Looked up when the user allows it, with the
//! release years (the same search); nothing is mirrored.

use crate::Library;
use crate::years::{Found, fetch};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// A genre tag that says nothing about the music: a chart, a shop, a
/// placeholder or a web address.
fn says_nothing(g: &str) -> bool {
    let l = g.trim().to_lowercase();
    let squashed: String = l.chars().filter(|c| c.is_alphanumeric()).collect();
    l.is_empty()
        || l.contains("www.")
        || l.contains("http")
        || l.contains(".com")
        || l.starts_with('<')
        || l.starts_with('(')
        || squashed.chars().all(|c| c.is_ascii_digit())
        || [
            "top40", "top100", "other", "others", "various", "variousartists", "unknown", "undefined",
            "genre", "none", "misc", "music", "default", "mp3", "single", "album", "jaarlijst", "hits",
            "charts", "chart",
        ]
        .contains(&squashed.as_str())
}

/// The file's genre tag, tidied, when it names a genre.
pub fn tag_genre(raw: &str) -> Option<String> {
    let g = raw.trim().trim_matches(|c: char| c == ';' || c == ',' || c == '/').trim();
    // "Electronic, House": the narrower word comes last.
    let g = match g.split([',', ';']).map(str::trim).filter(|p| !says_nothing(p)).collect::<Vec<_>>() {
        parts if parts.len() > 1 && parts[0].eq_ignore_ascii_case("electronic") => parts[1..].join(", "),
        parts => parts.join(", "),
    };
    (!says_nothing(&g)).then_some(g)
}

/// "tech house" as "Tech House"; "r&b", "uk garage", "drum and bass" kept readable.
fn title_case(g: &str) -> String {
    g.split(' ')
        .map(|w| match w {
            "and" | "n" | "of" | "the" => w.to_string(),
            "uk" | "us" | "edm" | "idm" | "ebm" | "r&b" | "dnb" => w.to_uppercase(),
            _ => {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The best one or two genres of some tags: on the genre list, by votes.
pub fn pick(tags: &[(String, u32)], genres: &HashSet<String>) -> Option<String> {
    let mut known: Vec<&(String, u32)> = tags.iter().filter(|(n, _)| genres.contains(n)).collect();
    known.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top: Vec<String> = known.iter().take(2).map(|(n, _)| title_case(n)).collect();
    (!top.is_empty()).then(|| top.join(" · "))
}

/// MusicBrainz's genre list (a few thousand names), kept a month in the
/// cache folder.
pub fn genre_list() -> Result<HashSet<String>, String> {
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache")))
        .ok_or("no cache folder")?
        .join("omarchy-patch");
    let file = dir.join("musicbrainz-genres.txt");
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 30 * 86_400));
    let text = match std::fs::read_to_string(&file) {
        Ok(t) if fresh && !t.is_empty() => t,
        cached => match fetch("genre/all", &[("fmt", "txt")]) {
            Ok(t) if t.lines().count() > 100 => {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&file, &t);
                t
            }
            // Offline: last month's list does.
            other => cached.ok().filter(|t| !t.is_empty()).ok_or_else(|| {
                other.err().unwrap_or_else(|| "MusicBrainz gave no genre list".into())
            })?,
        },
    };
    Ok(text.lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect())
}

/// An artist's genres at MusicBrainz, with their votes.
fn artist_tags(id: &str) -> Result<Vec<(String, u32)>, String> {
    let text = fetch(&format!("artist/{id}"), &[("inc", "genres"), ("fmt", "json")])?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v["genres"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| Some((g["name"].as_str()?.to_lowercase(), g["count"].as_i64().unwrap_or(1).max(1) as u32)))
        .collect())
}

impl Library {
    fn genres_table(&self) -> Result<(), String> {
        self.db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS track_genres (
                    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    genre TEXT, source TEXT, looked_up INTEGER);
                 CREATE TABLE IF NOT EXISTS artist_genres (mbid TEXT PRIMARY KEY, genre TEXT, looked_up INTEGER);",
            )
            .map_err(|e| e.to_string())
    }

    /// Each track's genre, by path.
    pub fn genres(&self) -> HashMap<String, String> {
        let Ok(mut st) = self.db.prepare(
            "SELECT t.path, g.genre FROM track_genres g JOIN tracks t ON t.id = g.track_id WHERE g.genre IS NOT NULL",
        ) else {
            return self
                .track_edits()
                .into_iter()
                .filter_map(|(p, e)| Some((p, e.genre?)))
                .collect();
        };
        let mut genres: HashMap<String, String> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        // The DJ's own genre wins.
        for (path, e) in self.track_edits() {
            if let Some(g) = e.genre {
                genres.insert(path, g);
            }
        }
        genres
    }

    /// Genres from the tags of tracks that have none yet: offline. A tag
    /// that says nothing leaves the track to look up. How many were added.
    pub fn genres_from_tags(&self) -> Result<usize, String> {
        self.genres_table()?;
        let rows: Vec<(i64, Option<String>)> = {
            let mut st = self
                .db
                .prepare(
                    "SELECT t.id, s.data FROM tracks t LEFT JOIN source_metadata s ON s.track_id = t.id
                     WHERE t.id NOT IN (SELECT track_id FROM track_genres)",
                )
                .map_err(|e| e.to_string())?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect()
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.db, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (id, data) in &rows {
            let genre = data
                .as_deref()
                .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
                .and_then(|v| v["genre"].as_str().and_then(tag_genre));
            let source = genre.as_ref().map(|_| "tag");
            tx.execute(
                "INSERT OR IGNORE INTO track_genres (track_id, genre, source) VALUES (?1, ?2, ?3)",
                params![id, genre, source],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows.len())
    }

    /// Tracks whose genre is still to look up: (id, artist, title).
    pub fn genres_to_look_up(&self, limit: usize) -> Vec<crate::years::Pending> {
        let Ok(mut st) = self.db.prepare(
            "SELECT t.id, t.artist, t.title FROM track_genres g JOIN tracks t ON t.id = g.track_id
             WHERE g.genre IS NULL AND g.looked_up IS NULL AND t.missing = 0 AND t.artist != '' AND t.title != ''
             ORDER BY t.id LIMIT ?1",
        ) else {
            return vec![];
        };
        st.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// How many tracks still wait for a genre lookup.
    pub fn genres_left(&self) -> usize {
        self.db
            .query_row(
                "SELECT count(*) FROM track_genres g JOIN tracks t ON t.id = g.track_id
                 WHERE g.genre IS NULL AND g.looked_up IS NULL AND t.missing = 0 AND t.artist != '' AND t.title != ''",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
    }

    fn needs_genre(&self, track: i64) -> bool {
        self.db
            .query_row(
                "SELECT genre IS NULL AND looked_up IS NULL FROM track_genres WHERE track_id = ?1",
                params![track],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }

    /// The genre a lookup found for a track that needs one: the song's
    /// tags, else its artist's genres (asked once per artist). Network.
    pub fn set_genre_found(&self, track: i64, found: &Found, list: &HashSet<String>, now: i64) -> Result<(), String> {
        self.genres_table()?;
        if !self.needs_genre(track) {
            return Ok(());
        }
        let genre = match pick(&found.tags, list) {
            Some(g) => Some(g),
            None => match &found.artist_id {
                Some(id) => self.artist_genre(id, list, now)?,
                None => None,
            },
        };
        self.db
            .execute(
                "UPDATE track_genres SET genre = ?2, source = CASE WHEN ?2 IS NULL THEN NULL ELSE 'musicbrainz' END, looked_up = ?3 WHERE track_id = ?1",
                params![track, genre, now],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn artist_genre(&self, id: &str, list: &HashSet<String>, now: i64) -> Result<Option<String>, String> {
        if let Ok(g) = self.db.query_row(
            "SELECT genre FROM artist_genres WHERE mbid = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        ) {
            return Ok(g);
        }
        let g = pick(&artist_tags(id)?, list);
        self.db
            .execute(
                "INSERT OR REPLACE INTO artist_genres (mbid, genre, looked_up) VALUES (?1, ?2, ?3)",
                params![id, g, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_that_names_a_genre_counts_and_a_chart_does_not() {
        for junk in ["Top 40", "Top40", "top40", "Top 40   ", "Other", "Various", "www.RadioFLy.ws", "<Undefined>", "", "2010"] {
            assert_eq!(tag_genre(junk), None, "{junk}");
        }
        assert_eq!(tag_genre("Tech House").as_deref(), Some("Tech House"));
        assert_eq!(tag_genre("Techno (Peak Time / Driving)").as_deref(), Some("Techno (Peak Time / Driving)"));
        assert_eq!(tag_genre("Electronic, House").as_deref(), Some("House"));
        assert_eq!(tag_genre("Nu Disco / Disco").as_deref(), Some("Nu Disco / Disco"));
    }

    #[test]
    fn only_listed_genres_by_votes() {
        let list: HashSet<String> = ["house", "tech house", "uk garage", "new wave"].map(String::from).into();
        let tags = vec![("seen live".into(), 9), ("house".into(), 2), ("tech house".into(), 5), ("uk garage".into(), 1)];
        assert_eq!(pick(&tags, &list).as_deref(), Some("Tech House · House"));
        assert_eq!(pick(&[("uk garage".into(), 1)], &list).as_deref(), Some("UK Garage"));
        assert_eq!(pick(&[("2010s".into(), 4)], &list), None);
    }

    #[test]
    fn genres_from_tags_then_the_lookup_fills_the_rest() {
        let dir = std::env::temp_dir().join(format!("patch-genres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp3");
        std::fs::write(&file, vec![5u8; 90]).unwrap();
        let db = Library::in_memory().unwrap();
        let (id, _) = db.import(&file, |_| None).unwrap();
        assert_eq!(db.genres_from_tags().unwrap(), 1);
        assert!(db.genres().is_empty(), "no tag: nothing yet");
        assert!(db.needs_genre(id));
        let list: HashSet<String> = ["new wave"].map(String::from).into();
        let found = Found { tags: vec![("new wave".into(), 2)], ..Default::default() };
        db.set_genre_found(id, &found, &list, 7).unwrap();
        assert_eq!(db.genres().get(file.to_str().unwrap()).map(String::as_str), Some("New Wave"));
        assert!(!db.needs_genre(id) && db.genres_left() == 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
