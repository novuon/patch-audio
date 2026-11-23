//! Audio Unit. Every value shown comes from an [`AudioProvider`]; today that is
//! [`MockAudioProvider`]. The UI never talks to audio APIs directly.

mod aidj;
pub mod num;
mod console_fit;
mod graph_layout;
mod aivis;
mod analyzer;
mod app;
pub mod budget;
mod diag;
mod dj;
mod dj_workspace;
mod eq_view;
mod inspect;
mod law;
mod library_place;
mod library_worker;
mod meter_style;
mod metering;
mod mission;
mod overview;
pub mod perf;
mod perf_view;
pub mod player;
mod plugins;
mod poll;
mod portal;
mod record_view;
mod remote_track;
mod broadcast_view;
mod broadcast_performer;
mod scope;
pub mod spec;
#[cfg(test)]
mod keymap_tests;
mod visual;
mod visual_control;
mod visual_gen;
mod visual_gl;
mod visual_layers;

pub mod ui {
    slint::include_modules!();
}

pub use app::AudioApp;
pub use dj::clock as clock_text;
pub use dj::music_dir;
pub use spec::spec;

/// "about 1 h 10 min" for n tracks at s seconds each ("" unknown).
pub fn library_eta(n: usize, per_track_s: f64) -> String {
    let secs = (n as f64 * per_track_s).round() as u64;
    if n == 0 || per_track_s <= 0.0 {
        String::new()
    } else if secs < 90 {
        "about a minute".into()
    } else if secs < 3600 {
        format!("about {} min", secs.div_ceil(60))
    } else {
        format!("about {} h {} min", secs / 3600, (secs % 3600) / 60)
    }
}

mod stems;
