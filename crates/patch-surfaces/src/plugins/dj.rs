//! DJ booth unit. Deck A, Deck B, the DJ mixer and the library are
//! implemented (see `crate::dj`); waveforms and the sampler are still planned.

use super::Manifest;
use patch_core::{SurfaceSpec, UnitKind};

pub const PLUGIN: &str = "DJ booth";

pub fn manifest() -> Manifest {
    let mut surfaces = vec![
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "deck-a", "Deck A").min(520, 440),
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "deck-b", "Deck B").min(520, 440),
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "deck-c", "Deck C").min(520, 440),
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "deck-d", "Deck D").min(520, 440),
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "dj-mixer", "Crossfader").min(380, 520),
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "dj", "DJ Booth").min(560, 440),
        // Decks A and B, their channels, the crossfader and the AI DJ's set
        // in one short, wide window for a small set.
        SurfaceSpec::unit(UnitKind::DjBooth, PLUGIN, "dj-mini", "Mini Booth").min(760, 220),
        SurfaceSpec::unit(UnitKind::Library, PLUGIN, "library", "Library")
            .multi()
            .min(520, 360),
        SurfaceSpec::planned(UnitKind::DjBooth, PLUGIN, "waveforms", "Waveforms"),
        SurfaceSpec::planned(UnitKind::DjBooth, PLUGIN, "sampler", "Sampler"),
    ];
    // The same decks as club gear: two players and a four-channel mixer
    // (no letter key of its own: opened from the controller). Not in
    // tester kits (scripts/build-kit.sh sets PATCH_KIT): without the
    // user's photos it draws its own gear, which isn't ready to show.
    if option_env!("PATCH_KIT").is_none() {
        surfaces.insert(
            4,
            SurfaceSpec::unit(UnitKind::ClubBooth, PLUGIN, "dj-club", "Club Booth").min(1180, 600),
        );
    }
    Manifest {
        surfaces,
        // The DJ presets live with the others (spec.rs).
        layouts: vec![],
    }
}
