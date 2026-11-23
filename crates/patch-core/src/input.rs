//! Input abstraction: any physical input → a semantic Patch command.
//!
//! Keyboards, MIDI controllers and HID devices produce
//! [`InputEvent`]s. A [`Keymap`] — plain data, loadable from JSON — binds
//! events in a context (the focused surface, e.g. `deck-a`, `dj-mixer`) to
//! command templates. Templates are ordinary command JSON with two
//! placeholders: `"{deck}"` (from the context) and `"{value}"` (a MIDI
//! controller's value scaled into the binding's range). The result is a
//! [`Command`] that goes through the same validation as everything else.
//! No widget decides what a key or a knob does.

use crate::command::Command;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// A key press (or release, `down == false`). `text` is what the key
    /// types, or a name: "left", "right", "up", "down", "space", "enter".
    Key {
        text: String,
        shift: bool,
        ctrl: bool,
        alt: bool,
        down: bool,
    },
    /// MIDI note on (velocity > 0) or off.
    MidiNote { channel: u8, note: u8, velocity: u8 },
    /// Linux evdev button, relative encoder or absolute axis.
    Hid {
        event_type: u16,
        code: u16,
        value: i32,
    },
    /// One field of a raw HID input report, from a device whose protocol the
    /// kernel does not decode (a vendor-protocol controller).
    ///
    /// `mask` (within `width()` bytes at `offset`, after the report-id byte)
    /// identifies the field, so several buttons in one byte stay distinct.
    /// `value` is already the field's own value: 0 or 1 for a button, the raw
    /// value for an axis, a movement for a relative encoder. The transport
    /// keeps the last value of each field and emits an event only when it
    /// changes, which is how a platter-touch *value* becomes a press/release.
    HidField {
        report: u8,
        offset: u16,
        width: u8,
        mask: u32,
        value: i32,
    },
    /// MIDI control change, value 0 … 127. A relative encoder is this same
    /// event; the binding says how to read the value.
    MidiCc { channel: u8, cc: u8, value: u8 },
    /// A 14-bit control change, its MSB/LSB pair already assembled by the
    /// device profile. `cc` is the MSB's controller number; `value` is
    /// 0 … 16383.
    MidiCc14 { channel: u8, cc: u8, value: u16 },
    /// Pitch bend, centred at 0 (−8192 … +8191).
    MidiPitchBend { channel: u8, value: i16 },
    /// Program change.
    MidiProgram { channel: u8, program: u8 },
}

/// How a 14-bit-unaware hardware encoder reports direction and amount in a
/// single 7-bit CC value.
///
/// Only three decodings exist in practice, and Mixxx's seven option names map
/// onto them: `diff`, `selectknob` and `hercjog` are two's complement (they
/// differ only in whether Mixxx accumulates into its absolute ControlObject),
/// `rot64`/`rot64fast`/`spread64` are offset binary, and `hercjogfast` is
/// two's complement at three times the rate.
///
/// Mixxx accumulates these into an absolute value because its control objects
/// are absolute. Patch's canonical commands have explicit relative forms
/// (`dj.bend`, `dj.crossfader_step`, `dj.jump`), so a binding yields the
/// *movement* and `step` carries any multiplier — `hercjogfast` in Mixxx is
/// `{"relative": "twos_complement", "step": 3}` here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelativeMode {
    /// Two's complement over the whole field: 0…63 = up, 64…127 = down.
    /// Mixxx: `diff`, `selectknob`, `hercjog`.
    TwosComplement,
    /// Offset binary: 64 is the centre, 65…127 = up, 0…63 = down.
    /// Mixxx: `rot64`, `rot64fast`, `spread64`.
    OffsetBinary,
    /// Bit 6 is the sign, bits 0-5 are the amount (3Fh up / 41h down). Not a
    /// Mixxx MIDI option — Patch decodes it directly for hardware that uses
    /// this encoding.
    SignedBit,
}

impl RelativeMode {
    /// A raw CC value's movement, in encoder steps. Zero means "no movement",
    /// which callers drop rather than dispatch as a change of 0.
    pub fn decode(self, value: u8) -> i32 {
        let v = i32::from(value & 0x7F);
        match self {
            RelativeMode::TwosComplement => {
                if v < 64 {
                    v
                } else {
                    v - 128
                }
            }
            RelativeMode::OffsetBinary => v - 64,
            RelativeMode::SignedBit => {
                if v & 0x40 != 0 {
                    -(v & 0x3F)
                } else {
                    v & 0x3F
                }
            }
        }
    }
}

/// What a binding listens to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Trigger {
    Key {
        key: String,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        /// Fire on release instead of press.
        #[serde(default)]
        release: bool,
    },
    Note {
        channel: u8,
        note: u8,
        #[serde(default)]
        release: bool,
    },
    HidButton {
        code: u16,
        #[serde(default)]
        release: bool,
    },
    HidRelative {
        code: u16,
        step: f64,
    },
    HidAxis {
        code: u16,
        min: i32,
        max: i32,
    },
    /// A field of a raw HID input report. `width` is in bytes (1, 2 or 4);
    /// `mask` applies within those bytes, so a byte holding eight buttons
    /// yields eight triggers.
    HidField {
        report: u8,
        offset: u16,
        width: u8,
        mask: u32,
        field: HidFieldKind,
    },
    Cc {
        channel: u8,
        cc: u8,
        /// Set for a relative encoder; the raw value then reports movement
        /// instead of position.
        #[serde(default)]
        relative: Option<RelativeMode>,
        /// Multiplier for a relative encoder's steps (an encoder sends
        /// direction and amount, not a value).
        #[serde(default = "one")]
        step: f64,
    },
    /// A 14-bit control change: `cc` is the MSB's number, the LSB is
    /// `cc + 32`, as MIDI's registered pairs and Mixxx's
    /// `fourteen-bit-msb`/`fourteen-bit-lsb` options both use.
    Cc14 {
        channel: u8,
        cc: u8,
    },
    PitchBend {
        channel: u8,
    },
    Program {
        channel: u8,
        program: u8,
    },
}

