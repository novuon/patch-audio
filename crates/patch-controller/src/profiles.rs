//! Profiles Patch ships for hardware it knows, as data.
//!
//! The Traktor Kontrol S2 Mk1 speaks a vendor HID protocol. Mixxx drives it
//! entirely from JavaScript, so its preset has no declarative controls to
//! import; the field table below is written from the protocol facts in
//! `docs/controllers-traktor-s2-mk1.md`, which were extracted from the device
//! and measured against the hardware.
//!
//! Every action is parsed as a command in the tests, so a profile that names a
//! command Patch does not have fails to build rather than shipping. Every
//! field is asserted to be one the resolver answers, so a mistyped offset or
//! mask shows up as a test failure rather than as a dead control.

use crate::feedback::{Output, OutputTarget};
use crate::profile::{Criteria, Profile, Protocol};
use patch_core::input::{Binding, HidFieldKind, Keymap, Trigger};
use serde_json::{Value, json};

/// One deck's controls in the device's two input reports. The S2's two decks
/// differ only in offsets, so they are one table used twice.
struct DeckSpec {
    /// `a` or `b`: the deck every action for these controls names.
    deck: &'static str,
    /// Report `0x01`: the byte holding shift/sync/cue/play and pads 1–4.
    buttons: u16,
    /// Report `0x01`: the byte holding pfl, loop in and loop out.
    transport: u16,
    /// Report `0x01`: where this deck's jog wheel lives (32-bit).
    jog: u16,
    /// Report `0x02`: the tempo fader, channel fader and three EQ knobs.
    rate: u16,
    volume: u16,
    eq_high: u16,
    eq_mid: u16,
    eq_low: u16,
    /// Report `0x02`: the gain encoder, in the low nibble of this offset.
    gain: u16,
    /// Report `0x01`: the byte and bit its centre press reports on.
    gain_press: (u16, u32),
    /// Report `0x80`: where this deck's LEDs sit in the output payload.
    led_play: u16,
    led_cue: u16,
    led_pfl: u16,
    led_loop: u16,
    led_loaded: u16,
    led_pads: [u16; 4],
    led_vu: u16,
}

// The reference implementation's offsets index a buffer that *includes* the
// report id, so every offset it gives is one more than the payload byte: its
// deck 0 play sits at 0x0D and arrives in this device's byte 0x0C, its jog at
// 0x01 and arrives at 0x00. Measured against the hardware on both report-0x01
// bytes and four report-0x02 faders, which is how this was found: the tempo
// fader, crossfader and EQ knobs were all dead because they were reading the
// high byte of a 16-bit value, or one byte off. The same shift applies to its
// LED table in report 0x80 — those offsets are id-inclusive too — so the LED
// bytes below are each one lower than the reference's. With the shift applied,
// the reference's deck 0 is this device's left deck: the row labels were never
// wrong, only the offsets.
const DECKS: [DeckSpec; 2] = [
    DeckSpec {
        deck: "a",
        buttons: 0x0C,
        transport: 0x08,
        jog: 0x00,
        rate: 0x0E,
        volume: 0x2A,
        eq_high: 0x10,
        eq_mid: 0x24,
        eq_low: 0x26,
        gain: 0x00,
        gain_press: (0x0D, 0x01),
        led_play: 0x02,
        led_cue: 0x06,
        led_pfl: 0x1F,
        led_loop: 0x01,
        led_loaded: 0x1E,
        led_pads: [0x0B, 0x0A, 0x09, 0x08],
        led_vu: 0x14,
    },
    DeckSpec {
        deck: "b",
        buttons: 0x0B,
        transport: 0x0A,
        jog: 0x04,
        rate: 0x1E,
        volume: 0x2C,
        eq_high: 0x20,
        eq_mid: 0x22,
        eq_low: 0x28,
        gain: 0x02,
        gain_press: (0x0D, 0x10),
        led_play: 0x22,
        led_cue: 0x26,
        led_pfl: 0x1C,
        led_loop: 0x21,
        led_loaded: 0x1D,
        led_pads: [0x2B, 0x2A, 0x29, 0x28],
        led_vu: 0x10,
    },
];

