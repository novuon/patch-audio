//! Raw HID transport: a controller whose protocol the kernel does not decode.
//!
//! A vendor-protocol controller (an NI Traktor Kontrol, and most DJ hardware)
//! exposes no evdev node: the kernel reads HID for its own input subsystem, and
//! this device is vendor usage. Patch therefore reads `/dev/hidraw*` itself,
//! extracts the report fields a profile binds, and writes output reports for
//! LEDs.
//!
//! Four rules shape this module, three of them from the device documentation:
//!
//! * **Only a path the user named is ever opened.** The device comes from
//!   `Config::hid_devices` — a path, or a device identity such as
//!   `usb:17cc:1101` resolved to whatever node it currently has. Nothing here
//!   hunts for hardware.
//! * **A first report is a value, not a change.** The device's startup packet
//!   is the current state of every control; treating it as movement would fire
//!   every binding on connect.
//! * **The last value of every field is kept**, because a platter reports a
//!   hand on the platter in one report and movement in another: press and
//!   movement arrive separately and have to be paired.
//! * **Ranges come from the device.** The `0xD0`–`0xD4` feature reports carry
//!   the calibration the unit was built with; a profile's numbers are checked
//!   against them rather than trusted blindly.
//!
//! An *offset* is measured from the first byte after the report id, and a
//! value's mask is applied without shifting, so a nibble field reads as its
//! masked byte (`0x30`, not `3`) and a binding's `min`/`max` are in that space.

use patch_core::input::{HID_FIELD_WIDTHS, InputEvent};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// One field a profile binds: report id, offset, width in bytes, mask, and
/// whether it is a relative counter — whose *change*, not its value, is the
/// movement (a jog wheel).
pub type Field = (u8, u16, u8, u32, bool);

/// The most reports one call will read, so a misbehaving device cannot hold the
/// control thread.
pub const READ_BUDGET: usize = 64;

/// The largest report this transport will read, in bytes. The S2's input
/// reports are 16 and 62 bytes; HID allows 8 KiB, which is not a budget for a
/// control-plane thread.
pub const MAX_REPORT: usize = 256;

/// What the device says about its own ranges, read from `0xD0`–`0xD4`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    /// The device's own version byte, when it answered.
    pub version: Option<u8>,
    /// Fader minimum, centre and maximum.
    pub fader: Option<(i32, i32, i32)>,
    /// The platter's released and pressed values.
    pub touch: Option<(i32, i32)>,
}

/// What makes a stream of reports a stream of events: the last value of every
/// field, which window fields are pressed, and which fields are windows.
#[derive(Clone, Debug, Default)]
pub struct DecodeState {
    last: HashMap<Field, i64>,
    windows: HashMap<Field, (i32, i32)>,
    pressed: HashMap<Field, bool>,
    /// When each relative field last moved, and whether its release has already
    /// been emitted. A jog wheel has no release of its own, so its silence is
    /// the release, and that is what ends a scratch.
    moved_at: HashMap<Field, Instant>,
    released: HashSet<Field>,
}

impl DecodeState {
    /// Forget every value, so the next report is an initial value again. Used
    /// after a reconnect: a held control must be re-learned, not assumed.
    pub fn clear(&mut self) {
        self.last.clear();
        self.pressed.clear();
        self.moved_at.clear();
        self.released.clear();
    }

    /// The release of a relative field: emitted once, for any field that has
    /// been quiet for `within`. A jog wheel has no release of its own, so its
    /// silence is one — and that is what tells a scratch to stop when the hand
    /// does. Called from the read loop, because only it knows elapsed time.
    pub fn quiet_releases(&mut self, within: Duration) -> Vec<InputEvent> {
        let now = Instant::now();
        let mut due = Vec::new();
        for (field, at) in &self.moved_at {
            let (report, offset, width, mask, relative) = *field;
            if !relative || self.released.contains(field) || now.duration_since(*at) < within {
                continue;
            }
            due.push((*field, report, offset, width, mask));
        }
        let mut out = Vec::with_capacity(due.len());
        for (field, report, offset, width, mask) in due {
            self.released.insert(field);
            out.push(InputEvent::HidField {
                report,
                offset,
                width,
                mask,
                value: 0,
            });
        }
        out
    }

