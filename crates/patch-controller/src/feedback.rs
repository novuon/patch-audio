//! Hardware feedback: what the controller lights up, and why it can never
//! become a loop.
//!
//! An output binding follows **authoritative Patch state** — the same values
//! the Mixer, DJ Booth, AI DJ and the remote pages read. There is no separate
//! copy of "what the controller thinks the deck is doing": a play light is on
//! because the deck is playing, whichever source started it.
//!
//! # Why this cannot echo
//!
//! Three independent guards, each tested:
//!
//! 1. **Direction.** Output bytes are written to the device and never fed to
//!    the input parser. There is no path from [`Feedback::poll`] back into
//!    dispatch.
//! 2. **Change detection.** A value equal to the last one sent produces no
//!    bytes at all, so a poll every 50 ms does not become 20 messages a
//!    second per light.
//! 3. **Reflection suppression.** When a control's new value arrived *from
//!    the hardware itself*, the device already knows it: [`Feedback::reflect`]
//!    records that, and the next emission for exactly that source is dropped
//!    once. A device that reflects received MIDI back as input therefore
//!    cannot ping-pong.
//!
//! Level outputs are also rate-limited and coalesced: the newest value lands
//! when the interval allows, older pending values are discarded rather than
//! queued, so a jog wheel's fast updates cannot build a backlog.

use crate::profile::Profile;
use patch_core::command::HOT_CUES;
use patch_core::midi::encode;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The most messages one poll may produce, so a slow device cannot turn a
/// feedback pass into an unbounded burst.
pub const MAX_PER_POLL: usize = 32;
/// Level outputs (VU, position, tempo) are coalesced to this rate.
pub const LEVEL_INTERVAL_MS: u64 = 20;

/// A value on the hardware. 0..1 unless the source says otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum OutputTarget {
    /// A momentary indicator (a play, cue or loop light).
    Note {
        channel: u8,
        note: u8,
        #[serde(default = "full")]
        on_value: u8,
        #[serde(default)]
        off_value: u8,
    },
    /// A continuous indicator: a ring LED, a fader LED ladder, a VU.
    Cc {
        channel: u8,
        cc: u8,
        #[serde(default = "full")]
        max: u8,
    },
    /// A 14-bit indicator, sent as its MSB/LSB pair.
    Cc14 { channel: u8, cc: u8 },
    /// A pitch-bend indicator (some jog rings).
    PitchBend { channel: u8 },
    /// A fixed sequence, sent once per connection in declaration order —
    /// a controller's init handshake, a display clear, a vendor query.
    Sysex { payload: Vec<u8> },
    /// One byte of a raw HID output report: an LED's brightness. A report goes
    /// out whole, so every output sharing one is assembled into a single
    /// message, and a byte no output drives stays as it was.
    HidReport {
        report: u8,
        /// The byte's position, measured after the report id.
        offset: u16,
        /// The report's payload length, so a write is always a whole report.
        len: u16,
        /// What full scale means for this byte — an LED's maximum brightness.
        #[serde(default = "hid_max")]
        max: u8,
    },
}

/// The brightness ceiling a raw HID LED byte reaches at full scale, before a
/// profile says otherwise. The Traktor Kontrol S2's LED report uses 0x00–0x1F.
fn hid_max() -> u8 {
    0x1F
}

fn full() -> u8 {
    127
}

impl OutputTarget {
    fn validate(&self) -> Result<(), String> {
        match self {
            OutputTarget::Note {
                channel,
                note,
                on_value,
                off_value,
            } => {
                if *channel > 15 || *note > 127 || *on_value > 127 || *off_value > 127 {
                    return Err("output note must be channel 0…15, note/values 0…127".into());
                }
            }
            OutputTarget::Cc { channel, cc, max } => {
                if *channel > 15 || *cc > 127 || *max == 0 || *max > 127 {
                    return Err("output CC must be channel 0…15, cc 0…127, max 1…127".into());
                }
            }
            OutputTarget::Cc14 { channel, cc } => {
                if *channel > 15 || *cc > 111 {
                    return Err("output 14-bit CC must be channel 0…15, cc 0…111".into());
                }
            }
            OutputTarget::PitchBend { channel } => {
                if *channel > 15 {
                    return Err("output pitch bend channel must be 0…15".into());
                }
            }
            OutputTarget::Sysex { payload } => {
                if payload.len() > 512 {
                    return Err("output SysEx payload must be at most 512 bytes".into());
                }
            }
            OutputTarget::HidReport {
                offset, len, max, ..
            } => {
                if *len == 0
                    || usize::from(*len) > crate::hid::MAX_REPORT
                    || *offset >= *len
                    || *max == 0
                {
                    return Err(format!(
                        "output HID report must be 1…{} bytes with the byte inside it, \
                         and a non-zero maximum",
                        crate::hid::MAX_REPORT
                    ));
                }
            }
        }
        Ok(())
    }

