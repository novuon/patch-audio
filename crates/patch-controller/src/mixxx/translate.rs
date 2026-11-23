//! Mixxx control objects → canonical Patch commands, and everything that
//! cannot be translated, reported by name.
//!
//! The table below is the whole value of the importer. Mixxx's corpus uses
//! 1,452 distinct control-object names across its non-scripted entries; Patch
//! has a canonical command for a subset of them, and the rest have a *reason*
//! rather than silence. Three kinds of reason recur:
//!
//! * **Patch has no such command yet** — samplers, the DJ Booth's library
//!   selection, Mixxx's effect units and their parameters, and beat sync. (Hot
//!   cues, the manual loop points, reverse, the beatloops and the EQ kills each
//!   *gained* a command as this work went on, so the importer emits them.)
//! * **The unit does not match** — Mixxx's `pregain` is a normalised knob and
//!   Patch's channel gain is in dB, so no scaling can be invented honestly;
//!   Patch's tempo is in percent and Mixxx's `rate` is normalised, so the
//!   imported range is stated as an assumption.
//! * **The mapping needed JavaScript** — an entry carrying `script-binding`
//!   is Mixxx handing the control to its script engine, which Patch does not
//!   have. Measured over the corpus, that is 67 % of all control entries and
//!   *all* platter touches and 325 of 337 jog movements, so no imported mapping
//!   can drive a real jog wheel; `docs/controllers-scripting.md` has the
//!   numbers and the surface a runtime would need.
//!
//! Nothing is guessed into a binding, and nothing is dropped quietly.

use std::collections::BTreeSet;

use super::preset::{Direction, Kind, MixxxControl, MixxxMapping};
use crate::feedback::{Output, OutputTarget, Source};
use crate::profile::{Criteria, Profile, Protocol};
use patch_core::input::{Binding, Keymap, Trigger};

/// One control that was not translated, aggregated by name and reason.
#[derive(Clone, Debug, PartialEq)]
pub struct Unsupported {
    pub what: String,
    pub count: usize,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub controls: usize,
    pub translated: usize,
    pub scripted: usize,
    pub unsupported: Vec<Unsupported>,
    /// Low bytes of a 14-bit pair whose MSB is declared in the same file.
    pub pairs_merged: usize,
    pub outputs: usize,
    pub outputs_translated: usize,
    pub outputs_unsupported: Vec<Unsupported>,
    /// Translations that need the user's judgement.
    pub warnings: Vec<String>,
}

impl Report {
    fn refuse(&mut self, what: &str, reason: &str) {
        if let Some(entry) = self
            .unsupported
            .iter_mut()
            .find(|entry| entry.what == what && entry.reason == reason)
        {
            entry.count += 1;
            return;
        }
        self.unsupported.push(Unsupported {
            what: what.to_string(),
            count: 1,
            reason: reason.to_string(),
        });
    }

    fn refuse_output(&mut self, what: &str, reason: &str) {
        if let Some(entry) = self
            .outputs_unsupported
            .iter_mut()
            .find(|entry| entry.what == what && entry.reason == reason)
        {
            entry.count += 1;
            return;
        }
        self.outputs_unsupported.push(Unsupported {
            what: what.to_string(),
            count: 1,
            reason: reason.to_string(),
        });
    }

    pub(super) fn warn(&mut self, warning: &str) {
        if !self.warnings.iter().any(|existing| existing == warning) {
            self.warnings.push(warning.to_string());
        }
    }

    /// A short human summary, for the CLI and for a commit message.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} controls: {} translated, {} script-bound, {} without a Patch equivalent; \
             {} outputs: {} translated, {} without a Patch equivalent",
            self.controls,
            self.translated,
            self.scripted,
            self.unsupported.iter().map(|u| u.count).sum::<usize>(),
            self.outputs,
            self.outputs_translated,
            self.outputs_unsupported
                .iter()
                .map(|u| u.count)
                .sum::<usize>(),
        );
        if self.pairs_merged > 0 {
            text.push_str(&format!(
                "; {} low bytes merged into their 14-bit pair",
                self.pairs_merged
            ));
        }
        if !self.warnings.is_empty() {
            text.push_str(&format!("; {} warnings", self.warnings.len()));
        }
        text
    }
}

/// A translated mapping plus what it could not cover.
#[derive(Clone, Debug)]
pub struct Import {
    pub profile: Profile,
    pub report: Report,
}

/// A binding and, when the control is a held pad, the release it also needs.
struct Plan {
    bindings: Vec<Binding>,
    /// A translation that needs the user's judgement.
    warning: Option<String>,
    /// The authoritative state this control drives, for soft takeover.
    source: Option<Source>,
    /// This control was translated as a scratch rather than a beat jump, so
    /// its tick size is in seconds rather than beats.
    scratching: bool,
}

fn binding(
    context: &str,
    on: Trigger,
    action: serde_json::Value,
    range: Option<(f64, f64)>,
) -> Binding {
    Binding {
        context: context.to_string(),
        on,
        action,
        range,
        soft_takeover: false,
        source: None,
    }
}

fn note(channel: u8, note: u8, release: bool) -> Trigger {
    Trigger::Note {
        channel,
        note,
        release,
    }
}

fn deck_action(op: &str, what: &str) -> serde_json::Value {
    serde_json::json!({"op": op, "deck": "{deck}", "what": what})
}

/// Whether a Mixxx control object names a platter's *movement* — the thing a
/// hand turns. Mixxx's own mappings use both its canonical names (`wheel`,
/// `jog`) and their own (`PioneerDDJ400.jogTurn`, `Hercules4Mx.jogWheel`,
/// `VestaxVCI100.jog_wheel`, `NumarkNS7.jog_move`), so both have to be
/// recognised. A scratch *mode* toggle is not movement, and neither is the
/// search ring around a platter.
fn is_jog_move(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    (key.contains("jog") || key.contains("wheel") || key.contains("platter"))
        && ![
            "touch", "scratch", "mode", "search", "seek", "ring", "button", "helper",
        ]
        .iter()
        .any(|word| key.contains(word))
}

/// Whether a control object names the platter's *touch* — the momentary
/// contact that says a hand is on it, which is what ends a scratch. Mouse-area
/// touches (`needleSearchTouch`, a touch strip or screen) are not platters.
fn is_jog_touch(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("touch")
        && (key.contains("jog") || key.contains("wheel") || key.contains("platter"))
        && ![
            "search", "strip", "screen", "effect", "pitch", "button", "mode", "scratch",
        ]
        .iter()
        .any(|word| key.contains(word))
}

/// Whether this is one of Mixxx's effect controls.
///
/// They are reported together because the answer is the same for all of them and
/// it is about Patch rather than about the mapping: the DJ Booth has no effect
/// units, so there is no slot, parameter or enable for any of these to drive.
fn effect_control(key: &str) -> bool {
    if matches!(
        key,
        "enabled" | "next_effect" | "prev_effect" | "flanger" | "meta" | "m1" | "m2" | "m3" | "m4"
    ) {
        return true;
    }
    key.starts_with("parameter")
        // Every LFO knob is an effect parameter.
        || key.starts_with("lfo")
        || key.starts_with("button_parameter")
        || key.starts_with("super")
        || key.starts_with("effect")
        // `group_[Channel1]_enable` enables an effect unit on a group.
        || (key.starts_with("group_[") && key.ends_with("_enable"))
}

/// A binding's action with the placeholders the reader fills from a message.
///
/// Only the corpus check needs this, so it is compiled only for tests; the reader
/// itself does the real substitution.
#[cfg(test)]
///
/// `{deck}` is the letter of the group the mapping named and `{value}` is the
/// message's own value; both are what the reader substitutes before the action
/// becomes a command, which is what the corpus check needs to reproduce.
fn fill_placeholders(action: &serde_json::Value, value: f64) -> serde_json::Value {
    use serde_json::Value;
    match action {
        Value::String(text) if text == "{deck}" => Value::String("a".into()),
        Value::String(text) if text == "{value}" => serde_json::json!(value),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| fill_placeholders(item, value))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), fill_placeholders(item, value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The EQ band a Mixxx kill switch kills, if that is what this control is.
///
/// Mixxx writes them as `filterLowKill`, `filterMidKill` and `filterHighKill`;
/// anything else ending in `Kill` is not a band of Patch's EQ.
fn kill_band(key: &str) -> Option<&'static str> {
    match key.strip_suffix("Kill")?.to_ascii_lowercase().as_str() {
        "filterlow" => Some("low"),
        "filtermid" => Some("mid"),
        "filterhigh" => Some("high"),
        _ => None,
    }
}

/// The loop length a Mixxx beatloop control object names, and the form it used.
///
/// `beatloop_4` → `(4.0, "")`, `beatloop_0.5_toggle` → `(0.5, "toggle")`. A name
/// that carries no length (Mixxx's bare `beatloop_activate`) returns `None`
/// rather than a guessed size.
fn beatloop(key: &str) -> Option<(f32, &str)> {
    let rest = key.strip_prefix("beatloop_")?;
    let (number, suffix) = rest.split_once('_').unwrap_or((rest, ""));
    Some((number.parse().ok()?, suffix))
}

/// A Mixxx hot-cue control object, as one Patch command or a refusal.
///
/// Mixxx writes these as `hotcue_N_activate`, `_set`, `_clear` and `_goto`, and
/// the four differ in ways only some of which Patch can express: its hot cue
/// command jumps to a cue if one is stored and otherwise sets it where the deck
/// is, so `activate` and `clear` map onto it exactly, while `set` (which
/// overwrites a stored cue) and `goto` (which must not set one) do not.
fn hot_cue(rest: &str, key: &str, deck: &'static str) -> Result<serde_json::Value, String> {
    // `hotcue_1_activate` names cue 1, not cue `1_activate`.
    let (number, action) = rest.split_once('_').unwrap_or((rest, ""));
    let n: u8 = number
        .parse()
        .map_err(|_| format!("'{key}' names no hot cue number"))?;
    if !(1..=8).contains(&n) {
        return Err(format!(
            "Patch holds eight hot cues per deck, and this is cue {n} ({key})"
        ));
    }
    match action {
        "activate" => Ok(serde_json::json!({"op":"dj.hot_cue","deck":deck,"n":n})),
        "clear" => Ok(serde_json::json!({"op":"dj.hot_cue","deck":deck,"n":n,"clear":true})),
        other => Err(format!(
            "Mixxx's hot-cue '{other}' has no Patch equivalent ({key}): Patch's hot cue command \
             jumps to a stored cue or sets one where the deck is, so it cannot overwrite a cue \
             that is already set, and it always sets an empty one"
        )),
    }
}