    /// Read a field as a press while it is inside `min … max` — how a platter
    /// reports a hand, since it has no touch bit.
    pub fn bind_window(&mut self, field: Field, min: i32, max: i32) {
        self.windows.insert(field, (min, max));
    }

    /// Whether a window field is currently pressed.
    pub fn is_pressed(&self, field: Field) -> bool {
        self.pressed.get(&field).copied().unwrap_or(false)
    }

    /// Whether a field has been seen at all.
    pub fn has_value(&self, field: Field) -> bool {
        self.last.contains_key(&field)
    }
}

/// Extract `width` bytes at `offset` (after the report id) and apply `mask`.
///
/// Little-endian: the device's own calibration pairs are little-endian, and a
/// capture of a moved fader is what confirms the input reports follow.
fn extract(data: &[u8], offset: u16, width: u8, mask: u32) -> Option<i64> {
    let start = offset as usize;
    let end = start.checked_add(width as usize)?;
    if end > data.len() || !HID_FIELD_WIDTHS.contains(&width) {
        return None;
    }
    let mut raw: u32 = 0;
    for (i, b) in data[start..end].iter().enumerate() {
        raw |= u32::from(*b) << (8 * i);
    }
    Some(i64::from(raw & mask))
}

/// The signed change between two readings of an endless counter, over the
/// field's own span. A byte counter stepping `0xff → 0x00` reads as `+1`, and
/// one stepping `0x00 → 0xff` as `-1`. Without this the wrap looks like a
/// 255-count lurch, which is exactly what a jog wheel must never do.
///
/// The modulus comes from the *mask*, not the byte the field sits in: the S2's
/// endless gain knobs are four bits wide and share a byte with another control,
/// so a nibble stepping `0x0f → 0x00` is one tick forward. A byte-wide modulus
/// calls that `-15` — a lurch every sixteenth tick.
fn signed_delta(raw: i64, previous: i64, mask: u32) -> i64 {
    // The field's span in bits, taken from the mask itself: 0x0F -> 4, 0xFF -> 8,
    // a full-width mask -> 32. The arithmetic is widened to u64/i64 first,
    // because in u32 `mask + 1` wraps to zero at u32::MAX and
    // `next_power_of_two` overflows above 0x7fffffff — both of which a
    // four-byte field's mask reaches.
    let bits = (64 - u64::from(mask).leading_zeros()).max(1);
    let modulus = 1i64 << bits;
    let mut delta = (raw - previous).rem_euclid(modulus);
    if delta >= modulus >> 1 {
        delta -= modulus;
    }
    delta
}

/// Turn one input report into events: one per bound field that changed.
///
/// A button emits 1 or 0 so the binding's `release` flag can select a
/// direction, exactly as a note's velocity does. A window field emits only the
/// press and the release, never the value in between — a hand resting on a
/// platter moves the value continuously, and only the touch matters. A relative
/// field emits its movement — the signed change since the last reading — and an
/// axis its value, both only when they changed.
pub fn decode(
    report: u8,
    data: &[u8],
    fields: &[Field],
    state: &mut DecodeState,
) -> Vec<InputEvent> {
    let mut out = Vec::new();
    for field in fields {
        let (r, offset, width, mask, relative) = *field;
        if r != report {
            continue;
        }
        let Some(raw) = extract(data, offset, width, mask) else {
            continue;
        };
        let previous = state.last.insert(*field, raw);
        if previous.is_none() {
            // The device's first packet is a value, not a change.
            continue;
        }
        let value = match state.windows.get(field).copied() {
            Some((min, max)) => {
                let pressed = raw >= i64::from(min) && raw <= i64::from(max);
                if state.pressed.insert(*field, pressed) == Some(pressed) {
                    continue;
                }
                i64::from(pressed)
            }
            None => {
                if previous == Some(raw) {
                    continue;
                }
                if relative {
                    // A counter's movement is its change, not its value — and its
                    // *silence* will be its release. Record when it last moved.
                    state.moved_at.insert(*field, Instant::now());
                    state.released.remove(field);
                    signed_delta(raw, previous.unwrap_or(raw), mask)
                } else {
                    raw
                }
            }
        };
        out.push(InputEvent::HidField {
            report,
            offset,
            width,
            mask,
            value: value as i32,
        });
    }
    out
}