/// What a raw HID field is, and therefore what its value means.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "lowercase")]
pub enum HidFieldKind {
    /// A bit (or a value derived from one): non-zero is a press. `release`
    /// selects the other direction, exactly as it does for a note or a key.
    Button {
        #[serde(default)]
        release: bool,
    },
    /// An absolute value, mapped through the binding's `range`.
    Axis { min: i32, max: i32 },
    /// A movement reported by the device (a jog wheel), in device units.
    /// `release` marks the sibling binding that fires when the control goes
    /// *quiet*: the transport emits a zero for a relative field that has not
    /// changed for a moment, exactly as a button's release arrives as a zero.
    /// That is what ends a scratch when the platter stops — without it a deck
    /// that has been nudged stays in scratch mode until something else clears
    /// it, repeating a short segment.
    Relative {
        #[serde(default = "one")]
        step: f64,
        #[serde(default)]
        release: bool,
    },
    /// A value that reads as a press while it is inside `min … max` — how a
    /// platter reports a hand. The transport derives the press, so the
    /// binding below matches it as a button; `release` fires on leaving.
    Window {
        min: i32,
        max: i32,
        #[serde(default)]
        release: bool,
    },
}

/// Field widths, in bytes after the report id.
pub const HID_FIELD_WIDTHS: [u8; 3] = [1, 2, 4];

fn one() -> f64 {
    1.0
}

impl Trigger {
    /// An absolute control change: the common case, and the identity of every
    /// mapping written before relative encoders and 14-bit pairs existed.
    pub fn cc(channel: u8, cc: u8) -> Self {
        Trigger::Cc {
            channel,
            cc,
            relative: None,
            step: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// Surfaces this binding applies in ("deck" matches deck-a/deck-b and
    /// fills {deck}); empty = everywhere.
    #[serde(default)]
    pub context: String,
    pub on: Trigger,
    /// Command JSON with "{deck}" / "{value}" placeholders.
    pub action: Value,
    /// The source range mapped onto the target's full range. A reversed pair
    /// mirrors it.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// Make this control soft-takeover: it only starts driving Patch once the
    /// hardware has reached the value `source` reports. Absolute controls
    /// only — a relative encoder has no position to mismatch. Requires
    /// `source`.
    #[serde(default)]
    pub soft_takeover: bool,
    /// The authoritative Patch state this control drives, spelled like an
    /// output's `source` (`crossfader`, `deck.a.fader`). Required by
    /// `soft_takeover`, and used to arm it with the value Patch is already
    /// on — so nothing needs a second copy of the mixer's state.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Keymap {
    pub bindings: Vec<Binding>,
}

fn fill(template: &Value, deck: Option<&str>, value: Option<f64>) -> Value {
    match template {
        Value::String(s) if s == "{deck}" => deck.map(Value::from).unwrap_or(Value::Null),
        Value::String(s) if s == "{value}" => value.map(Value::from).unwrap_or(Value::Null),
        Value::Array(a) => Value::Array(a.iter().map(|v| fill(v, deck, value)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), fill(v, deck, value)))
                .collect(),
        ),
        v => v.clone(),
    }
}

/// "deck-a" → Some("a").
fn deck_of(context: &str) -> Option<&str> {
    context.strip_prefix("deck-")
}

