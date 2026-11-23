//! MIDI: byte stream → [`InputEvent`]s, and outbound message encoding.
//!
//! A controller's raw MIDI (ALSA rawmidi, USB MIDI) is a byte stream with
//! running status, real-time bytes that may appear anywhere (even inside a
//! message) and SysEx blocks. [`Parser`] turns it into note, controller,
//! pitch-bend and program events for the [`crate::input::Keymap`]; everything
//! else is skipped without desynchronising the stream.
//!
//! [`encode`] is the other direction: the bytes a feedback binding sends back
//! to a controller (LEDs, ring indicators, display blocks). Both directions
//! live here so there is exactly one place that knows MIDI's wire format, and
//! `patch-controller` never invents a second one.
//!
//! 14-bit controller pairs are *not* assembled here: `Parser` emits the raw
//! CC, and the device's profile decides which (channel, CC) pairs are 14-bit
//! and assembles them (see `patch_controller`). Auto-pairing every CC with
//! `cc + 32` would invent values for the many controllers that use both
//! numbers as independent controls.

use crate::input::InputEvent;

#[derive(Clone, Debug, Default)]
pub struct Parser {
    /// Running status (a channel message's status byte).
    status: Option<u8>,
    data: [u8; 2],
    have: usize,
    in_sysex: bool,
    /// Bytes seen since the last `0xF0`, for the diagnostic count only.
    sysex_bytes: u32,
}

/// Data bytes a status byte takes (None: not a message we complete).
fn data_len(status: u8) -> Option<usize> {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => Some(2),
        0xC0 | 0xD0 => Some(1),
        _ => match status {
            0xF1 | 0xF3 => Some(1),
            0xF2 => Some(2),
            _ => None,
        },
    }
}

impl Parser {
    /// Feed bytes; `out` receives each completed channel-message event.
    pub fn feed(&mut self, bytes: &[u8], out: &mut impl FnMut(InputEvent)) {
        for &b in bytes {
            // Real-time (clock, start, stop, active sensing…): anywhere, no
            // effect on running status.
            if b >= 0xF8 {
                continue;
            }
            if b == 0xF0 {
                self.in_sysex = true;
                self.sysex_bytes = 0;
                self.status = None;
                continue;
            }
            if b == 0xF7 {
                self.in_sysex = false;
                continue;
            }
            if self.in_sysex {
                if b & 0x80 != 0 {
                    // A status byte ends an unterminated SysEx.
                    self.in_sysex = false;
                } else {
                    self.sysex_bytes = self.sysex_bytes.saturating_add(1);
                    continue;
                }
            }
            if b & 0x80 != 0 {
                self.have = 0;
                // System common messages cancel running status.
                self.status = (b < 0xF0 || data_len(b).is_some()).then_some(b);
                continue;
            }
            let Some(status) = self.status else { continue };
            let Some(need) = data_len(status) else {
                continue;
            };
            self.data[self.have] = b;
            self.have += 1;
            if self.have < need {
                continue;
            }
            self.have = 0;
            let ch = status & 0x0F;
            match status & 0xF0 {
                0x90 => out(InputEvent::MidiNote {
                    channel: ch,
                    note: self.data[0],
                    velocity: self.data[1],
                }),
                0x80 => out(InputEvent::MidiNote {
                    channel: ch,
                    note: self.data[0],
                    velocity: 0,
                }),
                0xB0 => out(InputEvent::MidiCc {
                    channel: ch,
                    cc: self.data[0],
                    value: self.data[1],
                }),
                0xC0 => out(InputEvent::MidiProgram {
                    channel: ch,
                    program: self.data[0],
                }),
                // 14-bit little-endian: LSB first in the message, centred at
                // 0x2000 (the raw pair 0x00 0x40).
                0xE0 => out(InputEvent::MidiPitchBend {
                    channel: ch,
                    value: ((self.data[1] as i16) << 7 | self.data[0] as i16) - 8192,
                }),
                _ => {}
            }
            // System common messages don't run.
            if status >= 0xF0 {
                self.status = None;
            }
        }
    }