/// An open raw HID device.
pub struct HidTransport {
    pub path: std::path::PathBuf,
    file: File,
    fields: Vec<Field>,
    state: DecodeState,
    pub calibration: Calibration,
}

impl HidTransport {
    #[cfg(test)]
    pub(crate) fn from_file_for_test(file: File, fields: Vec<Field>) -> Self {
        Self {
            path: std::path::PathBuf::from("test-hidraw"),
            file,
            fields,
            state: DecodeState::default(),
            calibration: Calibration::default(),
        }
    }

    /// Open a node read-only and nonblocking, so a read never parks the
    /// control thread. `windows` are fields read as presses while inside a
    /// range; the device's own calibration is read at open.
    pub fn open(
        path: &Path,
        fields: Vec<Field>,
        windows: &[(Field, i32, i32)],
    ) -> Result<Self, String> {
        // Read-write where the device allows it: an LED report has to be written
        // back, and a read-only descriptor makes every feedback write fail with
        // EBADF — which the service reports as `sent: 0, dropped: n`, and which
        // is why the lights never lit however correct the offsets were. Fall back
        // to read-only rather than losing the input path when a rule grants only
        // read access: worst case the LEDs stay dark and everything else works.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .or_else(|_| {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)
            })
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut state = DecodeState::default();
        for (field, min, max) in windows {
            state.bind_window(*field, *min, *max);
        }
        let mut t = Self {
            path: path.to_path_buf(),
            file,
            fields,
            state,
            calibration: Calibration::default(),
        };
        t.calibration = t.read_calibration();
        // The device only starts reporting after it has been read once. That
        // first report is the current state of every control, and its values
        // are initial values rather than changes.
        let _ = t.read_input(READ_BUDGET);
        Ok(t)
    }

    /// The device's file descriptor, so the controller's existing poll set
    /// drives this transport exactly as it drives a MIDI port.
    pub fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn state(&self) -> &DecodeState {
        &self.state
    }

    /// Read whatever the device has, up to `budget` reports, and return the
    /// events they produce. Never blocks.
    pub fn read_input(&mut self, budget: usize) -> Vec<InputEvent> {
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_REPORT];
        for _ in 0..budget {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let report = buf[0];
                    let events = decode(report, &buf[1..n], &self.fields, &mut self.state);
                    out.extend(events);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // A relative control reports no release of its own. Emit one for every
        // jog that has stopped moving, so a scratch ends with the hand instead
        // of repeating until something else clears it.
        out.extend(self.quiet_releases());
        out
    }

    /// Emit releases for relative controls that have been quiet long enough.
    /// This must also be called when `poll(2)` times out: a stopped jog sends
    /// no further HID report to wake the input fd.
    pub fn quiet_releases(&mut self) -> Vec<InputEvent> {
        self.state.quiet_releases(Duration::from_millis(250))
    }

    /// Forget every value: after a reconnect nothing is known.
    pub fn forget(&mut self) {
        self.state.clear();
    }

    /// Write bytes the caller has assembled, report id first — how an LED
    /// report is sent, since it goes out whole.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write;
        if bytes.len() < 2 || bytes.len() > MAX_REPORT {
            return Err(format!("a raw report is 2…{MAX_REPORT} bytes"));
        }
        self.file.write_all(bytes).map_err(|e| {
            format!(
                "{}: writing a {}-byte report: {e}",
                self.path.display(),
                bytes.len()
            )
        })
    }

    /// Write an output report (the LED report), prefixing its id.
    pub fn write_report(&mut self, report: u8, payload: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let mut buf = Vec::with_capacity(payload.len() + 1);
        buf.push(report);
        buf.extend_from_slice(payload);
        self.file
            .write_all(&buf)
            .map_err(|e| format!("{}: writing report {report:#04x}: {e}", self.path.display()))
    }

    /// Read a feature report: `buffer` holds the report id on entry, as the
    /// ioctl requires, and the device's answer on return.
    pub fn read_feature(&mut self, report: u8, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len.max(1)];
        buf[0] = report;
        // HIDIOCGFEATURE(len) is _IOC(READ|WRITE, 'H', 0x07, len).
        let request = 0xC000_0000u64 | ((buf.len() as u64) << 16) | (u64::from(b'H') << 8) | 0x07;
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request as libc::c_ulong,
                buf.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(format!(
                "{}: feature report {report:#04x}: {}",
                self.path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(buf)
    }

    /// Ask the device what its own ranges are (`0xD0`–`0xD4`). A device that
    /// will not answer leaves the profile's numbers to stand.
    fn read_calibration(&mut self) -> Calibration {
        // Little-endian 16-bit pairs, so a value spans two bytes.
        fn pair(bytes: &[u8], i: usize) -> i32 {
            let lo = i32::from(bytes.get(i).copied().unwrap_or(0));
            let hi = i32::from(bytes.get(i + 1).copied().unwrap_or(0));
            (hi << 8) | lo
        }
        let mut c = Calibration::default();
        if let Ok(d0) = self.read_feature(0xD0, 9) {
            c.version = Some(d0.get(1).copied().unwrap_or(0));
            c.fader = Some((pair(&d0, 3), pair(&d0, 5), pair(&d0, 7)));
        }
        if let Ok(d4) = self.read_feature(0xD4, 5) {
            c.touch = Some((pair(&d4, 1), pair(&d4, 3)));
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Report 0x01: buttons, one bit each, in the byte at offset 0x0C.
    fn buttons(byte: u8) -> Vec<u8> {
        let mut v = vec![0u8; 16];
        v[12] = byte;
        v
    }

    /// Report 0x02: a 16-bit crossfader at offset 0x2E.
    fn fader(value: u16) -> Vec<u8> {
        let mut v = vec![0u8; 0x31];
        v[0x2E] = (value & 0xFF) as u8;
        v[0x2F] = (value >> 8) as u8;
        v
    }

    const PLAY: Field = (0x01, 0x0C, 1, 0x10, false);
    const PAD1: Field = (0x01, 0x0C, 1, 0x08, false);
    const CROSSFADER: Field = (0x02, 0x2E, 2, 0xFFFF, false);
    const JOG: Field = (0x01, 0x00, 1, 0xFF, true);

    #[test]
    fn a_full_width_mask_wraps_without_overflowing() {
        use super::signed_delta;
        // A four-byte field's mask is u32::MAX. Deriving the modulus as
        // `(mask + 1).next_power_of_two()` in u32 fails twice on it: the
        // addition wraps to zero, and the next power of two overflows.
        assert_eq!(signed_delta(0, -1, u32::MAX), 1, "one step forward over the wrap");
        assert_eq!(signed_delta(-1, 0, u32::MAX), -1, "and one step back");
        assert_eq!(signed_delta(0x7fff_ffff, 0, u32::MAX), 0x7fff_ffff, "half way up");
        // The narrower spans are unchanged.
        assert_eq!(signed_delta(0x00, 0xFF, 0xFF), 1, "a byte wraps at 256");
        assert_eq!(signed_delta(0x00, 0x0F, 0x0F), 1, "a nibble wraps at 16");
    }

    #[test]
    fn a_shifted_mask_keeps_its_shift_and_its_span_is_the_modulus() {
        use super::signed_delta;
        // A mask whose run starts above bit 0 is *not* normalised: the value
        // keeps the shift, so one step reads as 0x10 rather than 1. The modulus
        // is still the mask's own span, so a delta is in the field's raw units.
        // This matters only for a *relative* field, and no profile binds one to
        // a shifted mask — the S2's shifted masks are single-bit buttons, where
        // the value is only ever tested against zero.
        assert_eq!(signed_delta(0x00, 0xF0, 0xF0), 0x10, "one step up, in raw units");
        assert_eq!(signed_delta(0xF0, 0x00, 0xF0), -0x10, "and one step down");
    }

    /// A four-bit endless counter wraps at 16, not 256: the S2's gain knobs
    /// share a report-`0x02` byte and occupy only its low nibble, so `0x0f`
    /// stepping to `0x00` is one tick forward. The modulus comes from the
    /// field's mask — reading it off the byte instead made every sixteenth
    /// tick a fifteen-count lurch.
    #[test]
    fn a_four_bit_counter_wraps_at_sixteen() {
        const GAIN: Field = (0x02, 0x00, 1, 0x0F, true);
        let fields = [GAIN];
        let mut state = DecodeState::default();
        let mut data = vec![0u8; 16];

        let value = |ev: &[InputEvent]| match ev {
            [InputEvent::HidField { value, .. }] => *value,
            other => panic!("one field per change, got {other:?}"),
        };

        // The first packet is a value, not a change.
        data[0] = 0x0e;
        assert!(decode(0x02, &data, &fields, &mut state).is_empty());

        data[0] = 0x0f;
        assert_eq!(value(&decode(0x02, &data, &fields, &mut state)), 1);
        data[0] = 0x00;
        assert_eq!(
            value(&decode(0x02, &data, &fields, &mut state)),
            1,
            "a nibble wrapping to zero is one step forward, not fifteen back"
        );
        data[0] = 0x07;
        assert_eq!(value(&decode(0x02, &data, &fields, &mut state)), 7);
        data[0] = 0x05;
        assert_eq!(value(&decode(0x02, &data, &fields, &mut state)), -2);
    }

    /// A relative field is an endless counter, so what it reports is the
    /// *change* since the last packet, signed over the field's own width. The
    /// jog wheel was measured wrapping 0xff → 0x00; as a change that is one
    /// step forward, whereas a raw value would be a 255-count lurch.
    #[test]
    fn a_relative_field_reports_its_change_across_a_wrap() {
        let fields = [JOG];
        let mut state = DecodeState::default();
        let mut data = vec![0u8; 16];

        let value = |ev: &[InputEvent]| match ev {
            [InputEvent::HidField { value, .. }] => *value,
            other => panic!("one field per change, got {other:?}"),
        };

        // The first packet is a value, not a change.
        data[0] = 0xfe;
        assert!(decode(0x01, &data, &fields, &mut state).is_empty());

        data[0] = 0xff;
        assert_eq!(value(&decode(0x01, &data, &fields, &mut state)), 1);
        data[0] = 0x00;
        assert_eq!(
            value(&decode(0x01, &data, &fields, &mut state)),
            1,
            "wrapping to zero is one step forward"
        );
        data[0] = 0x80;
        assert_eq!(value(&decode(0x01, &data, &fields, &mut state)), -0x80);
        data[0] = 0x7f;
        assert_eq!(value(&decode(0x01, &data, &fields, &mut state)), -1);
        data[0] = 0x40;
        assert_eq!(value(&decode(0x01, &data, &fields, &mut state)), 0x40 - 0x7f);
    }

    #[test]
    fn a_relative_field_releases_once_after_silence() {
        let fields = [JOG];
        let mut state = DecodeState::default();
        let mut data = vec![0u8; 16];

        data[0] = 0;
        assert!(decode(0x01, &data, &fields, &mut state).is_empty());
        data[0] = 1;
        assert!(matches!(
            decode(0x01, &data, &fields, &mut state).as_slice(),
            [InputEvent::HidField { value: 1, .. }]
        ));

        let release = state.quiet_releases(Duration::ZERO);
        assert!(matches!(
            release.as_slice(),
            [InputEvent::HidField { value: 0, .. }]
        ));
        assert!(state.quiet_releases(Duration::ZERO).is_empty());

        data[0] = 2;
        assert!(matches!(
            decode(0x01, &data, &fields, &mut state).as_slice(),
            [InputEvent::HidField { value: 1, .. }]
        ));
        assert!(matches!(
            state.quiet_releases(Duration::ZERO).as_slice(),
            [InputEvent::HidField { value: 0, .. }]
        ));
    }

    /// The device's first packet is a value, not a change: nothing fires on
    /// connect, even for a control that is held.
    #[test]
    fn the_first_report_is_a_value_rather_than_a_change() {
        let fields = [PLAY, PAD1, CROSSFADER];
        let mut state = DecodeState::default();
        assert!(decode(0x01, &buttons(0x18), &fields, &mut state).is_empty());
        assert!(decode(0x02, &fader(2048), &fields, &mut state).is_empty());
        assert!(state.has_value(CROSSFADER));
    }

    #[test]
    fn a_button_in_a_shared_byte_is_read_from_its_own_bit() {
        let fields = [PLAY, PAD1];
        let mut state = DecodeState::default();
        decode(0x01, &buttons(0x00), &fields, &mut state); // initial
        let ev = decode(0x01, &buttons(0x10), &fields, &mut state);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(
            matches!(
                &ev[0],
                InputEvent::HidField {
                    report: 0x01,
                    offset: 0x0C,
                    width: 1,
                    mask: 0x10,
                    // The mask is applied without shifting, so a bit's value
                    // keeps its position in the byte; any non-zero is a press.
                    value: 0x10,
                }
            ),
            "{ev:?}"
        );
        // The pad, whose bit is in the same byte, is its own field.
        let ev = decode(0x01, &buttons(0x18), &fields, &mut state);
        assert_eq!(ev.len(), 1, "only the pad changed: {ev:?}");
        assert!(matches!(&ev[0], InputEvent::HidField { mask: 0x08, .. }));
        // And holding play while the pad is released is not a play event.
        let ev = decode(0x01, &buttons(0x10), &fields, &mut state);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(matches!(
            &ev[0],
            InputEvent::HidField {
                mask: 0x08,
                value: 0,
                ..
            }
        ));
        // Release.
        let ev = decode(0x01, &buttons(0x00), &fields, &mut state);
        assert!(matches!(
            &ev[0],
            InputEvent::HidField {
                mask: 0x10,
                value: 0,
                ..
            }
        ));
    }

    #[test]
    fn a_resting_field_sends_nothing() {
        let fields = [PLAY, CROSSFADER];
        let mut state = DecodeState::default();
        decode(0x01, &buttons(0x00), &fields, &mut state);
        decode(0x02, &fader(16), &fields, &mut state);
        assert!(decode(0x01, &buttons(0x00), &fields, &mut state).is_empty());
        assert!(decode(0x02, &fader(16), &fields, &mut state).is_empty());
        // Another report id, or a report too short to hold the field.
        assert!(decode(0x03, &buttons(0x10), &fields, &mut state).is_empty());
        assert!(decode(0x01, &[0u8; 4], &fields, &mut state).is_empty());
    }

    #[test]
    fn a_two_byte_field_is_little_endian() {
        let fields = [CROSSFADER];
        let mut state = DecodeState::default();
        decode(0x02, &fader(16), &fields, &mut state);
        let ev = decode(0x02, &fader(2048), &fields, &mut state);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(matches!(&ev[0], InputEvent::HidField { value: 2048, .. }));
        let ev = decode(0x02, &fader(4080), &fields, &mut state);
        assert!(matches!(&ev[0], InputEvent::HidField { value: 4080, .. }));
    }

    #[test]
    fn the_mask_keeps_a_nibble_field_to_its_own_bits() {
        // The browse encoder turns in the high nibble of offset 0x02, and the
        // masked value keeps the nibble's position in the byte.
        let fields: [Field; 1] = [(0x02, 0x02, 1, 0xF0, false)];
        let mut state = DecodeState::default();
        let mut data = vec![0u8; 8];
        data[2] = 0x00;
        decode(0x02, &data, &fields, &mut state); // initial
        data[2] = 0x30;
        let ev = decode(0x02, &data, &fields, &mut state);
        assert!(
            matches!(&ev[0], InputEvent::HidField { value: 0x30, .. }),
            "{ev:?}"
        );
        // A change in the low nibble alone is not a change of this field.
        data[2] = 0x3F;
        assert!(decode(0x02, &data, &fields, &mut state).is_empty());
    }

    #[test]
    fn a_field_the_report_does_not_hold_is_skipped_rather_than_guessed() {
        let beyond: [Field; 1] = [(0x02, 0x40, 2, 0xFFFF, false)];
        let mut state = DecodeState::default();
        assert!(decode(0x02, &fader(2048), &beyond, &mut state).is_empty());
        // A width that is not 1, 2 or 4 is not a field.
        let odd: [Field; 1] = [(0x02, 0x2F, 3, 0xFFFFFF, false)];
        assert!(decode(0x02, &fader(2048), &odd, &mut state).is_empty());
    }

    /// The platter has no touch bit: a value inside a window is a press, and
    /// only the press and release are events, not the value in between.
    #[test]
    fn a_window_field_reports_the_press_and_not_the_value() {
        let touch: Field = (0x02, 0x0C, 2, 0xFFFF, false);
        let fields = [touch];
        let mut state = DecodeState::default();
        state.bind_window(touch, 3000, 4095);
        let value = |v: u16| {
            let mut d = vec![0u8; 0x10];
            d[0x0C] = (v & 0xFF) as u8;
            d[0x0D] = (v >> 8) as u8;
            d
        };
        decode(0x02, &value(100), &fields, &mut state); // initial: no hand
        let ev = decode(0x02, &value(3500), &fields, &mut state);
        assert_eq!(ev.len(), 1, "a hand arriving is the event: {ev:?}");
        assert!(
            matches!(&ev[0], InputEvent::HidField { value: 1, .. }),
            "{ev:?}"
        );
        assert!(state.is_pressed(touch));
        // A hand resting on the platter moves the value; that is not an event.
        assert!(decode(0x02, &value(3700), &fields, &mut state).is_empty());
        // Leaving the window is the release.
        let ev = decode(0x02, &value(120), &fields, &mut state);
        assert!(
            matches!(&ev[0], InputEvent::HidField { value: 0, .. }),
            "{ev:?}"
        );
        assert!(!state.is_pressed(touch));
        // The boundary counts as inside: inside means inside.
        let ev = decode(0x02, &value(3000), &fields, &mut state);
        assert!(
            matches!(&ev[0], InputEvent::HidField { value: 1, .. }),
            "{ev:?}"
        );
    }

    #[test]
    fn forgetting_a_device_makes_the_next_report_an_initial_value() {
        let fields = [PLAY];
        let mut state = DecodeState::default();
        decode(0x01, &buttons(0x00), &fields, &mut state);
        assert_eq!(decode(0x01, &buttons(0x10), &fields, &mut state).len(), 1);
        assert!(decode(0x01, &buttons(0x10), &fields, &mut state).is_empty());
        state.clear();
        assert!(decode(0x01, &buttons(0x10), &fields, &mut state).is_empty());
        assert_eq!(decode(0x01, &buttons(0x00), &fields, &mut state).len(), 1);
    }

    #[test]
    fn a_missing_node_is_an_error_not_a_panic() {
        let err = HidTransport::open(Path::new("/dev/hidraw-does-not-exist"), vec![], &[]);
        assert!(
            err.is_err(),
            "opening a node that is not there must not panic"
        );
    }
}