impl Keymap {
    /// The command bound to `ev` in `context`, if any.
    pub fn resolve(&self, context: &str, ev: &InputEvent) -> Option<Command> {
        let deck = deck_of(context);
        for b in &self.bindings {
            let ctx_ok = b.context.is_empty()
                || b.context == context
                || (b.context == "deck" && deck.is_some());
            if !ctx_ok {
                continue;
            }
            let mut skip = false;
            let value = match (&b.on, ev) {
                (
                    Trigger::Key {
                        key,
                        shift,
                        ctrl,
                        alt,
                        release,
                    },
                    InputEvent::Key {
                        text,
                        shift: s,
                        ctrl: c,
                        alt: a,
                        down,
                    },
                ) if key == text && shift == s && ctrl == c && alt == a && *release != *down => {
                    None
                }
                (
                    Trigger::Note {
                        channel,
                        note,
                        release,
                    },
                    InputEvent::MidiNote {
                        channel: ch,
                        note: n,
                        velocity,
                    },
                ) if channel == ch && note == n && *release == (*velocity == 0) => None,
                (
                    Trigger::Cc {
                        channel,
                        cc,
                        relative,
                        step,
                    },
                    InputEvent::MidiCc {
                        channel: ch,
                        cc: c,
                        value,
                    },
                ) if channel == ch && cc == c && step.is_finite() => match relative {
                    Some(mode) => {
                        let delta = mode.decode(*value);
                        // A relative control that reports no movement must
                        // not dispatch a change of zero.
                        if delta == 0 {
                            skip = true;
                            None
                        } else {
                            Some(f64::from(delta) * step)
                        }
                    }
                    None => {
                        let (lo, hi) = b.range.unwrap_or((0.0, 1.0));
                        Some(lo + (hi - lo) * *value as f64 / 127.0)
                    }
                },
                (
                    Trigger::Cc14 { channel, cc },
                    InputEvent::MidiCc14 {
                        channel: ch,
                        cc: c,
                        value,
                    },
                ) if channel == ch && cc == c => {
                    let (lo, hi) = b.range.unwrap_or((0.0, 1.0));
                    Some(lo + (hi - lo) * *value as f64 / 16383.0)
                }
                (
                    Trigger::PitchBend { channel },
                    InputEvent::MidiPitchBend { channel: ch, value },
                ) if channel == ch => {
                    // Centred at 0, so a bend is naturally symmetric: the
                    // ±8192 field spans 16384 steps, putting 0 at the middle
                    // and the extremes exactly at the range's ends.
                    let (lo, hi) = b.range.unwrap_or((-1.0, 1.0));
                    let fraction = (f64::from(*value) + 8192.0) / 16384.0;
                    Some(lo + (hi - lo) * fraction)
                }
                (
                    Trigger::Program { channel, program },
                    InputEvent::MidiProgram {
                        channel: ch,
                        program: p,
                    },
                ) if channel == ch && program == p => None,
                (
                    Trigger::HidButton { code, release },
                    InputEvent::Hid {
                        event_type: 1,
                        code: c,
                        value,
                    },
                ) if code == c && (*value == 0 || *value == 1) && *release == (*value == 0) => None,
                (
                    Trigger::HidRelative { code, step },
                    InputEvent::Hid {
                        event_type: 2,
                        code: c,
                        value,
                    },
                ) if code == c && step.is_finite() => Some(*value as f64 * step),
                (
                    Trigger::HidAxis { code, min, max },
                    InputEvent::Hid {
                        event_type: 3,
                        code: c,
                        value,
                    },
                ) if code == c && max > min => {
                    let (lo, hi) = b.range.unwrap_or((0.0, 1.0));
                    let fraction = ((*value as f64 - *min as f64) / (*max as f64 - *min as f64))
                        .clamp(0.0, 1.0);
                    Some(lo + (hi - lo) * fraction)
                }
                (
                    Trigger::HidField {
                        report,
                        offset,
                        width,
                        mask,
                        field,
                    },
                    InputEvent::HidField {
                        report: r,
                        offset: o,
                        width: w,
                        mask: m,
                        value,
                    },
                ) if report == r
                    && offset == o
                    && width == w
                    && mask == m
                    && HID_FIELD_WIDTHS.contains(width) => {
                    match field {
                        // The transport has already derived the bit (and, for
                        // a window, the press), so these differ only in which
                        // direction fires — as a note's does.
                        HidFieldKind::Button { release }
                        | HidFieldKind::Window { release, .. } => {
                            // `release` selects the direction, exactly as it
                            // does for a note: the other direction belongs to
                            // the sibling binding.
                            let pressed = *value != 0;
                            if pressed == *release {
                                skip = true;
                            }
                            None
                        }
                        HidFieldKind::Relative { step, release } if step.is_finite() => {
                            // A relative control's *release* is its silence: the
                            // transport emits a zero once the field has gone quiet,
                            // and `release` selects which of the pair fires — the
                            // movement or the release — exactly as a note's does.
                            let silent = *value == 0;
                            if silent != *release {
                                skip = true;
                                None
                            } else if silent {
                                Some(0.0)
                            } else {
                                Some(*value as f64 * step)
                            }
                        }
                        HidFieldKind::Axis { min, max } if max > min => {
                            let (lo, hi) = b.range.unwrap_or((0.0, 1.0));
                            let fraction =
                                ((*value as f64 - *min as f64) / (*max as f64 - *min as f64))
                                    .clamp(0.0, 1.0);
                            Some(lo + (hi - lo) * fraction)
                        }
                        _ => {
                            skip = true;
                            None
                        }
                    }
                }
                _ => continue,
            };
            if skip {
                continue;
            }
            let json = fill(&b.action, deck, value);
            if let Ok(mut cmd) = serde_json::from_value::<Command>(json) {
                // Absolute hardware has no exact midpoint on odd-sized MIDI
                // ranges, and ADCs can jitter around a physical centre detent.
                // Snap EQ only; relative encoders must retain their tiny steps.
                let absolute = matches!(b.on,
                    Trigger::Cc { relative: None, .. } | Trigger::Cc14 { .. }
                    | Trigger::PitchBend { .. } | Trigger::HidAxis { .. }
                    | Trigger::HidField { field: HidFieldKind::Axis { .. }, .. });
                if absolute && let Command::DeckEqKnob { value, .. } = &mut cmd
                    && value.abs() <= 0.01 {
                    *value = 0.0;
                }
                return Some(cmd);
            }
        }
        None
    }