    /// A key for change detection: two bytes for a toggle, a number for a
    /// level. Round-tripping through the encoder would work but would encode
    /// on every poll.
    fn slot(&self, value: f64) -> Slot {
        match self {
            OutputTarget::Note {
                on_value,
                off_value,
                ..
            } => Slot::Flag(if value >= 0.5 { *on_value } else { *off_value }),
            OutputTarget::Cc { max, .. } => {
                Slot::Level((value.clamp(0.0, 1.0) * f64::from(*max)).round() as u8 as u16)
            }
            OutputTarget::Cc14 { .. } => {
                Slot::Level((value.clamp(0.0, 1.0) * 16383.0).round() as u16)
            }
            OutputTarget::PitchBend { .. } => {
                Slot::Bend((value.clamp(-1.0, 1.0) * 8191.0).round() as i16)
            }
            OutputTarget::Sysex { .. } => Slot::Flag(0),
            OutputTarget::HidReport { max, .. } => {
                Slot::Level((value.clamp(0.0, 1.0) * f64::from(*max)).round() as u16)
            }
        }
    }

    /// The bytes for a slot. `None` when the slot is unchanged (`Some` means
    /// "send this").
    fn message(&self, slot: Slot) -> Vec<u8> {
        match (self, slot) {
            (
                OutputTarget::Note {
                    channel,
                    note,
                    off_value,
                    ..
                },
                Slot::Flag(value),
            ) => {
                if value == *off_value {
                    encode::note_off(*channel, *note, 0).to_vec()
                } else {
                    encode::note_on(*channel, *note, value).to_vec()
                }
            }
            (OutputTarget::Cc { channel, cc, .. }, Slot::Level(value)) => {
                encode::cc(*channel, *cc, value.min(127) as u8).to_vec()
            }
            (OutputTarget::Cc14 { channel, cc }, Slot::Level(value)) => {
                encode::cc14(*channel, *cc, value).to_vec()
            }
            (OutputTarget::PitchBend { channel }, Slot::Bend(value)) => {
                encode::pitch_bend(*channel, value).to_vec()
            }
            (OutputTarget::Sysex { payload }, _) => encode::sysex(payload),
            // A raw HID byte is not a message of its own: `poll` assembles the
            // report it belongs to, because the report goes out whole.
            (OutputTarget::HidReport { .. }, _) => Vec::new(),
            // A slot and a target that cannot go together: encode nothing
            // rather than something wrong. Slots are built from the target in
            // `slot`, so this is unreachable in practice.
            _ => Vec::new(),
        }
    }
}

/// A hardware value, in the smallest form that identifies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Flag(u8),
    Level(u16),
    Bend(i16),
}

/// Which authoritative state an output follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Crossfader,
    MasterVu,
    DeckFader(usize),
    DeckLoaded(usize),
    DeckPlay(usize),
    DeckPfl(usize),
    DeckLoop(usize),
    DeckKeylock(usize),
    DeckSlip(usize),
    DeckQuantize(usize),
    DeckTempo(usize),
    DeckProgress(usize),
    DeckVu(usize),
    /// Whether hot cue `n` (1 … 8) is set on the deck: Mixxx's `hotcue_N_enabled`
    /// and `hotcue_N_status`, which 25 mappings drive LEDs from.
    DeckHotCue(usize, usize),
    /// Whether the deck's main cue is set: Mixxx's `cue_indicator`.
    DeckCueSet(usize),
    /// Whether the engaged loop is `beats` long: Mixxx's `beatloop_N_enabled`, one
    /// indicator per loop length.
    DeckBeatloop(usize, u32),
    /// Whether an EQ band is killed, 0 low … 2 high: Mixxx's `filterXKill`
    /// indicators, which follow the same state the kill command sets.
    DeckKill(usize, usize),
}