/// The Patch commands a Mixxx control object maps onto, if any.
fn plan(control: &MixxxControl, scratch_groups: &BTreeSet<&str>) -> Result<Plan, String> {
    let key = control.key.as_str();
    let channel = control.channel();
    if matches!(control.kind(), Kind::Other(_)) {
        return Err("aftertouch and system messages are not controller input".into());
    }
    // A data byte outside the 7-bit field is not something a MIDI device can
    // send, so a mapping file that says it is wrong. Translating it would
    // produce a profile Patch then refuses to load, which is worse than saying
    // so here: `us428.midi.xml` in Mixxx's corpus carries CC 164.
    if !matches!(control.kind(), Kind::PitchBend) && control.midino > 127 {
        return Err(format!(
            "the mapping's data byte {} is outside MIDI's 7-bit field, so it is not a control \
             Patch can bind",
            control.midino
        ));
    }
    if control.fourteen_bit() == Some(Direction::Up) && control.midino > 111 {
        return Err(format!(
            "a 14-bit pair at CC {} would put its low byte at {}, outside MIDI's 7-bit field",
            control.midino,
            control.midino as u16 + 32
        ));
    }
    // Deck-relative keys need a deck group; mixer keys need [Master].
    let deck = control.deck();
    let deck_key = |wanted: bool| -> Result<&'static str, String> {
        match (wanted, deck) {
            (true, Some(letter)) => Ok(letter),
            (true, None) => Err(match control.group.trim() {
                // A sampler is a whole group Patch does not have.
                g if g.starts_with("[Sampler") => {
                    format!("group '{g}' is a sampler, and Patch has four decks and no sampler")
                }
                // The master strip is a mixer target, not a deck — and what can
                // drive it is named in dB, so a normalised knob has no curve here.
                "[Master]" => "group '[Master]' is the mixer's master strip, not a deck: \
                    Patch's gain there is in dB and this control is a normalised knob, so \
                    the mapping's own curve would have to be stated"
                    .to_string(),
                // A preview deck is a Mixxx concept Patch has not got.
                g if g.starts_with("[PreviewDeck") => {
                    format!("group '{g}' is a Mixxx preview deck, which Patch has not got")
                }
                other => format!("group '{other}' is not one of the four Patch decks"),
            }),
            (false, _) => Ok(""),
        }
    };
    // The reader passes "midi"/"hid" as the context for a device event, so a
    // controller binding never sees a surface context and never uses {deck}.
    let context = "midi";
    let momentary = matches!(control.kind(), Kind::Note);
    let trigger = |release: bool| match control.kind() {
        Kind::Note => Some(note(channel, control.midino, release)),
        Kind::Cc => Some(if release {
            // A CC has one event; only a note has a release form.
            return None;
        } else if control.fourteen_bit() == Some(Direction::Up) {
            // The pair is declared once, on its MSB; the reader assembles it
            // from the two messages the device sends.
            Trigger::Cc14 {
                channel,
                cc: control.midino,
            }
        } else {
            Trigger::Cc {
                channel,
                cc: control.midino,
                relative: control.relative().map(|hint| match hint {
                    super::preset::RelativeHint::TwosComplement
                    | super::preset::RelativeHint::TwosComplementFast => {
                        patch_core::input::RelativeMode::TwosComplement
                    }
                    super::preset::RelativeHint::OffsetBinary => {
                        patch_core::input::RelativeMode::OffsetBinary
                    }
                }),
                step: control.relative().map(|hint| hint.patch().1).unwrap_or(1.0),
            }
        }),
        Kind::PitchBend => Some(Trigger::PitchBend { channel }),
        Kind::Program => Some(Trigger::Program {
            channel,
            program: control.midino,
        }),
        Kind::Other(_) => None,
    };
    let mut plan = Plan {
        bindings: Vec::new(),
        warning: None,
        source: None,
        scratching: false,
    };

    macro_rules! add {
        ($action:expr, $range:expr) => {{
            if let Some(on) = trigger(false) {
                plan.bindings.push(binding(context, on, $action, $range));
            }
        }};
    }
    macro_rules! add_held {
        ($press:expr, $release:expr, $range:expr) => {{
            if let Some(on) = trigger(false) {
                plan.bindings.push(binding(context, on, $press, $range));
            }
            if momentary && let Some(off) = trigger(true) {
                plan.bindings.push(binding(context, off, $release, None));
            }
        }};
    }

    match key {
        "volume" => {
            let letter = deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.fader","deck":"{deck}","position":"{value}"}),
                Some((0.0, 1.0))
            );
            plan.source = Some(Source::DeckFader(deck_index(letter)));
        }
        "play" => {
            // A pad toggles; an absolute control is a switch only when the
            // mapping says so. Anything else would flip on both limits.
            if momentary || control.has("switch") {
                add!(
                    serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"toggle"}),
                    None
                );
            } else {
                return Err(
                    "an absolute 'play' control needs the mapping's <switch/> option; \
                     Patch's toggle cannot read a position"
                        .into(),
                );
            }
        }
        "start_play" | "start_stop" | "start" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"toggle"}),
                None
            );
        }
        "cue_default" => {
            deck_key(true)?;
            add_held!(
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"cue"}),
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"cue_release"}),
                None
            );
            if !momentary {
                plan.warning = Some(
                    "a 'cue_default' control is a held pad in Mixxx; on an absolute control \
                     the release is not expressed"
                        .into(),
                );
            }
        }
        "cue_gotoandplay" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"play"}),
                None
            );
            plan.warning = Some(
                "Mixxx's cue_gotoandplay jumps to the cue point first; Patch plays from the \
                 playhead, so the imported control starts the deck where it is"
                    .into(),
            );
        }
        // Mixxx's `stop` on a deck is Patch's pause; on a sampler the group check
        // refuses it, which is the honest answer rather than pausing a deck.
        "stop" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"pause"}),
                None
            );
        }
        "cue_gotoandstop" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.transport","deck":"{deck}","action":"pause"}),
                None
            );
        }
        "pfl" => {
            deck_key(true)?;
            if momentary {
                add_held!(
                    serde_json::json!({"op":"dj.pfl","deck":"{deck}","on":true}),
                    serde_json::json!({"op":"dj.pfl","deck":"{deck}","on":false}),
                    None
                );
            } else {
                return Err(
                    "Patch's headphone cue is on/off; an absolute control for it needs a switch"
                        .into(),
                );
            }
        }
        "beatsync" | "sync_enabled" | "sync_master" => {
            deck_key(true)?;
            if momentary || control.has("switch") || key == "beatsync" {
                add!(serde_json::json!({"op":"dj.sync","deck":"{deck}"}), None);
            } else {
                return Err(
                    "Patch's sync is a one-shot command; a persistent sync control is not \
                     expressible without new state"
                        .into(),
                );
            }
        }
        "keylock" => {
            deck_key(true)?;
            if momentary || control.has("switch") {
                add!(deck_action("dj.toggle", "keylock"), None);
            } else {
                return Err("an absolute 'keylock' control needs a switch".into());
            }
        }
        "slip_enabled" => {
            deck_key(true)?;
            if momentary || control.has("switch") {
                add!(deck_action("dj.toggle", "slip"), None);
            } else {
                return Err("an absolute 'slip_enabled' control needs a switch".into());
            }
        }
        "quantize" => {
            deck_key(true)?;
            if momentary || control.has("switch") {
                add!(deck_action("dj.toggle", "quantize"), None);
            } else {
                return Err("an absolute 'quantize' control needs a switch".into());
            }
        }
        "rate" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.tempo","deck":"{deck}","percent":"{value}"}),
                Some((-8.0, 8.0))
            );
            plan.warning = Some(
                "Mixxx's 'rate' is a normalised pitch fader; Patch's tempo is in percent, so \
                 the imported range is the common +/-8 % and may need editing for this hardware"
                    .into(),
            );
        }
        "rate_temp_up" => {
            deck_key(true)?;
            add_held!(
                serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":0.04}),
                serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":0.0}),
                None
            );
        }
        "rate_temp_down" => {
            deck_key(true)?;
            add_held!(
                serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":-0.04}),
                serde_json::json!({"op":"dj.bend","deck":"{deck}","amount":0.0}),
                None
            );
        }
        "rate_perm_up" | "rate_perm_up_small" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.tempo_step","deck":"{deck}","percent":1.0}),
                None
            );
        }
        "rate_perm_down" | "rate_perm_down_small" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.tempo_step","deck":"{deck}","percent":-1.0}),
                None
            );
        }
        "fwd" | "back" => {
            deck_key(true)?;
            let beats = if key == "fwd" { 4.0 } else { -4.0 };
            add!(
                serde_json::json!({"op":"dj.jump","deck":"{deck}","beats":beats}),
                None
            );
            plan.warning = Some(
                "Mixxx's fwd/back search while held; Patch's beat jump moves by a fixed number \
                 of beats, so the imported control jumps rather than searches"
                    .into(),
            );
        }
        "crossfader" => {
            add!(
                serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                Some((0.0, 1.0))
            );
            plan.source = Some(Source::Crossfader);
        }
        "filterLow" | "filterMid" | "filterHigh" => {
            deck_key(true)?;
            let band = match key {
                "filterLow" => "low",
                "filterMid" => "mid",
                _ => "high",
            };
            add!(
                serde_json::json!({"op":"dj.eq_knob","deck":"{deck}","band":band,"value":"{value}"}),
                Some((-1.0, 1.0))
            );
        }
        key if is_jog_touch(key) => {
            deck_key(true)?;
            if !scratch_groups.contains(control.group.as_str()) {
                return Err(
                    "the platter's touch has nothing to end: this mapping's jog sends no \
                     relative movement in this deck's group that Patch can scratch (see that \
                     control's own report)"
                        .into(),
                );
            }
            match trigger(true) {
                Some(off) => plan.bindings.push(binding(
                    context,
                    off,
                    serde_json::json!({"op":"dj.scratch_end","deck":"{deck}"}),
                    None,
                )),
                None => {
                    return Err("the platter's touch is not a note, so it has no release".into());
                }
            }
        }
        key if is_jog_move(key) => {
            deck_key(true)?;
            if control.relative().is_none() {
                return Err(
                    "Mixxx's jog is a relative control; an absolute one has no meaning to \
                     translate"
                        .into(),
                );
            }
            if scratch_groups.contains(control.group.as_str()) {
                // The mapping also has this platter's touch control, which ends
                // the scratch on release, so the jog can be a real scratch: each
                // tick moves the hand's target and the deck glides after it.
                plan.scratching = true;
                add!(
                    serde_json::json!({
                        "op":"dj.scratch",
                        "deck":"{deck}",
                        "delta":"{value}",
                        "velocity":0.0
                    }),
                    None
                );
                plan.warning = Some(
                    "the jog wheel scratches with one tick = 0.01 s of playhead: the wheel's \
                     own resolution decides that, so the multiplier is a starting point to \
                     tune (the encoder's own rate multiplier is applied to it). The platter's \
                     touch control ends the scratch; a wheel moved with no touch event leaves \
                     the deck under your hand"
                        .into(),
                );
            } else {
                // No touch control in this deck's group: a scratch would have
                // nothing to end it, and the deck would stay held under the hand
                // forever. A relative playhead move is the safe translation.
                add!(
                    serde_json::json!({"op":"dj.jump","deck":"{deck}","beats":"{value}"}),
                    None
                );
                plan.warning = Some(
                    "the jog wheel is translated as a relative beat jump with one tick = 0.05 \
                     beats: Patch scratches only when the mapping also has the platter's touch \
                     control (Mixxx's `wheel_touch`, or whatever the maker calls it), and this \
                     one has not, so a scratch here would have nothing to end it. The tick size \
                     also depends on the wheel's own resolution, so this multiplier is a \
                     starting point to tune (the encoder's own rate multiplier is applied to \
                     it)"
                    .into(),
                );
            }
        }
        "scratch2_enable" => {
            return Err(
                "Mixxx's scratch mode is a toggle that decides whether the wheel scratches; in \
                 Patch a wheel scratches whenever the mapping pairs it with a platter touch, so \
                 there is no equivalent control"
                    .into(),
            );
        }
        "loop_double" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.loop_scale","deck":"{deck}","factor":2.0}),
                None
            );
        }
        "loop_halve" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.loop_scale","deck":"{deck}","factor":0.5}),
                None
            );
        }
        "reloop_exit" => {
            deck_key(true)?;
            add!(
                serde_json::json!({"op":"dj.loop","deck":"{deck}","beats":null}),
                None
            );
            plan.warning = Some(
                "Mixxx's reloop_exit returns to the stored loop; Patch's loop command leaves \
                 the loop, so the imported control exits rather than reloops"
                    .into(),
            );
        }
        _ => {
            // Families with a numeric suffix.
            // A loop pad. Mixxx writes the same control three ways —
            // `beatloop_4`, `beatloop_4_toggle` and `beatloop_4_activate` — and
            // all three mean "a loop of four beats, toggled". A *roll*
            // (`beatlooproll_*`) is momentary, a loop that plays only while the
            // pad is held, and Patch has no roll command; the bare
            // `beatloop_activate` means "whatever size was chosen last", which
            // Patch does not track.
            if key.starts_with("beatlooproll_") {
                return Err(format!(
                    "a beatloop roll runs a loop only while the pad is held, and Patch has no \
                     roll command ({key})"
                ));
            } else if key == "beatloop_activate" {
                return Err(format!(
                    "Mixxx's `beatloop_activate` re-uses the loop size chosen last, which Patch \
                     does not track: bind a sized control (`beatloop_<beats>_activate`) instead \
                     ({key})"
                ));
            } else if let Some((beats, suffix)) = beatloop(key)
                && matches!(suffix, "" | "toggle" | "activate")
            {
                deck_key(true)?;
                add!(
                    serde_json::json!({"op":"dj.loop_toggle","deck":"{deck}","beats":beats}),
                    None
                );
            } else if let Some((_, suffix)) = beatloop(key) {
                return Err(format!(
                    "Mixxx's beatloop form '{suffix}' has no Patch equivalent ({key})"
                ));
            } else if let Some(rest) = key.strip_prefix("beatjump_") {
                deck_key(true)?;
                let (beats, reversed) = match rest.strip_suffix("_backward") {
                    Some(number) => (number, true),
                    None => (rest.strip_suffix("_forward").unwrap_or(rest), false),
                };
                let beats: f32 = beats
                    .parse()
                    .map_err(|_| format!("'{key}' does not name a beat count"))?;
                let beats = if reversed { -beats } else { beats };
                add!(
                    serde_json::json!({"op":"dj.jump","deck":"{deck}","beats":beats}),
                    None
                );
            } else if let Some(rest) = key.strip_prefix("hotcue_") {
                let action = hot_cue(rest, key, deck_key(true)?)?;
                add!(action, None);
            } else if let Some(number) = key.strip_prefix("sampler") {
                return Err(format!(
                    "Patch has no sample player command (Mixxx control '{key}', sampler {number})"
                ));
            // A manual loop: the first button sets the in-point, the second sets
            // the out-point and engages it. The engine keeps the pending in-point
            // itself, which is why the mapping needs no state.
            } else if key == "loop_in" || key == "loop_out" {
                deck_key(true)?;
                let op = if key == "loop_in" {
                    "dj.loop_in"
                } else {
                    "dj.loop_out"
                };
                add!(serde_json::json!({"op": op, "deck": "{deck}"}), None);
            } else if key.starts_with("loop_in") || key.starts_with("loop_out") {
                return Err(format!(
                    "this jumps to the loop's own in- or out-point, which a mapping cannot name \
                     ({key}): Patch's loop commands set those points or leave the loop, but never \
                     seek to one"
                ));
            } else if key == "loop_enabled" {
                return Err(format!(
                    "Mixxx's `loop_enabled` switches a loop that is already set on or off; Patch \
                     engages a loop by naming its length (`beatloop_<beats>`) and leaves it with \
                     `reloop_exit`, with no separate enable ({key})"
                ));
            } else if key == "loop_scale" {
                return Err(format!(
                    "Mixxx's `loop_scale` is a knob whose multiplier range is the mapping's own, \
                     and Patch takes an absolute factor, so the scaling cannot be assumed ({key})"
                ));
            } else if key == "reverse" {
                // Mixxx's reverse is a switch: the deck plays backwards while the
                // control is held, so the note's release is what ends it.
                if momentary {
                    add_held!(
                        serde_json::json!({"op":"dj.reverse","deck":"{deck}","on":true}),
                        serde_json::json!({"op":"dj.reverse","deck":"{deck}","on":false}),
                        None
                    );
                } else {
                    return Err(format!(
                        "Patch's reverse command takes an explicit on/off, and this control sends \
                         no release to end the reverse with ({key})"
                    ));
                }
            } else if key == "LoopSelectedTrack"
                || key.starts_with("LoadSelected")
                || key == "eject"
            {
                return Err(format!(
                    "this names the DJ Booth's library *selection*, which a controller cannot see \
                     ({key}): Patch loads a deck by query or by path, so a mapping can bind a load \
                     button to a title of its own (`dj.load_track`) but not to 'whatever is \
                     selected'"
                ));
            } else if key.starts_with("parameter") || key.starts_with("button_parameter") {
                return Err("effect parameters have no canonical Patch command yet".into());
            } else if key == "headMix" || key == "headVolume" || key == "headGain" {
                return Err(
                    "Patch's headphone routing is per-deck cueing, not a mix control".into(),
                );
            } else if key == "pregain" || key == "gain" {
                return Err(
                    "Mixxx's pregain is a normalised knob and Patch's channel gain is in dB; the \
                     mapping's curve is unknown"
                        .into(),
                );
            } else if key == "enabled" || key.starts_with("group_") {
                return Err(
                    "effect-unit and group enable have no canonical Patch command yet".into(),
                );
            } else if let Some(band) = kill_band(key) {
                // An EQ kill. Patch's own command carries the kill — `db: null` is
                // silence, and 0 dB is unity — so no floor is invented, and the
                // release lifts the band. Patch keeps no memory of where the band
                // was before the kill, so flat is the honest restoration.
                deck_key(true)?;
                add_held!(
                    serde_json::json!({"op":"dj.eq","deck":"{deck}","band":band,"db":null}),
                    serde_json::json!({"op":"dj.eq","deck":"{deck}","band":band,"db":0.0}),
                    None
                );
            } else if effect_control(key) {
                return Err(format!(
                    "'{key}' is one of Mixxx's effect controls, and Patch's DJ Booth has no \
                     effect units: it has per-deck EQ, filter, stems and the loop tools, but \
                     no effect slot for this to drive"
                ));
            } else if key == "cue_goto" {
                return Err(format!(
                    "Mixxx's `cue_goto` only seeks to the cue, while Patch's cue command \
                     previews from it when the deck is paused — binding this would start the \
                     deck ({key})"
                ));
            } else if key == "playposition" {
                return Err(format!(
                    "this sets the play position directly, and Patch has no needle-drop \
                     command: it jumps by a number of beats (`dj.jump`) or the booth's \
                     waveform is dragged ({key})"
                ));
            } else if key == "reloop_toggle" || key == "reloop_andstop" {
                return Err(format!(
                    "Mixxx re-engages the loop it used last; Patch engages a loop by naming its \
                     length, and which length that was is not something a mapping can know \
                     ({key})"
                ));
            } else if key.starts_with("rate_temp") {
                return Err(format!(
                    "this nudges the rate by a fixed amount while it is held; Patch's bend takes \
                     a fraction, and the amount here is Mixxx's own convention, so the nudge \
                     would have to be invented ({key})"
                ));
            } else if key.starts_with("sync") {
                return Err(format!(
                    "Patch does not beat-sync decks: there is no tempo master for a deck to \
                     follow ({key})"
                ));
            } else if key == "play_stutter" {
                return Err(format!(
                    "Mixxx's stutter re-triggers play at the playhead; Patch has no stutter \
                     ({key})"
                ));
            } else if key.starts_with("show_") {
                return Err(format!(
                    "this shows or hides part of Mixxx's own interface, and Patch's Units are \
                     separate windows a mapping cannot open ({key})"
                ));
            } else if key == "cue_set" {
                return Err(format!(
                    "Mixxx's `cue_set` places the cue wherever the deck is; Patch's cue command \
                     sets the cue when the deck is paused and returns to it otherwise, so \
                     binding this would stop a playing deck ({key})"
                ));
            } else if key.starts_with("beats_translate") {
                return Err(format!(
                    "this moves the beatgrid, which Patch sets from the track's own analysis \
                     and from the load path — a mapping cannot move it ({key})"
                ));
            } else if key == "bpm_tap" {
                return Err(format!(
                    "Patch takes a deck's tempo from the track's analysis rather than a tapped \
                     tempo, so there is nothing to tap ({key})"
                ));
            } else if key.starts_with("SelectNext")
                || key.starts_with("SelectPrev")
                || key.starts_with("SelectLast")
                || key.starts_with("SelectTrackKnob")
            {
                return Err(format!(
                    "this moves the DJ Booth's library *selection*, which a controller cannot \
                     see ({key}): Patch loads a deck by query or by path, and a mapping can \
                     bind a load button to a title of its own (`dj.load_track`)"
                ));
            } else if key.is_empty() {
                return Err("the entry names no control object".into());
            } else {
                return Err("no Patch command corresponds to this Mixxx control object".into());
            }
        }
    }
    if plan.bindings.is_empty() {
        return Err("nothing to bind (no usable trigger)".into());
    }
    if is_jog_move(key) {
        for binding in &mut plan.bindings {
            if let Trigger::Cc { step, .. } = &mut binding.on {
                // The musical tick size, or the playhead's for a scratch: the
                // encoder's own rate multiplier (what Mixxx's hercjogfast means)
                // is applied to whichever unit the action speaks.
                *step *= if plan.scratching { 0.01 } else { 0.05 };
            }
        }
    }
    // A controller binding has no surface context, so `{deck}` would be filled
    // with nothing. The deck is known here, from the control's own group, and
    // is written into the action literally.
    match deck_key(true) {
        Ok(letter) => {
            for binding in &mut plan.bindings {
                fill_deck(&mut binding.action, letter);
            }
        }
        Err(reason) => {
            if plan
                .bindings
                .iter()
                .any(|binding| holds_deck_placeholder(&binding.action))
            {
                return Err(reason);
            }
        }
    }
    if control.soft_takeover() && plan.source.is_none() {
        plan.warning = Some(format!(
            "Mixxx asked for soft takeover on '{}', but Patch has no state to arm that control \
             against, so it is left live",
            control.key
        ));
    } else if control.soft_takeover() {
        for binding in &mut plan.bindings {
            binding.soft_takeover = true;
            binding.source = plan.source.map(|source| source.name());
        }
    }
    Ok(plan)
}

