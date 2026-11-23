//! Semantic sample addresses: how a pattern names material that doesn't
//! exist yet.
//!
//! `derived:<deck>:<stem>:<region>[:<sub-range>][:as-<kind>]`, for example
//! `derived:deck-a:vocals:phrase-last:beats-2` ("the first two beats of deck
//! A's last vocal phrase"). An address is musical, never seconds; the engine
//! turns it into a [`Recipe`] with the deck's track and analysis, and the
//! recipe's id is what the pattern plays.
//!
//! * deck: `a`…`d` or `deck-a`…`deck-d`
//! * stem: `vocals`, `drums`, `bass`, `other`, or `mix` (the full track)
//! * region: `phrase-last`, `phrase-<n>`, `before-drop`, `after-drop`,
//!   `bar-<n>`, `bars-<n>-<count>`, `beat-<n>`, `beats-<n>-<count>`,
//!   `section-<kind>`
//! * sub-range: `beats-<count>` (from the region's start) or
//!   `beats-<from>-<to>`
//! * kind: inferred from stem and length, or given (`as-bar_loop`)

use crate::model::{Kind, Phrase, Recipe, RegionSpec, SourceRef, Transform, VERSION};
use patch_analysis::Analysis;

pub const PREFIX: &str = "derived:";

#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    /// Deck index (A = 0).
    pub deck: usize,
    /// `None`: the full mix.
    pub stem: Option<String>,
    pub region: RegionSpec,
    pub kind: Kind,
}

fn int(s: &str, what: &str) -> Result<i64, String> {
    s.parse().map_err(|_| format!("'{s}' is not a {what}"))
}

fn region(word: &str) -> Result<RegionSpec, String> {
    let parts: Vec<&str> = word.split('-').collect();
    let phrase = |which| RegionSpec::Phrase { which, beats: None };
    Ok(match parts.as_slice() {
        ["phrase", "last"] => phrase(Phrase::Last),
        ["phrase", "before", "drop"] | ["before", "drop"] => phrase(Phrase::BeforeDrop),
        ["phrase", "after", "drop"] | ["after", "drop"] => phrase(Phrase::AfterDrop),
        ["phrase", n] => phrase(Phrase::Index(int(n, "phrase number")?)),
        ["bar", n] => RegionSpec::Bars {
            bar: int(n, "bar")?,
            count: 1,
            beats: None,
        },
        ["bars", n, c] => RegionSpec::Bars {
            bar: int(n, "bar")?,
            count: int(c, "bar count")?,
            beats: None,
        },
        ["beat", n] => RegionSpec::Beats {
            beat: int(n, "beat")?,
            count: 1,
            beats: None,
        },
        ["beats", n, c] => RegionSpec::Beats {
            beat: int(n, "beat")?,
            count: int(c, "beat count")?,
            beats: None,
        },
        ["section", kind @ ..] if !kind.is_empty() => RegionSpec::Section {
            kind: kind.join("-"),
            beats: None,
        },
        _ => {
            return Err(format!(
                "'{word}' is not a region (phrase-last, bar-4, before-drop, …)"
            ));
        }
    })
}

fn sub_range(word: &str) -> Option<Result<(f64, f64), String>> {
    let rest = word.strip_prefix("beats-")?;
    let nums: Vec<&str> = rest.split('-').collect();
    Some(match nums.as_slice() {
        [n] => int(n, "beat count").map(|n| (0.0, n as f64)),
        [a, b] => int(a, "beat").and_then(|a| Ok((a as f64, int(b, "beat")? as f64))),
        _ => Err(format!("'{word}' is not a beat range (beats-2, beats-4-6)")),
    })
}

/// Beats the region spans, when the address alone says (for inferring a kind).
fn beats_of(region: &RegionSpec) -> Option<f64> {
    let sub = |b: &Option<(f64, f64)>| b.map(|(a, z)| z - a);
    match region {
        RegionSpec::Phrase { beats, .. } | RegionSpec::Section { beats, .. } => {
            sub(beats).or(match region {
                RegionSpec::Phrase { .. } => Some(32.0),
                _ => None,
            })
        }
        RegionSpec::Bars { count, beats, .. } => sub(beats).or(Some(*count as f64 * 4.0)),
        RegionSpec::Beats { count, beats, .. } => sub(beats).or(Some(*count as f64)),
        RegionSpec::Seconds { .. } => None,
    }
}

fn infer(stem: Option<&str>, beats: Option<f64>) -> Kind {
    let b = beats.unwrap_or(32.0);
    match stem {
        Some("vocals") if b <= 2.0 => Kind::VocalChop,
        Some("vocals") => Kind::VocalPhrase,
        Some("bass") => Kind::BassFragment,
        Some("other") if b >= 16.0 => Kind::HarmonicTexture,
        Some("other") => Kind::MelodicFragment,
        // Drums and the full mix: loops by length.
        _ if b < 4.0 => Kind::BeatLoop,
        _ if b < 16.0 => Kind::BarLoop,
        _ => Kind::PhraseLoop,
    }
}

