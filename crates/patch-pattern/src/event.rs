//! The event contract: what a running pattern hands to the rest of Patch.
//!
//! A layer of a program has one [`Target`]; each event is that target, a
//! [`Value`] and its exact time. Consumers subscribe by target kind: the
//! Sequencer takes `Sound`, the sampler takes `Sample` (derived material,
//! by typed id), the service turns `Deck`/`Stem`/`Mixer`/`Crossfader` into
//! validated [`Command`]s, the Visualiser reads `Visual` and `Scene`, AI
//! Stage reads `Ai`. [`route`] is the one place an event becomes an action.

use crate::ast::Value;
use crate::time::{Span, Time};
use patch_core::command::{Band, Command, Deck, FxKind, Transport};
use patch_core::material::SampleSource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckParam {
    /// −1 … 1 (low-pass … high-pass); a value over 1 is a low-pass cutoff in Hz.
    Filter,
    /// Channel fader 0 … 1.
    Volume,
    #[serde(rename = "low")]
    EqLow,
    #[serde(rename = "mid")]
    EqMid,
    #[serde(rename = "high")]
    EqHigh,
    /// An effect's name turns it on; "off" turns it off.
    Fx,
    /// play, pause, cue.
    Transport,
    /// A loop of this many beats; 0 or "off" exits.
    Loop,
    /// Jump this many beats.
    Jump,
    /// Hot cue 1 … 8.
    Cue,
}

impl DeckParam {
    pub const ALL: [DeckParam; 10] = [
        DeckParam::Filter,
        DeckParam::Volume,
        DeckParam::EqLow,
        DeckParam::EqMid,
        DeckParam::EqHigh,
        DeckParam::Fx,
        DeckParam::Transport,
        DeckParam::Loop,
        DeckParam::Jump,
        DeckParam::Cue,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DeckParam::Filter => "filter",
            DeckParam::Volume => "volume",
            DeckParam::EqLow => "low",
            DeckParam::EqMid => "mid",
            DeckParam::EqHigh => "high",
            DeckParam::Fx => "fx",
            DeckParam::Transport => "transport",
            DeckParam::Loop => "loop",
            DeckParam::Jump => "jump",
            DeckParam::Cue => "cue",
        }
    }

    pub fn parse(s: &str) -> Option<DeckParam> {
        DeckParam::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixerParam {
    /// dB.
    Gain,
    /// mute / unmute (or 1 / 0).
    Mute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiKind {
    Note,
    Cc(u8),
}

/// Where a layer's events go.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Target {
    /// A Sequencer voice by name (bd, sd, hh, oh, cp, lt, rim, sh).
    Sound,
    /// Derived material: an asset, or a kit whose slot is the event's word.
    Sample {
        source: SampleSource,
    },
    /// A pitch (MIDI number or a name like c4), for instrument units.
    Note,
    /// The pattern instrument: the event's value is a sound (`bd`,
    /// `superpiano`), its params the controls (`n`, `gain`, `cutoff`, …).
    Instrument,
    Deck {
        deck: Deck,
        param: DeckParam,
    },
    /// A deck's stem: mute, unmute, solo, unsolo, or a gain 0 … 2.
    Stem {
        deck: Deck,
        stem: String,
    },
    Mixer {
        strip: String,
        param: MixerParam,
    },
    /// 0 (A) … 1 (B).
    Crossfader,
    Midi {
        channel: u8,
        kind: MidiKind,
    },
    /// A named control value any unit can bind.
    Control {
        name: String,
    },
    /// A Visualiser parameter (intensity, symmetry, hue, …).
    Visual {
        param: String,
    },
    /// A Visualiser scene by name.
    Scene,
    /// An instruction for the AI director, as a word.
    Ai,
}

impl Target {
    /// Short label for the surface: "deck a filter", "visual intensity".
    pub fn label(&self) -> String {
        match self {
            Target::Sound => "sound".into(),
            Target::Sample { source } => match source {
                SampleSource::Asset(a) => format!("sample {a}"),
                SampleSource::Kit(k) => format!("kit {k}"),
            },
            Target::Note => "note".into(),
            Target::Instrument => "instrument".into(),
            Target::Deck { deck, param } => format!("deck {} {}", deck.id(), param.name()),
            Target::Stem { deck, stem } => format!("deck {} stem {stem}", deck.id()),
            Target::Mixer { strip, param } => format!(
                "mixer {strip} {}",
                match param {
                    MixerParam::Gain => "gain",
                    MixerParam::Mute => "mute",
                }
            ),
            Target::Crossfader => "crossfader".into(),
            Target::Midi { channel, kind } => match kind {
                MidiKind::Note => format!("midi {channel} note"),
                MidiKind::Cc(n) => format!("midi {channel} cc {n}"),
            },
            Target::Control { name } => format!("control {name}"),
            Target::Visual { param } => format!("visual {param}"),
            Target::Scene => "scene".into(),
            Target::Ai => "ai".into(),
        }
    }

    /// Played by the audio thread at its sample (else by the control thread).
    pub fn sample_accurate(&self) -> bool {
        matches!(
            self,
            Target::Sound | Target::Sample { .. } | Target::Instrument
        )
    }

    /// Read by the Visualiser from the program itself (not sent as events).
    pub fn visual(&self) -> bool {
        matches!(self, Target::Visual { .. } | Target::Scene)
    }
}

/// One event of a running program.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The layer's index in its program.
    pub layer: u16,
    /// Exact start and end, in cycles.
    pub whole: Span,
    pub target: Target,
    pub value: Value,
    /// Extra controls an engine attached (gain, velocity, pan), by name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub params: std::collections::BTreeMap<String, Value>,
    /// Where in the source it came from (byte range), when the engine knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loc: Option<(usize, usize)>,
    /// Every word of the code it came from, as `[source, from, to]` byte
    /// ranges (the Tidal Unit: a source is a channel's or a `let`'s code),
    /// so an editor lights them up while the event plays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locs: Vec<[u32; 3]>,
}