impl Source {
    /// The spelling used in a profile. Kept in one place so the parser, the
    /// error message and the documentation cannot drift apart.
    pub const NAMES: &'static [&'static str] = &[
        "crossfader",
        "master.vu",
        "deck.<a|b|c|d>.fader",
        "deck.<a|b|c|d>.loaded",
        "deck.<a|b|c|d>.play",
        "deck.<a|b|c|d>.pfl",
        "deck.<a|b|c|d>.loop",
        "deck.<a|b|c|d>.keylock",
        "deck.<a|b|c|d>.slip",
        "deck.<a|b|c|d>.quantize",
        "deck.<a|b|c|d>.tempo",
        "deck.<a|b|c|d>.progress",
        "deck.<a|b|c|d>.vu",
        "deck.<a|b|c|d>.cue1 … cue8",
        "deck.<a|b|c|d>.cue",
        "deck.<a|b|c|d>.beatloop.<1|2|4|8|16|32>",
        "deck.<a|b|c|d>.kill.<low|mid|high>",
    ];

    /// The loop lengths an indicator can distinguish: the musically meaningful
    /// ones, which is also what the imported corpus names.
    pub const BEATLOOPS: &'static [u32] = &[1, 2, 4, 8, 16, 32];

    /// Whether the value is a level (0…1, rate-limited and coalesced) or an
    /// on/off state (sent on change).
    pub fn is_level(self) -> bool {
        matches!(
            self,
            Source::Crossfader
                | Source::MasterVu
                | Source::DeckFader(_)
                | Source::DeckTempo(_)
                | Source::DeckProgress(_)
                | Source::DeckVu(_)
        )
    }

    pub fn deck(self) -> Option<usize> {
        match self {
            Source::DeckFader(d)
            | Source::DeckLoaded(d)
            | Source::DeckPlay(d)
            | Source::DeckPfl(d)
            | Source::DeckLoop(d)
            | Source::DeckKeylock(d)
            | Source::DeckSlip(d)
            | Source::DeckQuantize(d)
            | Source::DeckTempo(d)
            | Source::DeckProgress(d)
            | Source::DeckVu(d)
            | Source::DeckHotCue(d, _)
            | Source::DeckCueSet(d)
            | Source::DeckBeatloop(d, _)
            | Source::DeckKill(d, _) => Some(d),
            Source::Crossfader | Source::MasterVu => None,
        }
    }

    /// Canonical spelling, for the monitor and for tests.
    pub fn name(self) -> String {
        match self {
            Source::Crossfader => "crossfader".into(),
            Source::MasterVu => "master.vu".into(),
            Source::DeckHotCue(d, n) => {
                let letter = ["a", "b", "c", "d"][d.min(3)];
                format!("deck.{letter}.cue{n}")
            }
            Source::DeckCueSet(d) => {
                let letter = ["a", "b", "c", "d"][d.min(3)];
                format!("deck.{letter}.cue")
            }
            Source::DeckBeatloop(d, beats) => {
                let letter = ["a", "b", "c", "d"][d.min(3)];
                format!("deck.{letter}.beatloop.{beats}")
            }
            Source::DeckKill(d, band) => {
                let letter = ["a", "b", "c", "d"][d.min(3)];
                let band = ["low", "mid", "high"][band.min(2)];
                format!("deck.{letter}.kill.{band}")
            }
            other => {
                let deck = ["a", "b", "c", "d"][other.deck().unwrap_or(0).min(3)];
                let what = match other {
                    Source::DeckFader(_) => "fader",
                    Source::DeckLoaded(_) => "loaded",
                    Source::DeckPlay(_) => "play",
                    Source::DeckPfl(_) => "pfl",
                    Source::DeckLoop(_) => "loop",
                    Source::DeckKeylock(_) => "keylock",
                    Source::DeckSlip(_) => "slip",
                    Source::DeckQuantize(_) => "quantize",
                    Source::DeckTempo(_) => "tempo",
                    Source::DeckProgress(_) => "progress",
                    Source::DeckVu(_) => "vu",
                    Source::Crossfader
                    | Source::MasterVu
                    | Source::DeckHotCue(..)
                    | Source::DeckCueSet(_)
                    | Source::DeckBeatloop(_, _)
                    | Source::DeckKill(_, _) => unreachable!(),
                };
                format!("deck.{deck}.{what}")
            }
        }
    }
}

