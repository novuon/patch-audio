//! Release years: this release's year (the file's own date tag: a Beatport
//! top 100 copy of an old song says when *it* came out) and the original's
//! (the first release of the song, any version), looked up at MusicBrainz
//! when the user allows it. Nothing is mirrored: one lookup per track,
//! remembered in the library.

use crate::Library;
use rusqlite::params;
use serde_json::Value;
use std::collections::HashMap;

/// Years from a file's tags: (this release, the original), each when the
/// tags say it.
pub fn tag_years(tags: &Value) -> (Option<u16>, Option<u16>) {
    let mut release = None;
    let mut original = None;
    for part in ["format", "stream"] {
        let Some(map) = tags[part].as_object() else { continue };
        for (k, v) in map {
            let key = k.to_ascii_lowercase();
            let year = v.as_str().and_then(year_of);
            match key.as_str() {
                "date" | "year" | "tdrc" | "tyer" | "release_date" => release = release.or(year),
                "tdor" | "tory" | "originaldate" | "original_date" | "originalyear"
                | "original_year" | "origyear" => original = original.or(year),
                _ => {}
            }
        }
    }
    (release, original)
}

/// The year a date starts with ("1987", "2026-03-13", "1987-00-00").
pub fn year_of(text: &str) -> Option<u16> {
    let t = text.trim();
    let y: u16 = t.get(..4)?.parse().ok()?;
    (t.len() == 4 || !t.as_bytes()[4].is_ascii_digit()).then_some(y).filter(|y| (1900..=2100).contains(y))
}

/// The song's title without the version: "Ghost (Extended Mix)",
/// "Ghost - Remastered 2011" and "Ghost [Club Edit]" are all "Ghost".
pub fn base_title(title: &str) -> String {
    const VERSION: &[&str] = &[
        "mix", "remix", "edit", "version", "dub", "rework", "remaster", "radio", "extended",
        "original", "club", "vip", "instrumental", "bootleg", "flip", "re-edit", "live", "mono",
        "stereo", "single",
    ];
    let mut t = title.trim().to_string();
    loop {
        let before = t.clone();
        // A trailing (…) or […] that names a version.
        for (open, close) in [('(', ')'), ('[', ']')] {
            if t.ends_with(close)
                && let Some(i) = t.rfind(open)
            {
                let inner = t[i + 1..t.len() - 1].to_lowercase();
                if VERSION.iter().any(|v| inner.contains(v)) {
                    t = t[..i].trim().to_string();
                }
            }
        }
        // " - Remastered 2011", " - Radio Edit".
        if let Some(i) = t.rfind(" - ") {
            let tail = t[i + 3..].to_lowercase();
            if VERSION.iter().any(|v| tail.contains(v)) {
                t = t[..i].trim().to_string();
            }
        }
        if t == before {
            return t;
        }
    }
}

/// Lowercase letters and digits only, for comparing names.
fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The first artist of a credit ("A & B feat. C" is "A").
pub fn main_artist(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let mut end = artist.len();
    for sep in [",", " & ", " feat", " ft.", " ft ", " x ", " vs", " and ", " + ", ";", "/"] {
        if let Some(i) = lower.find(sep) {
            end = end.min(i);
        }
    }
    artist[..end].trim().to_string()
}

/// What a MusicBrainz recording search found: the earliest year any
/// version of the song came out, and the year this exact version did.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    pub original: Option<u16>,
    pub this_version: Option<u16>,
    /// The tags people gave the song's recordings, with their votes
    /// (summed over the matching ones): genres among them.
    pub tags: Vec<(String, u32)>,
    /// The main artist's MusicBrainz id, for their genres when the song
    /// has none.
    pub artist_id: Option<String>,
}

