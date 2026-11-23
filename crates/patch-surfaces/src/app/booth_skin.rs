//! The Club Booth's photo skin: the user's own photos of a player and a
//! mixer, prepared by `tools/booth-skin/club.py` into
//! `$XDG_DATA_HOME/omarchy-patch/booths/club`. They never ship with Patch;
//! without them the Club Booth draws its own gear.

use super::*;
use std::path::PathBuf;

/// The knob sprites the booth turns, in `BoothSkin.knobs` order: HI, HI MID
/// (the engine's mid), LOW and FILTER of photo channels 2 and 3, then
/// master, phones mix and level, Beat FX type and depth; finally channels
/// 1 and 4 (C/D), then input trims of channels 1–4. New sprites are
/// appended to retain the existing local skin indices.
const KNOBS: [&str; 25] = [
    "hi-1", "himid-1", "low-1", "filter-1", "hi-2", "himid-2", "low-2", "filter-2", "master",
    "hp-mix", "hp-level", "fx-type", "fx-depth",
    "hi-0", "himid-0", "low-0", "filter-0", "hi-3", "himid-3", "low-3", "filter-3",
    "trim-0", "trim-1", "trim-2", "trim-3",
];

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("omarchy-patch").join("booths").join("club"))
}

/// Loads the skin into a Club Booth window; leaves it drawn when any part
/// is missing.
pub(super) fn bind(w: &PatchWindow) {
    let Some(dir) = dir() else { return };
    let load = |name: &str| slint::Image::load_from_path(&dir.join(name)).ok();
    let (Some(player), Some(player_lit), Some(mixer), Some(mixer_lit), Some(cap_tempo), Some(cap_fader)) = (
        load("player.png"),
        load("player-lit.png"),
        load("mixer.png"),
        load("mixer-lit.png"),
        load("cap-tempo.png"),
        load("cap-fader.png"),
    ) else {
        return;
    };
    let Some(knobs) = KNOBS
        .iter()
        .map(|k| load(&format!("knob-{k}.png")))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let table: serde_json::Value = std::fs::read_to_string(dir.join("skin.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let angles: Vec<f32> = KNOBS
        .iter()
        .map(|k| table["angles"][k].as_f64().unwrap_or(0.0) as f32)
        .collect();
    let s = w.global::<BoothSkin>();
    s.set_player(player);
    s.set_player_lit(player_lit);
    s.set_mixer(mixer);
    s.set_mixer_lit(mixer_lit);
    s.set_cap_tempo(cap_tempo);
    s.set_cap_fader(cap_fader);
    s.set_knobs(ModelRc::new(VecModel::from(knobs)));
    s.set_angles(ModelRc::new(VecModel::from(angles)));
    s.set_ready(true);
}
