//! The pattern instrument: the sounds a pattern names (`bd`, `superpiano`)
//! and the controls it sets (`n`, `gain`, `cutoff`, `room`) as what the
//! engine's instrument plays.
//!
//! The engine has one drum bank (Patch's own designs, the Sequencer's), a
//! handful of synths, and the sample folders ([`crate::samples`]). A folder
//! wins its name: with Dirt-Samples installed `bd` is Tidal's kick; without,
//! Tidal's usual names reach the built-in sounds: `bd`, `sn`, `hh`, `808bd`,
//! `drum:2`, `superpiano`, `supersaw`. A name Patch has no sound for is
//! refused, and said so, rather than played as something else.

use crate::ast::Value;
use crate::event::Event;
use crate::rt::RtParams;

/// Sound numbers: the drum bank's voices below, synths from here.
pub const SYNTH_BASE: u32 = 16;
/// Sample slots from here: sound `SAMPLE_BASE + slot`.
pub const SAMPLE_BASE: u32 = 1 << 20;

/// A loaded sample, as the engine holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleInfo {
    pub slot: u32,
    pub frames: u64,
    /// The file's own rate (the engine plays it at the graph's).
    pub rate: u32,
}

/// The engine's sample folders, as the instrument asks for them.
pub trait Samples {
    /// `None`: no folder of that name (a built-in sound may play). An error
    /// says why it can't play now (still loading, unreadable).
    fn sample(&self, name: &str, n: f64) -> Option<Result<SampleInfo, String>>;
}

/// No sample folders: built-in sounds only.
pub struct NoSamples;

impl Samples for NoSamples {
    fn sample(&self, _: &str, _: f64) -> Option<Result<SampleInfo, String>> {
        None
    }
}

/// A sample's trim at gain 1: Dirt-Samples are cut loud.
const SAMPLE_LEVEL: f32 = 0.8;

/// Drum names: (name, bank voice, playback rate).
pub const DRUMS: &[(&str, u32, f32)] = &[
    ("bd", 0, 1.0),
    ("kick", 0, 1.0),
    ("808bd", 0, 0.9),
    ("909", 0, 1.1),
    ("sn", 1, 1.0),
    ("sd", 1, 1.0),
    ("snare", 1, 1.0),
    ("808sd", 1, 0.95),
    ("hh", 2, 1.0),
    ("hc", 2, 1.0),
    ("ch", 2, 1.0),
    ("hat", 2, 1.0),
    ("hh27", 2, 1.1),
    ("808hc", 2, 0.95),
    ("oh", 3, 1.0),
    ("ho", 3, 1.0),
    ("808oh", 3, 0.95),
    ("cr", 3, 0.8),
    ("crash", 3, 0.8),
    ("ride", 3, 1.25),
    ("cp", 4, 1.0),
    ("clap", 4, 1.0),
    ("realclaps", 4, 0.95),
    ("808cp", 4, 0.9),
    ("lt", 5, 1.0),
    ("tom", 5, 1.0),
    ("808lt", 5, 0.95),
    ("mt", 5, 1.25),
    ("808mt", 5, 1.2),
    ("ht", 5, 1.5),
    ("808ht", 5, 1.45),
    ("rim", 6, 1.0),
    ("rs", 6, 1.0),
    ("cb", 6, 0.7),
    ("cowbell", 6, 0.7),
    ("sh", 7, 1.0),
    ("shaker", 7, 1.0),
    ("perc", 7, 1.2),
];

/// Kits whose `n` picks the voice: `drum:0` a kick … `drum:7` a shaker.
pub const KITS: &[&str] = &["drum", "808", "kit", "patch"];

/// Synths: (names, model). The first name is the one shown.
pub const SYNTHS: &[(&[&str], u32)] = &[
    (&["superpiano", "piano"], 0),
    (&["supersaw", "saw", "superzow"], 1),
    (&["supersquare", "square", "superchip", "superpwm"], 2),
    (&["supersine", "sine"], 3),
    (&["superfm", "fm"], 4),
    (&["superhammond", "organ"], 5),
    (&["supermandolin", "pluck", "arpy"], 6),
    (&["superreese", "reese", "bass"], 7),
    (&["superpad", "pad"], 8),
];

/// Synth models the engine has.
pub const MODELS: u32 = 9;