pub fn parse(address: &str) -> Result<Address, String> {
    let body = address
        .strip_prefix(PREFIX)
        .ok_or_else(|| format!("'{address}' doesn't start with '{PREFIX}'"))?;
    let parts: Vec<&str> = body.split(':').collect();
    let [deck, stem, reg, rest @ ..] = parts.as_slice() else {
        return Err(format!(
            "'{address}': derived:<deck>:<stem>:<region>[:beats-n]"
        ));
    };
    let deck = match deck.strip_prefix("deck-").unwrap_or(deck) {
        "a" => 0,
        "b" => 1,
        "c" => 2,
        "d" => 3,
        other => return Err(format!("'{other}' is not a deck (a … d)")),
    };
    let stem = match *stem {
        "mix" | "full" => None,
        "vocals" | "drums" | "bass" | "other" => Some(stem.to_string()),
        other => {
            return Err(format!(
                "'{other}' is not a stem (vocals, drums, bass, other, mix)"
            ));
        }
    };
    let mut region = region(reg)?;
    let mut kind = None;
    for word in rest {
        if let Some(k) = word.strip_prefix("as-") {
            kind = Some(
                serde_json::from_value(serde_json::Value::String(k.into()))
                    .map_err(|_| format!("'{k}' is not a material kind"))?,
            );
        } else if let Some(r) = sub_range(word) {
            let r = r?;
            if r.1 <= r.0 {
                return Err(format!("'{word}' is empty"));
            }
            match &mut region {
                RegionSpec::Phrase { beats, .. }
                | RegionSpec::Bars { beats, .. }
                | RegionSpec::Beats { beats, .. }
                | RegionSpec::Section { beats, .. } => *beats = Some(r),
                RegionSpec::Seconds { .. } => unreachable!("addresses never name seconds"),
            }
        } else {
            return Err(format!("'{word}' is neither a beat range nor as-<kind>"));
        }
    }
    let kind = kind.unwrap_or_else(|| infer(stem.as_deref(), beats_of(&region)));
    Ok(Address {
        deck,
        stem,
        region,
        kind,
    })
}

impl Address {
    /// The edits every asset of this kind gets: loops seam, one-shots snap
    /// to their transient; every edge is faded.
    pub fn transforms(&self) -> Vec<Transform> {
        match self.kind {
            Kind::BeatLoop | Kind::BarLoop | Kind::PhraseLoop => vec![
                Transform::LoopSeam { crossfade_ms: 10.0 },
                Transform::Fade {
                    in_ms: 2.0,
                    out_ms: 2.0,
                },
            ],
            _ => vec![
                Transform::SnapTransient { max_ms: 30.0 },
                Transform::Fade {
                    in_ms: 2.0,
                    out_ms: 10.0,
                },
            ],
        }
    }

    /// The recipe for this address over a track (the deck's), resolved with
    /// its analysis. `source.stem` is set from the address.
    pub fn recipe(
        &self,
        mut source: SourceRef,
        analysis: &Analysis,
        rate: u32,
    ) -> Result<Recipe, String> {
        let region = crate::resolve::resolve(&self.region, analysis, rate)?;
        source.stem = self.stem.clone();
        if source.stem.is_none() {
            source.stem_set = None;
        }
        Ok(Recipe {
            version: VERSION,
            source,
            region,
            kind: self.kind,
            transforms: self.transforms(),
            analysis_version: analysis.generation(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_address_parses() {
        let a = parse("derived:deck-a:vocals:phrase-last:beats-2").unwrap();
        assert_eq!(a.deck, 0);
        assert_eq!(a.stem.as_deref(), Some("vocals"));
        assert_eq!(
            a.region,
            RegionSpec::Phrase {
                which: Phrase::Last,
                beats: Some((0.0, 2.0))
            }
        );
        assert_eq!(a.kind, Kind::VocalChop);
    }

    #[test]
    fn regions_kinds_and_overrides() {
        let a = parse("derived:b:drums:bar-16").unwrap();
        assert_eq!((a.deck, a.kind), (1, Kind::BarLoop));
        assert_eq!(
            parse("derived:a:mix:before-drop").unwrap().kind,
            Kind::PhraseLoop
        );
        assert_eq!(
            parse("derived:a:drums:beats-8-2").unwrap().kind,
            Kind::BeatLoop
        );
        assert_eq!(
            parse("derived:a:bass:bars-4-2").unwrap().kind,
            Kind::BassFragment
        );
        let s = parse("derived:a:other:section-chorus:beats-4-8:as-granular_source").unwrap();
        assert_eq!(s.kind, Kind::GranularSource);
        assert_eq!(
            s.region,
            RegionSpec::Section {
                kind: "chorus".into(),
                beats: Some((4.0, 8.0))
            }
        );
        assert!(
            parse("derived:a:vocals:phrase-last")
                .unwrap()
                .transforms()
                .iter()
                .any(|t| matches!(t, Transform::SnapTransient { .. }))
        );
        assert!(
            parse("derived:a:drums:bar-1")
                .unwrap()
                .transforms()
                .iter()
                .any(|t| matches!(t, Transform::LoopSeam { .. }))
        );
    }

    #[test]
    fn mistakes_say_what_is_wrong() {
        for (bad, says) in [
            ("dm-0123", "doesn't start"),
            ("derived:e:vocals:bar-1", "not a deck"),
            ("derived:a:piano:bar-1", "not a stem"),
            ("derived:a:vocals:middle", "not a region"),
            ("derived:a:vocals:bar-1:beats-4-2", "empty"),
            ("derived:a:vocals:bar-1:as-banjo", "not a material kind"),
            ("derived:a:vocals", "derived:<deck>"),
        ] {
            let e = parse(bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
    }
}