impl std::str::FromStr for Source {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let deck = |letter: &str| -> Result<usize, String> {
            match letter {
                "a" => Ok(0),
                "b" => Ok(1),
                "c" => Ok(2),
                "d" => Ok(3),
                other => Err(format!("unknown deck '{other}'")),
            }
        };
        match text {
            "crossfader" => return Ok(Source::Crossfader),
            "master.vu" => return Ok(Source::MasterVu),
            _ => {}
        }
        // The deck-letter word at the end of these is the part `name` writes, so
        // `deck.a.beatloop.8` parses back to the thing that lights that pad.
        let parts: Vec<&str> = text.split('.').collect();
        if let ["deck", letter, group, what] = parts[..] {
            let d = deck(letter)?;
            return match group {
                "beatloop" => {
                    let beats: u32 = what
                        .parse()
                        .map_err(|_| format!("'{what}' is not a loop length"))?;
                    if !Source::BEATLOOPS.contains(&beats) {
                        return Err(format!(
                            "a loop indicator names one of {} beats, not '{what}'",
                            Source::BEATLOOPS
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    Ok(Source::DeckBeatloop(d, beats))
                }
                "kill" => Ok(Source::DeckKill(
                    d,
                    match what {
                        "low" => 0,
                        "mid" => 1,
                        "high" => 2,
                        other => return Err(format!("no EQ band is called '{other}'")),
                    },
                )),
                other => Err(format!("unknown deck group '{other}'")),
            };
        }
        if let ["deck", letter, what] = parts[..] {
            let d = deck(letter)?;
            return Ok(match what {
                "fader" => Source::DeckFader(d),
                "loaded" => Source::DeckLoaded(d),
                "play" => Source::DeckPlay(d),
                "pfl" => Source::DeckPfl(d),
                "loop" => Source::DeckLoop(d),
                "keylock" => Source::DeckKeylock(d),
                "slip" => Source::DeckSlip(d),
                "quantize" => Source::DeckQuantize(d),
                "tempo" => Source::DeckTempo(d),
                "progress" => Source::DeckProgress(d),
                "vu" => Source::DeckVu(d),
                // The main cue, as opposed to the numbered hot cues below.
                "cue" => Source::DeckCueSet(d),
                // `cue1` … `cue8`: one indicator per cue, numbered as the command
                // and the hardware number them.
                what if what.starts_with("cue") => {
                    let n: usize = what[3..]
                        .parse()
                        .map_err(|_| format!("unknown deck state '{what}'"))?;
                    if !(1..=HOT_CUES).contains(&n) {
                        return Err(format!(
                            "hot cues are 1 … {HOT_CUES}, so '{what}' names none"
                        ));
                    }
                    Source::DeckHotCue(d, n)
                }
                other => return Err(format!("unknown deck state '{other}'")),
            });
        }
        Err(format!(
            "'{text}' is not a controller state; expected one of {}",
            Source::NAMES.join(", ")
        ))
    }
}

/// One output binding: a hardware indicator and the state it follows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// The hardware indicator.
    pub target: OutputTarget,
    /// The authoritative Patch state it follows, e.g. `deck.a.play`.
    pub source: String,
    /// The source range mapped onto the target's full range. A reversed pair
    /// mirrors it.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// The shortest interval between two updates, overriding the default for
    /// the source. Level outputs default to [`LEVEL_INTERVAL_MS`]; states are
    /// sent on change with no interval.
    #[serde(default)]
    pub min_interval_ms: Option<u64>,
}

impl Output {
    pub fn validate(&self) -> Result<(), String> {
        self.target.validate()?;
        let source = self
            .source
            .parse::<Source>()
            .map_err(|e| format!("output source: {e}"))?;
        let _ = source;
        if self
            .range
            .is_some_and(|(lo, hi)| !lo.is_finite() || !hi.is_finite())
        {
            return Err("output range must be finite".into());
        }
        if self.min_interval_ms.is_some_and(|ms| ms > 60_000) {
            return Err("output min_interval_ms must be at most 60000".into());
        }
        Ok(())
    }
}

/// A deck's authoritative state, as the engine knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckState {
    /// The channel fader, 0 … 1.
    pub fader: f32,
    pub loaded: bool,
    pub playing: bool,
    pub pfl: bool,
    pub loop_on: bool,
    pub keylock: bool,
    pub slip: bool,
    pub quantize: bool,
    /// The tempo as a fraction of the fader's span, −1 … +1.
    pub tempo: f32,
    /// Playhead position as a fraction of the track, 0 … 1.
    pub progress: f32,
    /// Output level, 0 … 1.
    pub vu: f32,
    /// Which of the deck's hot cues are set, so a pad's LED can follow one. Only
    /// the fact that a cue exists: where it sits is not something an indicator
    /// can show.
    pub cues: [bool; HOT_CUES],
    /// Whether the deck's *main* cue is set. A deck starts with its cue at the
    /// track's beginning, so what this reports is a cue placed away from it.
    pub cue_set: bool,
    /// The active loop's length in beats, or `None` when no loop is engaged.
    /// Patch stores the loop as a sample range, so this is derived against the
    /// grid — and a manual loop of some other length simply reports its own.
    pub loop_beats: Option<f32>,
    /// Which EQ bands are killed, in the order low, mid, high. A kill is the
    /// band's own state, the same one the `dj.eq` command sets with no dB.
    pub killed: [bool; 3],
}

/// The authoritative state a feedback pass reads. Every field is owned
/// elsewhere — this is a snapshot, not a second copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControllerState {
    pub crossfader: f32,
    pub master_vu: f32,
    pub decks: Vec<DeckState>,
}

impl ControllerState {
    /// The value of `source`, or `None` when the deck does not exist.
    pub fn value(&self, source: Source) -> Option<f64> {
        let deck = |d: usize| self.decks.get(d).copied();
        Some(match source {
            Source::Crossfader => f64::from(self.crossfader),
            Source::MasterVu => f64::from(self.master_vu),
            Source::DeckFader(d) => f64::from(deck(d)?.fader),
            Source::DeckLoaded(d) => flag(deck(d)?.loaded),
            Source::DeckPlay(d) => flag(deck(d)?.playing),
            Source::DeckPfl(d) => flag(deck(d)?.pfl),
            Source::DeckLoop(d) => flag(deck(d)?.loop_on),
            Source::DeckKeylock(d) => flag(deck(d)?.keylock),
            Source::DeckSlip(d) => flag(deck(d)?.slip),
            Source::DeckQuantize(d) => flag(deck(d)?.quantize),
            Source::DeckHotCue(d, n) => {
                let cues = deck(d)?.cues;
                flag(*cues.get(n.checked_sub(1)?)?)
            }
            Source::DeckCueSet(d) => flag(deck(d)?.cue_set),
            // A hair of tolerance, because the length comes from a sample range
            // divided by the grid's tempo.
            Source::DeckBeatloop(d, beats) => flag(
                deck(d)?
                    .loop_beats
                    .is_some_and(|on| (on - beats as f32).abs() < 0.05),
            ),
            Source::DeckKill(d, band) => flag(*deck(d)?.killed.get(band)?),
            Source::DeckTempo(d) => f64::from(deck(d)?.tempo),
            Source::DeckProgress(d) => f64::from(deck(d)?.progress),
            Source::DeckVu(d) => f64::from(deck(d)?.vu),
        })
    }
}