    /// Non-zero when a SysEx block was received and skipped. Feedback
    /// bindings can send SysEx; input SysEx is currently only counted.
    pub fn sysex_bytes(&self) -> u32 {
        self.sysex_bytes
    }
}

/// Outbound message bytes: the only place that knows MIDI's wire format.
///
/// Every constructor returns the complete message, including the status
/// byte, so a caller concatenates or writes directly without knowing MIDI's
/// layout.
pub mod encode {
    /// Note on. A velocity of 0 is a note on; use [`note_off`] for an
    /// explicit note-off status (controllers differ in what they accept).
    pub fn note_on(channel: u8, note: u8, velocity: u8) -> [u8; 3] {
        [0x90 | mask(channel), note & 0x7F, velocity & 0x7F]
    }

    pub fn note_off(channel: u8, note: u8, velocity: u8) -> [u8; 3] {
        [0x80 | mask(channel), note & 0x7F, velocity & 0x7F]
    }

    pub fn cc(channel: u8, cc: u8, value: u8) -> [u8; 3] {
        [0xB0 | mask(channel), cc & 0x7F, value & 0x7F]
    }

    /// A 14-bit control as its MSB/LSB pair, in the order Mixxx and most
    /// hardware expect: `[MSB cc value_msb, MSB+32 value_lsb]`.
    pub fn cc14(channel: u8, cc: u8, value: u16) -> [u8; 6] {
        let v = value.min(16383);
        let msb = (v >> 7) as u8;
        let lsb = (v & 0x7F) as u8;
        [
            0xB0 | mask(channel),
            cc & 0x7F,
            msb,
            0xB0 | mask(channel),
            (cc & 0x7F).wrapping_add(32),
            lsb,
        ]
    }

    /// Pitch bend, centre 0. Values outside ±8192 are clamped.
    pub fn pitch_bend(channel: u8, value: i16) -> [u8; 3] {
        let v = i32::from(value).clamp(-8192, 8191) + 8192;
        [
            0xE0 | mask(channel),
            (v & 0x7F) as u8,
            ((v >> 7) & 0x7F) as u8,
        ]
    }

    pub fn program_change(channel: u8, program: u8) -> [u8; 2] {
        [0xC0 | mask(channel), program & 0x7F]
    }

    /// A complete SysEx message (`F0 … F7`). A payload already carrying its
    /// delimiters is passed through unchanged so mapping files can be written
    /// either way.
    pub fn sysex(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 2);
        if payload.first() != Some(&0xF0) {
            out.push(0xF0);
        }
        out.extend_from_slice(payload);
        if payload.last() != Some(&0xF7) {
            out.push(0xF7);
        }
        out
    }

    fn mask(channel: u8) -> u8 {
        channel & 0x0F
    }
}

/// "ch 1 · note 36 on 100", for monitors (channels shown 1-based).
pub fn describe(ev: &InputEvent) -> String {
    match ev {
        InputEvent::MidiNote {
            channel,
            note,
            velocity: 0,
        } => format!("ch {} · note {note} off", channel + 1),
        InputEvent::MidiNote {
            channel,
            note,
            velocity,
        } => format!("ch {} · note {note} on {velocity}", channel + 1),
        InputEvent::MidiCc { channel, cc, value } => {
            format!("ch {} · cc {cc} = {value}", channel + 1)
        }
        InputEvent::MidiCc14 { channel, cc, value } => {
            format!("ch {} · cc {cc} (14-bit) = {value}", channel + 1)
        }
        InputEvent::MidiPitchBend { channel, value } => {
            format!("ch {} · pitch bend {value}", channel + 1)
        }
        InputEvent::MidiProgram { channel, program } => {
            format!("ch {} · program {program}", channel + 1)
        }
        InputEvent::Hid {
            event_type,
            code,
            value,
        } => format!("HID type {event_type} · code {code} = {value}"),
        InputEvent::HidField {
            report,
            offset,
            mask,
            value,
            ..
        } => format!("HID report {report:#04x} · field {offset:#06x}/{mask:#x} = {value}"),
        InputEvent::Key { text, .. } => format!("key {text}"),
    }
}

