//! The Derived Material data model: what is asked for, what it resolves to,
//! and the recipe that gives an asset its identity.
//!
//! Everything here is serde data: a recipe saved in a session is enough to
//! re-derive its audio on any machine, because the id is a hash of exactly
//! this data (see [`Recipe::id`]).

use patch_core::material::AssetId;
use serde::{Deserialize, Serialize};

/// The model version. Bump only when the recipe's meaning changes, so old
/// recipes stop resolving to the same id and are re-derived.
pub const VERSION: u32 = 1;

/// What a piece of derived material is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    VocalPhrase,
    VocalChop,
    Kick,
    Snare,
    Hat,
    Percussion,
    BassFragment,
    MelodicFragment,
    HarmonicTexture,
    BeatLoop,
    BarLoop,
    PhraseLoop,
    TransitionFx,
    GranularSource,
}

/// Where an asset comes from, and how to prove it: the file's fingerprint
/// (the stem cache's SHA-256 source hash) is part of the identity, so an asset
/// whose source changed is never rebuilt from the new file under the old id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub track_id: String,
    pub path: String,
    /// SHA-256 of the source file, lowercase hex.
    pub fingerprint: String,
    /// A single stem (e.g. "vocals"), when the material is not the full mix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<String>,
    /// The stem set that stem's cache belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem_set: Option<String>,
}

/// Which phrase (of the track's 32-beat phrases) to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phrase {
    /// 0-based phrase index from the track's start.
    Index(i64),
    Last,
    /// The phrase that ends at the drop.
    BeforeDrop,
    /// The first phrase after the drop.
    AfterDrop,
}

/// A musical region, not seconds. `beats` shrinks the region to a beat
/// sub-range (`(from, to)`, 0-based inside the region, half-open).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "of", rename_all = "snake_case")]
pub enum RegionSpec {
    Phrase {
        which: Phrase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        beats: Option<(f64, f64)>,
    },
    Bars {
        bar: i64,
        count: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        beats: Option<(f64, f64)>,
    },
    Beats {
        beat: i64,
        count: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        beats: Option<(f64, f64)>,
    },
    Section {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        beats: Option<(f64, f64)>,
    },
    /// The escape hatch: exact source seconds.
    Seconds { start: f64, end: f64 },
}

/// A region resolved to exact source frames, with the evidence for it.
///
/// `start_frame`/`end_frame` are at `rate` and rounded from beat times once;
/// the audio path reads exactly these frames. `why` records how it was
/// resolved and `confidence` how sure the grid made us (0…1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolved {
    pub rate: u32,
    pub start_frame: u64,
    pub end_frame: u64,
    /// Source seconds of the first beat of the region.
    pub first_beat: f64,
    /// Beats in the region.
    pub beats: f64,
    /// 0-based bar the region starts on.
    pub bar: i64,
    /// 0-based phrase the region starts on, when the grid defines one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase: Option<i64>,
    pub source_bpm: Option<f64>,
    pub source_key: Option<String>,
    pub confidence: f32,
    /// Human-readable evidence, e.g. "phrase 3 of 8 (last before the drop)".
    pub why: String,
}

impl Resolved {
    pub fn frames(&self) -> u64 {
        self.end_frame.saturating_sub(self.start_frame)
    }

    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / (self.rate.max(1) as f64)
    }
}

/// A transform is data, applied in order when rendering. Stretch and pitch
/// need the engine's Rubber Band: if it is absent they are refused, never
/// faked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Transform {
    /// Cut to an exact length (seconds); the rest is dropped.
    Trim {
        seconds: f64,
    },
    /// Move the region's start to the nearest strong onset within `max_ms`.
    SnapTransient {
        max_ms: f64,
    },
    /// Raised-cosine fade in/out.
    Fade {
        in_ms: f64,
        out_ms: f64,
    },
    Reverse,
    Gain {
        db: f64,
    },
    Normalize {
        peak_db: f64,
    },
    /// Equal-power crossfade of the tail into the head, so a loop seam is
    /// smooth when the buffer repeats.
    LoopSeam {
        crossfade_ms: f64,
    },
    Filter {
        kind: FilterKind,
        hz: f64,
    },
    Envelope {
        a: f64,
        d: f64,
        s: f64,
        r: f64,
    },
    /// Resample by semitones (changes both speed and pitch).
    Repitch {
        semitones: f64,
    },
    /// Time-stretch to a target tempo (needs [`crate::Stretch`]).
    TimeStretch {
        to_bpm: f64,
    },
    /// Shift pitch without changing tempo (needs [`crate::Stretch`]).
    PitchShift {
        semitones: f64,
    },
    /// Hold the material, per `beats`, without changing gain.
    Freeze {
        beats: f64,
    },
    /// Repeat a slice of the material `repeats` times.
    Stutter {
        division: f64,
        repeats: u32,
    },
    /// Slice into `n` equal parts (the material itself keeps its order).
    Slices {
        n: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    LowPass,
    HighPass,
}

/// The identity of an asset: source, exact region, kind and transforms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub version: u32,
    pub source: SourceRef,
    pub region: Resolved,
    pub kind: Kind,
    pub transforms: Vec<Transform>,
    /// The analysis generation the region was resolved against.
    pub analysis_version: u32,
}