/// Read a MusicBrainz recording search (`/ws/2/recording?fmt=json`).
pub fn read_search(json: &Value, artist: &str, title: &str) -> Found {
    let want_artist = plain(&main_artist(artist));
    let want_base = plain(&base_title(title));
    let want_full = plain(title);
    let mut found = Found::default();
    for r in json["recordings"].as_array().into_iter().flatten() {
        if r["score"].as_u64().unwrap_or(0) < 70 {
            continue;
        }
        let year = r["first-release-date"].as_str().and_then(year_of);
        let credit: String = r["artist-credit"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["name"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let credit = plain(&credit);
        if want_artist.is_empty() || !credit.contains(&want_artist) {
            continue;
        }
        let name = r["title"].as_str().unwrap_or("");
        if plain(&base_title(name)) != want_base {
            continue;
        }
        if found.artist_id.is_none() {
            found.artist_id = r["artist-credit"][0]["artist"]["id"].as_str().map(str::to_owned);
        }
        for t in r["tags"].as_array().into_iter().flatten() {
            let (Some(name), votes) = (t["name"].as_str(), t["count"].as_i64().unwrap_or(1).max(1) as u32) else {
                continue;
            };
            let name = name.trim().to_lowercase();
            match found.tags.iter_mut().find(|(n, _)| *n == name) {
                Some((_, v)) => *v += votes,
                None => found.tags.push((name, votes)),
            }
        }
        let Some(year) = year else { continue };
        found.original = Some(found.original.map_or(year, |y| y.min(year)));
        if plain(name) == want_full {
            found.this_version = Some(found.this_version.map_or(year, |y| y.min(year)));
        }
    }
    found
}

/// One recording search at MusicBrainz.
fn search(query: &str) -> Result<Value, String> {
    let text = fetch(
        "recording",
        &[("query", query), ("fmt", "json"), ("limit", "100")],
    )?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// One request to MusicBrainz's web service (`/ws/2/<path>`), waiting out
/// its one-request-a-second rule (and a busy answer, 503) here. The
/// caller has the user's leave to ask.
pub(crate) fn fetch(path: &str, query: &[(&str, &str)]) -> Result<String, String> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    for attempt in 0..4 {
        {
            let mut last = LAST.lock().unwrap();
            if let Some(at) = *last {
                let gap = Duration::from_millis(1100 * (1 + attempt));
                if at.elapsed() < gap {
                    std::thread::sleep(gap - at.elapsed());
                }
            }
            *last = Some(Instant::now());
        }
        let out = std::process::Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "20",
                "-w",
                "\n%{http_code}",
                "-A",
                concat!("Patch/", env!("CARGO_PKG_VERSION"), " ( https://github.com/novuon/patch )"),
                "--get",
            ])
            .args(query.iter().flat_map(|(k, v)| ["--data-urlencode".to_string(), format!("{k}={v}")]))
            .arg(format!("https://musicbrainz.org/ws/2/{path}"))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", ""));
        match code.trim() {
            "200" => return Ok(body.to_string()),
            "503" => continue,
            "" => return Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            c => return Err(format!("MusicBrainz answered {c}")),
        }
    }
    Err("MusicBrainz is busy".into())
}

/// Ask MusicBrainz about one song: a search, then again for anything
/// released before the earliest year found, until nothing earlier turns
/// up (a song with hundreds of recordings lists its first one far down).
/// The caller has the user's leave to ask.
pub fn musicbrainz(artist: &str, title: &str) -> Result<Found, String> {
    // Quotes would end the phrase early in the search syntax.
    let clean = |s: &str| s.replace(['"', '\\'], " ");
    let base = format!(
        "recording:\"{}\" AND artist:\"{}\"",
        clean(&base_title(title)),
        clean(&main_artist(artist))
    );
    let mut found = read_search(&search(&base)?, artist, title);
    // The years' narrower searches add no tags: the first search's stand.
    for _ in 0..4 {
        let Some(year) = found.original else { break };
        let earlier = read_search(
            &search(&format!("{base} AND firstreleasedate:[* TO {}]", year - 1))?,
            artist,
            title,
        );
        match earlier.original {
            Some(y) if y < year => found.original = Some(y),
            _ => break,
        }
    }
    Ok(found)
}