/// Replaces `"{deck}"` with the group's deck letter, anywhere in an action.
fn fill_deck(action: &mut serde_json::Value, letter: &str) {
    match action {
        serde_json::Value::String(text) if text == "{deck}" => *text = letter.to_string(),
        serde_json::Value::Array(items) => {
            for item in items {
                fill_deck(item, letter);
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values_mut() {
                fill_deck(value, letter);
            }
        }
        _ => {}
    }
}

/// Whether an action still asks for a deck.
fn holds_deck_placeholder(action: &serde_json::Value) -> bool {
    match action {
        serde_json::Value::String(text) => text == "{deck}",
        serde_json::Value::Array(items) => items.iter().any(holds_deck_placeholder),
        serde_json::Value::Object(fields) => fields.values().any(holds_deck_placeholder),
        _ => false,
    }
}

fn deck_index(letter: &str) -> usize {
    match letter {
        "a" => 0,
        "b" => 1,
        "c" => 2,
        _ => 3,
    }
}

/// The Patch state a Mixxx output control object follows.
fn output_source(key: &str, control: &MixxxControl) -> Option<Source> {
    let deck = control.deck()?;
    let index = deck_index(deck);
    // `hotcue_N_enabled` and `hotcue_N_status` light a pad that has a cue on it.
    // Patch can report that fact and nothing finer: a cue's position is not a thing
    // an indicator shows, and Mixxx's status enum's other values (intro, outro,
    // loop) are cue kinds Patch has not got, so a set cue is the one value it can
    // honestly state. Both keys are the same question here — Mixxx answers them
    // from one control — and 25 mappings in the corpus drive LEDs this way.
    if let Some(rest) = key.strip_prefix("hotcue_")
        && let Some((number, what)) = rest.split_once('_')
        && matches!(what, "enabled" | "status")
        && let Ok(n) = number.parse::<usize>()
        && (1..=patch_core::command::HOT_CUES).contains(&n)
    {
        return Some(Source::DeckHotCue(index, n));
    }
    // The rest of a deck's indicators that Patch can answer: whether the main cue
    // is set, whether a loop of a named length is on, and which EQ bands are killed.
    // Each follows state the deck already holds — the same state the cue, loop and
    // kill commands set — so nothing here is a second copy of it.
    if key == "cue_indicator" {
        return Some(Source::DeckCueSet(index));
    }
    if let Some(rest) = key.strip_prefix("beatloop_")
        && let Some(number) = rest.strip_suffix("_enabled")
        && let Ok(beats) = number.parse::<u32>()
        && Source::BEATLOOPS.contains(&beats)
    {
        return Some(Source::DeckBeatloop(index, beats));
    }
    if let Some(band) = kill_band(key) {
        return Some(Source::DeckKill(
            index,
            match band {
                "low" => 0,
                "mid" => 1,
                _ => 2,
            },
        ));
    }
    Some(match key {
        "play_indicator" | "play" => Source::DeckPlay(index),
        "pfl" => Source::DeckPfl(index),
        "keylock" => Source::DeckKeylock(index),
        "track_loaded" => Source::DeckLoaded(index),
        "loop_enabled" => Source::DeckLoop(index),
        "slip_enabled" => Source::DeckSlip(index),
        // Patch has had quantize state — and a source for it — all along; only this
        // map omitted the key, so a mapping's quantize LED was reported as having
        // nothing to follow.
        "quantize" => Source::DeckQuantize(index),
        "vu_meter" | "master" => Source::DeckVu(index),
        "rate" => Source::DeckTempo(index),
        "playposition" => Source::DeckProgress(index),
        "volume" => Source::DeckFader(index),
        _ => return None,
    })
}

/// Indicators whose absence is about the control rather than about the deck.
///
/// Both are cases where the deck has state but the mapping is asking about
/// something else: Mixxx's loop rolls, which exist only while a pad is held, and
/// loop lengths below a whole beat, which Patch's indicators do not name.
fn output_refusal(key: &str) -> Option<String> {
    if key.starts_with("beatlooproll_") {
        return Some(
            "Mixxx's loop roll is a loop that exists only while the pad is held, and Patch \
             has no roll"
                .to_string(),
        );
    }
    if let Some(rest) = key.strip_prefix("beatloop_")
        && let Some(number) = rest.strip_suffix("_enabled")
        && number.contains('.')
    {
        return Some(format!(
            "Patch's loop indicators name whole beat lengths, and this is {number} of a \
             beat ({key})"
        ));
    }
    None
}

fn output_target(control: &MixxxControl) -> Option<OutputTarget> {
    let channel = control.channel();
    Some(match control.kind() {
        Kind::Note => {
            // `invert` in Mixxx means the LED is dark when the state is on.
            let (on, off) = if control.invert() { (0, 127) } else { (127, 0) };
            OutputTarget::Note {
                channel,
                note: control.midino,
                on_value: on,
                off_value: off,
            }
        }
        Kind::Cc => match control.fourteen_bit() {
            Some(Direction::Up) => OutputTarget::Cc14 {
                channel,
                cc: control.midino,
            },
            _ => OutputTarget::Cc {
                channel,
                cc: control.midino,
                max: 127,
            },
        },
        Kind::PitchBend => OutputTarget::PitchBend { channel },
        Kind::Program | Kind::Other(_) => return None,
    })
}

/// Translates a Mixxx mapping into a Patch profile.
pub fn translate(mapping: &MixxxMapping) -> Import {
    let mut report = Report {
        // Entries that name no control still happened: counting them keeps the
        // report's arithmetic honest.
        controls: mapping.controls.len() + mapping.skipped,
        outputs: mapping.outputs.len() + mapping.skipped_outputs,
        ..Default::default()
    };
    // A 14-bit pair is declared on its MSB. Its low byte, if the file also
    // lists one, is part of that same pair rather than a second control.
    let msb_pairs: Vec<(u8, u8)> = mapping
        .controls
        .iter()
        .filter(|control| {
            !control.is_script()
                && matches!(control.kind(), Kind::Cc)
                && control.fourteen_bit() == Some(Direction::Up)
        })
        .map(|control| (control.channel(), control.midino))
        .collect();
    let mut bindings: Vec<Binding> = Vec::new();
    if mapping.skipped > 0 {
        let skipped = mapping.skipped;
        report.refuse(
            "(entry with no control)",
            "the entry names no status byte or no data byte, so it addresses no control",
        );
        if let Some(entry) = report
            .unsupported
            .last_mut()
            .filter(|entry| entry.what == "(entry with no control)")
        {
            entry.count = skipped;
        }
    }
    // A platter's movement and its touch, per deck group. A scratch needs both
    // halves, and they must belong to the same deck: a wheel with no touch would
    // leave the deck held under the hand for good, so only a group that has both
    // (declaratively — a scripted entry is not translated at all) is scratched.
    let scratch_groups: BTreeSet<&str> = {
        let moves: Vec<&str> = mapping
            .controls
            .iter()
            .filter(|c| !c.is_script() && is_jog_move(&c.key))
            .map(|c| c.group.as_str())
            .collect();
        let touches: Vec<&str> = mapping
            .controls
            .iter()
            .filter(|c| !c.is_script() && is_jog_touch(&c.key))
            .map(|c| c.group.as_str())
            .collect();
        moves
            .into_iter()
            .filter(|group| touches.contains(group))
            .collect()
    };
    for control in &mapping.controls {
        if control.is_script() {
            report.scripted += 1;
            continue;
        }
        if control.fourteen_bit() == Some(Direction::Down)
            && msb_pairs
                .iter()
                .any(|(channel, msb)| *channel == control.channel() && *msb + 32 == control.midino)
        {
            report.pairs_merged += 1;
            continue;
        }
        match plan(control, &scratch_groups) {
            Ok(planned) => {
                report.translated += 1;
                bindings.extend(planned.bindings);
                if let Some(warning) = planned.warning {
                    report.warn(&warning);
                }
            }
            Err(reason) => {
                let what = if control.key.is_empty() {
                    format!("{} (no key)", control.group)
                } else {
                    format!("{} (group {})", control.key, control.group)
                };
                report.refuse(&what, &reason);
            }
        }
    }
    if mapping.skipped_outputs > 0 {
        let skipped = mapping.skipped_outputs;
        report.refuse_output(
            "(output with no indicator)",
            "the entry names no status byte or no data byte, so it lights nothing",
        );
        if let Some(entry) = report
            .outputs_unsupported
            .last_mut()
            .filter(|entry| entry.what == "(output with no indicator)")
        {
            entry.count = skipped;
        }
    }
    let mut outputs: Vec<Output> = Vec::new();
    for control in &mapping.outputs {
        match output_source(&control.key, control)
            .zip(output_target(control))
            .map(|(source, target)| Output {
                target,
                source: source.name(),
                range: None,
                min_interval_ms: None,
            }) {
            Some(output) => {
                report.outputs_translated += 1;
                outputs.push(output);
            }
            None => {
                let what = control.key.clone();
                let reason = if output_source(&control.key, control).is_none() {
                    if let Some(said) = output_refusal(&control.key) {
                        said
                    } else if control.deck().is_none() {
                        "Patch has no state for this group".to_string()
                    } else {
                        "Patch has no state for this indicator".to_string()
                    }
                } else {
                    "the status byte is not an indicator Patch can write".to_string()
                };
                report.refuse_output(&what, &reason);
            }
        }
    }

    // Match on the mapping's own device name: Mixxx's `<controller id>` is the
    // MIDI port name, which is exactly what a name criterion means here.
    let mut names = Vec::new();
    if !mapping.device_id.is_empty() {
        names.push(mapping.device_id.clone());
    } else if !mapping.name.is_empty() {
        names.push(mapping.name.clone());
        report.warn(
            "the mapping names no device, so the profile matches on the mapping's own name, \
             which is a weak criterion; add a vendor/product or a path before relying on it",
        );
    } else {
        report.warn(
            "the mapping names no device and no preset name; the imported profile needs a \
             device path or a match criterion before Patch will load it",
        );
    }
    let profile = Profile {
        id: slug(&mapping.name, &mapping.device_id),
        name: mapping.name.clone(),
        manufacturer: String::new(),
        protocol: if mapping.is_hid() {
            Protocol::Hid
        } else {
            Protocol::Midi
        },
        attribution: mapping.attribution(),
        version: 1,
        device: None,
        r#match: Criteria {
            names,
            ..Default::default()
        },
        keymap: Keymap { bindings },
        outputs,
    };
    report.warn(
        "imported profiles are a starting point: no physical controller has been used to check \
         any converted mapping",
    );
    Import { profile, report }
}