/// One of the S2's two FX units. Patch's FX is per *deck*: one effect with a
/// depth and a length, where Traktor's unit is one box with three effect slots
/// and a parameter each. So a unit is mapped onto the deck it sits over, and
/// the three effect buttons choose which effect that deck runs rather than
/// stacking three — Patch cannot hold three depths at once.
struct FxSpec {
    /// The deck this unit drives by default: the one it sits above.
    deck: &'static str,
    /// Report `0x01`: the byte and bit holding this unit's focus button.
    focus: (u16, u32),
    /// Report `0x01`: byte and bit of the three effect buttons, in order.
    buttons: [(u16, u32); 3],
    /// Report `0x02`: the dry/wet knob, and the first effect knob (see below).
    mix: u16,
    /// Report `0x01`: byte and bit of the two channel-assign buttons, A then B.
    channels: [(u16, u32); 2],
    /// The effect each of the three buttons selects.
    kinds: [&'static str; 3],
}

/// The two FX units. The reference's offsets are id-inclusive here too, so
/// each is one less than the table. Unit 2's buttons share byte 0x09 with unit
/// 1's channel buttons, which is why neither can be read as a byte on its own.
const FX_UNITS: [FxSpec; 2] = [
    FxSpec {
        deck: "a",
        focus: (0x08, 0x08),
        buttons: [(0x08, 0x04), (0x08, 0x02), (0x08, 0x01)],
        mix: 0x0A,
        channels: [(0x09, 0x02), (0x09, 0x08)],
        kinds: ["echo", "reverb", "sweep"],
    },
    FxSpec {
        deck: "b",
        focus: (0x09, 0x80),
        buttons: [(0x09, 0x40), (0x09, 0x20), (0x09, 0x10)],
        mix: 0x1A,
        channels: [(0x09, 0x01), (0x09, 0x04)],
        kinds: ["echo", "reverb", "sweep"],
    },
];

/// The value a fader or knob reports at an end of its travel. The device's own
/// calibration (`0xD0`–`0xD4`) is read at open and shown in the unit; these are
/// the reference implementation's measured ends, used only until then.
const FADER_MIN: i32 = 16;
const FADER_MAX: i32 = 4080;

fn on(report: u8, offset: u16, width: u8, mask: u32, field: HidFieldKind) -> Trigger {
    Trigger::HidField {
        report,
        offset,
        width,
        mask,
        field,
    }
}

fn button(report: u8, offset: u16, mask: u32, release: bool) -> Trigger {
    on(report, offset, 1, mask, HidFieldKind::Button { release })
}

fn axis(offset: u16, width: u8, mask: u32) -> Trigger {
    on(
        0x02,
        offset,
        width,
        mask,
        HidFieldKind::Axis {
            min: FADER_MIN,
            max: FADER_MAX,
        },
    )
}

/// The DJ mixer's unity gain as a fader *position*: `session::fader_db` maps a
/// position 0…1 onto −60…+6 dB, so 0 dB sits at 60/66. Duplicated on purpose —
/// the controller crate does not depend on the engine — and pinned by a test.
const FADER_UNITY: f64 = 60.0 / 66.0;

/// A 4-bit endless encoder: the S2's gain knobs share a report-`0x02` byte and
/// wrap at 16. The mask is what says so, because the transport takes a relative
/// field's modulus from the mask rather than from the byte it occupies.
fn relative_nibble(offset: u16, mask: u32) -> Trigger {
    on(
        0x02,
        offset,
        1,
        mask,
        HidFieldKind::Relative {
            step: 0.05,
            release: false,
        },
    )
}

/// A relative field: an endless counter whose *change* is the movement. The jog
/// wheel's byte was measured as 8-bit and wrapping — the capture caught it
/// going `0xff → 0x00` — so the field is one byte wide, and the transport turns
/// each change into a signed delta over that width. Reading it as a 32-bit
/// value (as the reference does) makes the wrap read as a 255-count lurch.
fn relative(offset: u16) -> Trigger {
    on(
        0x01,
        offset,
        1,
        0xFF,
        HidFieldKind::Relative {
            step: 1.0,
            release: false,
        },
    )
}

/// The release half of a relative field: the transport emits a zero once the
/// wheel has been quiet, and this binding fires then. Without it a deck that
/// was nudged stays in scratch mode until something else clears it, repeating a
/// short segment — which is what a stuck jog actually sounds like.
fn relative_release(offset: u16) -> Trigger {
    on(
        0x01,
        offset,
        1,
        0xFF,
        HidFieldKind::Relative {
            step: 1.0,
            release: true,
        },
    )
}

fn binding(on: Trigger, action: Value) -> Binding {
    Binding {
        context: String::new(),
        on,
        action,
        range: None,
        soft_takeover: false,
        source: None,
    }
}

/// A control whose position Patch already knows: the hardware has to reach
/// Patch's value before it starts moving it, so nothing jumps on connect.
fn takeover(mut binding: Binding, source: &str) -> Binding {
    binding.soft_takeover = true;
    binding.source = Some(source.into());
    binding
}

/// The device's output report: 61 brightness bytes, one per LED.
const S2_LED_REPORT: u8 = 0x80;
const S2_LED_LEN: u16 = 61;

/// One LED: a byte of the output report, following an authoritative source.
fn led(offset: u16, source: &str) -> Output {
    Output {
        target: OutputTarget::HidReport {
            report: S2_LED_REPORT,
            offset,
            len: S2_LED_LEN,
            max: 0xFF,
        },
        source: source.into(),
        range: None,
        min_interval_ms: None,
    }
}

/// The LEDs this profile drives, from the device's output report. A byte no
/// output names stays dark; the two colours of a pad are separate LEDs, and
/// only the cue colour is driven because no source describes a pad's mode.
fn s2_outputs() -> Vec<Output> {
    let mut outputs = Vec::new();
    for spec in &DECKS {
        let deck = spec.deck;
        // The transport lights: play, cue, pfl, loop, track loaded.
        for (offset, source) in [
            (spec.led_play, format!("deck.{deck}.play")),
            (spec.led_cue, format!("deck.{deck}.cue")),
            (spec.led_pfl, format!("deck.{deck}.pfl")),
            (spec.led_loop, format!("deck.{deck}.loop")),
            (spec.led_loaded, format!("deck.{deck}.loaded")),
        ] {
            outputs.push(led(offset, &source));
        }
        // Pads 1–4. Each pad has two colours and the profile drives both: the
        // green from the cue being set and the blue four bytes along from the
        // same source, so a set cue lights the pair. No source describes a
        // pad's *mode*, so a mode colour is not invented here.
        for (pad, offset) in spec.led_pads.iter().enumerate() {
            let source = format!("deck.{deck}.cue{}", pad + 1);
            outputs.push(led(*offset, &source));
            outputs.push(led(offset + 4, &source));
        }
        // The level meter. A meter is a rate-limited path, not one message per
        // state change.
        let mut vu = led(spec.led_vu, &format!("deck.{deck}.vu"));
        vu.min_interval_ms = Some(50);
        outputs.push(vu);
    }
    outputs
}

/// The Traktor Kontrol S2 Mk1, as a profile Patch can drive.
pub fn traktor_kontrol_s2() -> Profile {
    let mut bindings = Vec::new();

    for spec in &DECKS {
        let deck = spec.deck;
        let b = spec.buttons;
        let t = spec.transport;

        // Transport: the four buttons sharing the report-0x01 byte.
        bindings.push(binding(
            button(0x01, b, 0x10, false),
            json!({"op": "dj.transport", "deck": deck, "action": "toggle"}),
        ));
        bindings.push(binding(
            button(0x01, b, 0x20, false),
            json!({"op": "dj.transport", "deck": deck, "action": "cue"}),
        ));
        bindings.push(binding(
            button(0x01, b, 0x20, true),
            json!({"op": "dj.transport", "deck": deck, "action": "cue_release"}),
        ));
        bindings.push(binding(
            button(0x01, b, 0x40, false),
            json!({"op": "dj.sync", "deck": deck}),
        ));

        // Pads 1–4: hot cues. The shift layer is not a command, so a pad is
        // always its hot cue.
        for (pad, mask) in [0x08u32, 0x04, 0x02, 0x01].into_iter().enumerate() {
            bindings.push(binding(
                button(0x01, b, mask, false),
                json!({"op": "dj.hot_cue", "deck": deck, "n": pad + 1}),
            ));
        }

        // Pfl, then the three loop buttons. This device has real in and out
        // points — Patch grew `dj.loop_in`/`dj.loop_out` — so the buttons are
        // the two-button loop the panel labels, and RESET leaves it. An in
        // point waiting for its out point is not yet a loop, which is what the
        // loop light shows in the meantime.
        bindings.push(binding(
            button(0x01, t, 0x80, false),
            json!({"op": "dj.toggle", "deck": deck, "what": "pfl"}),
        ));
        bindings.push(binding(
            button(0x01, t, 0x40, false),
            json!({"op": "dj.loop_in", "deck": deck}),
        ));
        bindings.push(binding(
            button(0x01, t, 0x20, false),
            json!({"op": "dj.loop_out", "deck": deck}),
        ));
        bindings.push(binding(
            button(0x01, t, 0x10, false),
            json!({"op": "dj.loop", "deck": deck, "beats": null}),
        ));

        // The knob's centre press puts the filter back to off — an endless
        // control has no other way back to the middle.
        bindings.push(binding(
            button(0x01, spec.gain_press.0, spec.gain_press.1, false),
            json!({"op": "dj.filter", "deck": deck, "amount": 0.0}),
        ));

        // The channel fader and the tempo fader are positions Patch knows, so
        // they take over rather than jump.
        // The channel fader's top is unity, not Patch's +6 dB ceiling, so a
        // deck fader at its maximum cannot drive the mix into the master
        // limiter. `range` maps the hardware's travel onto the fader positions
        // 0…unity rather than the whole 0…1.
        let mut fader = binding(
            axis(spec.volume, 2, 0xFFFF),
            json!({"op": "dj.fader", "deck": deck, "position": "{value}"}),
        );
        fader.range = Some((0.0, FADER_UNITY));
        bindings.push(takeover(fader, &format!("deck.{deck}.fader")));
        let mut tempo = binding(
            axis(spec.rate, 2, 0xFFFF),
            json!({"op": "dj.tempo", "deck": deck, "percent": "{value}"}),
        );
        tempo.range = Some((-8.0, 8.0));
        bindings.push(takeover(tempo, &format!("deck.{deck}.tempo")));

        // EQ: no authoritative equaliser value is published, so these are plain
        // axes, and the bottom of the travel stops short of the kill threshold
        // — a kill is refused from a controller, and a knob pulled to its end
        // must not turn into a rejected command.
        for (offset, band) in [
            (spec.eq_high, "high"),
            (spec.eq_mid, "mid"),
            (spec.eq_low, "low"),
        ] {
            let mut knob = binding(
                axis(offset, 2, 0xFFFF),
                json!({"op": "dj.eq_knob", "deck": deck, "band": band, "value": "{value}"}),
            );
            knob.range = Some((-0.97, 1.0));
            bindings.push(knob);
        }
        // The endless knob drives the deck's *filter*, not its gain: an endless
        // encoder cannot send a position, and the filter is the bipolar control
        // that suits one. Each tick is a twentieth of the knob's travel.
        bindings.push(binding(
            relative_nibble(spec.gain, 0x0F),
            json!({"op": "dj.filter_step", "deck": deck, "amount": "{value}"}),
        ));

        // The jog wheel moves; its *silence* ends the scratch. The transport
        // emits a release for a relative field once the wheel goes quiet, so
        // movement and release come from the same report — no platter touch is
        // needed, and this unit publishes none to read.
        bindings.push(binding(
            relative(spec.jog),
            json!({"op": "dj.scratch", "deck": deck, "delta": "{value}", "velocity": 0.0}),
        ));
        bindings.push(binding(
            relative_release(spec.jog),
            json!({"op": "dj.scratch_end", "deck": deck}),
        ));
    }

    // The crossfader is a mixer control, and Patch knows where it is.
    let mut crossfader = binding(
        axis(0x2E, 2, 0xFFFF),
        json!({"op": "dj.crossfader", "position": "{value}"}),
    );
    crossfader.range = Some((0.0, 1.0));
    bindings.push(takeover(crossfader, "crossfader"));

    // The two FX units. Each drives the deck it sits over: its three effect
    // buttons choose that deck's effect (Patch holds one effect at a time, not
    // three slots), the dry/wet knob is the effect's depth, and the channel
    // buttons hand the same choice to the other deck. The unit's second and
    // third effect knobs are deliberately left unbound: Patch's FX has one
    // depth and one length, not three parameters, and wiring them to something
    // else would move the sound under the DJ's hand.
    for unit in &FX_UNITS {
        let deck = unit.deck;
        for (slot, (offset, mask)) in unit.buttons.iter().enumerate() {
            bindings.push(binding(
                button(0x01, *offset, *mask, false),
                json!({
                    "op": "dj.fx",
                    "deck": deck,
                    "kind": unit.kinds[slot],
                    "on": true,
                }),
            ));
        }
        // Focus takes the deck's FX off again: the panel has no other switch
        // for it.
        bindings.push(binding(
            button(0x01, unit.focus.0, unit.focus.1, false),
            json!({"op": "dj.fx", "deck": deck, "on": false}),
        ));
        bindings.push(binding(
            axis(unit.mix, 2, 0xFFFF),
            json!({"op": "dj.fx", "deck": deck, "depth": "{value}"}),
        ));
        // Channel A / channel B: put that deck's FX on, so one unit can drive
        // either side without the profile remembering an assignment.
        for (which, (offset, mask)) in unit.channels.iter().enumerate() {
            bindings.push(binding(
                button(0x01, *offset, *mask, false),
                json!({"op": "dj.fx", "deck": if which == 0 { "a" } else { "b" }, "on": true}),
            ));
        }
    }

    let outputs = s2_outputs();

    Profile {
        id: "traktor-kontrol-s2-mk1".into(),
        name: "Traktor Kontrol S2 Mk1".into(),
        manufacturer: "Native Instruments".into(),
        protocol: Protocol::Hid,
        r#match: Criteria {
            vendor_id: Some(0x17cc),
            product_id: Some(0x1101),
            ..Default::default()
        },
        attribution: "Written by Patch from the device's own protocol, measured on the \
                      hardware. Mixxx's preset for this controller is JavaScript, so no \
                      mapping data was copied from it."
            .into(),
        version: crate::profile::current_version(),
        device: None,
        keymap: Keymap { bindings },
        outputs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_core::command::Command;

    fn s2() -> Profile {
        traktor_kontrol_s2()
    }

    /// The offsets are the ones measured on the hardware, and the reference's
    /// tables are each one higher (its buffer includes the report id). Both
    /// halves are pinned here on purpose: re-transcribing the reference's table
    /// would shift every fader and every LED by one and quietly break them —
    /// which is exactly what happened, twice.
    #[test]
    fn the_offsets_are_the_measured_ones() {
        let profile = s2();
        let mut fields = std::collections::BTreeSet::new();
        for binding in &profile.keymap.bindings {
            if let Trigger::HidField {
                report, offset, ..
            } = &binding.on
            {
                fields.insert((*report, *offset));
            }
        }
        let expected: std::collections::BTreeSet<(u8, u16)> = [
            // Measured off the device.
            (0x01, 0x00),
            (0x01, 0x04),
            (0x01, 0x08),
            (0x01, 0x0A),
            (0x01, 0x0B),
            (0x01, 0x0C),
            (0x02, 0x00),
            (0x02, 0x02),
            (0x02, 0x0E),
            (0x02, 0x10),
            (0x02, 0x1E),
            (0x02, 0x20),
            (0x02, 0x22),
            (0x02, 0x24),
            (0x02, 0x26),
            (0x02, 0x28),
            (0x02, 0x2A),
            (0x02, 0x2C),
            (0x02, 0x2E),
            // The FX units and the gain knob's press, from the reference table
            // with its id-inclusive shift applied — the same rule the measured
            // bytes above confirmed: its `gainEncoderPress` 0x0E arrives in
            // 0x0D, its unit-1 dry/wet 0x0B in 0x0A.
            (0x01, 0x09),
            (0x01, 0x0D),
            (0x02, 0x0A),
            (0x02, 0x1A),
        ]
        .into_iter()
        .collect();
        assert_eq!(fields, expected, "input offsets are the measured ones");

        // LEDs: the left deck's play and cue at the measured bytes.
        assert_eq!(deck_spec("a").led_play, 0x02);
        assert_eq!(deck_spec("a").led_cue, 0x06);
        assert_eq!(deck_spec("b").led_play, 0x22);
        assert_eq!(deck_spec("b").led_cue, 0x26);

        // No two LEDs share a byte — a wrong shift would land two on one.
        let mut seen = std::collections::BTreeSet::new();
        for spec in &DECKS {
            for offset in [
                spec.led_play,
                spec.led_cue,
                spec.led_pfl,
                spec.led_loop,
                spec.led_loaded,
                spec.led_vu,
            ] {
                assert!(seen.insert(offset), "two LEDs on byte {offset:#04x}");
            }
            for offset in spec.led_pads {
                assert!(seen.insert(offset), "two LEDs on byte {offset:#04x}");
            }
        }
    }

    /// The spec for one deck, by the deck it drives — never by array position,
    /// which the measured A/B swap reorders.
    fn deck_spec(deck: &str) -> &'static DeckSpec {
        DECKS
            .iter()
            .find(|d| d.deck == deck)
            .expect("both decks are described")
    }