impl Event {
    /// A numeric control, if the engine attached one.
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params.get(name).and_then(Value::num)
    }

    pub fn onset(&self) -> Time {
        self.whole.begin
    }
}

/// The Sequencer's voices, by every name a pattern may use.
pub const VOICES: [(&str, u8); 20] = [
    ("bd", 0),
    ("kick", 0),
    ("sd", 1),
    ("snare", 1),
    ("hh", 2),
    ("ch", 2),
    ("hat", 2),
    ("oh", 3),
    ("cp", 4),
    ("clap", 4),
    ("lt", 5),
    ("tom", 5),
    ("rim", 6),
    ("rs", 6),
    ("sh", 7),
    ("shaker", 7),
    ("perc", 6),
    ("lo", 5),
    ("ho", 3),
    ("cr", 3),
];

pub fn voice(name: &str) -> Option<u8> {
    let base = name.split(':').next().unwrap_or(name);
    VOICES.iter().find(|(n, _)| *n == base).map(|v| v.1)
}

/// "c4" → 60, "a#3" → 58, "eb5" → 75; a number is itself.
pub fn note_number(v: &Value) -> Option<f64> {
    if let Some(n) = v.num() {
        return Some(n);
    }
    let s = v.text()?.to_ascii_lowercase();
    let mut chars = s.chars();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, oct) = if let Some(r) = rest.strip_prefix('#').or(rest.strip_prefix('s')) {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = if oct.is_empty() { 5 } else { oct.parse().ok()? };
    Some(((octave + 1) * 12 + base + acc) as f64)
}

/// What an event asks of Patch.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A Sequencer voice at its sample.
    Voice {
        voice: u8,
        velocity: f32,
    },
    /// Derived material at its sample: an asset, or the kit slot `slot`.
    Sample {
        source: SampleSource,
        slot: Option<String>,
        gain: f32,
    },
    /// A command, validated with the program's origin before it runs.
    Command(Command),
    Note {
        pitch: f64,
    },
    Midi {
        channel: u8,
        kind: MidiKind,
        value: f64,
    },
    Control {
        name: String,
        value: Value,
    },
    Visual {
        param: String,
        value: Value,
    },
    Scene(String),
    Ai(String),
    /// A rest or a value the target can't take: nothing happens.
    Nothing,
}

/// Low-pass cutoff (Hz) as the deck filter's amount: 20 kHz is open (0),
/// 20 Hz fully closed (−1), log-spaced like the knob.
fn cutoff_amount(hz: f64) -> f32 {
    let x = (hz.clamp(20.0, 20_000.0) / 20.0).log10() / 3.0;
    (x - 1.0) as f32
}

fn word(v: &Value) -> String {
    v.to_string().to_ascii_lowercase()
}