    /// The user's keymap (~/.config/omarchy-patch/dj-keys.json) or the
    /// default.
    pub fn load() -> Keymap {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".config")
            });
        std::fs::read_to_string(base.join("omarchy-patch").join("dj-keys.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(Keymap::default_dj)
    }

    /// The DJ Booth's default keyboard map.
    pub fn default_dj() -> Keymap {
        let key = |k: &str| Trigger::Key {
            key: k.into(),
            shift: false,
            ctrl: false,
            alt: false,
            release: false,
        };
        let shift = |k: &str| Trigger::Key {
            key: k.into(),
            shift: true,
            ctrl: false,
            alt: false,
            release: false,
        };
        let up = |k: &str| Trigger::Key {
            key: k.into(),
            shift: false,
            ctrl: false,
            alt: false,
            release: true,
        };
        let deck = |on: Trigger, action: Value| Binding {
            context: "deck".into(),
            on,
            action,
            range: None,
            soft_takeover: false,
            source: None,
        };
        let mixer = |on: Trigger, action: Value| Binding {
            context: "dj-mixer".into(),
            on,
            action,
            range: None,
            soft_takeover: false,
            source: None,
        };
        use serde_json::json;
        let mut b = vec![
            deck(
                key("space"),
                json!({"op": "dj.transport", "deck": "{deck}", "action": "toggle"}),
            ),
            deck(
                key("c"),
                json!({"op": "dj.transport", "deck": "{deck}", "action": "cue"}),
            ),
            deck(
                up("c"),
                json!({"op": "dj.transport", "deck": "{deck}", "action": "cue_release"}),
            ),
            deck(
                key("left"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": -4.0}),
            ),
            deck(
                key("right"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": 4.0}),
            ),
            deck(
                key("h"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": -4.0}),
            ),
            deck(
                key("l"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": 4.0}),
            ),
            deck(
                shift("left"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": -16.0}),
            ),
            deck(
                shift("right"),
                json!({"op": "dj.jump", "deck": "{deck}", "beats": 16.0}),
            ),
            deck(
                key("up"),
                json!({"op": "dj.tempo_step", "deck": "{deck}", "percent": 0.1}),
            ),
            deck(
                key("down"),
                json!({"op": "dj.tempo_step", "deck": "{deck}", "percent": -0.1}),
            ),
            deck(
                shift("up"),
                json!({"op": "dj.tempo_step", "deck": "{deck}", "percent": 1.0}),
            ),
            deck(
                shift("down"),
                json!({"op": "dj.tempo_step", "deck": "{deck}", "percent": -1.0}),
            ),
            deck(
                key("0"),
                json!({"op": "dj.tempo", "deck": "{deck}", "percent": 0.0}),
            ),
            deck(key("r"), json!({"op": "dj.tempo_range", "deck": "{deck}"})),
            deck(
                key("k"),
                json!({"op": "dj.toggle", "deck": "{deck}", "what": "keylock"}),
            ),
            deck(
                key("z"),
                json!({"op": "dj.toggle", "deck": "{deck}", "what": "slip"}),
            ),
            deck(
                key("x"),
                json!({"op": "dj.sample", "deck": "{deck}", "action": "panel"}),
            ),
            deck(
                key("q"),
                json!({"op": "dj.toggle", "deck": "{deck}", "what": "quantize"}),
            ),
            deck(
                key("p"),
                json!({"op": "dj.toggle", "deck": "{deck}", "what": "pfl"}),
            ),
            deck(
                key("["),
                json!({"op": "dj.key_step", "deck": "{deck}", "semitones": -1}),
            ),
            deck(
                key("]"),
                json!({"op": "dj.key_step", "deck": "{deck}", "semitones": 1}),
            ),
            deck(key("s"), json!({"op": "dj.sync", "deck": "{deck}"})),
            deck(
                key("o"),
                json!({"op": "dj.reloop", "deck": "{deck}"}),
            ),
            deck(
                key("-"),
                json!({"op": "dj.loop_scale", "deck": "{deck}", "factor": 0.5}),
            ),
            deck(
                key("="),
                json!({"op": "dj.loop_scale", "deck": "{deck}", "factor": 2.0}),
            ),
            deck(
                key(","),
                json!({"op": "dj.bend", "deck": "{deck}", "amount": -0.04}),
            ),
            deck(
                key("."),
                json!({"op": "dj.bend", "deck": "{deck}", "amount": 0.04}),
            ),
            deck(
                up(","),
                json!({"op": "dj.bend", "deck": "{deck}", "amount": 0.0}),
            ),
            deck(
                up("."),
                json!({"op": "dj.bend", "deck": "{deck}", "amount": 0.0}),
            ),
            mixer(
                key("left"),
                json!({"op": "dj.crossfader_step", "delta": -0.05}),
            ),
            mixer(
                key("right"),
                json!({"op": "dj.crossfader_step", "delta": 0.05}),
            ),
            mixer(
                key("h"),
                json!({"op": "dj.crossfader_step", "delta": -0.05}),
            ),
            mixer(key("l"), json!({"op": "dj.crossfader_step", "delta": 0.05})),
            mixer(key("0"), json!({"op": "dj.crossfader", "position": 0.5})),
        ];
        // The eight pads the deck shows (crate::command::HOT_CUES).
        for n in 1..=crate::command::HOT_CUES as u8 {
            let k = n.to_string();
            b.push(deck(
                key(&k),
                json!({"op": "dj.hot_cue", "deck": "{deck}", "n": n}),
            ));
            b.push(deck(
                shift(&k),
                json!({"op": "dj.hot_cue", "deck": "{deck}", "n": n, "clear": true}),
            ));
            // US layout: Shift+1 types "!".
            let sym = ["!", "@", "#", "$", "%", "^", "&", "*"][n as usize - 1];
            b.push(deck(
                shift(sym),
                json!({"op": "dj.hot_cue", "deck": "{deck}", "n": n, "clear": true}),
            ));
        }
        Keymap { bindings: b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Deck, Feature, Transport};

    fn press(k: &str, shift: bool) -> InputEvent {
        InputEvent::Key {
            text: k.into(),
            shift,
            ctrl: false,
            alt: false,
            down: true,
        }
    }

    #[test]
    fn keys_become_commands_for_the_focused_deck() {
        let m = Keymap::default_dj();
        assert_eq!(
            m.resolve("deck-b", &press("space", false)),
            Some(Command::DeckTransport {
                deck: Deck::B,
                action: Transport::Toggle
            })
        );
        assert_eq!(
            m.resolve("deck-a", &press("k", false)),
            Some(Command::DeckToggle {
                deck: Deck::A,
                what: Feature::Keylock
            })
        );
        assert_eq!(
            m.resolve("deck-a", &press("right", true)),
            Some(Command::DeckJump {
                deck: Deck::A,
                beats: 16.0
            })
        );
        assert_eq!(
            m.resolve("deck-a", &press("2", false)),
            Some(Command::HotCue {
                deck: Deck::A,
                n: 2,
                clear: false
            })
        );
        assert_eq!(
            m.resolve("deck-b", &press("8", false)),
            Some(Command::HotCue {
                deck: Deck::B,
                n: 8,
                clear: false
            }),
            "the deck shows eight cue pads: all eight have a key"
        );
        assert_eq!(
            m.resolve("deck-a", &press("*", true)),
            Some(Command::HotCue {
                deck: Deck::A,
                n: 8,
                clear: true
            })
        );
        assert_eq!(
            m.resolve("dj-mixer", &press("left", false)),
            Some(Command::CrossfaderStep { delta: -0.05 })
        );
        assert_eq!(
            m.resolve("library", &press("space", false)),
            None,
            "decks' keys don't leak"
        );
    }

    #[test]
    fn loop_key_exits_or_recalls_the_focused_decks_loop() {
        let m = Keymap::default_dj();
        assert_eq!(
            m.resolve("deck-a", &press("o", false)),
            Some(Command::DeckReloop { deck: Deck::A })
        );
        assert_eq!(m.resolve("library", &press("o", false)), None);
    }

    #[test]
    fn cue_is_held_press_and_release() {
        let m = Keymap::default_dj();
        let ev = |down| InputEvent::Key {
            text: "c".into(),
            shift: false,
            ctrl: false,
            alt: false,
            down,
        };
        assert_eq!(
            m.resolve("deck-b", &ev(true)),
            Some(Command::DeckTransport {
                deck: Deck::B,
                action: Transport::Cue
            })
        );
        assert_eq!(
            m.resolve("deck-b", &ev(false)),
            Some(Command::DeckTransport {
                deck: Deck::B,
                action: Transport::CueRelease
            })
        );
    }

    #[test]
    fn nudge_releases_on_key_up() {
        let m = Keymap::default_dj();
        let up = InputEvent::Key {
            text: ".".into(),
            shift: false,
            ctrl: false,
            alt: false,
            down: false,
        };
        assert_eq!(
            m.resolve("deck-a", &up),
            Some(Command::DeckBend {
                deck: Deck::A,
                amount: 0.0
            })
        );
    }

    #[test]
    fn midi_cc_scales_into_the_command() {
        let m = Keymap {
            bindings: vec![Binding {
                context: String::new(),
                on: Trigger::cc(0, 8),
                action: serde_json::json!({"op": "dj.crossfader", "position": "{value}"}),
                range: Some((0.0, 1.0)),
                soft_takeover: false,
                source: None,
            }],
        };
        assert_eq!(
            m.resolve(
                "anything",
                &InputEvent::MidiCc {
                    channel: 0,
                    cc: 8,
                    value: 127
                }
            ),
            Some(Command::Crossfader { position: 1.0 })
        );
        let cmd = m.resolve(
            "anything",
            &InputEvent::MidiCc {
                channel: 0,
                cc: 8,
                value: 64,
            },
        );
        assert!(
            matches!(cmd, Some(Command::Crossfader { position }) if (position - 0.5039).abs() < 1e-3)
        );
    }

    #[test]
    fn a_controller_knob_turns_a_stem_like_the_eq() {
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::cc(0, 20),
                action: serde_json::json!({"op": "dj.stem", "deck": "{deck}", "stem": "vocals",
                    "value": {"parameter": "knob", "value": "{value}"}}),
                range: Some((-1.0, 1.0)),
                soft_takeover: false,
                source: None,
            }],
        };
        let at = |value| match m.resolve(
            "deck-b",
            &InputEvent::MidiCc {
                channel: 0,
                cc: 20,
                value,
            },
        ) {
            Some(Command::StemControl {
                deck: Deck::B,
                stem,
                value,
            }) if stem == "vocals" => value.gain(),
            other => panic!("{other:?}"),
        };
        assert_eq!(at(0), Some(0.0));
        // The middle of a 0…127 knob is its own level.
        assert_eq!(at(64), Some(1.0));
        assert_eq!(at(127), Some(2.0));
    }

    #[test]
    fn keymaps_round_trip_as_json() {
        let m = Keymap::default_dj();
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Keymap>(&j).unwrap(), m);
    }

    #[test]
    fn relative_encodings_decode_to_signed_steps() {
        use RelativeMode::*;
        // Two's complement: the whole field is the signed value
        // (Mixxx `diff`/`selectknob`/`hercjog`).
        assert_eq!(TwosComplement.decode(0), 0);
        assert_eq!(TwosComplement.decode(1), 1);
        assert_eq!(TwosComplement.decode(63), 63);
        assert_eq!(TwosComplement.decode(64), -64);
        assert_eq!(TwosComplement.decode(127), -1);
        // Offset binary: 64 is the detent (Mixxx `rot64`/`spread64`).
        assert_eq!(OffsetBinary.decode(64), 0);
        assert_eq!(OffsetBinary.decode(65), 1);
        assert_eq!(OffsetBinary.decode(63), -1);
        assert_eq!(OffsetBinary.decode(0), -64);
        assert_eq!(OffsetBinary.decode(127), 63);
        // Signed bit: amount in bits 0-5, sign in bit 6.
        assert_eq!(SignedBit.decode(0x00), 0);
        assert_eq!(SignedBit.decode(0x01), 1);
        assert_eq!(SignedBit.decode(0x3F), 63);
        assert_eq!(SignedBit.decode(0x41), -1);
        assert_eq!(SignedBit.decode(0x7F), -63);
    }

    #[test]
    fn mixxx_option_names_map_onto_the_three_decodings() {
        // The table a Mixxx importer depends on: option name → decoding and
        // the byte that means "one step up" on that hardware. It lives here so
        // the mapping is tested rather than asserted in prose.
        use RelativeMode::*;
        let table = [
            ("diff", TwosComplement, 1.0, 1u8, 127u8),
            ("selectknob", TwosComplement, 1.0, 1, 127),
            ("hercjog", TwosComplement, 1.0, 1, 127),
            ("hercjogfast", TwosComplement, 3.0, 1, 127),
            ("rot64", OffsetBinary, 1.0, 65, 63),
            ("rot64fast", OffsetBinary, 1.0, 65, 63),
            ("spread64", OffsetBinary, 1.0, 65, 63),
        ];
        for (name, mode, step, up, down) in table {
            assert_eq!(mode.decode(up), 1, "{name} one step up");
            assert_eq!(mode.decode(down), -1, "{name} one step down");
            assert!(step > 0.0, "{name} step");
        }
        // Two's complement and offset binary disagree on the same byte, which
        // is why the profile has to say which one the hardware uses.
        assert_ne!(TwosComplement.decode(64), OffsetBinary.decode(64));
    }

    #[test]
    fn a_jog_tick_reaches_the_scratch_command_as_a_number() {
        // What an imported mapping writes: the tick's value is the movement,
        // measured from wherever the deck is. `{value}` has to arrive as a
        // number, not a string, or the engine cannot read the command at all.
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::Cc {
                    channel: 0,
                    cc: 0x21,
                    relative: Some(RelativeMode::TwosComplement),
                    step: 0.01,
                },
                action: serde_json::json!({
                    "op":"dj.scratch","deck":"{deck}","delta":"{value}","velocity":0.0
                }),
                range: None,
                soft_takeover: false,
                source: None,
            }],
        };
        let at = |value| {
            m.resolve(
                "deck-a",
                &InputEvent::MidiCc {
                    channel: 0,
                    cc: 0x21,
                    value,
                },
            )
        };
        assert_eq!(
            at(1),
            Some(Command::DeckScratch {
                deck: Deck::A,
                seconds: None,
                delta: Some(0.01),
                velocity: 0.0,
            })
        );
        assert_eq!(
            at(127),
            Some(Command::DeckScratch {
                deck: Deck::A,
                seconds: None,
                delta: Some(-0.01),
                velocity: 0.0,
            })
        );
        // No movement is not a command: a still wheel cannot hold the deck.
        assert_eq!(at(0), None);
        // A wheel's raw 64 is a *large* negative move in this encoding, not a
        // centre. It is bounded by the command's own ±60 s limit rather than
        // being treated as a detent.
        assert_eq!(
            at(64),
            Some(Command::DeckScratch {
                deck: Deck::A,
                seconds: None,
                delta: Some(-0.64),
                velocity: 0.0,
            })
        );
    }

    #[test]
    fn a_platter_release_ends_the_scratch() {
        // The other half of the pair: Mixxx's platter touch is momentary, and
        // its release is what takes the deck out of the hand.
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::Note {
                    channel: 0,
                    note: 0x30,
                    release: true,
                },
                action: serde_json::json!({"op":"dj.scratch_end","deck":"{deck}"}),
                range: None,
                soft_takeover: false,
                source: None,
            }],
        };
        let note = |velocity| {
            m.resolve(
                "deck-a",
                &InputEvent::MidiNote {
                    channel: 0,
                    note: 0x30,
                    velocity,
                },
            )
        };
        // The press is not the binding's business: the wheel's first tick is
        // what starts the scratch.
        assert_eq!(note(127), None);
        assert_eq!(
            note(0),
            Some(Command::DeckScratchEnd { deck: Deck::A }),
            "the platter's release has to reach the deck"
        );
    }

    #[test]
    fn a_relative_encoder_dispatches_a_delta_and_skips_its_detent() {
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::Cc {
                    channel: 0,
                    cc: 0x21,
                    relative: Some(RelativeMode::TwosComplement),
                    step: 0.01,
                },
                action: serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":"{value}"}),
                range: None,
                soft_takeover: false,
                source: None,
            }],
        };
        let at = |value| {
            m.resolve(
                "deck-a",
                &InputEvent::MidiCc {
                    channel: 0,
                    cc: 0x21,
                    value,
                },
            )
        };
        assert_eq!(
            at(1),
            Some(Command::DeckBend {
                deck: Deck::A,
                amount: 0.01
            })
        );
        assert_eq!(
            at(127),
            Some(Command::DeckBend {
                deck: Deck::A,
                amount: -0.01
            })
        );
        // A centred relative control reports no movement.
        assert_eq!(at(0), None, "no movement is not a command");
    }

    #[test]
    fn a_fourteen_bit_control_scales_over_its_full_range() {
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::Cc14 {
                    channel: 0,
                    cc: 0x10,
                },
                action: serde_json::json!({"op":"dj.jump","deck":"{deck}","beats":"{value}"}),
                range: Some((-16.0, 16.0)),
                soft_takeover: false,
                source: None,
            }],
        };
        let at = |value| match m.resolve(
            "deck-b",
            &InputEvent::MidiCc14 {
                channel: 0,
                cc: 0x10,
                value,
            },
        ) {
            Some(Command::DeckJump {
                deck: Deck::B,
                beats,
            }) => beats,
            other => panic!("{other:?}"),
        };
        assert_eq!(at(0), -16.0);
        assert_eq!(at(16383), 16.0);
        // 16383 steps have no exact centre: 8192 is half a step high.
        assert!(
            (at(8192) - 0.0).abs() < 0.01,
            "the middle is roughly no jump: {}",
            at(8192)
        );
        // The LSB alone is a different trigger, not a 14-bit value.
        assert!(
            m.resolve(
                "deck-b",
                &InputEvent::MidiCc14 {
                    channel: 0,
                    cc: 0x30,
                    value: 8192
                }
            )
            .is_none()
        );
    }

    #[test]
    fn a_pitch_bend_is_symmetric_about_its_centre() {
        let m = Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on: Trigger::PitchBend { channel: 0 },
                action: serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":"{value}"}),
                range: None,
                soft_takeover: false,
                source: None,
            }],
        };
        let at = |value| match m.resolve("deck-a", &InputEvent::MidiPitchBend { channel: 0, value })
        {
            Some(Command::DeckBend {
                deck: Deck::A,
                amount,
            }) => amount,
            other => panic!("{other:?}"),
        };
        assert!((at(0) - 0.0).abs() < 1e-6, "centred is no bend");
        assert!((at(8191) - 1.0).abs() < 1e-3);
        assert!((at(-8192) + 1.0).abs() < 1e-3);
        // Another channel's bend is not this binding's.
        assert!(
            m.resolve(
                "deck-a",
                &InputEvent::MidiPitchBend {
                    channel: 1,
                    value: 0
                }
            )
            .is_none()
        );
    }

    #[test]
    fn a_program_change_binding_takes_no_value() {
        let m = Keymap {
            bindings: vec![Binding {
                context: String::new(),
                on: Trigger::Program {
                    channel: 2,
                    program: 7,
                },
                action: serde_json::json!({"op":"dj.crossfader","position":0.5}),
                range: None,
                soft_takeover: false,
                source: None,
            }],
        };
        assert_eq!(
            m.resolve(
                "anywhere",
                &InputEvent::MidiProgram {
                    channel: 2,
                    program: 7
                }
            ),
            Some(Command::Crossfader { position: 0.5 })
        );
        assert!(
            m.resolve(
                "anywhere",
                &InputEvent::MidiProgram {
                    channel: 2,
                    program: 8
                }
            )
            .is_none()
        );
    }

    #[test]
    fn the_new_triggers_round_trip_as_json_and_json_without_options_still_loads() {
        let m = Keymap {
            bindings: vec![
                Binding {
                    context: String::new(),
                    on: Trigger::Cc {
                        channel: 0,
                        cc: 1,
                        relative: Some(RelativeMode::SignedBit),
                        step: 0.5,
                    },
                    action: serde_json::json!({"op":"dj.crossfader_step","delta":"{value}"}),
                    range: None,
                    soft_takeover: false,
                    source: None,
                },
                Binding {
                    context: String::new(),
                    on: Trigger::Cc14 {
                        channel: 1,
                        cc: 0x10,
                    },
                    action: serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                    range: None,
                    soft_takeover: false,
                    source: None,
                },
                Binding {
                    context: String::new(),
                    on: Trigger::PitchBend { channel: 0 },
                    action: serde_json::json!({"op":"dj.crossfader","position":0.5}),
                    range: None,
                    soft_takeover: false,
                    source: None,
                },
            ],
        };
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Keymap>(&j).unwrap(), m);
        // A controller file written before relative/14-bit support: `step`
        // defaults, so nothing needs migrating.
        let old: Keymap = serde_json::from_str(
            r#"{"bindings":[{"on":{"kind":"cc","channel":0,"cc":7},
                "action":{"op":"dj.crossfader","position":"{value}"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            old.bindings[0].on,
            Trigger::Cc {
                channel: 0,
                cc: 7,
                relative: None,
                step: 1.0
            }
        );
    }
}

