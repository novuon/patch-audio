//! Domain model and provider traits.
//!
//! Patch's surfaces only ever talk to an `AudioProvider`: the simulated one in
//! `patch-mock` or the live `PipeWireAudioProvider` in `patch-pipewire`. Providers declare their [`Provenance`] so the UI
//! can mark simulated data without guessing.
//!
//! Providers are deliberately snapshot + command shaped rather than handing out
//! references into live state: that keeps the door open for providers that are
//! thin clients of an out-of-process engine (the audio engine must survive UI
//! crashes).

pub mod audio;
pub mod channels;
pub mod timing;

/// Where the data behind a provider comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Generated or canned data. The UI must label it MOCK / SIMULATED.
    Simulated,
    /// Real system data, read-only.
    Live,
}

impl Provenance {
    pub fn is_simulated(self) -> bool {
        self == Provenance::Simulated
    }
}