/// An event as the action it asks for.
pub fn route(target: &Target, v: &Value) -> Action {
    let num = v.num();
    match target {
        Target::Sound => match v.text().and_then(voice) {
            Some(voice) => Action::Voice {
                voice,
                velocity: 1.0,
            },
            None => Action::Nothing,
        },
        Target::Sample { source } => Action::Sample {
            source: source.clone(),
            slot: match source {
                SampleSource::Kit(_) => Some(word(v)),
                SampleSource::Asset(_) => None,
            },
            gain: num.map_or(1.0, |n| n.clamp(0.0, 2.0) as f32),
        },
        Target::Note => note_number(v).map_or(Action::Nothing, |pitch| Action::Note { pitch }),
        // Played with its controls (see `instrument::play`), not routed.
        Target::Instrument => Action::Nothing,
        Target::Deck { deck, param } => {
            let deck = *deck;
            let cmd = match param {
                DeckParam::Filter => num.map(|n| Command::DeckFilter {
                    deck,
                    amount: if n.abs() > 1.0 {
                        cutoff_amount(n)
                    } else {
                        n as f32
                    },
                }),
                DeckParam::Volume => num.map(|n| Command::DeckFader {
                    deck,
                    position: n.clamp(0.0, 1.0) as f32,
                }),
                DeckParam::EqLow | DeckParam::EqMid | DeckParam::EqHigh => {
                    num.map(|n| Command::DeckEqKnob {
                        deck,
                        band: match param {
                            DeckParam::EqLow => Band::Low,
                            DeckParam::EqMid => Band::Mid,
                            _ => Band::High,
                        },
                        value: n.clamp(0.0, 1.0) as f32,
                    })
                }
                DeckParam::Fx => match word(v).as_str() {
                    "off" | "0" => Some(Command::DeckFx {
                        deck,
                        kind: None,
                        on: Some(false),
                        beats: None,
                        depth: None,
                    }),
                    w => FxKind::parse(w).map(|k| Command::DeckFx {
                        deck,
                        kind: Some(k),
                        on: Some(true),
                        beats: None,
                        depth: None,
                    }),
                },
                DeckParam::Transport => match word(v).as_str() {
                    "play" => Some(Transport::Play),
                    "pause" | "stop" => Some(Transport::Pause),
                    "cue" => Some(Transport::Cue),
                    _ => None,
                }
                .map(|action| Command::DeckTransport { deck, action }),
                DeckParam::Loop => Some(Command::DeckLoop {
                    deck,
                    beats: num.filter(|n| *n > 0.0).map(|n| n as f32),
                }),
                DeckParam::Jump => num.map(|n| Command::DeckJump {
                    deck,
                    beats: n as f32,
                }),
                DeckParam::Cue => {
                    num.filter(|n| (1.0..=8.0).contains(n))
                        .map(|n| Command::HotCue {
                            deck,
                            n: n as u8 - 1,
                            clear: false,
                        })
                }
            };
            cmd.map_or(Action::Nothing, Action::Command)
        }
        Target::Stem { deck, stem } => {
            use patch_core::stems::StemValue;
            let value = match word(v).as_str() {
                "mute" => Some(StemValue::Mute(true)),
                "unmute" | "on" => Some(StemValue::Mute(false)),
                "solo" => Some(StemValue::Solo(true)),
                "unsolo" => Some(StemValue::Solo(false)),
                _ => num.map(|n| StemValue::Gain(n.clamp(0.0, 2.0) as f32)),
            };
            value.map_or(Action::Nothing, |value| {
                Action::Command(Command::StemControl {
                    deck: *deck,
                    stem: stem.clone(),
                    value,
                })
            })
        }
        Target::Mixer { strip, param } => match param {
            MixerParam::Gain => num.map_or(Action::Nothing, |db| {
                Action::Command(Command::SetGain {
                    strip: strip.clone(),
                    db: db as f32,
                })
            }),
            MixerParam::Mute => Action::Command(Command::SetMute {
                strip: strip.clone(),
                muted: matches!(word(v).as_str(), "mute" | "1" | "on" | "true"),
            }),
        },
        Target::Crossfader => num.map_or(Action::Nothing, |x| {
            Action::Command(Command::Crossfader {
                position: x.clamp(0.0, 1.0) as f32,
            })
        }),
        Target::Midi { channel, kind } => {
            let value = match kind {
                MidiKind::Note => note_number(v),
                MidiKind::Cc(_) => num,
            };
            value.map_or(Action::Nothing, |value| Action::Midi {
                channel: *channel,
                kind: *kind,
                value,
            })
        }
        Target::Control { name } => Action::Control {
            name: name.clone(),
            value: v.clone(),
        },
        Target::Visual { param } => Action::Visual {
            param: param.clone(),
            value: v.clone(),
        },
        Target::Scene => Action::Scene(v.to_string()),
        Target::Ai => Action::Ai(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_voices() {
        assert_eq!(note_number(&Value::atom("c4")), Some(60.0));
        assert_eq!(note_number(&Value::atom("a#3")), Some(58.0));
        assert_eq!(note_number(&Value::atom("eb5")), Some(75.0));
        assert_eq!(note_number(&Value::atom("67")), Some(67.0));
        assert_eq!(voice("bd:3"), Some(0));
        assert_eq!(voice("zz"), None);
    }

    #[test]
    fn deck_events_become_valid_commands() {
        let t = Target::Deck {
            deck: Deck::A,
            param: DeckParam::Filter,
        };
        let Action::Command(Command::DeckFilter { amount, .. }) = route(&t, &Value::Num(20_000.0))
        else {
            panic!()
        };
        assert!(amount.abs() < 1e-6, "20 kHz is open");
        let Action::Command(Command::DeckFilter { amount, .. }) = route(&t, &Value::Num(200.0))
        else {
            panic!()
        };
        assert!((-1.0..-0.5).contains(&amount), "{amount}");
        let ctx = patch_core::command::Context::default();
        for v in [Value::Num(0.5), Value::Num(800.0), Value::Num(-1.0)] {
            if let Action::Command(c) = route(&t, &v) {
                patch_core::command::validate(c, patch_core::command::Origin::Ai, &ctx).unwrap();
            }
        }
        let stem = Target::Stem {
            deck: Deck::B,
            stem: "vocals".into(),
        };
        assert!(matches!(
            route(&stem, &Value::atom("mute")),
            Action::Command(Command::StemControl { .. })
        ));
        assert_eq!(route(&stem, &Value::atom("wobble")), Action::Nothing);
    }
}