#[cfg(test)]
mod hid_field_tests {
    use super::*;
    use serde_json::json;

    fn keymap(on: Trigger, action: Value, range: Option<(f64, f64)>) -> Keymap {
        Keymap {
            bindings: vec![Binding {
                context: String::new(),
                on,
                action,
                range,
                soft_takeover: false,
                source: None,
            }],
        }
    }

    fn field(report: u8, offset: u16, width: u8, mask: u32, value: i32) -> InputEvent {
        InputEvent::HidField {
            report,
            offset,
            width,
            mask,
            value,
        }
    }

    /// A one-byte field: a button, or a value read as one.
    fn bit(report: u8, offset: u16, mask: u32, value: i32) -> InputEvent {
        field(report, offset, 1, mask, value)
    }

    /// A button in a byte shared with seven others, pressed and released.
    #[test]
    fn a_button_in_a_shared_byte_fires_on_press_and_only_press() {
        let press = keymap(
            Trigger::HidField {
                report: 0x01,
                offset: 0x0D,
                width: 1,
                mask: 0x10,
                field: HidFieldKind::Button { release: false },
            },
            json!({"op": "dj.transport", "deck": "{deck}", "action": "toggle"}),
            None,
        );
        assert!(press.resolve("deck-a", &bit(0x01, 0x0D, 0x10, 1)).is_some());
        assert!(
            press.resolve("deck-a", &bit(0x01, 0x0D, 0x10, 0)).is_none(),
            "the release must not fire a press binding"
        );
        // A neighbouring button in the same byte is a different field.
        assert!(press.resolve("deck-a", &bit(0x01, 0x0D, 0x20, 1)).is_none());
        assert!(press.resolve("deck-a", &bit(0x02, 0x0D, 0x10, 1)).is_none());
    }

