//! In-memory search over the library: text (every word must match title,
//! artist, album, genre, path or a tag), BPM range (half/double time
//! included on request), Camelot key or harmonic compatibility, energy,
//! rating, tags, format; sorted as asked. Rebuilt from the table on change.

use crate::Track;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub bpm: Option<(f64, f64)>,
    /// Match half and double time too (a 87 BPM track fits 174).
    #[serde(default)]
    pub bpm_halves: bool,
    /// Exact Camelot key, e.g. "8A".
    #[serde(default)]
    pub key: Option<String>,
    /// Harmonically compatible with this Camelot key.
    #[serde(default)]
    pub compatible_with: Option<String>,
    #[serde(default)]
    pub energy: Option<(f64, f64)>,
    #[serde(default)]
    pub min_rating: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub include_missing: bool,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Title,
    Artist,
    Bpm,
    Key,
    Added,
    Played,
}

fn camelot(k: &str) -> Option<(u8, bool)> {
    let (n, l) = k.split_at(k.len().checked_sub(1)?);
    Some((
        n.parse().ok()?,
        match l {
            "A" | "a" => true,
            "B" | "b" => false,
            _ => return None,
        },
    ))
}

/// Same key, ±1 on the same ring, or the relative major/minor.
pub fn compatible(a: &str, b: &str) -> bool {
    let (Some((na, ma)), Some((nb, mb))) = (camelot(a), camelot(b)) else {
        return false;
    };
    let step = (na as i32 - nb as i32).rem_euclid(12);
    if ma == mb {
        step == 0 || step == 1 || step == 11
    } else {
        step == 0
    }
}

pub struct Index {
    tracks: Vec<Track>,
    /// Lower-cased searchable text per track.
    hay: Vec<String>,
    title_keys: Vec<String>,
    artist_keys: Vec<String>,
    ids: std::collections::HashMap<i64, usize>,
    paths: std::collections::HashMap<String, usize>,
}

impl Index {
    pub fn new(tracks: Vec<Track>) -> Index {
        let hay = tracks
            .iter()
            .map(|t| {
                format!(
                    "{} {} {} {} {} {}",
                    t.title,
                    t.artist,
                    t.album,
                    t.genre,
                    t.path,
                    t.tags.join(" ")
                )
                .to_lowercase()
            })
            .collect();
        let title_keys = tracks.iter().map(|t| t.title.to_lowercase()).collect();
        let artist_keys = tracks.iter().map(|t| t.artist.to_lowercase()).collect();
        let mut ids = std::collections::HashMap::with_capacity(tracks.len());
        let mut paths = std::collections::HashMap::with_capacity(tracks.len());
        for (i, t) in tracks.iter().enumerate() {
            // Preserve the old first-match behavior even for duplicate inputs.
            ids.entry(t.id).or_insert(i);
            paths.entry(t.path.clone()).or_insert(i);
        }
        Index {
            tracks,
            hay,
            title_keys,
            artist_keys,
            ids,
            paths,
        }
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn get(&self, id: i64) -> Option<&Track> {
        self.ids.get(&id).map(|&i| &self.tracks[i])
    }

    pub fn by_path(&self, path: &str) -> Option<&Track> {
        self.paths.get(path).map(|&i| &self.tracks[i])
    }

    pub fn search(&self, f: &Filter) -> Vec<&Track> {
        let words: Vec<String> = f
            .text
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        let bpm_ok = |t: &Track| match (f.bpm, t.bpm) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some((lo, hi)), Some(b)) => {
                let m: &[f64] = if f.bpm_halves {
                    &[0.5, 1.0, 2.0]
                } else {
                    &[1.0]
                };
                m.iter().any(|k| (lo..=hi).contains(&(b * k)))
            }
        };
        let mut out: Vec<usize> = self
            .tracks
            .iter()
            .zip(&self.hay)
            .enumerate()
            .filter(|(_, (t, h))| {
                (f.include_missing || !t.missing)
                    && words.iter().all(|w| h.contains(w.as_str()))
                    && bpm_ok(t)
                    && f.key
                        .as_ref()
                        .is_none_or(|k| t.key.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(k)))
                    && f.compatible_with
                        .as_ref()
                        .is_none_or(|k| t.key.as_deref().is_some_and(|x| compatible(x, k)))
                    && f.energy
                        .is_none_or(|(lo, hi)| t.energy.is_some_and(|e| (lo..=hi).contains(&e)))
                    && t.rating >= f.min_rating
                    && f.tags
                        .iter()
                        .all(|tag| t.tags.iter().any(|x| x.eq_ignore_ascii_case(tag)))
            })
            .map(|(i, _)| i)
            .collect();
        match f.sort {
            Sort::Title => out.sort_by(|&a, &b| self.title_keys[a].cmp(&self.title_keys[b])),
            Sort::Artist => out.sort_by(|&a, &b| {
                (&self.artist_keys[a], &self.title_keys[a])
                    .cmp(&(&self.artist_keys[b], &self.title_keys[b]))
            }),
            Sort::Bpm => out.sort_by(|&a, &b| {
                self.tracks[a]
                    .bpm
                    .unwrap_or(0.0)
                    .total_cmp(&self.tracks[b].bpm.unwrap_or(0.0))
            }),
            Sort::Key => out.sort_by(|&a, &b| self.tracks[a].key.cmp(&self.tracks[b].key)),
            Sort::Added => out.sort_by_key(|&i| std::cmp::Reverse(self.tracks[i].added_at)),
            Sort::Played => out.sort_by_key(|&i| std::cmp::Reverse(self.tracks[i].last_played)),
        }
        if let Some(n) = f.limit {
            out.truncate(n);
        }
        out.into_iter().map(|i| &self.tracks[i]).collect()
    }
}