/// A sound's trim at gain 1: the eight drum voices, then the nine synths.
/// A hat is not a kick and a chord is not a snare; gain staging here keeps
/// dense patterns from piling up into the master limiter. `gain` stays the
/// player's control (and keeps Tidal's squared feel).
const DRUM_BASE: [f32; 8] = [
    1.0, 0.8, 0.28, 0.24, 0.72, 0.6, 0.5, 0.36, // kick, snare, hat, open, clap, tom, rim, shaker
];
/// The nine synths, in `SYNTHS` order.
const SYNTH_BASE_LEVEL: [f32; MODELS as usize] = [
    0.55, 0.6, 0.55, 0.7, 0.6, 0.55, 0.55, 0.6, 0.5,
];

/// A sound by name (and `n`): its number and a playback-rate factor.
pub fn sound(name: &str, n: f64) -> Option<(u32, f32)> {
    let name = name.trim();
    if let Some(&(_, voice, rate)) = DRUMS.iter().find(|d| d.0 == name) {
        return Some((voice, rate));
    }
    if KITS.contains(&name) {
        return Some(((n.max(0.0) as i64).rem_euclid(8) as u32, 1.0));
    }
    SYNTHS
        .iter()
        .find(|(names, _)| names.contains(&name))
        .map(|(_, m)| (SYNTH_BASE + m, 1.0))
}

pub fn is_synth(sound: u32) -> bool {
    (SYNTH_BASE..SAMPLE_BASE).contains(&sound)
}

pub fn is_sample(sound: u32) -> bool {
    sound >= SAMPLE_BASE
}

/// Every sound name, as shown in the help.
pub fn names() -> (Vec<&'static str>, Vec<&'static str>) {
    let drums = DRUMS.iter().map(|d| d.0).chain(KITS.iter().copied()).collect();
    let synths = SYNTHS.iter().flat_map(|(n, _)| n.iter().copied()).collect();
    (drums, synths)
}

/// Controls the instrument uses (others are carried but change nothing).
pub const USED: &[&str] = &[
    "s", "n", "note", "octave", "gain", "amp", "velocity", "pan", "speed", "cutoff", "resonance",
    "hcutoff", "attack", "release", "sustain", "legato", "shape", "crush", "delay", "delaytime",
    "delayfeedback", "room", "size", "orbit", "dry", "begin", "end", "accelerate", "cut", "unit",
    "loop", "coarse",
];

/// One event as the instrument plays it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Play {
    pub sound: u32,
    pub level: f32,
    pub params: RtParams,
}

/// The sound an event names: its value, else its `s` control.
pub fn sound_name(e: &Event) -> String {
    match &e.value {
        Value::Text(s) if !s.is_empty() => s.clone(),
        _ => e.params.get("s").map(|v| v.to_string()).unwrap_or_default(),
    }
}

/// An instrument event as what plays, with the built-in sounds only.
pub fn play(e: &Event, secs: f64) -> Result<Play, String> {
    play_with(e, secs, 0.5625, &NoSamples)
}