fn flag(on: bool) -> f64 {
    if on { 1.0 } else { 0.0 }
}

#[derive(Clone, Debug)]
struct Compiled {
    output: Output,
    source: Source,
    /// The last slot written to the device.
    sent: Option<Slot>,
    /// The newest value waiting to go out (level outputs only).
    pending: Option<Slot>,
    at_ms: u64,
    /// A SysEx init sequence goes once per connection.
    sent_once: bool,
}

/// The feedback engine for one configured device.
#[derive(Debug)]
pub struct Feedback {
    compiled: Vec<Compiled>,
    /// Sources whose next emission must be dropped because the hardware
    /// itself caused the change.
    reflected: Vec<Source>,
    /// Counters for the surface, so the rate is observable rather than
    /// claimed.
    pub sent: u64,
    pub suppressed: u64,
    /// The bytes of every raw HID output report this device has driven, by
    /// report id, so setting one LED does not blank the rest of its report.
    reports: Vec<(u8, Vec<u8>)>,
}

/// The buffer for one output report, created at the declared length and grown
/// if a profile declares a longer report than the last one did.
fn report_buffer(reports: &mut Vec<(u8, Vec<u8>)>, report: u8, len: u16) -> &mut Vec<u8> {
    let len = usize::from(len).clamp(1, crate::hid::MAX_REPORT);
    if let Some(i) = reports.iter().position(|(r, _)| *r == report) {
        let buf = &mut reports[i].1;
        if buf.len() < len {
            buf.resize(len, 0);
        }
        return buf;
    }
    reports.push((report, vec![0u8; len]));
    &mut reports.last_mut().expect("just pushed").1
}