    #[test]
    fn a_release_binding_fires_on_release_only() {
        let release = keymap(
            Trigger::HidField {
                report: 0x01,
                offset: 0x0D,
                width: 1,
                mask: 0x10,
                field: HidFieldKind::Button { release: true },
            },
            json!({"op": "dj.transport", "deck": "{deck}", "action": "pause"}),
            None,
        );
        assert!(release.resolve("deck-a", &bit(0x01, 0x0D, 0x10, 0)).is_some());
        assert!(release.resolve("deck-a", &bit(0x01, 0x0D, 0x10, 1)).is_none());
    }

    /// A 12-bit fader: 16…4080 is the device's own range, and the binding
    /// normalises it through `range`.
    #[test]
    fn an_axis_normalises_the_devices_own_range() {
        let fader = keymap(
            Trigger::HidField {
                report: 0x02,
                offset: 0x2F,
                width: 2,
                mask: 0xFFFF,
                field: HidFieldKind::Axis { min: 16, max: 4080 },
            },
            json!({"op": "dj.crossfader", "position": "{value}"}),
            None,
        );
        match fader.resolve("dj-mixer", &field(0x02, 0x2F, 2, 0xFFFF, 16)) {
            Some(Command::Crossfader { position: p }) => assert!((p - 0.0).abs() < 1e-9, "{p}"),
            other => panic!("bottom of the range: {other:?}"),
        }
        match fader.resolve("dj-mixer", &field(0x02, 0x2F, 2, 0xFFFF, 4080)) {
            Some(Command::Crossfader { position: p }) => assert!((p - 1.0).abs() < 1e-9, "{p}"),
            other => panic!("top of the range: {other:?}"),
        }
        match fader.resolve("dj-mixer", &field(0x02, 0x2F, 2, 0xFFFF, 2048)) {
            Some(Command::Crossfader { position: p }) => {
                assert!((p - 0.5).abs() < 0.01, "the middle: {p}")
            }
            other => panic!("middle of the range: {other:?}"),
        }
    }