    /// The profile must bind the controller it is written for, by the ids the
    /// hardware actually reports (measured: USB 17cc:1101, and a name that
    /// carries no "Mk1").
    #[test]
    fn the_s2_profile_matches_the_controller_it_is_for() {
        let identity = crate::identity::Identity {
            path: "/dev/hidraw6".into(),
            transport: "HID".into(),
            name: "Native Instruments Traktor Kontrol S2".into(),
            vendor_id: Some(0x17cc),
            product_id: Some(0x1101),
            serial: Some("522EB6F6".into()),
            ..Default::default()
        };
        let selection = crate::profile::select(&[s2()], &identity);
        assert!(
            matches!(selection, crate::profile::Selection::One { .. }),
            "{selection:?}"
        );
        // And it must not claim another device.
        let other = crate::identity::Identity {
            vendor_id: Some(0x17cc),
            product_id: Some(0x1320), // S2 Mk2: a different protocol
            ..Default::default()
        };
        assert!(matches!(
            crate::profile::select(&[s2()], &other),
            crate::profile::Selection::None
        ));
    }

    /// The `{value}` / `{deck}` placeholders, substituted the way the
    /// resolver substitutes them, so a templated action can be parsed here.
    fn filled(action: &Value) -> Value {
        match action {
            Value::String(s) if s == "{value}" => json!(0.5),
            Value::String(s) if s == "{deck}" => json!("a"),
            Value::Array(a) => Value::Array(a.iter().map(filled).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), filled(v))).collect())
            }
            v => v.clone(),
        }
    }

    /// A profile that names a command Patch does not have, or one it will not
    /// accept, must not ship. Validated as a user command, which is the
    /// strictest origin — if a control is acceptable there, it is acceptable
    /// everywhere.
    #[test]
    fn every_action_is_a_command_patch_has() {
        use patch_core::command::{Context, Origin, validate};
        for binding in &s2().keymap.bindings {
            let action = filled(&binding.action);
            let command: Command =
                serde_json::from_value(action.clone()).unwrap_or_else(|e| panic!("{action}: {e}"));
            validate(command, Origin::User, &Context::default())
                .unwrap_or_else(|e| panic!("{action}: {e}"));
        }
    }

    /// Every field is one the resolver answers: a mistyped offset or mask is a
    /// dead control, and must be a failing test instead.
    #[test]
    fn every_field_resolves_to_its_action() {
        let profile = s2();
        let map = profile.keymap.clone();
        for binding in &profile.keymap.bindings {
            let (report, offset, width, mask) = match &binding.on {
                Trigger::HidField {
                    report,
                    offset,
                    width,
                    mask,
                    ..
                } => (*report, *offset, *width, *mask),
                other => panic!("not a field: {other:?}"),
            };
            // A value that fires this binding: a press for a button or a
            // window, a release for one that listens for release, movement for
            // a relative field, and a mid-travel position for an axis.
            let value = match &binding.on {
                Trigger::HidField {
                    field: HidFieldKind::Button { release } | HidFieldKind::Window { release, .. },
                    ..
                } => i32::from(!*release),
                Trigger::HidField {
                    field: HidFieldKind::Relative { .. },
                    ..
                } => 7,
                _ => 2048,
            };
            let event = patch_core::input::InputEvent::HidField {
                report,
                offset,
                width,
                mask,
                value,
            };
            assert!(
                map.resolve("", &event).is_some(),
                "no action for {:?} = {value}",
                binding.on
            );
        }
    }

    /// The two decks share report shapes but never a field, and each action
    /// names its own deck.
    #[test]
    fn the_decks_do_not_share_a_field() {
        let profile = s2();
        let mut seen = std::collections::HashSet::new();
        for binding in &profile.keymap.bindings {
            if let Trigger::HidField {
                report,
                offset,
                width,
                mask,
                field,
            } = &binding.on
            {
                assert!(
                    seen.insert((*report, *offset, *width, *mask, format!("{field:?}"))),
                    "two bindings on one field: {:?}",
                    binding.on
                );
            }
        }
        // The left deck's play and the right deck's play are different bytes.
        let play_a = Trigger::HidField {
            report: 0x01,
            offset: 0x0C,
            width: 1,
            mask: 0x10,
            field: HidFieldKind::Button { release: false },
        };
        let play_b = Trigger::HidField {
            report: 0x01,
            offset: 0x0B,
            width: 1,
            mask: 0x10,
            field: HidFieldKind::Button { release: false },
        };
        let a = profile
            .keymap
            .resolve("", &event(play_a.clone(), 1))
            .unwrap();
        let b = profile.keymap.resolve("", &event(play_b, 1)).unwrap();
        assert_ne!(format!("{a:?}"), format!("{b:?}"));
        assert!(matches!(a, Command::DeckTransport { .. }));
    }

    fn event(trigger: Trigger, value: i32) -> patch_core::input::InputEvent {
        let Trigger::HidField {
            report,
            offset,
            width,
            mask,
            ..
        } = trigger
        else {
            unreachable!()
        };
        patch_core::input::InputEvent::HidField {
            report,
            offset,
            width,
            mask,
            value,
        }
    }

    /// The LEDs all share one output report, so the profile must assemble
    /// them: two lit LEDs are one message on the wire, not two.
    #[test]
    fn two_leds_on_one_report_are_one_message() {
        use crate::feedback::{ControllerState, DeckState, Feedback};
        let profile = s2();
        assert!(!profile.outputs.is_empty());
        let mut feedback = Feedback::new(&profile).expect("every output validates");
        let deck = DeckState {
            playing: true,
            cue_set: true,
            ..Default::default()
        };
        let state = ControllerState {
            crossfader: 0.5,
            master_vu: 0.0,
            decks: vec![deck],
        };
        let messages = feedback.poll(&state, 1_000);
        assert_eq!(messages.len(), 1, "one report, one message: {messages:?}");
        let m = &messages[0];
        assert_eq!(m[0], S2_LED_REPORT, "the report id leads the payload");
        assert_eq!(m.len(), usize::from(S2_LED_LEN) + 1, "a whole report");
        // Deck A's play LED and its cue LED, both lit, both in this report.
        let a = deck_spec("a");
        assert_eq!(m[usize::from(a.led_play) + 1], 0xFF, "play lit");
        assert_eq!(m[usize::from(a.led_cue) + 1], 0xFF, "cue lit");
        // An LED whose source says nothing is dark, and stays dark.
        let b = deck_spec("b");
        assert_eq!(
            m[usize::from(b.led_play) + 1],
            0x00,
            "deck B is not playing"
        );
        // Nothing changed: no message at all.
        assert!(feedback.poll(&state, 1_050).is_empty());
    }

    /// A pad's cue colour follows the hot cue, and the pad LEDs of the two
    /// decks are different bytes.
    #[test]
    fn a_pad_led_follows_its_hot_cue() {
        use crate::feedback::{ControllerState, DeckState, Feedback};
        let profile = s2();
        let mut feedback = Feedback::new(&profile).expect("outputs");
        let mut cues = [false; patch_core::command::HOT_CUES];
        cues[2] = true; // pad 3
        let deck = DeckState {
            cues,
            ..Default::default()
        };
        let state = ControllerState {
            crossfader: 0.5,
            master_vu: 0.0,
            decks: vec![deck],
        };
        let messages = feedback.poll(&state, 1_000);
        assert_eq!(messages.len(), 1, "{messages:?}");
        let m = &messages[0];
        let a = deck_spec("a");
        assert_eq!(m[usize::from(a.led_pads[2]) + 1], 0xFF, "pad 3 lit");
        assert_eq!(m[usize::from(a.led_pads[0]) + 1], 0x00, "pad 1 dark");
    }

    /// The profile as JSON, for the file the service loads. Gated so the
    /// suite does not print it: `PATCH_PRINT_PROFILE=1 cargo test … -- --nocapture`.
    #[test]
    fn print_the_s2_profile_as_json() {
        if std::env::var("PATCH_PRINT_PROFILE").is_err() {
            return;
        }
        println!("{}", serde_json::to_string_pretty(&s2()).unwrap());
    }
}
