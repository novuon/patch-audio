//! Derived Material: musical material cut from tracks Patch already knows.
//!
//! Patch derives temporary material on demand ("a two-beat vocal from deck A's
//! phrase before the drop", "a one-bar percussion loop") and serves it to
//! Pattern, the Deck, the DJ Booth, the Sampler and the AI director through typed ids
//! ([`patch_core::material`]). Nothing here is Pattern: Pattern only consumes
//! [`patch_core::material::AssetId`] and `KitId`.
//!
//! The shape is always the same:
//!
//! * a **request** is musical (a phrase, a bar, beats, a section) — never raw
//!   seconds except as an explicit escape hatch;
//! * [`resolve`] turns it into exact source frames from the track's analysis
//!   (the beat grid, downbeat and structure), and says *why* and *how sure*;
//! * a **recipe** (source fingerprint + region + kind + transforms) is the
//!   asset's identity: the same recipe is the same id on any machine;
//! * [`dsp`] applies the transforms in order when rendering — pure Rust, no
//!   engine, no audio thread;
//! * [`cache`] keeps rendered audio and metadata under a budget, with the
//!   consumer references that make an asset evictable.
//!
//! Audio and stretching come in through traits ([`traits::SourceAudio`],
//! [`traits::Stretch`]) so the engine can supply the real PipeWire graph rate
//! and Rubber Band while tests use synthetic audio; a transform the engine
//! cannot perform is refused, never faked.

pub mod address;
pub mod cache;
pub mod dsp;
pub mod kit;
pub mod model;
pub mod query;
pub mod resolve;
pub mod service;
pub mod traits;

#[cfg(test)]
pub(crate) mod testutil;

pub use model::{
    Asset, Canonical, Kind, Meta, Policy, Quality, Recipe, RegionSpec, Resolved, SourceRef,
    Transform,
};
pub use traits::{SourceAudio, Stretch};