    /// A jog wheel reports movement, and a movement of zero is not a change.
    #[test]
    fn a_relative_field_yields_movement_and_drops_none_of_it() {
        let jog = keymap(
            Trigger::HidField {
                report: 0x01,
                offset: 0x01,
                width: 4,
                mask: 0xFFFF_FFFF,
                field: HidFieldKind::Relative {
                    step: 1.0,
                    release: false,
                },
            },
            json!({"op": "dj.scratch", "deck": "{deck}", "delta": "{value}"}),
            None,
        );
        assert!(jog.resolve("deck-a", &field(0x01, 0x01, 4, 0xFFFF_FFFF, 0)).is_none());
        assert!(jog.resolve("deck-a", &field(0x01, 0x01, 4, 0xFFFF_FFFF, 7)).is_some());
        assert!(jog.resolve("deck-a", &field(0x01, 0x01, 4, 0xFFFF_FFFF, -7)).is_some());
    }

    /// A platter reports a hand as a value *inside* a window; the transport
    /// derives the press, so the binding matches it as a button.
    #[test]
    fn a_window_field_fires_on_press_and_on_release() {
        let touch = keymap(
            Trigger::HidField {
                report: 0x02,
                offset: 0x0D,
                width: 2,
                mask: 0xFFFF,
                field: HidFieldKind::Window {
                    min: 3000,
                    max: 4095,
                    release: false,
                },
            },
            json!({"op": "dj.scratch", "deck": "{deck}", "delta": 1.0}),
            None,
        );
        assert!(touch.resolve("deck-a", &field(0x02, 0x0D, 2, 0xFFFF, 1)).is_some());
        assert!(touch.resolve("deck-a", &field(0x02, 0x0D, 2, 0xFFFF, 0)).is_none());
    }