/// Parse a search box into a filter: free words plus `bpm:120-128`,
/// `bpm:124` (±3 %), `key:8A`, `fits:8A`, `rating:4`, `#tag`, `energy:0.6-1`,
/// `sort:bpm`.
pub fn parse_query(q: &str) -> Filter {
    let mut f = Filter::default();
    let mut words = vec![];
    for w in q.split_whitespace() {
        let range = |v: &str| -> Option<(f64, f64)> {
            match v.split_once('-') {
                Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
                None => v.parse::<f64>().ok().map(|x| (x * 0.97, x * 1.03)),
            }
        };
        if let Some(v) = w.strip_prefix("bpm:") {
            f.bpm = range(v);
            f.bpm_halves = true;
        } else if let Some(v) = w.strip_prefix("key:") {
            f.key = Some(v.to_uppercase());
        } else if let Some(v) = w.strip_prefix("fits:") {
            f.compatible_with = Some(v.to_uppercase());
        } else if let Some(v) = w.strip_prefix("rating:") {
            f.min_rating = v.parse().unwrap_or(0);
        } else if let Some(v) = w.strip_prefix("energy:") {
            f.energy = range(v);
        } else if let Some(v) = w.strip_prefix("sort:") {
            f.sort = match v {
                "artist" => Sort::Artist,
                "bpm" => Sort::Bpm,
                "key" => Sort::Key,
                "added" => Sort::Added,
                "played" => Sort::Played,
                _ => Sort::Title,
            };
        } else if let Some(v) = w.strip_prefix('#') {
            f.tags.push(v.to_string());
        } else {
            words.push(w);
        }
    }
    f.text = words.join(" ");
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: i64, title: &str, bpm: f64, key: &str) -> Track {
        Track {
            id,
            title: title.into(),
            bpm: Some(bpm),
            key: Some(key.into()),
            ..Default::default()
        }
    }

    #[test]
    fn normalized_sorting_preserves_ties_limits_and_first_lookup() {
        let mut a = t(1, "zULu", 125.0, "8A");
        a.artist = "B".into();
        a.path = "same".into();
        let mut b = t(2, "alpha", 120.0, "7A");
        b.artist = "a".into();
        b.path = "second".into();
        let mut c = t(1, "ALPHA", 130.0, "9A");
        c.artist = "A".into();
        c.path = "same".into();
        let ix = Index::new(vec![a, b, c]);
        for sort in [Sort::Title, Sort::Artist] {
            let result = ix.search(&Filter {
                sort,
                limit: Some(2),
                ..Default::default()
            });
            assert_eq!(
                result.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
                ["alpha", "ALPHA"]
            );
        }
        assert_eq!(ix.get(1).unwrap().title, "zULu");
        assert_eq!(ix.by_path("same").unwrap().title, "zULu");
        assert!(ix.by_path("absent").is_none());
    }

    #[test]
    fn filters_combine() {
        let ix = Index::new(vec![
            t(1, "Night Ferry", 122.0, "8B"),
            t(2, "Salt & Static", 174.0, "10A"),
            t(3, "Harbour Lights", 125.5, "9A"),
            t(4, "Slow Signal", 87.0, "8A"),
        ]);
        let ids = |q: &str| {
            ix.search(&parse_query(q))
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("night"), [1]);
        assert_eq!(ids("bpm:120-128 sort:bpm"), [1, 3]);
        assert_eq!(ids("bpm:174 sort:bpm"), [4, 2], "half time included");
        assert_eq!(ids("fits:8A sort:bpm"), [4, 1, 3]);
        assert_eq!(ids("fits:8A bpm:120-130 sort:bpm"), [1, 3]);
    }

    #[test]
    fn ten_thousand_tracks_search_fast() {
        let tracks: Vec<Track> = (0..10_000)
            .map(|i| {
                let mut x = t(
                    i,
                    &format!(
                        "Track {i} {}",
                        ["deep", "tech", "house", "garage"][i as usize % 4]
                    ),
                    100.0 + (i % 80) as f64,
                    &format!("{}A", i % 12 + 1),
                );
                x.artist = format!("Artist {}", i % 300);
                x
            })
            .collect();
        let ix = Index::new(tracks);
        let f = parse_query("tech bpm:120-130");
        let t0 = std::time::Instant::now();
        let n = ix.search(&f).len();
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        assert!(n > 0);
        assert!(ms < 5.0, "search took {ms:.2} ms");
    }
}