/// An instrument event as what plays: `secs` is the event's length in
/// seconds (a synth holds for it, unless `sustain` or `legato` say), `cps`
/// the tempo there (for `unit "c"`), `samples` the folders.
pub fn play_with(e: &Event, secs: f64, cps: f64, samples: &dyn Samples) -> Result<Play, String> {
    let name = sound_name(e);
    if name.is_empty() {
        return Err("no sound: give it one with s \"…\" (s \"bd\", s \"superpiano\")".into());
    }
    let num = |k: &str| e.param(k);
    let n = num("n").unwrap_or(0.0);
    let sample = match samples.sample(&name, n) {
        Some(Ok(info)) => Some(info),
        Some(Err(why)) => return Err(why),
        None => None,
    };
    let (sound, rate) = match sample {
        Some(info) => (SAMPLE_BASE + info.slot, 1.0),
        None => sound(&name, n).ok_or_else(|| format!("Patch has no sound '{name}'"))?,
    };
    let mut p = RtParams::default();
    let note = num("note").unwrap_or(0.0);
    let octave = num("octave").unwrap_or(5.0);
    if is_synth(sound) {
        p.note = (60.0 + n + note + (octave - 5.0) * 12.0).clamp(0.0, 127.0) as f32;
    }
    let mut speed = num("speed").unwrap_or(1.0) * rate as f64;
    if !is_synth(sound) && note != 0.0 {
        speed *= 2f64.powf(note / 12.0);
    }
    if let Some(info) = sample {
        // `unit "c"`: speed in cycles per sample (the file fits a cycle).
        if e.params.get("unit").is_some_and(|u| u.to_string() == "c") && info.rate > 0 {
            speed *= cps.max(0.01) * info.frames as f64 / info.rate as f64;
        }
        // Samples play backwards below 0; not slower than a 20th.
        let size = speed.abs().clamp(0.05, 16.0);
        p.speed = (if speed < 0.0 { -size } else { size }) as f32;
        p.begin = num("begin").unwrap_or(0.0).clamp(0.0, 1.0) as f32;
        p.end = num("end").unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        p.accelerate = num("accelerate").unwrap_or(0.0).clamp(-16.0, 16.0) as f32;
        p.loops = num("loop").unwrap_or(1.0).clamp(1.0, 64.0) as f32;
    } else {
        // Backwards isn't something the bank plays: the rate's size only.
        p.speed = speed.abs().clamp(0.05, 8.0) as f32;
    }
    p.cut = num("cut").map_or(0, |c| c.clamp(0.0, 1e6) as u32);
    p.coarse = num("coarse").map_or(0.0, |c| if c > 1.0 { c.min(64.0) } else { 0.0 }) as f32;
    let gain = num("gain").unwrap_or(1.0).clamp(0.0, 2.0);
    let amp = num("amp").map_or(1.0, |a| a / 0.4) * num("velocity").unwrap_or(1.0);
    let trim = if is_sample(sound) {
        SAMPLE_LEVEL
    } else if is_synth(sound) {
        SYNTH_BASE_LEVEL[(sound - SYNTH_BASE) as usize]
    } else {
        DRUM_BASE[sound as usize]
    };
    let level = (gain * gain * amp).clamp(0.0, 4.0) as f32 * trim;
    p.pan = (num("pan").unwrap_or(0.5).clamp(0.0, 1.0) * 2.0 - 1.0) as f32;
    p.cutoff = num("cutoff").map_or(0.0, |c| c.clamp(20.0, 20_000.0)) as f32;
    p.resonance = num("resonance").unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    p.hcutoff = num("hcutoff").map_or(0.0, |c| c.clamp(20.0, 20_000.0)) as f32;
    p.attack = num("attack").map_or(-1.0, |a| a.clamp(0.0, 10.0)) as f32;
    p.release = num("release").map_or(-1.0, |a| a.clamp(0.0, 20.0)) as f32;
    p.sustain = match (num("sustain"), num("legato")) {
        (Some(s), _) => s.clamp(0.0, 30.0),
        (None, Some(l)) => (l * secs).clamp(0.0, 30.0),
        (None, None) if is_synth(sound) => secs.clamp(0.01, 30.0),
        _ => -1.0,
    } as f32;
    p.shape = num("shape").unwrap_or(0.0).clamp(0.0, 0.99) as f32;
    p.crush = num("crush").map_or(0.0, |c| c.clamp(1.0, 16.0)) as f32;
    p.delay = num("delay").unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    p.delaytime = num("delaytime").map_or(-1.0, |t| t.clamp(0.001, 2.0)) as f32;
    p.delayfeedback = num("delayfeedback").map_or(-1.0, |f| f.clamp(0.0, 0.95)) as f32;
    p.room = num("room").unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    p.size = num("size").map_or(-1.0, |s| s.clamp(0.0, 1.0)) as f32;
    Ok(Play {
        sound,
        level,
        params: p,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Target;
    use crate::time::{Span, Time};

    fn event(s: &str, params: &[(&str, f64)]) -> Event {
        Event {
            layer: 0,
            whole: Span::new(Time::ZERO, Time::new(1, 4)),
            target: Target::Instrument,
            value: Value::Text(s.into()),
            params: params.iter().map(|(k, v)| (k.to_string(), Value::Num(*v))).collect(),
            loc: None,
            locs: Vec::new(),
        }
    }

    #[test]
    fn drums_kits_and_synths() {
        assert_eq!(sound("bd", 0.0), Some((0, 1.0)));
        assert_eq!(sound("drum", 9.0).map(|s| s.0), Some(1));
        assert_eq!(sound("superpiano", 0.0).map(|s| s.0), Some(SYNTH_BASE));
        assert_eq!(sound("arpy", 0.0).map(|s| s.0), Some(SYNTH_BASE + 6));
        assert_eq!(sound("nosuch", 0.0), None);
    }

    #[test]
    fn sounds_have_their_own_level() {
        let kick = play(&event("bd", &[]), 0.5).unwrap().level;
        let hat = play(&event("hh", &[]), 0.5).unwrap().level;
        let pad = play(&event("superpad", &[]), 0.5).unwrap().level;
        assert!(hat < kick * 0.4, "hats must sit under the kick: {hat} {kick}");
        assert!(pad < kick, "a pad must not bury the drums: {pad} {kick}");
        let loud = play(&event("hh", &[("gain", 2.0)]), 0.5).unwrap().level;
        assert!((loud - hat * 4.0).abs() < 1e-6, "gain stays squared");
    }

    #[test]
    fn notes_and_controls() {
        let p = play(&event("superpiano", &[("n", 7.0), ("octave", 4.0)]), 0.5).unwrap();
        assert_eq!(p.params.note, 55.0);
        assert_eq!(p.params.sustain, 0.5);
        let p = play(&event("bd", &[("note", 12.0), ("pan", 1.0), ("gain", 1.2)]), 0.5).unwrap();
        assert!((p.params.speed - 2.0).abs() < 1e-6);
        assert_eq!(p.params.pan, 1.0);
        assert!((p.level - 1.44).abs() < 1e-6);
        assert_eq!(p.params.sustain, -1.0, "drums ring out");
        let p = play(&event("supersaw", &[("legato", 0.5)]), 2.0).unwrap();
        assert_eq!(p.params.sustain, 1.0);
        assert!(play(&event("arpeggio", &[]), 1.0).unwrap_err().contains("no sound 'arpeggio'"));
    }

    struct Folders;

    impl Samples for Folders {
        fn sample(&self, name: &str, n: f64) -> Option<Result<SampleInfo, String>> {
            match name {
                // two seconds at 44.1 kHz; n picks the slot
                "bd" | "break" => Some(Ok(SampleInfo { slot: 3 + n as u32, frames: 88_200, rate: 44_100 })),
                "late" => Some(Err("late is still loading".into())),
                _ => None,
            }
        }
    }

    #[test]
    fn a_folder_wins_its_name_and_plays_its_controls() {
        let at = |s: &str, ps: &[(&str, f64)]| play_with(&event(s, ps), 0.25, 0.5, &Folders);
        let p = at("bd", &[("n", 1.0)]).unwrap();
        assert_eq!(p.sound, SAMPLE_BASE + 4);
        assert!(is_sample(p.sound) && !is_synth(p.sound));
        // Not a folder: the built-in sound.
        assert_eq!(at("sn", &[]).unwrap().sound, 1);
        assert!(at("late", &[]).unwrap_err().contains("loading"));
        let p = at("break", &[("begin", 0.25), ("end", 0.5), ("speed", -2.0), ("cut", 1.0), ("accelerate", -1.0)]).unwrap();
        assert_eq!((p.params.begin, p.params.end), (0.25, 0.5));
        assert_eq!(p.params.speed, -2.0, "samples play backwards");
        assert_eq!(p.params.cut, 1);
        assert_eq!(p.params.accelerate, -1.0);
        assert_eq!(p.params.sustain, -1.0, "a sample plays to its end");
        // unit "c": a two-second file at 0.5 cycles a second fits a cycle.
        let mut e = event("break", &[("speed", 1.0)]);
        e.params.insert("unit".into(), Value::Text("c".into()));
        let p = play_with(&e, 0.25, 0.5, &Folders).unwrap();
        assert!((p.params.speed - 1.0).abs() < 1e-6, "{}", p.params.speed);
        let p = play_with(&e, 0.25, 1.0, &Folders).unwrap();
        assert!((p.params.speed - 2.0).abs() < 1e-6);
        // Drums still play forwards only.
        assert_eq!(at("sn", &[("speed", -2.0)]).unwrap().params.speed, 2.0);
    }
}