/// The song's first search alone: its tags and artist, for the genre of a
/// track whose years were looked up already.
pub fn musicbrainz_tags(artist: &str, title: &str) -> Result<Found, String> {
    let clean = |s: &str| s.replace(['"', '\\'], " ");
    let base = format!(
        "recording:\"{}\" AND artist:\"{}\"",
        clean(&base_title(title)),
        clean(&main_artist(artist))
    );
    Ok(read_search(&search(&base)?, artist, title))
}

/// A track still to look up: (id, artist, title).
pub type Pending = (i64, String, String);

impl Library {
    fn years_table(&self) -> Result<(), String> {
        self.db
            .execute(
                "CREATE TABLE IF NOT EXISTS track_years (
                    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    release INTEGER, original INTEGER, looked_up INTEGER)",
                [],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Each track's (original, this release) years, by path.
    pub fn years(&self) -> HashMap<String, (Option<u16>, Option<u16>)> {
        let Ok(mut st) = self.db.prepare(
            "SELECT t.path, y.original, y.release FROM track_years y JOIN tracks t ON t.id = y.track_id",
        ) else {
            // No years looked up yet: the DJ's own, if any.
            return self
                .track_edits()
                .into_iter()
                .filter_map(|(p, e)| Some((p, (Some(e.year?), None))))
                .collect();
        };
        let mut years: HashMap<String, (Option<u16>, Option<u16>)> = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        // The DJ's own year wins (the one year it shows).
        for (path, e) in self.track_edits() {
            if let Some(y) = e.year {
                years.insert(path, (Some(y), None));
            }
        }
        years
    }

    /// Years from the tags of tracks that have none yet: offline, cheap.
    /// How many were added.
    pub fn years_from_tags(&self) -> Result<usize, String> {
        self.years_table()?;
        let rows: Vec<(i64, Option<String>)> = {
            let mut st = self
                .db
                .prepare(
                    "SELECT t.id, s.data FROM tracks t LEFT JOIN source_metadata s ON s.track_id = t.id
                     WHERE t.id NOT IN (SELECT track_id FROM track_years)",
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
            let tags = data
                .as_deref()
                .and_then(|d| serde_json::from_str::<Value>(d).ok())
                .map(|v| v["original_tags"].clone())
                .unwrap_or(Value::Null);
            let (release, original) = tag_years(&tags);
            tx.execute(
                "INSERT OR IGNORE INTO track_years (track_id, release, original) VALUES (?1, ?2, ?3)",
                params![id, release, original],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows.len())
    }

    /// Tracks not looked up yet, with an artist and title to ask about.
    pub fn years_to_look_up(&self, limit: usize) -> Vec<Pending> {
        let Ok(mut st) = self.db.prepare(
            "SELECT t.id, t.artist, t.title FROM track_years y JOIN tracks t ON t.id = y.track_id
             WHERE y.looked_up IS NULL AND t.missing = 0 AND t.artist != '' AND t.title != ''
             ORDER BY t.id LIMIT ?1",
        ) else {
            return vec![];
        };
        st.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// What a lookup found. The original is never after this release (a
    /// later year means another song of that name); a release year the tags
    /// lacked comes from this version's own first release.
    pub fn set_years_found(&self, track: i64, found: &Found, now: i64) -> Result<(), String> {
        self.years_table()?;
        let (release, tag_original): (Option<u16>, Option<u16>) = self
            .db
            .query_row(
                "SELECT release, original FROM track_years WHERE track_id = ?1",
                params![track],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((None, None));
        let release = release.or(found.this_version);
        let original = [found.original, tag_original, release]
            .into_iter()
            .flatten()
            .min()
            .filter(|_| found.original.is_some() || tag_original.is_some());
        self.db
            .execute(
                "INSERT INTO track_years (track_id, release, original, looked_up) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track_id) DO UPDATE SET release = ?2, original = ?3, looked_up = ?4",
                params![track, release, original, now],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// How many tracks have been looked up, and how many wait.
    pub fn years_progress(&self) -> (usize, usize) {
        self.db
            .query_row(
                "SELECT count(y.looked_up), count(*) - count(y.looked_up) FROM track_years y
                 JOIN tracks t ON t.id = y.track_id
                 WHERE t.missing = 0 AND t.artist != '' AND t.title != ''",
                [],
                |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
            )
            .unwrap_or((0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A real lookup (network): `cargo test -p patch-library -- --ignored years_online`.
    #[test]
    #[ignore]
    fn years_online() {
        let f = musicbrainz("New Order", "Blue Monday (2026 Extended Mix)").unwrap();
        assert_eq!(f.original, Some(1983), "{f:?}");
    }

    #[test]
    fn years_come_from_dates_and_original_tags() {
        assert_eq!(year_of("2026-03-13"), Some(2026));
        assert_eq!(year_of("1987"), Some(1987));
        assert_eq!(year_of("1987-00-00"), Some(1987));
        assert_eq!(year_of("20260313"), None, "not a date we can read");
        assert_eq!(year_of("abc"), None);
        let tags = json!({"format": {"date": "2026", "TDOR": "1987-05-01"}, "stream": {}});
        assert_eq!(tag_years(&tags), (Some(2026), Some(1987)));
        assert_eq!(tag_years(&json!({"format": {"DATE": "2015"}})), (Some(2015), None));
        assert_eq!(tag_years(&Value::Null), (None, None));
    }

    #[test]
    fn the_song_title_drops_the_version() {
        assert_eq!(base_title("Zoolander  (Extended Mix)"), "Zoolander");
        assert_eq!(base_title("Ghost - Remastered 2011"), "Ghost");
        assert_eq!(base_title("Ghost [Club Edit] (Radio Version)"), "Ghost");
        assert_eq!(base_title("(I've Had) The Time Of My Life"), "(I've Had) The Time Of My Life");
        assert_eq!(base_title("Somebody (Feat. X)"), "Somebody (Feat. X)");
        assert_eq!(main_artist("Bill Medley & Jennifer Warnes"), "Bill Medley");
        assert_eq!(main_artist("Rafael, Adam Ten"), "Rafael");
    }

    #[test]
    fn a_search_gives_the_first_year_of_any_version_and_of_this_one() {
        let json = json!({"recordings": [
            {"score": 100, "title": "Blue Monday", "first-release-date": "1983-03-07",
             "artist-credit": [{"name": "New Order"}]},
            {"score": 95, "title": "Blue Monday (2026 Extended Mix)", "first-release-date": "2026-01-10",
             "artist-credit": [{"name": "New Order"}]},
            {"score": 90, "title": "Blue Monday", "first-release-date": "1979",
             "artist-credit": [{"name": "Someone Else"}]},
            {"score": 40, "title": "Blue Monday", "first-release-date": "1970",
             "artist-credit": [{"name": "New Order"}]}
        ]});
        let f = read_search(&json, "New Order", "Blue Monday (2026 Extended Mix)");
        assert_eq!((f.original, f.this_version), (Some(1983), Some(2026)));
        let dir = std::env::temp_dir().join(format!("patch-years-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp3");
        std::fs::write(&file, vec![3u8; 90]).unwrap();
        let db = Library::in_memory().unwrap();
        let (id, _) = db.import(&file, |_| None).unwrap();
        assert_eq!(db.years_from_tags().unwrap(), 1);
        assert_eq!(
            db.years_progress(),
            (0, 0),
            "tracks without a title or artist cannot be looked up"
        );
        db.db
            .execute(
                "UPDATE tracks SET artist = ?1, title = ?2 WHERE id = ?3",
                rusqlite::params!["New Order", "Blue Monday (2026 Extended Mix)", id],
            )
            .unwrap();
        db.set_years_found(id, &f, 5).unwrap();
        let y = db.years();
        assert_eq!(y.get(file.to_str().unwrap()), Some(&(Some(1983), Some(2026))));
        assert_eq!(db.years_progress(), (1, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