/// A canonical projection of the recipe: everything that defines the audio,
/// and nothing that merely describes how it was found (`why`) or how sure we
/// were (`confidence`) — so a wrinkle in the wording never mints a new id.
#[derive(Serialize)]
struct CanonicalRecipe<'a> {
    version: u32,
    source: &'a SourceRef,
    rate: u32,
    start_frame: u64,
    end_frame: u64,
    kind: &'a Kind,
    transforms: &'a [Transform],
    analysis_version: u32,
}

/// Marker type re-exported for callers that want the same canonical JSON.
pub struct Canonical;

impl Recipe {
    /// The asset id: `dm-` + 16 hex of FNV-1a 64 over the recipe's canonical
    /// JSON. Deterministic, and independent of `why`/`confidence`.
    pub fn id(&self) -> AssetId {
        AssetId(format!(
            "dm-{:016x}",
            fnv1a64(self.canonical_json().as_bytes())
        ))
    }

    /// The canonical JSON the id hashes.
    pub fn canonical_json(&self) -> String {
        let c = CanonicalRecipe {
            version: self.version,
            source: &self.source,
            rate: self.region.rate,
            start_frame: self.region.start_frame,
            end_frame: self.region.end_frame,
            kind: &self.kind,
            transforms: &self.transforms,
            analysis_version: self.analysis_version,
        };
        serde_json::to_string(&c).unwrap_or_default()
    }
}

/// FNV-1a, 64-bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// How long an asset may stay in the cache without references.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum Policy {
    /// Evictable as soon as no consumer references it.
    #[default]
    WhileReferenced,
    /// Evictable at the end of the session (never evicted mid-session).
    Session,
    /// Evictable `seconds` after its last use.
    Reuse { seconds: f64 },
    /// Kept: expensive to regenerate (evicted only under hard pressure).
    Keep,
}

/// A quality verdict: a score and the reasons for it. A poor asset says so.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quality {
    pub score: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Measured metadata: what the rendered audio actually is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub frames: u64,
    pub duration: f64,
    pub peak: f32,
    pub rms: f32,
    /// Onset offsets in seconds from the start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transients: Vec<f64>,
    /// 0…1: how well the buffer loops (seam discontinuity, if any).
    pub loop_suitability: f32,
    /// Estimated bleed of other stems, when the material is a stem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bleed: Option<f32>,
    pub quality: Quality,
    pub result_bpm: Option<f64>,
    pub result_key: Option<String>,
    /// Unix seconds.
    pub created: u64,
    pub cost_ms: f64,
    pub policy: Policy,
    /// Who holds this asset (references released on demand).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumers: Vec<String>,
}

/// A derived asset: id, the recipe that made it, and what it measured out to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub recipe: Recipe,
    pub meta: Meta,
}

impl Asset {
    pub fn new(recipe: Recipe, meta: Meta) -> Asset {
        Asset {
            id: recipe.id(),
            recipe,
            meta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceRef {
        SourceRef {
            track_id: "track-a".into(),
            path: "/music/a.flac".into(),
            fingerprint: "0".repeat(64),
            stem: Some("vocals".into()),
            stem_set: Some("set-1".into()),
        }
    }

    fn resolved() -> Resolved {
        Resolved {
            rate: 48_000,
            start_frame: 48_000,
            end_frame: 96_000,
            first_beat: 1.0,
            beats: 2.0,
            bar: 1,
            phrase: Some(3),
            source_bpm: Some(126.0),
            source_key: Some("A minor".into()),
            confidence: 0.9,
            why: "phrase 3 (last before the drop)".into(),
        }
    }

    fn recipe() -> Recipe {
        Recipe {
            version: VERSION,
            source: source(),
            region: resolved(),
            kind: Kind::VocalPhrase,
            transforms: vec![Transform::Fade {
                in_ms: 2.0,
                out_ms: 2.0,
            }],
            analysis_version: 7,
        }
    }

    #[test]
    fn ids_are_valid_and_stable() {
        let r = recipe();
        let id = r.id();
        assert!(id.valid(), "{id}");
        assert_eq!(id, recipe().id());
        // A different transform is a different asset.
        let mut other = recipe();
        other.transforms.push(Transform::Reverse);
        assert_ne!(id, other.id());
    }

    #[test]
    fn id_is_independent_of_wording_and_confidence() {
        // `why` and `confidence` are description, not audio: they must not
        // mint a new id (or a cache would miss whenever an analysis wording
        // changed).
        let a = recipe();
        let mut b = recipe();
        b.region.why = "a completely different explanation".into();
        b.region.confidence = 0.11;
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn a_changed_source_fingerprint_is_a_different_asset() {
        let a = recipe();
        let mut b = recipe();
        b.source.fingerprint = "f".repeat(64);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn recipes_round_trip_through_json() {
        let r = recipe();
        let text = serde_json::to_string(&r).unwrap();
        let back: Recipe = serde_json::from_str(&text).unwrap();
        assert_eq!(r, back);
        assert_eq!(r.id(), back.id());
    }
}