    /// The trigger is profile data, so it round-trips through JSON as written.
    #[test]
    fn a_field_trigger_round_trips_through_the_profile() {
        let text = r#"{"kind":"hidfield","report":1,"offset":13,"width":1,"mask":16,"field":{"as":"button","release":false}}"#;
        let t: Trigger = serde_json::from_str(text).expect("parses");
        match &t {
            Trigger::HidField {
                report: 1,
                offset: 13,
                width: 1,
                mask: 16,
                field: HidFieldKind::Button { release: false },
            } => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(serde_json::to_string(&t).unwrap(), text);
    }

    #[test]
    fn a_field_width_outside_one_two_four_never_resolves() {
        let odd = keymap(
            Trigger::HidField {
                report: 0x02,
                offset: 0x2F,
                width: 3,
                mask: 0xFFFFFF,
                field: HidFieldKind::Axis { min: 0, max: 4095 },
            },
            json!({"op": "dj.crossfader", "position": "{value}"}),
            None,
        );
        assert!(odd.resolve("dj-mixer", &field(0x02, 0x2F, 3, 0xFFFFFF, 2048)).is_none());
    }
}

#[cfg(test)]
mod eq_detent_tests {
    use super::*;
    fn knob(on: Trigger) -> Keymap {
        Keymap {
            bindings: vec![Binding {
                context: "deck".into(),
                on,
                action: serde_json::json!({"op":"dj.eq_knob","deck":"{deck}","band":"low","value":"{value}"}),
                range: Some((-1.0, 1.0)),
                soft_takeover: false,
                source: None,
            }],
        }
    }
    fn value(map: &Keymap, event: InputEvent) -> f32 {
        match map.resolve("deck-a", &event) {
            Some(Command::DeckEqKnob { value, .. }) => value,
            other => panic!("unexpected command: {other:?}"),
        }
    }
    #[test]
    fn midi_eq_has_a_true_neutral_at_both_middle_steps() {
        let map = knob(Trigger::cc(0, 20));
        let at = |v| {
            value(
                &map,
                InputEvent::MidiCc {
                    channel: 0,
                    cc: 20,
                    value: v,
                },
            )
        };
        assert_eq!(at(63), 0.0);
        assert_eq!(at(64), 0.0);
        assert!(at(62) < 0.0);
        assert!(at(65) > 0.0);
        assert_eq!(at(0), -1.0);
        assert_eq!(at(127), 1.0);
    }
    #[test]
    fn hid_eq_ignores_detent_jitter_but_preserves_nearby_adjustments() {
        let map = knob(Trigger::HidAxis {
            code: 1,
            min: 0,
            max: 4095,
        });
        let at = |v| {
            value(
                &map,
                InputEvent::Hid {
                    event_type: 3,
                    code: 1,
                    value: v,
                },
            )
        };
        for position in [2040, 2047, 2048, 2055] {
            assert_eq!(at(position), 0.0);
        }
        assert!(at(2020) < 0.0);
        assert!(at(2075) > 0.0);
    }

    #[test]
    fn relative_eq_adjustments_keep_their_fine_steps() {
        let map = knob(Trigger::Cc {
            channel: 0,
            cc: 20,
            relative: Some(RelativeMode::TwosComplement),
            step: 0.001,
        });
        assert_eq!(
            value(
                &map,
                InputEvent::MidiCc {
                    channel: 0,
                    cc: 20,
                    value: 1
                }
            ),
            0.001
        );
    }

    #[test]
    fn native_s2_eq_field_uses_the_same_exact_detent() {
        let map = knob(Trigger::HidField {
            report: 2,
            offset: 10,
            width: 2,
            mask: 0xffff,
            field: HidFieldKind::Axis { min: 16, max: 4080 },
        });
        let at = |v| {
            value(
                &map,
                InputEvent::HidField {
                    report: 2,
                    offset: 10,
                    width: 2,
                    mask: 0xffff,
                    value: v,
                },
            )
        };
        for position in [2040, 2047, 2048, 2055] {
            assert_eq!(at(position), 0.0);
        }
        assert!(at(2010) < 0.0);
        assert!(at(2085) > 0.0);
    }
}