/// The keymap trigger JSON for an event (to paste into a binding).
pub fn trigger_json(ev: &InputEvent) -> Option<String> {
    match ev {
        InputEvent::MidiNote { channel, note, .. } => Some(format!(
            r#"{{"kind":"note","channel":{channel},"note":{note}}}"#
        )),
        InputEvent::MidiCc { channel, cc, .. } => {
            Some(format!(r#"{{"kind":"cc","channel":{channel},"cc":{cc}}}"#))
        }
        InputEvent::MidiCc14 { channel, cc, .. } => Some(format!(
            r#"{{"kind":"cc14","channel":{channel},"cc":{cc}}}"#
        )),
        InputEvent::MidiPitchBend { channel, .. } => {
            Some(format!(r#"{{"kind":"pitch_bend","channel":{channel}}}"#))
        }
        InputEvent::MidiProgram { channel, program } => Some(format!(
            r#"{{"kind":"program","channel":{channel},"program":{program}}}"#
        )),
        InputEvent::Hid {
            event_type: 1,
            code,
            value,
        } => Some(format!(
            r#"{{"kind":"hidbutton","code":{code},"release":{}}}"#,
            *value == 0
        )),
        InputEvent::Hid {
            event_type: 2,
            code,
            ..
        } => Some(format!(
            r#"{{"kind":"hidrelative","code":{code},"step":0.01}}"#
        )),
        // A raw HID field, ready to paste into a profile. The width comes
        // from the field the transport read, so the binding is exact; the
        // kind is the name the device has not told us yet, and is the one
        // thing a person writes by hand.
        InputEvent::HidField {
            report,
            offset,
            width,
            mask,
            value,
        } => Some(format!(
            r#"{{"kind":"hidfield","report":{report},"offset":{offset},"width":{width},"mask":{mask},"field":{{"as":{},"release":{}}}}}"#,
            if mask.count_ones() == 1 { "\"button\"" } else { "\"axis\"" },
            *value == 0
        )),
        InputEvent::Hid { .. } | InputEvent::Key { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<InputEvent> {
        let mut p = Parser::default();
        let mut v = vec![];
        p.feed(bytes, &mut |e| v.push(e));
        v
    }

    fn note(channel: u8, note: u8, velocity: u8) -> InputEvent {
        InputEvent::MidiNote {
            channel,
            note,
            velocity,
        }
    }

    fn cc(channel: u8, cc: u8, value: u8) -> InputEvent {
        InputEvent::MidiCc { channel, cc, value }
    }

    #[test]
    fn notes_and_controllers() {
        assert_eq!(
            parse(&[0x90, 36, 100, 0x80, 36, 64, 0xB1, 7, 127]),
            vec![note(0, 36, 100), note(0, 36, 0), cc(1, 7, 127)]
        );
    }

    #[test]
    fn running_status_and_note_on_zero_is_off() {
        assert_eq!(
            parse(&[0x92, 60, 90, 62, 0, 0xB0, 1, 10, 1, 11]),
            vec![note(2, 60, 90), note(2, 62, 0), cc(0, 1, 10), cc(0, 1, 11)]
        );
    }

    #[test]
    fn real_time_bytes_inside_a_message_change_nothing() {
        assert_eq!(
            parse(&[0xB0, 0xF8, 16, 0xFE, 64, 0xF8]),
            vec![cc(0, 16, 64)]
        );
    }

    #[test]
    fn sysex_and_other_messages_are_skipped() {
        let bytes = [
            0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7, 0xC0, 5, 0xE0, 0, 64, 0xF2, 1, 2, 0x90, 40, 1,
        ];
        assert_eq!(
            parse(&bytes),
            vec![
                InputEvent::MidiProgram {
                    channel: 0,
                    program: 5
                },
                // Centred pitch bend is 0, not a spurious +8192.
                InputEvent::MidiPitchBend {
                    channel: 0,
                    value: 0
                },
                note(0, 40, 1),
            ]
        );
    }

    #[test]
    fn split_across_reads() {
        let mut p = Parser::default();
        let mut v = vec![];
        p.feed(&[0xB3, 20], &mut |e| v.push(e));
        assert!(v.is_empty());
        p.feed(&[99, 21, 98], &mut |e| v.push(e));
        assert_eq!(v, vec![cc(3, 20, 99), cc(3, 21, 98)]);
    }

    #[test]
    fn system_common_cancels_running_status() {
        // Song select (F3 xx), then a stray data byte must not be a CC.
        assert_eq!(parse(&[0xB0, 1, 2, 0xF3, 4, 5, 6]), vec![cc(0, 1, 2)]);
    }

    #[test]
    fn pitch_bend_ranges_and_channel() {
        let bend = |v: i16| InputEvent::MidiPitchBend {
            channel: 3,
            value: v,
        };
        assert_eq!(parse(&[0xE3, 0x00, 0x00]), vec![bend(-8192)]);
        assert_eq!(parse(&[0xE3, 0x7F, 0x7F]), vec![bend(8191)]);
        // Running status: two bends in a row.
        assert_eq!(
            parse(&[0xE0, 0, 0x40, 0x7F, 0x7F]),
            vec![
                InputEvent::MidiPitchBend {
                    channel: 0,
                    value: 0
                },
                InputEvent::MidiPitchBend {
                    channel: 0,
                    value: 8191
                },
            ]
        );
    }

    #[test]
    fn program_change_does_not_consume_an_extra_byte() {
        // After a 1-data-byte status, running status still applies.
        assert_eq!(
            parse(&[0xC0, 7, 8]),
            vec![
                InputEvent::MidiProgram {
                    channel: 0,
                    program: 7
                },
                InputEvent::MidiProgram {
                    channel: 0,
                    program: 8
                },
            ]
        );
    }

    #[test]
    fn descriptions_and_triggers() {
        assert_eq!(describe(&note(0, 36, 100)), "ch 1 · note 36 on 100");
        assert_eq!(describe(&cc(15, 7, 3)), "ch 16 · cc 7 = 3");
        assert_eq!(
            trigger_json(&cc(0, 7, 3)).unwrap(),
            r#"{"kind":"cc","channel":0,"cc":7}"#
        );
        assert_eq!(
            trigger_json(&InputEvent::MidiPitchBend {
                channel: 1,
                value: 0
            })
            .unwrap(),
            r#"{"kind":"pitch_bend","channel":1}"#
        );
    }

    #[test]
    fn encoders_round_trip_through_the_parser() {
        let note_on = encode::note_on(2, 11, 127);
        assert_eq!(parse(&note_on), vec![note(2, 11, 127)]);
        let off = encode::note_off(2, 11, 0);
        assert_eq!(parse(&off), vec![note(2, 11, 0)]);
        let control = encode::cc(3, 21, 42);
        assert_eq!(parse(&control), vec![cc(3, 21, 42)]);
        let bend = encode::pitch_bend(4, -1234);
        assert_eq!(
            parse(&bend),
            vec![InputEvent::MidiPitchBend {
                channel: 4,
                value: -1234
            }]
        );
        assert_eq!(
            parse(&encode::program_change(0, 9)),
            vec![InputEvent::MidiProgram {
                channel: 0,
                program: 9
            }]
        );
    }

    #[test]
    fn fourteen_bit_pairs_encode_msb_first() {
        assert_eq!(encode::cc14(0, 0x10, 0), [0xB0, 0x10, 0, 0xB0, 0x30, 0]);
        assert_eq!(
            encode::cc14(0, 0x10, 16383),
            [0xB0, 0x10, 127, 0xB0, 0x30, 127]
        );
        // Both halves of the pair arrive as ordinary CCs, in order.
        assert_eq!(
            parse(&encode::cc14(1, 0x10, 1)),
            vec![cc(1, 0x10, 0), cc(1, 0x30, 1)]
        );
    }

    #[test]
    fn sysex_is_delimited_exactly_once() {
        assert_eq!(encode::sysex(&[0x7E, 0x00]), vec![0xF0, 0x7E, 0x00, 0xF7]);
        assert_eq!(encode::sysex(&[0xF0, 0x7E, 0xF7]), vec![0xF0, 0x7E, 0xF7]);
    }
}
