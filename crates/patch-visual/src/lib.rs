//! Visualiser Unit: our own animations played to the music.
//!
//! Everything is drawn in character cells: [`stage`] paints backdrops and
//! acts on the song's title (big block letters from [`banner`]), moving
//! with the beat, the spectrum and ttfx's easing curves (omacom/ttfx, MIT;
//! its stock effects, Omarchy's screensaver, are not used). A [`pack`] is
//! each song's look; the [`director`] picks what plays when; [`render`]
//! draws the cells as glowing glyphs at any window size.

pub mod ambient;
pub mod banner;
pub mod beat;
pub mod choreography;
pub mod director;
pub mod face;
pub mod feed;
pub mod fonts;
pub mod generative;
pub mod grid;
pub mod groove;
pub mod handover;
pub mod hydra;
pub mod isf;
pub mod pack;
pub mod performance;
pub mod render;
pub mod stage;
pub mod steering;

/// The shared semantic-intent contract (the AI director and patterns write it).
pub use patch_core::visual_intent::Intent as VisualIntent;

/// ttfx's easing curves: what our motion follows.
pub use ttfx::utils::easing::Easing;

/// Frame-time budgets are an explicit check, not a guess about the machine.
///
/// `cargo test --release` stays green on a busy or shared runner. Set
/// `PATCH_CHECK_BUDGETS=1` (release build, not debug) to enforce the
/// millisecond ceilings in `render` and `stage_tests`.
#[cfg(test)]
pub(crate) fn check_budgets() -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    std::env::var("PATCH_CHECK_BUDGETS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}
