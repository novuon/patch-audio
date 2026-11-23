//! Temporary kits: named slots of derived assets for one performance.
//!
//! A kit holds *references* ([`AssetId`]), never audio. Pattern's `Sample`
//! events play a kit slot by name; the service resolves the slot to the asset
//! and the sampler slot. A kit saved in a session is re-derivable because the
//! ids are recipes.

use patch_core::material::{AssetId, KitId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A kit's lifetime: a transition, a set, the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One transition (freed when the mix moves on).
    Transition,
    /// One DJ set.
    Set,
    /// The whole session.
    Session,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemporaryKit {
    pub id: KitId,
    /// Slot name (the word a pattern plays) → asset id.
    pub slots: BTreeMap<String, AssetId>,
    pub scope: Scope,
    /// Unix seconds.
    pub created: u64,
}

impl TemporaryKit {
    pub fn new(id: KitId, scope: Scope, created: u64) -> TemporaryKit {
        TemporaryKit {
            id,
            slots: BTreeMap::new(),
            scope,
            created,
        }
    }

    /// Fill a slot. Refuses the kit's own id as a slot name (it would resolve
    /// to nothing) and non-valid asset ids.
    pub fn set(&mut self, slot: impl Into<String>, asset: AssetId) -> Result<(), String> {
        let slot = slot.into();
        if slot.is_empty() || slot.len() > 64 {
            return Err("a kit slot is 1 … 64 characters".into());
        }
        if !asset.valid() {
            return Err(format!("{asset} is not a derived-material asset id"));
        }
        self.slots.insert(slot, asset);
        Ok(())
    }

    pub fn get(&self, slot: &str) -> Option<&AssetId> {
        self.slots.get(slot)
    }

    /// Everything this kit references (for cache consumer lists).
    pub fn assets(&self) -> impl Iterator<Item = &AssetId> {
        self.slots.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kit_holds_valid_asset_references() {
        let mut k = TemporaryKit::new(KitId("transition-1".into()), Scope::Transition, 0);
        k.set("kick", AssetId("dm-0123456789abcdef".into()))
            .unwrap();
        assert_eq!(k.get("kick"), Some(&AssetId("dm-0123456789abcdef".into())));
        assert!(k.set("bad", AssetId("nope".into())).is_err());
        assert!(k.set("", AssetId("dm-0123456789abcdef".into())).is_err());
        assert_eq!(k.assets().count(), 1);
    }
}
