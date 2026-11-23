//! Derived Material references: what consumers (Pattern, Deck, the DJ
//! Booth, the AI director, the Sequencer) hold to name a piece of derived
//! material or a temporary kit. The Derived Material service
//! (`patch-material`) owns the assets; everyone else holds these ids.
//!
//! An id is content-addressed: the hash of the asset's recipe (source
//! fingerprint, stem, exact region, transformations, analysis version). The
//! same recipe always gets the same id, so an id saved in a session can be
//! re-derived after its cache file is gone.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A derived asset: `dm-` and 16 hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(pub String);

/// A temporary kit: named slots, each an asset.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KitId(pub String);

impl AssetId {
    pub fn valid(&self) -> bool {
        self.0.len() == 19
            && self.0.starts_with("dm-")
            && self.0[3..].bytes().all(|b| b.is_ascii_hexdigit())
    }
}

impl KitId {
    /// Letters, digits, `-`, `_`; 1 … 48 long.
    pub fn valid(&self) -> bool {
        (1..=48).contains(&self.0.len())
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for KitId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a sample event plays: one asset, or a slot of a kit.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleSource {
    Asset(AssetId),
    Kit(KitId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_checked() {
        assert!(AssetId("dm-0123456789abcdef".into()).valid());
        assert!(!AssetId("dm-xyz".into()).valid());
        assert!(!AssetId("/tmp/a.wav".into()).valid());
        assert!(KitId("transition-1".into()).valid());
        assert!(!KitId("../etc".into()).valid());
    }
}