impl Feedback {
    /// Compiles a profile's outputs, reporting the first bad one.
    pub fn new(profile: &Profile) -> Result<Self, String> {
        let mut compiled = Vec::with_capacity(profile.outputs.len());
        for output in &profile.outputs {
            let source: Source = output
                .source
                .parse()
                .map_err(|e| format!("{}: {e}", profile.label()))?;
            compiled.push(Compiled {
                output: output.clone(),
                source,
                sent: None,
                pending: None,
                at_ms: 0,
                sent_once: false,
            });
        }
        Ok(Self {
            compiled,
            reflected: Vec::new(),
            sent: 0,
            suppressed: 0,
            reports: Vec::new(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    /// Notes that `source` just changed *because of this device*. The next
    /// emission for that source is dropped once: the hardware already shows
    /// it, and re-sending is what a loop starts from.
    pub fn reflect(&mut self, source: Source) {
        if !self.reflected.contains(&source) && self.reflected.len() < 64 {
            self.reflected.push(source);
        }
    }

    /// Whether `source` is marked as changed by this very device, and so will
    /// have its next emission suppressed. Read by the takeover re-arm pass,
    /// which must not treat the controller's own movement as external.
    pub fn is_reflected(&self, source: Source) -> bool {
        self.reflected.contains(&source)
    }

    /// A new connection: nothing is known to be on the device any more, so
    /// every output is re-sent once (LEDs after a replug), in declaration
    /// order — which is why an init SysEx placed first goes first.
    pub fn reconnect(&mut self) {
        for c in &mut self.compiled {
            c.sent = None;
            c.pending = None;
            c.sent_once = false;
        }
        self.reflected.clear();
    }

    /// The messages to write, given the authoritative state and a monotonic
    /// millisecond clock. Bounded by [`MAX_PER_POLL`]; anything left over is
    /// superseded by the next poll rather than queued.
    pub fn poll(&mut self, state: &ControllerState, now_ms: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        // Taken out for the walk, so the compiled outputs and the report
        // buffers can be borrowed at the same time.
        let mut reports = std::mem::take(&mut self.reports);
        // The raw HID reports this poll changed. Each goes out whole, once.
        let mut dirty: Vec<u8> = Vec::new();
        for c in &mut self.compiled {
            if out.len() + dirty.len() >= MAX_PER_POLL {
                break;
            }
            let Some(raw) = state.value(c.source) else {
                continue;
            };
            if self.reflected.contains(&c.source) {
                // Exactly one suppressed emission, and the value is treated as
                // already on the device.
                c.sent = Some(c.output.target.slot(scale(raw, c.output.range)));
                self.reflected.retain(|s| *s != c.source);
                self.suppressed += 1;
                continue;
            }
            let value = scale(raw, c.output.range);
            if matches!(c.output.target, OutputTarget::Sysex { .. }) {
                if c.sent_once {
                    continue;
                }
                let bytes = c.output.target.message(Slot::Flag(0));
                c.sent_once = true;
                if !bytes.is_empty() {
                    self.sent += 1;
                    out.push(bytes);
                }
                continue;
            }
            let slot = c.output.target.slot(value);
            if c.sent == Some(slot) {
                continue;
            }
            let interval = c.output.min_interval_ms.unwrap_or(if c.source.is_level() {
                LEVEL_INTERVAL_MS
            } else {
                0
            });
            // The first value after a connection is always due, whatever the
            // clock says; the interval only governs later updates.
            let due = c.sent.is_none() || now_ms.saturating_sub(c.at_ms) >= interval;
            match (c.pending, c.sent) {
                // Nothing waiting: this is the newest value.
                (None, _) if due => {}
                (Some(pending), _) if pending == slot && !due => continue,
                _ => {
                    c.pending = Some(slot);
                    if !due {
                        continue;
                    }
                }
            }
            let slot = c.pending.take().unwrap_or(slot);
            if let OutputTarget::HidReport {
                report,
                offset,
                len,
                max,
            } = &c.output.target
            {
                let (report, offset, len, max) = (*report, *offset, *len, *max);
                let buf = report_buffer(&mut reports, report, len);
                let byte = match slot {
                    Slot::Level(v) => v.min(u16::from(max)) as u8,
                    Slot::Flag(v) => v.min(max),
                    Slot::Bend(_) => 0,
                };
                if let Some(b) = buf.get_mut(usize::from(offset)) {
                    *b = byte;
                }
                if !dirty.contains(&report) {
                    dirty.push(report);
                }
                c.sent = Some(slot);
                c.at_ms = now_ms;
                self.sent += 1;
                continue;
            }
            let bytes = c.output.target.message(slot);
            if bytes.is_empty() {
                continue;
            }
            c.sent = Some(slot);
            c.at_ms = now_ms;
            self.sent += 1;
            out.push(bytes);
        }
        for report in dirty {
            if let Some((_, buf)) = reports.iter().find(|(r, _)| *r == report) {
                let mut message = Vec::with_capacity(buf.len() + 1);
                message.push(report);
                message.extend_from_slice(buf);
                out.push(message);
            }
        }
        self.reports = reports;
        out
    }
}

pub fn unit(value: f64, range: Option<(f64, f64)>) -> f64 {
    scale(value, range)
}

/// Maps a source value in `range` (default 0…1) onto 0…1.
fn scale(value: f64, range: Option<(f64, f64)>) -> f64 {
    let (lo, hi) = range.unwrap_or((0.0, 1.0));
    if (hi - lo).abs() < f64::EPSILON {
        return 0.0;
    }
    ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// The default rate for level outputs, as a Duration, for callers that want
/// to size a poll interval.
pub fn level_interval() -> Duration {
    Duration::from_millis(LEVEL_INTERVAL_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(target: OutputTarget, source: &str) -> Output {
        Output {
            target,
            source: source.into(),
            range: None,
            min_interval_ms: None,
        }
    }

    fn profile(outputs: Vec<Output>) -> Profile {
        Profile {
            id: "test".into(),
            name: "Test".into(),
            manufacturer: String::new(),
            protocol: crate::profile::Protocol::Any,
            attribution: String::new(),
            version: 1,
            device: Some("/dev/snd/midiC9D0".into()),
            r#match: Default::default(),
            keymap: Default::default(),
            outputs,
        }
    }

    fn state(playing: bool, crossfader: f32) -> ControllerState {
        ControllerState {
            crossfader,
            master_vu: 0.0,
            decks: vec![
                DeckState {
                    loaded: true,
                    playing,
                    ..Default::default()
                },
                DeckState::default(),
            ],
        }
    }

    fn note(channel: u8, note: u8) -> OutputTarget {
        OutputTarget::Note {
            channel,
            note,
            on_value: 127,
            off_value: 0,
        }
    }

    #[test]
    fn source_names_round_trip_and_bad_ones_explain_themselves() {
        for name in [
            "crossfader",
            "master.vu",
            "deck.a.play",
            "deck.b.pfl",
            "deck.a.progress",
            "deck.d.vu",
            "deck.c.tempo",
            "deck.a.cue1",
            "deck.d.cue8",
            "deck.b.cue",
            "deck.a.beatloop.8",
            "deck.c.kill.mid",
        ] {
            let source: Source = name.parse().unwrap();
            assert_eq!(source.name(), name);
        }
        let err = "deck.a.speed".parse::<Source>().unwrap_err();
        assert!(err.contains("unknown deck state"), "{err}");
        let err = "deck.z.play".parse::<Source>().unwrap_err();
        assert!(err.contains("unknown deck"), "{err}");
        let err = "platter".parse::<Source>().unwrap_err();
        assert!(err.contains("is not a controller state"), "{err}");
    }

    #[test]
    fn the_main_cue_the_loop_length_and_the_kills_have_their_own_sources() {
        let mut state = ControllerState::default();
        state.decks.push(DeckState {
            cue_set: true,
            loop_beats: Some(8.0),
            killed: [true, false, true],
            ..Default::default()
        });
        assert_eq!(state.value("deck.a.cue".parse().unwrap()), Some(1.0));
        // The loop indicator for the length that is on, and only that one.
        assert_eq!(state.value("deck.a.beatloop.8".parse().unwrap()), Some(1.0));
        assert_eq!(state.value("deck.a.beatloop.4".parse().unwrap()), Some(0.0));
        assert_eq!(state.value("deck.a.kill.low".parse().unwrap()), Some(1.0));
        assert_eq!(
            state.value("deck.a.kill.mid".parse().unwrap()),
            Some(0.0),
            "a band that is not killed stays dark"
        );
        // A deck with no loop on reports no length at all, so every indicator for
        // one is dark rather than the nearest match.
        let mut idle = ControllerState::default();
        idle.decks.push(DeckState::default());
        assert_eq!(idle.value("deck.a.cue".parse().unwrap()), Some(0.0));
        assert_eq!(idle.value("deck.a.beatloop.8".parse().unwrap()), Some(0.0));
        assert_eq!(idle.value("deck.a.kill.high".parse().unwrap()), Some(0.0));
        // Spellings that name nothing are refused by name.
        for bad in [
            "deck.a.beatloop.3",
            "deck.a.beatloop.zero",
            "deck.a.kill.purple",
            "deck.a.fader.low",
        ] {
            assert!(bad.parse::<Source>().is_err(), "{bad}");
        }
        // And a deck that is not there resolves to nothing.
        assert_eq!(state.value("deck.c.cue".parse().unwrap()), None);
    }

    #[test]
    fn a_cue_source_follows_the_cue_that_is_set_and_nothing_else() {
        let mut state = ControllerState::default();
        let mut cues = [false; HOT_CUES];
        cues[1] = true;
        state.decks.push(DeckState {
            cues,
            ..Default::default()
        });
        let cue2: Source = "deck.a.cue2".parse().unwrap();
        assert_eq!(
            state.value(cue2),
            Some(1.0),
            "cue 2 is set, so the pad lights"
        );
        assert_eq!(
            state.value("deck.a.cue3".parse().unwrap()),
            Some(0.0),
            "and a cue that is not set stays dark"
        );
        assert_eq!(cue2.name(), "deck.a.cue2");
        assert_eq!(cue2.deck(), Some(0));
        assert!(!cue2.is_level(), "a cue is an on/off state, not a level");
        // A deck that is not there resolves to nothing, not to deck A's cue.
        assert_eq!(state.value("deck.c.cue1".parse().unwrap()), None);
        // And a cue the deck cannot hold is refused by name.
        let err = "deck.a.cue9".parse::<Source>().unwrap_err();
        assert!(err.contains("hot cues are 1 … 8"), "{err}");
        assert!("deck.a.cue0".parse::<Source>().is_err());
        // `deck.a.cue` is the *main* cue now, so what must fail is a cue word that
        // names no number at all rather than a bare `cue`.
        assert!("deck.a.cuex".parse::<Source>().is_err());
    }

    #[test]
    fn a_play_light_follows_authoritative_state_from_any_source() {
        let mut feedback = Feedback::new(&profile(vec![out(note(0, 11), "deck.a.play")])).unwrap();
        // Nothing is known to be on the device yet, so the first poll sends
        // the current state, not a blind "on".
        assert_eq!(
            feedback.poll(&state(true, 0.5), 1000),
            vec![vec![0x90, 11, 127]]
        );
        // The same state again sends nothing: change detection, not a timer.
        assert_eq!(
            feedback.poll(&state(true, 0.5), 1020),
            Vec::<Vec<u8>>::new()
        );
        // A deck stopped by the AI DJ, the UI button or the remote page looks
        // exactly like a deck stopped by this controller.
        assert_eq!(
            feedback.poll(&state(false, 0.5), 1040),
            vec![vec![0x80, 11, 0]]
        );
        assert_eq!(feedback.sent, 2);
    }

    #[test]
    fn a_vu_is_rate_limited_and_coalesced_rather_than_queued() {
        let mut feedback = Feedback {
            ..Feedback::new(&profile(vec![out(
                OutputTarget::Cc {
                    channel: 0,
                    cc: 20,
                    max: 127,
                },
                "deck.a.vu",
            )]))
            .unwrap()
        };
        let vu = |level: f32| ControllerState {
            decks: vec![DeckState {
                vu: level,
                ..Default::default()
            }],
            ..Default::default()
        };
        // First value goes out immediately.
        let first = feedback.poll(&vu(0.1), 0);
        assert_eq!(first.len(), 1);
        // Four rapid changes inside the interval produce at most one message,
        // and it carries the newest value.
        let mut bursts = 0;
        for (i, level) in [0.2, 0.3, 0.4, 0.5].iter().enumerate() {
            bursts += feedback.poll(&vu(*level), 1 + i as u64).len();
        }
        assert!(bursts <= 1, "a burst must not become a queue: {bursts}");
        // Once the interval has passed, the newest value lands — 0.5, not the
        // 0.2 that was pending first.
        let after = feedback.poll(&vu(0.5), LEVEL_INTERVAL_MS + 10);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0][2], (0.5f64 * 127.0).round() as u8);
        assert_eq!(feedback.sent, 2);
    }

    #[test]
    fn hardware_caused_changes_are_not_echoed_back() {
        let mut feedback = Feedback::new(&profile(vec![out(note(0, 11), "deck.a.pfl")])).unwrap();
        // The user pressed the controller's own PFL button; the state changed
        // because of this device, so the light is already right.
        feedback.reflect(Source::DeckPfl(0));
        let pfl = ControllerState {
            decks: vec![DeckState {
                pfl: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(feedback.poll(&pfl, 100), Vec::<Vec<u8>>::new());
        assert_eq!(feedback.suppressed, 1);
        // The suppression is once, and only for that source: a change the
        // hardware did not cause still lights up.
        assert_eq!(
            feedback.poll(&state(false, 0.5), 120),
            vec![vec![0x80, 11, 0]],
            "the UI turned PFL off, and that is the hardware's business"
        );
        feedback.reflect(Source::DeckPfl(0));
        assert_eq!(feedback.suppressed, 1, "reflect is idempotent");
    }

    #[test]
    fn every_output_is_re_sent_once_after_a_reconnect() {
        let mut feedback = Feedback::new(&profile(vec![
            out(
                OutputTarget::Sysex {
                    payload: vec![0x7E, 0x00, 0x06, 0x01],
                },
                "deck.a.loaded",
            ),
            out(note(0, 11), "deck.a.play"),
        ]))
        .unwrap();
        let first = feedback.poll(&state(true, 0.5), 0);
        assert_eq!(first.len(), 2, "init sequence then the light");
        assert_eq!(first[0], vec![0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7]);
        assert_eq!(first[1], vec![0x90, 11, 127]);
        // The static sequence is not repeated on every poll.
        assert_eq!(feedback.poll(&state(true, 0.5), 50).len(), 0);
        // A replug restores the visible state.
        feedback.reconnect();
        assert_eq!(feedback.poll(&state(true, 0.5), 100).len(), 2);
        assert_eq!(feedback.poll(&state(true, 0.5), 150).len(), 0);
    }

    #[test]
    fn one_poll_cannot_become_an_unbounded_burst() {
        let outputs: Vec<Output> = (0..64)
            .map(|i| out(note(0, (i % 128) as u8), "deck.a.vu"))
            .collect();
        let mut feedback = Feedback::new(&profile(outputs)).unwrap();
        let loud = ControllerState {
            decks: vec![DeckState {
                vu: 0.9,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(feedback.poll(&loud, 0).len(), MAX_PER_POLL);
    }

    #[test]
    fn a_reversed_range_mirrors_the_indicator() {
        let mut o = out(
            OutputTarget::Cc {
                channel: 0,
                cc: 7,
                max: 127,
            },
            "crossfader",
        );
        o.range = Some((1.0, 0.0));
        let mut feedback = Feedback::new(&profile(vec![o])).unwrap();
        let first = feedback.poll(&state(false, 0.0), 0);
        assert_eq!(
            first.first().map(|b| b[2]),
            Some(127),
            "hardware mounted the other way round"
        );
        feedback.reconnect();
        let second = feedback.poll(&state(false, 1.0), 0);
        assert_eq!(second.first().map(|b| b[2]), Some(0));
    }

    #[test]
    fn a_missing_deck_or_a_bad_profile_is_reported() {
        let mut feedback = Feedback::new(&profile(vec![out(note(0, 11), "deck.c.play")])).unwrap();
        // Deck C does not exist in this state: nothing to send, no panic.
        assert_eq!(feedback.poll(&state(true, 0.5), 0), Vec::<Vec<u8>>::new());
        let err = Feedback::new(&profile(vec![out(note(0, 11), "platter")])).unwrap_err();
        assert!(
            err.contains("Test"),
            "the profile is named in the error: {err}"
        );
        // Out-of-range values are refused at validation time.
        assert!(
            out(
                OutputTarget::Cc {
                    channel: 0,
                    cc: 7,
                    max: 0
                },
                "crossfader"
            )
            .validate()
            .is_err()
        );
        assert!(
            out(OutputTarget::PitchBend { channel: 16 }, "crossfader")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn a_fourteen_bit_and_a_bend_indicator_encode_over_their_whole_range() {
        let target = OutputTarget::Cc14 {
            channel: 1,
            cc: 0x10,
        };
        assert_eq!(
            target.slot(0.0),
            Slot::Level(0),
            "the indicator's full range is used"
        );
        assert_eq!(
            target.message(Slot::Level(16383)),
            vec![0xB1, 0x10, 127, 0xB1, 0x30, 127]
        );
        let bend = OutputTarget::PitchBend { channel: 0 };
        assert_eq!(bend.message(bend.slot(0.0)), vec![0xE0, 0x00, 0x40]);
        // The bend field is symmetric about its centre, so a full-left
        // indicator lands one step inside the field's own floor.
        assert_eq!(bend.message(bend.slot(-1.0)), vec![0xE0, 0x01, 0x00]);
        assert_eq!(bend.message(bend.slot(1.0)), vec![0xE0, 0x7F, 0x7F]);
    }

    #[test]
    fn a_toggle_is_sent_as_note_on_and_off_with_the_profiles_values() {
        let target = OutputTarget::Note {
            channel: 3,
            note: 40,
            on_value: 60,
            off_value: 5,
        };
        assert_eq!(target.message(target.slot(1.0)), vec![0x93, 40, 60]);
        assert_eq!(target.message(target.slot(0.0)), vec![0x83, 40, 0]);
    }
}
