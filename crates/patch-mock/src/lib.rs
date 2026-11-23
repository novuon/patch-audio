//! Mock providers. Every value produced here is simulated; every provider
//! reports [`Provenance::Simulated`](patch_model::Provenance::Simulated).

mod audio;
mod noise;

pub use audio::MockAudioProvider;