/// A stable id from a name and a device id.
fn slug(name: &str, device: &str) -> String {
    let source = if name.is_empty() { device } else { name };
    let mut out = String::new();
    for c in source.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "imported".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(text: &str) -> Import {
        crate::mixxx::import(text).unwrap()
    }

    fn preset(controls: &str) -> String {
        format!(
            r#"<MixxxControllerPreset schemaVersion="1" mixxxVersion="2.6">
              <info><name>Test Controller</name><author>Someone</author></info>
              <controller id="Test Controller MIDI 1">
                <controls>{controls}</controls>
              </controller>
            </MixxxControllerPreset>"#
        )
    }

    fn control(status: &str, midino: &str, group: &str, key: &str, options: &str) -> String {
        format!(
            "<control><status>{status}</status><midino>{midino}</midino><group>{group}</group>\
             <key>{key}</key><options>{options}</options></control>"
        )
    }

    #[test]
    fn a_pad_transport_and_a_fader_become_patch_bindings() {
        let imported = import(&preset(&format!(
            "{}{}",
            control("0x90", "0x0B", "[Channel1]", "play", "<normal/>"),
            control("0xB0", "0x07", "[Channel1]", "volume", "<normal/>")
        )));
        let bindings = &imported.profile.keymap.bindings;
        assert_eq!(bindings.len(), 2);
        assert_eq!(imported.report.translated, 2);
        // The deck is written literally: a device event has no surface
        // context, so `{deck}` could never be filled from the event.
        assert_eq!(
            bindings[0].action,
            serde_json::json!({"op":"dj.transport","deck":"a","action":"toggle"})
        );
        assert_eq!(bindings[1].range, Some((0.0, 1.0)));
        assert_eq!(bindings[1].context, "midi");
        assert_eq!(bindings[1].on, Trigger::cc(0, 7));
        // The profile matches the mapping's own device id, and validates.
        assert_eq!(
            imported.profile.r#match.names,
            vec!["Test Controller MIDI 1".to_string()]
        );
        assert_eq!(imported.profile.protocol, Protocol::Midi);
        assert!(imported.profile.validate().is_ok());
        // Attribution is preserved; Patch does not claim the mapping.
        assert!(imported.profile.attribution.contains("Someone"));
    }

    #[test]
    fn a_held_cue_pad_gets_its_release_and_a_cc_does_not() {
        let imported = import(&preset(&format!(
            "{}{}",
            control("0x90", "0x24", "[Channel1]", "cue_default", ""),
            control("0xB0", "0x24", "[Channel1]", "cue_default", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        // The pad: cue on press, cue_release on release.
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].action["action"], "cue");
        assert_eq!(bindings[1].on, note(0, 0x24, true));
        assert_eq!(bindings[1].action["action"], "cue_release");
        // The CC: no release form exists, so it warns instead of inventing one.
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|w| w.contains("held pad")),
            "{:?}",
            imported.report.warnings
        );
    }

    #[test]
    fn soft_takeover_is_carried_over_only_when_patch_has_the_state() {
        let imported = import(&preset(&format!(
            "{}{}",
            control("0xB0", "0x07", "[Channel1]", "volume", "<soft-takeover/>"),
            control("0xB0", "0x13", "[Master]", "crossfader", "<soft-takeover/>")
        )));
        let bindings = &imported.profile.keymap.bindings;
        assert!(bindings[0].soft_takeover);
        assert_eq!(bindings[0].source.as_deref(), Some("deck.a.fader"));
        assert!(bindings[1].soft_takeover);
        assert_eq!(bindings[1].source.as_deref(), Some("crossfader"));
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_controller_with_no_state_to_arm_against_is_left_live_and_says_so() {
        let imported = import(&preset(&control(
            "0x90",
            "0x01",
            "[Channel1]",
            "play",
            "<soft-takeover/>",
        )));
        assert!(!imported.profile.keymap.bindings[0].soft_takeover);
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|w| w.contains("no state to arm")),
            "{:?}",
            imported.report.warnings
        );
    }

    #[test]
    fn a_wheel_and_its_platter_touch_become_a_scratch_and_its_end() {
        let imported = import(&preset(&format!(
            "{}{}",
            control("0xB0", "0x21", "[Channel1]", "wheel", "<hercjog/>"),
            control("0x90", "0x30", "[Channel1]", "wheel_touch", "<button/>")
        )));
        assert_eq!(imported.report.translated, 2);
        let bindings = &imported.profile.keymap.bindings;
        // The wheel: a scratch whose movement is measured from where the deck
        // is, so the controller only has to report how far the hand went. The
        // deck is written in literally — a controller has no surface context.
        assert_eq!(bindings[0].action["op"], "dj.scratch");
        assert_eq!(bindings[0].action["deck"], "a");
        assert_eq!(bindings[0].action["delta"], "{value}");
        assert_eq!(bindings[0].action["velocity"], 0.0);
        match bindings[0].on {
            Trigger::Cc { step, .. } => {
                // A scratch tick is playhead seconds, not beats.
                assert!((step - 0.01).abs() < 1e-9, "{step}");
            }
            ref other => panic!("{other:?}"),
        }
        // The touch: its release ends the scratch. Without this half the deck
        // would stay under the hand for good.
        assert_eq!(bindings[1].action["op"], "dj.scratch_end");
        assert_eq!(bindings[1].action["deck"], "a");
        assert_eq!(bindings[1].on, note(0, 0x30, true));
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_wheel_without_a_touch_is_not_scratched_and_a_mode_toggle_is_refused() {
        // No platter touch: nothing could end a scratch, so the wheel stays a
        // relative playhead move and the report says why.
        let imported = import(&preset(&control(
            "0xB0",
            "0x21",
            "[Channel1]",
            "wheel",
            "<hercjog/>",
        )));
        assert_eq!(
            imported.profile.keymap.bindings[0].action["op"], "dj.jump",
            "a scratch with no end would hold the deck"
        );
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|w| w.contains("wheel_touch")),
            "{:?}",
            imported.report.warnings
        );
        // Mixxx's scratch-mode toggle decides whether the wheel scratches; in
        // Patch that is the mapping's business, so it is refused rather than
        // guessed at.
        let imported = import(&preset(&format!(
            "{}{}",
            control("0x90", "0x31", "[Channel1]", "scratch2_enable", "<button/>"),
            control("0xB0", "0x21", "[Channel1]", "wheel", "<hercjog/>")
        )));
        assert!(
            imported
                .report
                .unsupported
                .iter()
                .any(|entry| entry.what.contains("scratch2_enable")),
            "{:?}",
            imported.report.unsupported
        );
    }

    #[test]
    fn relative_encoders_and_fourteen_bit_pairs_are_translated_as_declared() {
        let imported = import(&preset(&format!(
            "{}{}",
            control("0xB0", "0x21", "[Channel1]", "wheel", "<hercjogfast/>"),
            control(
                "0xB0",
                "0x10",
                "[Channel1]",
                "filterLow",
                "<fourteen-bit-msb/>"
            )
        )));
        let bindings = &imported.profile.keymap.bindings;
        match bindings[0].on {
            Trigger::Cc {
                relative: Some(mode),
                step,
                ..
            } => {
                assert_eq!(mode, patch_core::input::RelativeMode::TwosComplement);
                // hercjogfast is 3 ticks per message, and a wheel tick is
                // 0.05 beats: the product is what one message moves.
                assert!((step - 0.15).abs() < 1e-9, "{step}");
            }
            ref other => panic!("{other:?}"),
        }
        // A relative jog becomes a relative playhead move, with its tick size
        // stated in `step` and in a warning rather than left implicit.
        assert_eq!(bindings[0].action["op"], "dj.jump");
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|w| w.contains("one tick = 0.05 beats")),
            "{:?}",
            imported.report.warnings
        );
        // Filter knobs are -1..1 knobs, exactly as Patch's EQ knob is.
        assert_eq!(bindings[1].range, Some((-1.0, 1.0)));
        assert_eq!(bindings[1].action["band"], "low");
        // Its own MSB is a 14-bit pair, declared as such so the reader can
        // assemble the two halves the device sends.
        assert_eq!(
            bindings[1].on,
            Trigger::Cc14 {
                channel: 0,
                cc: 0x10
            }
        );
    }

    #[test]
    fn the_beatloop_and_beatjump_families_are_recognised() {
        let imported = import(&preset(&format!(
            "{}{}{}{}",
            control("0x90", "0x01", "[Channel1]", "beatloop_4", ""),
            control("0x90", "0x02", "[Channel1]", "beatloop_0.5", ""),
            control("0x90", "0x03", "[Channel1]", "beatjump_16_forward", ""),
            control("0x90", "0x04", "[Channel1]", "beatjump_32_backward", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        assert_eq!(bindings[0].action["beats"], 4.0);
        assert_eq!(bindings[1].action["beats"], 0.5);
        assert_eq!(bindings[2].action["beats"], 16.0);
        assert_eq!(bindings[3].action["beats"], -32.0);
        assert_eq!(imported.report.unsupported.len(), 0);
    }

    #[test]
    fn each_family_that_has_no_equivalent_says_which_family_it_is() {
        let imported = import(&preset(&format!(
            "{}{}{}{}{}{}{}{}",
            control("0xB0", "0x01", "[Channel1]", "super1", ""),
            control("0xB0", "0x02", "[Channel1]", "lfoDepth", ""),
            control("0x90", "0x03", "[Sampler1]", "cue_gotoandplay", ""),
            control("0xB0", "0x04", "[Master]", "volume", ""),
            control("0x90", "0x05", "[Channel1]", "cue_set", ""),
            control("0x90", "0x06", "[Channel1]", "bpm_tap", ""),
            control("0x90", "0x07", "[Channel1]", "beats_translate_curpos", ""),
            control("0x90", "0x08", "[Channel1]", "SelectNextTrack", "")
        )));
        let text = imported
            .report
            .unsupported
            .iter()
            .map(|u| format!("{} -> {}", u.what, u.reason))
            .collect::<Vec<_>>()
            .join("\n");
        // Each of these used to say only that no command corresponded.
        for expected in [
            "no effect units",
            "is a sampler",
            "master strip",
            "would stop a playing deck",
            "nothing to tap",
            "cannot move it",
            "library *selection*",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn what_patch_cannot_do_is_reported_with_a_reason_and_not_dropped() {
        let imported = import(&preset(&format!(
            "{}{}{}{}{}{}",
            control("0x90", "0x07", "[Channel1]", "loop_in_goto", ""),
            control("0x90", "0x08", "[Channel1]", "loop_enabled", ""),
            control("0xB0", "0x09", "[Channel1]", "pregain", ""),
            control("0x90", "0x0A", "[Sampler1]", "play", ""),
            control("0x90", "0x05", "[Channel1]", "hotcue_1_goto", ""),
            control("0x90", "0x06", "[Channel1]", "hotcue_1_goto", "")
        )));
        // Nothing was translated, and nothing vanished.
        assert_eq!(imported.report.translated, 0);
        assert_eq!(imported.report.controls, 6);
        let counts: usize = imported.report.unsupported.iter().map(|u| u.count).sum();
        assert_eq!(counts, 6);
        let text = imported
            .report
            .unsupported
            .iter()
            .map(|u| format!("{} -> {}", u.what, u.reason))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("never seek to one"), "{text}");
        assert!(text.contains("no separate enable"), "{text}");
        assert!(text.contains("gain is in dB"), "{text}");
        assert!(text.contains("is a sampler"), "{text}");
        assert!(text.contains("no Patch equivalent"), "{text}");
        // Two entries naming one control object and one reason aggregate into
        // one line, which is what keeps the report's arithmetic short and true.
        let same_control = imported
            .report
            .unsupported
            .iter()
            .find(|u| u.reason.contains("no Patch equivalent"))
            .unwrap();
        assert_eq!(same_control.count, 2, "{same_control:?}");
        assert!(imported.report.summary().contains("6 controls"));
    }

    #[test]
    fn a_kill_switch_kills_the_band_and_the_release_lifts_it() {
        let imported = import(&preset(&format!(
            "{}{}{}",
            control("0x90", "0x01", "[Channel1]", "filterLowKill", ""),
            control("0x90", "0x02", "[Channel3]", "filterHighKill", ""),
            control("0x90", "0x03", "[Sampler1]", "filterLowKill", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        // Press kills: the command's own `null` is silence, not a floor we chose.
        assert_eq!(bindings[0].action["op"], "dj.eq");
        assert_eq!(bindings[0].action["deck"], "a");
        assert_eq!(bindings[0].action["band"], "low");
        assert_eq!(bindings[0].action["db"], serde_json::Value::Null);
        // Release lifts it to unity.
        assert_eq!(bindings[1].action["db"], 0.0);
        assert_eq!(bindings[1].on, note(0, 0x01, true));
        // A third deck works, and its band is the high one.
        assert_eq!(bindings[2].action["deck"], "c");
        assert_eq!(bindings[2].action["band"], "high");
        // A sampler has no EQ: refused, by the same rule as any other non-deck
        // group, rather than mapped onto a deck that is not there.
        assert!(
            imported
                .report
                .unsupported
                .iter()
                .any(|u| u.reason.contains("is a sampler")),
            "{:?}",
            imported.report.unsupported
        );
        assert_eq!(imported.report.translated, 2);
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_manual_loop_and_reverse_reach_the_engine_and_the_rest_say_why_not() {
        let imported = import(&preset(&format!(
            "{}{}{}{}",
            control("0x90", "0x01", "[Channel1]", "loop_in", ""),
            control("0x90", "0x02", "[Channel1]", "loop_out", ""),
            control("0x90", "0x03", "[Channel2]", "reverse", ""),
            control("0x90", "0x04", "[Channel1]", "loop_in_goto", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        // The two-button manual loop, and the deck it belongs to, written in.
        assert_eq!(bindings[0].action["op"], "dj.loop_in");
        assert_eq!(bindings[0].action["deck"], "a");
        assert_eq!(bindings[1].action["op"], "dj.loop_out");
        // Reverse: press reverses, release stops, on the deck from the group.
        assert_eq!(bindings[2].action["op"], "dj.reverse");
        assert_eq!(bindings[2].action["deck"], "b");
        assert_eq!(bindings[2].action["on"], true);
        assert_eq!(bindings[3].action["op"], "dj.reverse");
        assert_eq!(bindings[3].action["on"], false);
        assert_eq!(bindings[3].on, note(0, 0x03, true), "the release ends it");
        // Three controls were translated; reverse contributes two bindings.
        assert_eq!(imported.report.translated, 3);
        assert_eq!(bindings.len(), 4);
        // Jumping to a loop point stays refused, and says why.
        assert!(
            imported
                .report
                .unsupported
                .iter()
                .any(|u| u.reason.contains("never seek to one")),
            "{:?}",
            imported.report.unsupported
        );
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_loop_pad_becomes_a_loop_toggle_and_a_roll_says_why_not() {
        let imported = import(&preset(&format!(
            "{}{}{}{}",
            control("0x90", "0x01", "[Channel1]", "beatloop_4_toggle", ""),
            control("0x90", "0x02", "[Channel1]", "beatloop_0.5_activate", ""),
            control("0x90", "0x03", "[Channel1]", "beatlooproll_2_activate", ""),
            control("0x90", "0x04", "[Channel1]", "beatloop_activate", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        // Both spellings of a sized loop become the same command, and the
        // fractional length survives.
        assert_eq!(bindings[0].action["op"], "dj.loop_toggle");
        assert_eq!(bindings[0].action["deck"], "a");
        assert_eq!(bindings[0].action["beats"], 4.0);
        assert_eq!(bindings[1].action["op"], "dj.loop_toggle");
        assert_eq!(bindings[1].action["beats"], 0.5);
        assert_eq!(imported.report.translated, 2);
        // A roll is momentary and an unsized beatloop has no size to use: both
        // are refused, with the difference stated.
        let text = imported
            .report
            .unsupported
            .iter()
            .map(|u| format!("{} -> {}", u.what, u.reason))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("only while the pad is held"), "{text}");
        assert!(text.contains("chosen last"), "{text}");
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn hot_cues_run_to_eight_and_the_ninth_says_so() {
        let imported = import(&preset(&format!(
            "{}{}{}",
            control("0x90", "0x01", "[Channel1]", "hotcue_8_activate", ""),
            control("0x90", "0x02", "[Channel1]", "hotcue_8_clear", ""),
            control("0x90", "0x03", "[Channel1]", "hotcue_9_activate", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        assert_eq!(bindings[0].action["op"], "dj.hot_cue");
        assert_eq!(bindings[0].action["n"], 8);
        assert_eq!(bindings[1].action["clear"], true);
        assert_eq!(imported.report.translated, 2);
        assert!(
            imported
                .report
                .unsupported
                .iter()
                .any(|u| u.reason.contains("eight hot cues")),
            "{:?}",
            imported.report.unsupported
        );
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_hot_cue_pad_becomes_a_hot_cue_command_and_the_rest_say_why_not() {
        let imported = import(&preset(&format!(
            "{}{}{}",
            control("0x90", "0x01", "[Channel1]", "hotcue_1_activate", ""),
            control("0x90", "0x02", "[Channel2]", "hotcue_3_clear", ""),
            control("0x90", "0x03", "[Channel1]", "hotcue_9_activate", "")
        )));
        let bindings = &imported.profile.keymap.bindings;
        // Activate: jump if the cue is stored, set it where the deck is if not —
        // which is Mixxx's own behaviour for the same control.
        assert_eq!(bindings[0].action["op"], "dj.hot_cue");
        assert_eq!(bindings[0].action["deck"], "a");
        assert_eq!(bindings[0].action["n"], 1);
        assert!(bindings[0].action.get("clear").is_none());
        // Clear, on the other deck.
        assert_eq!(bindings[1].action["op"], "dj.hot_cue");
        assert_eq!(bindings[1].action["deck"], "b");
        assert_eq!(bindings[1].action["n"], 3);
        assert_eq!(bindings[1].action["clear"], true);
        // A fifth cue now maps, because Patch holds eight; a ninth is refused
        // rather than emitted, since a profile the engine would refuse must not
        // be written. Eight is what the corpus names and what the hardware has.
        assert_eq!(imported.report.translated, 2);
        assert!(
            imported
                .report
                .unsupported
                .iter()
                .any(|u| u.reason.contains("eight hot cues")),
            "{:?}",
            imported.report.unsupported
        );
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn script_bound_entries_are_counted_separately_from_unsupported_ones() {
        let imported = import(&preset(&format!(
            "{}{}",
            control(
                "0xB0",
                "0x13",
                "[Master]",
                "crossfader",
                "<script-binding/>"
            ),
            control("0x90", "0x0B", "[Channel1]", "play", ""),
        )));
        assert_eq!(imported.report.scripted, 1);
        assert_eq!(imported.report.translated, 1);
        assert_eq!(imported.report.unsupported.len(), 0);
        assert!(imported.report.summary().contains("1 script-bound"));
    }

    #[test]
    fn the_main_cue_the_loop_and_the_kills_light_their_indicators() {
        let document = r#"<MixxxControllerPreset schemaVersion="1">
          <info><name>Test</name><author>Someone</author></info>
          <controller id="Test MIDI 1">
            <outputs>
              <output><status>0x90</status><midino>0x01</midino>
                <group>[Channel1]</group><key>cue_indicator</key></output>
              <output><status>0x90</status><midino>0x02</midino>
                <group>[Channel1]</group><key>beatloop_8_enabled</key></output>
              <output><status>0x90</status><midino>0x03</midino>
                <group>[Channel3]</group><key>beatloop_16_enabled</key></output>
              <output><status>0xB0</status><midino>0x04</midino>
                <group>[Channel2]</group><key>filterLowKill</key></output>
              <output><status>0x90</status><midino>0x05</midino>
                <group>[Channel2]</group><key>filterHighKill</key></output>
              <output><status>0x90</status><midino>0x06</midino>
                <group>[Channel1]</group><key>beatloop_3_enabled</key></output>
            </outputs>
          </controller>
        </MixxxControllerPreset>"#;
        let imported = import(document);
        let sources: Vec<&str> = imported
            .profile
            .outputs
            .iter()
            .map(|o| o.source.as_str())
            .collect();
        assert_eq!(
            sources,
            [
                "deck.a.cue",
                "deck.a.beatloop.8",
                "deck.c.beatloop.16",
                "deck.b.kill.low",
                "deck.b.kill.high",
            ],
            "each indicator follows its own state, on its own deck"
        );
        // A loop length no indicator can distinguish is reported, not rounded.
        assert_eq!(imported.report.outputs_unsupported.len(), 1);
        assert!(
            imported.report.outputs_unsupported[0]
                .reason
                .contains("no state for this indicator"),
            "{:?}",
            imported.report.outputs_unsupported
        );
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_hot_cue_led_follows_the_cue_and_the_rest_say_why_not() {
        let document = r#"<MixxxControllerPreset schemaVersion="1">
          <info><name>Test</name><author>Someone</author></info>
          <controller id="Test MIDI 1">
            <outputs>
              <output><status>0x90</status><midino>0x01</midino>
                <group>[Channel1]</group><key>hotcue_1_enabled</key></output>
              <output><status>0x9A</status><midino>0x08</midino>
                <group>[Channel2]</group><key>hotcue_8_status</key></output>
              <output><status>0x90</status><midino>0x09</midino>
                <group>[Channel1]</group><key>hotcue_9_enabled</key></output>
              <output><status>0x90</status><midino>0x01</midino>
                <group>[Sampler1]</group><key>hotcue_1_enabled</key></output>
            </outputs>
          </controller>
        </MixxxControllerPreset>"#;
        let imported = import(document);
        assert_eq!(imported.report.outputs_translated, 2);
        let outputs = &imported.profile.outputs;
        // The pad follows the cue the mapping names, on the deck its group names.
        assert_eq!(outputs[0].source, "deck.a.cue1");
        assert_eq!(outputs[1].source, "deck.b.cue8");
        assert_eq!(imported.profile.outputs.len(), 2);
        // A cue above the eighth, and a sampler group, are both reported rather
        // than bound to whichever cue or deck happens to be nearest.
        assert_eq!(imported.report.outputs_unsupported.len(), 2);
        let reasons: Vec<&str> = imported
            .report
            .outputs_unsupported
            .iter()
            .map(|u| u.reason.as_str())
            .collect();
        assert!(
            reasons.contains(&"Patch has no state for this indicator"),
            "{reasons:?}"
        );
        assert!(
            reasons.contains(&"Patch has no state for this group"),
            "{reasons:?}"
        );
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_quantize_led_follows_quantize_and_a_roll_says_what_it_is() {
        let document = r#"<MixxxControllerPreset schemaVersion="1">
          <info><name>Test</name><author>Someone</author></info>
          <controller id="Test MIDI 1">
            <outputs>
              <output><status>0x90</status><midino>0x01</midino>
                <group>[Channel1]</group><key>quantize</key></output>
              <output><status>0x90</status><midino>0x02</midino>
                <group>[Channel2]</group><key>beatlooproll_1_activate</key></output>
              <output><status>0x90</status><midino>0x03</midino>
                <group>[Channel1]</group><key>beatloop_0.5_enabled</key></output>
            </outputs>
          </controller>
        </MixxxControllerPreset>"#;
        let imported = import(document);
        assert_eq!(imported.report.outputs_translated, 1);
        assert_eq!(imported.profile.outputs[0].source, "deck.a.quantize");
        let reasons: Vec<&str> = imported
            .report
            .outputs_unsupported
            .iter()
            .map(|u| u.reason.as_str())
            .collect();
        assert!(reasons.iter().any(|r| r.contains("no roll")), "{reasons:?}");
        assert!(
            reasons.iter().any(|r| r.contains("whole beat lengths")),
            "{reasons:?}"
        );
    }

    #[test]
    fn outputs_follow_only_the_state_patch_has() {
        let document = r#"<MixxxControllerPreset schemaVersion="1">
          <info><name>Test</name><author>Someone</author></info>
          <controller id="Test MIDI 1">
            <outputs>
              <output><status>0x90</status><midino>0x0B</midino>
                <group>[Channel1]</group><key>play_indicator</key></output>
              <output><status>0xB0</status><midino>0x1F</midino>
                <group>[Channel1]</group><key>vu_meter</key></output>
              <output><status>0x90</status><midino>0x0C</midino>
                <group>[Channel1]</group><key>cue_indicator</key></output>
              <output><status>0x90</status><midino>0x0D</midino>
                <group>[Channel1]</group><key>sync_enabled</key></output>
              <output><status>0x90</status><midino>0x0E</midino>
                <group>[Channel1]</group><key>play_indicator</key>
                <options><invert/></options></output>
            </outputs>
          </controller>
        </MixxxControllerPreset>"#;
        let imported = import(document);
        // Four now: the cue indicator translates too, and only beat sync is left
        // without state to follow.
        assert_eq!(imported.report.outputs_translated, 4);
        assert_eq!(imported.report.outputs_unsupported.len(), 1);
        let reasons: Vec<&str> = imported
            .report
            .outputs_unsupported
            .iter()
            .map(|u| u.reason.as_str())
            .collect();
        assert!(
            reasons.contains(&"Patch has no state for this indicator"),
            "{reasons:?}"
        );
        let outputs = &imported.profile.outputs;
        assert_eq!(outputs[0].source, "deck.a.play");
        assert_eq!(outputs[1].source, "deck.a.vu");
        assert_eq!(outputs[2].source, "deck.a.cue");
        // `invert` swaps the LED's on and off values rather than being lost.
        match outputs[3].target {
            OutputTarget::Note {
                on_value,
                off_value,
                ..
            } => {
                assert_eq!((on_value, off_value), (0, 127));
            }
            ref other => panic!("{other:?}"),
        }
        assert!(imported.profile.validate().is_ok());
    }

    #[test]
    fn a_hid_mapping_is_marked_as_such_and_a_foreign_file_is_refused() {
        let imported = import(
            r#"<MixxxHIDPreset schemaVersion="1"><info><name>Pad</name></info>
               <controller id="Pad HID"/></MixxxHIDPreset>"#,
        );
        assert_eq!(imported.profile.protocol, Protocol::Hid);
        let error = crate::mixxx::import("<html/>").unwrap_err();
        assert!(error.contains("not a Mixxx controller preset"), "{error}");
        let error = crate::mixxx::import("<MixxxMIDIPreset><info>").unwrap_err();
        assert!(error.contains("never closed"), "{error}");
    }

    #[test]
    fn an_empty_mapping_still_produces_a_profile_the_user_can_edit() {
        let imported = import(
            r#"<MixxxControllerPreset schemaVersion="1">
               <info><name>Scripted Only</name><author>A</author></info>
               <controller id="Scripted MIDI 1">
                 <scriptfiles><file filename="common-controller-scripts.js"/></scriptfiles>
               </controller>
             </MixxxControllerPreset>"#,
        );
        assert_eq!(imported.profile.keymap.bindings.len(), 0);
        assert!(
            imported.profile.validate().is_ok(),
            "the profile is valid and editable"
        );
        assert!(
            imported
                .report
                .warnings
                .iter()
                .any(|w| w.contains("starting point")),
            "{:?}",
            imported.report.warnings
        );
        assert_eq!(imported.profile.id, "scripted-only");
    }

    /// Runs over a whole Mixxx mapping corpus, when one is pointed at.
    ///
    /// Skipped by default: the corpus is Mixxx's, not Patch's, and no test may
    /// depend on a file that is not in this repository. Point
    /// `PATCH_MIXXX_CORPUS` at a directory of `.midi.xml`/`.hid.xml` files to
    /// run it (`cargo test -p patch-controller -- --nocapture`).
    ///
    /// The invariants are the point, and they are absolute: whatever a mapping
    /// file contains, the importer produces a profile Patch accepts, with no
    /// placeholder left in any command, and every entry it could not translate
    /// accounted for in the report.
    #[test]
    fn a_mapping_corpus_imports_and_every_profile_validates() {
        let Ok(dir) = std::env::var("PATCH_MIXXX_CORPUS") else {
            return;
        };
        let mut files = 0usize;
        let mut controls = 0usize;
        let mut translated = 0usize;
        let mut scripted = 0usize;
        let mut unsupported = 0usize;
        let mut outputs_translated = 0usize;
        let mut outputs_unsupported = 0usize;
        let mut merged = 0usize;
        // Bulk presets are refused rather than imported: Patch has no bulk
        // transport. Counted here so the summary still accounts for every file.
        let mut bulk_refused = 0usize;
        // Only filled when PATCH_MIXXX_REFUSALS asks for it: the ranking below is
        // how the next implementation target is chosen, by count rather than by
        // guess, and it is not part of the invariants.
        let mut refusals: std::collections::BTreeMap<String, (usize, String)> =
            std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("corpus directory") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("xml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable mapping");
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let imported = match crate::mixxx::import_named(&text, &name) {
                Ok(imported) => imported,
                // Patch has no USB-bulk transport, so a bulk preset is refused on
                // purpose and counted as one; any *other* failure to import is a
                // real fault in the importer and stops the run.
                Err(error) if name.ends_with(".bulk.xml") => {
                    assert!(error.contains("no bulk transport"), "{name}: {error}");
                    files += 1;
                    bulk_refused += 1;
                    continue;
                }
                Err(error) => panic!("{name}: {error}"),
            };
            // Every action must be a command the engine would accept at dispatch.
            // The reader fills the two placeholders from the message, so the extremes
            // of a value are tried as well as its middle: a command that accepts only
            // part of a range would otherwise look fine here and fail on a pad.
            for binding in &imported.profile.keymap.bindings {
                // `{value}` is the message's value *scaled into the binding's range*
                // before it is filled in, so the values to try are the range's own
                // ends and middle rather than MIDI's 7-bit field.
                let tries = match binding.range {
                    Some((low, high)) => [low, (low + high) / 2.0, high],
                    None => [0.0, 64.0, 127.0],
                };
                for value in tries {
                    let filled = fill_placeholders(&binding.action, value);
                    let cmd: patch_core::command::Command = serde_json::from_value(filled)
                        .unwrap_or_else(|e| {
                            panic!(
                                "{name}: {} is not a command the mapping layer can read: {e}",
                                binding.action
                            )
                        });
                    patch_core::command::validate(
                        cmd,
                        patch_core::command::Origin::User,
                        &Default::default(),
                    )
                    .unwrap_or_else(|rejected| {
                        panic!(
                            "{name}: the engine would reject {} (value {value}): {rejected:?}",
                            binding.action
                        )
                    });
                }
            }
            // The summary's arithmetic has to be true: every entry is translated,
            // script-bound, reported with a reason, or a merged low byte. Anything
            // that falls outside those is an entry the report has lost, which is the
            // one thing an importer must never do.
            {
                let summed = imported.report.translated
                    + imported.report.scripted
                    + imported
                        .report
                        .unsupported
                        .iter()
                        .map(|u| u.count)
                        .sum::<usize>()
                    + imported.report.pairs_merged;
                assert_eq!(
                    summed,
                    imported.report.controls,
                    "{name}: the report lost {} of {} entries",
                    imported.report.controls.saturating_sub(summed),
                    imported.report.controls
                );
                // The same for the indicator half, which is where an omission hid
                // unnoticed until the refusal ranking was extended to it.
                let outputs_summed = imported.report.outputs_translated
                    + imported
                        .report
                        .outputs_unsupported
                        .iter()
                        .map(|u| u.count)
                        .sum::<usize>();
                assert_eq!(
                    outputs_summed,
                    imported.report.outputs,
                    "{name}: the report lost {} of {} indicators",
                    imported.report.outputs.saturating_sub(outputs_summed),
                    imported.report.outputs
                );
            }
            files += 1;
            controls += imported.report.controls;
            translated += imported.report.translated;
            scripted += imported.report.scripted;
            unsupported += imported
                .report
                .unsupported
                .iter()
                .map(|u| u.count)
                .sum::<usize>();
            outputs_translated += imported.report.outputs_translated;
            outputs_unsupported += imported
                .report
                .outputs_unsupported
                .iter()
                .map(|u| u.count)
                .sum::<usize>();
            merged += imported.report.pairs_merged;
            imported.profile.validate().unwrap_or_else(|error| {
                panic!("{name}: the imported profile is not valid: {error}")
            });
            for entry in &imported.report.unsupported {
                let slot = refusals
                    .entry(family(&entry.what))
                    .or_insert((0, entry.reason.clone()));
                slot.0 += entry.count;
            }
            // The output half is ranked in the same table, prefixed, so one run
            // answers what either side is still missing.
            for entry in &imported.report.outputs_unsupported {
                let slot = refusals
                    .entry(format!("output · {}", family(&entry.what)))
                    .or_insert((0, entry.reason.clone()));
                slot.0 += entry.count;
            }
            for binding in &imported.profile.keymap.bindings {
                assert!(
                    !super::holds_deck_placeholder(&binding.action),
                    "{name}: a command still asks for a deck: {:?}",
                    binding.action
                );
            }
            // Every entry is accounted for exactly once.
            assert_eq!(
                imported.report.translated
                    + imported.report.scripted
                    + imported.report.pairs_merged
                    + imported
                        .report
                        .unsupported
                        .iter()
                        .map(|u| u.count)
                        .sum::<usize>(),
                imported.report.controls,
                "{name}: entries went missing"
            );
            assert_eq!(
                imported.report.outputs_translated
                    + imported
                        .report
                        .outputs_unsupported
                        .iter()
                        .map(|u| u.count)
                        .sum::<usize>(),
                imported.report.outputs,
                "{name}: outputs went missing"
            );
        }
        assert!(files > 0, "no mapping files in {dir}");
        println!(
            "{files} mappings · {controls} controls: {translated} translated, {scripted} \
             script-bound, {unsupported} without a Patch equivalent, {merged} merged pairs · \
             outputs: {outputs_translated} translated, {outputs_unsupported} without · \
             {bulk_refused} bulk presets refused (no bulk transport)"
        );
        if std::env::var_os("PATCH_MIXXX_REFUSALS").is_some() {
            let mut ranked: Vec<(&String, &(usize, String))> = refusals.iter().collect();
            ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
            println!("\n-- refused, by control-object family --");
            for (family, (count, reason)) in ranked.iter().take(200) {
                println!("{count:5}  {family:26} {reason}");
            }
        }
    }

    /// `hotcue_1_goto (group [Channel1])` → `hotcue_N_goto`, so the ranking counts
    /// families rather than individual cue numbers.
    fn family(what: &str) -> String {
        let head = what.split(" (").next().unwrap_or(what);
        let mut out = String::with_capacity(head.len());
        let mut digits = false;
        for c in head.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    out.push('N');
                    digits = true;
                }
            } else {
                digits = false;
                out.push(c);
            }
        }
        out
    }
}
