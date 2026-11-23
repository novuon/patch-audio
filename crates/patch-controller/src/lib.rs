//! The controller service: hardware in, canonical Patch commands out,
//! authoritative Patch state back to the hardware.
//!
//! No audio backend, UI or DSP dependencies. One host-owned worker polls
//! nonblocking descriptors and dispatches semantic commands. All reads,
//! device counts and monitor storage are bounded.
//!
//! # The pipeline
//!
//! ```text
//! MIDI / evdev bytes
//!   → identity (identity.rs: what the device is, stably)
//!   → profile selection (profile.rs: which mapping, and on what evidence)
//!   → mapping (patch-core::input: control → canonical Command)
//!   → service dispatch (validation, human takeover of automation, execution)
//!   → authoritative state
//!   → feedback (feedback.rs: LED / ring / display output, change-driven)
//! ```
//!
//! Nothing here decides what a control means: a mapping does. Nothing here
//! owns mixer, deck or AI state: the service does, and both the input path
//! and the feedback path read that same state.
//!
//! # Real-time discipline
//!
//! This worker is a control-plane thread and never runs in the audio callback.
//! Reads are nonblocking with a fixed per-device budget; writes are
//! nonblocking and drop (counted) rather than block when a device's buffer is
//! full; the monitor ring, the device list, the binding index and the 14-bit
//! assembly table are all fixed-size or declared-bounded.

use patch_core::{
    command::Command,
    input::{Binding, InputEvent, Keymap, Trigger},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs::File,
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

pub mod feedback;
pub mod hid;
pub mod identity;
pub mod mixxx;
pub mod profile;
pub mod profiles;
pub mod takeover;

pub use feedback::{ControllerState, DeckState, Feedback, Output, OutputTarget, Source};
pub use identity::{Identity, Strength};
pub use profile::{Criteria, Evidence, Profile, Protocol, Selection};
pub use takeover::SoftTakeover;

pub const MAX_DEVICES: usize = 32;
pub const MONITOR_LINES: usize = 32;
const READ_BUDGET: usize = 4;
/// Events decoded from one read; a read is at most 256 bytes, so this cannot
/// truncate a message, only the batch a single read produces.
const EVENTS_PER_READ: usize = 128;

pub use profile::Config;

/// The user's profile file.
pub fn config_path() -> PathBuf {
    Config::path()
}

/// One device, as the Controller unit and the monitor see it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub transport: String,
    /// "connected", "disconnected", or the open error. The Controller unit
    /// shows this verbatim.
    pub state: String,
    pub events: u64,
    /// The stable identity: vendor/product/serial, or the card id, or the
    /// path. Never shown alone — [`Device::identity_class`] says how strong
    /// it is.
    pub identity: String,
    pub identity_strength: String,
    /// How the profile was chosen.
    pub profile: String,
    pub matched: bool,
    /// What the matched profile would do: its bindings and its indicators. A
    /// matched profile with no bindings — an imported HID preset, whose controls are
    /// all JavaScript — answers to nothing, which the unit has to say rather than
    /// leaving a lit "matched" to imply otherwise.
    #[serde(default)]
    pub bindings: usize,
    #[serde(default)]
    pub outputs: usize,
    /// Messages written back to this device since it opened.
    #[serde(default)]
    pub sent: u64,
    /// Messages dropped because the device's buffer was full.
    #[serde(default)]
    pub dropped: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorLine {
    pub device: String,
    pub what: String,
    pub action: String,
    pub bound: bool,
    pub accepted: bool,
    /// Host monotonic observation time, not an audio-frame timestamp.
    pub observed_us: u64,
    pub dispatch_us: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub events: VecDeque<MonitorLine>,
    pub config_error: Option<String>,
}

/// Native evdev packets can span reads. Ignore repeats. On SYN_DROPPED,
/// fail closed until reconnect: held controls cannot be safely reconstructed
/// without querying the kernel state, so never dispatch stale transitions.
#[derive(Default)]
pub struct HidParser {
    packet: [u8; std::mem::size_of::<libc::input_event>()],
    have: usize,
    pub desynced: bool,
}

impl HidParser {
    pub fn feed(&mut self, bytes: &[u8], out: &mut impl FnMut(InputEvent)) {
        for &byte in bytes {
            self.packet[self.have] = byte;
            self.have += 1;
            if self.have != self.packet.len() {
                continue;
            }
            self.have = 0;
            // SAFETY: a full native input_event, unaligned byte storage. All
            // fields are integer types; every bit pattern is valid.
            let event = unsafe {
                std::ptr::read_unaligned(self.packet.as_ptr().cast::<libc::input_event>())
            };
            if event.type_ == 0 && event.code == 3 {
                self.desynced = true;
            }
            if self.desynced {
                continue;
            }
            if matches!((event.type_, event.value), (1, 0 | 1)) || matches!(event.type_, 2 | 3) {
                out(InputEvent::Hid {
                    event_type: event.type_,
                    code: event.code,
                    value: event.value,
                });
            }
        }
    }
}

/// The identity of one control, for indexing a profile's bindings. A MIDI
/// control keeps the status-byte form; a raw HID field is identified by where
/// it sits in the report, so eight buttons in one byte stay eight controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ControlKey {
    Midi {
        status: u16,
        channel: u16,
        data: u16,
    },
    /// Report id, byte offset after it, width in bytes, and the mask within
    /// those bytes.
    Field {
        report: u8,
        offset: u16,
        width: u8,
        mask: u32,
    },
}

/// Index control identities once at profile load. Preserve binding order and
/// context semantics; unrelated controls do not lengthen event resolution.
#[derive(Default, Clone)]
pub struct CompiledKeymap {
    controls: HashMap<ControlKey, Vec<Binding>>,
    /// A small stable id per control, assigned in sorted order. The
    /// soft-takeover table is per device, so an id only has to be unique
    /// within one keymap — which is why it does not have to pack the key.
    ids: HashMap<ControlKey, u32>,
    /// Fields a binding reads as a relative counter (a jog wheel): report id,
    /// offset, width, mask. The transport turns each change of such a field
    /// into a signed delta over its width. This is captured at compile time
    /// because an incoming event cannot say whether the field it came from is
    /// relative — so the flag cannot live in `ControlKey`, which both the
    /// profile index and event resolution share.
    relative: Vec<(u8, u16, u8, u32)>,
}

/// The HID field a key stands for, if it is one.
fn key_field(key: &ControlKey) -> Option<crate::hid::Field> {
    match key {
        ControlKey::Field {
            report,
            offset,
            width,
            mask,
        } => Some((*report, *offset, *width, *mask, false)),
        ControlKey::Midi { .. } => None,
    }
}

/// The index key for a trigger, or None for a trigger that has no device
/// identity (a keyboard binding, which profiles may not contain).
fn key_of(trigger: &Trigger) -> Option<ControlKey> {
    Some(match trigger {
        Trigger::Note { channel, note, .. } => ControlKey::Midi {
            status: 0x90,
            channel: *channel as u16,
            data: *note as u16,
        },
        Trigger::Cc { channel, cc, .. } => ControlKey::Midi {
            status: 0xb0,
            channel: *channel as u16,
            data: *cc as u16,
        },
        // A 14-bit pair shares its MSB's slot: the event variant tells the two
        // bindings apart, and a profile must not declare both for one CC.
        Trigger::Cc14 { channel, cc } => ControlKey::Midi {
            status: 0xb0,
            channel: *channel as u16,
            data: *cc as u16,
        },
        Trigger::PitchBend { channel } => ControlKey::Midi {
            status: 0xe0,
            channel: *channel as u16,
            data: 0,
        },
        Trigger::Program { channel, program } => ControlKey::Midi {
            status: 0xc0,
            channel: *channel as u16,
            data: *program as u16,
        },
        Trigger::HidButton { code, .. } => ControlKey::Midi {
            status: 1,
            channel: 0,
            data: *code,
        },
        Trigger::HidRelative { code, .. } => ControlKey::Midi {
            status: 2,
            channel: 0,
            data: *code,
        },
        Trigger::HidAxis { code, .. } => ControlKey::Midi {
            status: 3,
            channel: 0,
            data: *code,
        },
        Trigger::HidField {
            report,
            offset,
            width,
            mask,
            ..
        } => ControlKey::Field {
            report: *report,
            offset: *offset,
            width: *width,
            mask: *mask,
        },
        Trigger::Key { .. } => return None,
    })
}

/// The index key for an incoming event.
fn key_of_event(event: &InputEvent) -> Option<ControlKey> {
    Some(match event {
        InputEvent::MidiNote { channel, note, .. } => ControlKey::Midi {
            status: 0x90,
            channel: *channel as u16,
            data: *note as u16,
        },
        InputEvent::MidiCc { channel, cc, .. } => ControlKey::Midi {
            status: 0xb0,
            channel: *channel as u16,
            data: *cc as u16,
        },
        InputEvent::MidiCc14 { channel, cc, .. } => ControlKey::Midi {
            status: 0xb0,
            channel: *channel as u16,
            data: *cc as u16,
        },
        InputEvent::MidiPitchBend { channel, .. } => ControlKey::Midi {
            status: 0xe0,
            channel: *channel as u16,
            data: 0,
        },
        InputEvent::MidiProgram {
            channel, program, ..
        } => ControlKey::Midi {
            status: 0xc0,
            channel: *channel as u16,
            data: *program as u16,
        },
        InputEvent::Hid {
            event_type, code, ..
        } => ControlKey::Midi {
            status: *event_type,
            channel: 0,
            data: *code,
        },
        InputEvent::HidField {
            report,
            offset,
            width,
            mask,
            ..
        } => ControlKey::Field {
            report: *report,
            offset: *offset,
            width: *width,
            mask: *mask,
        },
        InputEvent::Key { .. } => return None,
    })
}

impl CompiledKeymap {
    pub fn new(map: Keymap) -> Self {
        let mut compiled = Self::default();
        for binding in map.bindings {
            if let Trigger::HidField {
                report,
                offset,
                width,
                mask,
                field: patch_core::input::HidFieldKind::Relative { .. },
            } = &binding.on
            {
                compiled.relative.push((*report, *offset, *width, *mask));
            }
            let Some(key) = key_of(&binding.on) else {
                continue;
            };
            compiled.controls.entry(key).or_default().push(binding);
        }
        compiled.relative.sort_unstable();
        compiled.relative.dedup();
        compiled.assign_ids();
        compiled
    }

    /// Number the controls deterministically, so a takeover id means the same
    /// control from one run to the next.
    fn assign_ids(&mut self) {
        let mut keys: Vec<ControlKey> = self.controls.keys().copied().collect();
        keys.sort_unstable();
        self.ids = keys
            .into_iter()
            .enumerate()
            .map(|(i, k)| (k, i as u32))
            .collect();
    }

    /// A control's compiled id, for the soft-takeover table.
    pub fn control_id(&self, key: ControlKey) -> Option<u32> {
        self.ids.get(&key).copied()
    }

    /// Every field this profile reads as a window — a value that means a
    /// press while it is inside a range — with the range itself.
    pub fn hid_windows(&self) -> Vec<(crate::hid::Field, i32, i32)> {
        let mut ranges: Vec<(crate::hid::Field, i32, i32)> = Vec::new();
        for (key, bindings) in &self.controls {
            let Some(field) = key_field(key) else {
                continue;
            };
            for b in bindings {
                if let Trigger::HidField {
                    field: patch_core::input::HidFieldKind::Window { min, max, .. },
                    ..
                } = &b.on
                {
                    ranges.push((field, *min, *max));
                }
            }
        }
        ranges.sort_unstable();
        ranges.dedup();
        ranges
    }

    /// Every raw HID report field this profile binds: report, offset, width,
    /// mask, and whether a binding reads it as a relative counter — whose
    /// *change*, not its value, is the movement. The bindings *are* the field
    /// table, so a transport reads exactly what something listens to and
    /// nothing else. One entry per field: the flag is looked up, so a field
    /// bound twice cannot appear twice.
    pub fn hid_fields(&self) -> Vec<(u8, u16, u8, u32, bool)> {
        let mut fields: Vec<(u8, u16, u8, u32)> = self
            .controls
            .keys()
            .filter_map(|k| match k {
                ControlKey::Field {
                    report,
                    offset,
                    width,
                    mask,
                } => Some((*report, *offset, *width, *mask)),
                ControlKey::Midi { .. } => None,
            })
            .collect();
        fields.sort_unstable();
        fields.dedup();
        let relative = &self.relative;
        fields
            .into_iter()
            .map(|f| (f.0, f.1, f.2, f.3, relative.contains(&f)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.controls.is_empty()
    }

    /// The binding that will fire for `event` in `context`, if any. Used for
    /// soft takeover, which must know the control *before* dispatching it.
    pub fn binding_for(&self, context: &str, event: &InputEvent) -> Option<(usize, &Binding)> {
        let key = key_of_event(event)?;
        let deck = context.strip_prefix("deck-");
        let bindings = self.controls.get(&key)?;
        bindings
            .iter()
            .enumerate()
            .find(|(_, b)| context_matches(b, context, deck))
    }

    pub fn resolve(&self, context: &str, event: &InputEvent) -> Option<Command> {
        let key = key_of_event(event)?;
        let deck = context.strip_prefix("deck-");
        let bindings = self.controls.get(&key)?;
        let map = Keymap {
            bindings: bindings.clone(),
        };
        // The index narrows the candidates; the keymap applies context, order
        // and value semantics, so there is exactly one implementation of
        // "what does this event mean".
        let _ = (deck, map);
        let keymap = Keymap {
            bindings: bindings.clone(),
        };
        keymap.resolve(context, event)
    }
}

fn context_matches(binding: &Binding, context: &str, deck: Option<&str>) -> bool {
    binding.context.is_empty()
        || binding.context == context
        || (binding.context == "deck" && deck.is_some())
}

/// One open device: its identity, its mapping, its feedback and its takeover
/// table.
struct Reader {
    device: Device,
    /// The `hid_devices` entry this reader came from — the path, or the
    /// identity it was named by. This, and not the resolved path, is what a
    /// scan matches: an identity never equals the node it resolved to, and
    /// reopening a device every scan would drop its decode state mid-gesture.
    named: String,
    file: Option<File>,
    /// A raw HID device: its reports are read by the transport, not by a file
    /// handle here, because the kernel does not decode its protocol.
    raw: Option<crate::hid::HidTransport>,
    /// Whether this reader is a raw one. It stays true when the transport could
    /// not be opened, so the reopen loop for file handles leaves it alone and
    /// its reason for not opening survives.
    raw_intent: bool,
    writable: bool,
    midi: patch_core::midi::Parser,
    hid: HidParser,
    map: CompiledKeymap,
    /// The 14-bit pairs this profile declares, and the last MSB seen for
    /// each. Both bounded by the profile's bindings.
    cc14: Vec<(u8, u8)>,
    msb: Vec<Option<u8>>,
    feedback: Option<Feedback>,
    takeover: SoftTakeover,
    /// Whether the takeover table has been armed from authoritative state.
    armed: bool,
    /// A device Patch declines to open (a keyboard, a mistyped path). It stays
    /// listed, with the reason, and is never retried.
    refused: bool,
    /// A device named by its identity that is not plugged in. It is listed as
    /// not connected and looked for again at every scan, so plugging it in
    /// later is enough.
    absent: bool,
    /// The binding indexes currently waiting to take over, for the monitor.
    waiting: Vec<u32>,
}

impl Reader {
    fn context(&self) -> &'static str {
        if self.device.transport == "MIDI" {
            "midi"
        } else {
            "hid"
        }
    }
}

pub struct Controller {
    config: Config,
    legacy: CompiledKeymap,
    readers: Vec<Reader>,
    /// HID devices the kernel lists that Patch has no transport for yet.
    /// Detected and reported, never opened.
    detected: Vec<Device>,
    snapshot: Snapshot,
    midi_dir: PathBuf,
    /// Where the kernel lists its HID devices. Overridable so a test can point
    /// it at a fixture tree; production always uses the kernel's own.
    hidraw_dir: PathBuf,
    last_scan: Option<Instant>,
    started: Instant,
    /// The last authoritative state the host supplied. Read-only, used to arm
    /// soft takeover with the value Patch is on; never written to and never a
    /// source of truth.
    state: ControllerState,
    /// Reused across reads so decoding does not allocate.
    pending: Vec<InputEvent>,
}

impl Default for Controller {
    fn default() -> Self {
        let mut controller =
            Self::new(Config::default(), Keymap::load(), "/dev/snd").expect("default config");
        controller.reload();
        controller
    }
}

impl Controller {
    pub fn new(
        config: Config,
        legacy: Keymap,
        midi_dir: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            legacy: CompiledKeymap::new(legacy),
            readers: vec![],
            detected: vec![],
            snapshot: Snapshot::default(),
            midi_dir: midi_dir.into(),
            hidraw_dir: PathBuf::from("/sys/class/hidraw"),
            last_scan: None,
            started: Instant::now(),
            state: ControllerState::default(),
            pending: Vec::with_capacity(EVENTS_PER_READ),
        })
    }

    /// Failed reload preserves the previous working map and devices.
    pub fn reload(&mut self) {
        match Config::load() {
            Ok(config) => {
                self.legacy = CompiledKeymap::new(Keymap::load());
                self.config = config;
                // Readers are reopened so each one picks up its (possibly new)
                // profile, identity and feedback outputs.
                self.readers.clear();
                self.last_scan = None;
                self.snapshot.config_error = None;
            }
            Err(error) => self.snapshot.config_error = Some(error),
        }
    }

    /// A controller is open (its input is read, its lights fed). A device
    /// listed but refused or unopenable (a keyboard, a node that is not an
    /// event device) has nothing to read.
    pub fn has_devices(&self) -> bool {
        self.readers.iter().any(|r| r.file.is_some() || r.raw.is_some())
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.devices = self.readers.iter().map(|r| r.device.clone()).collect();
        // Devices Patch can see but not read come after the ones it has opened,
        // deduped by path and by identity so one controller is never listed
        // twice — a device open through `/dev/input/by-id` and the same device's
        // hidraw node share an identity but not a path.
        let open_paths: Vec<&str> = self
            .readers
            .iter()
            .map(|r| r.device.path.as_str())
            .collect();
        let open_keys: Vec<&str> = self
            .readers
            .iter()
            .map(|r| r.device.identity.as_str())
            .collect();
        snapshot.devices.extend(
            self.detected
                .iter()
                .filter(|device| {
                    !open_paths.contains(&device.path.as_str())
                        && !open_keys.contains(&device.identity.as_str())
                })
                .cloned(),
        );
        snapshot
    }

    pub fn suspend(&mut self) {
        self.readers.clear();
        self.snapshot.devices.clear();
        self.last_scan = None;
    }

    /// The profile that owns `path`, if the user named the path itself.
    fn profile_for_path(&self, path: &str) -> Option<usize> {
        self.config.profile_for_path(path)
    }

    /// Chooses a profile for a device: an explicit path first (it is the
    /// user's own statement), then automatic recognition by hardware identity.
    fn resolve_profile(&self, identity: &Identity) -> Selection {
        if let Some(index) = self.profile_for_path(&identity.path) {
            return Selection::One {
                index,
                evidence: Evidence::Exact,
            };
        }
        profile::select(&self.config.profiles, identity)
    }

    fn scan(&mut self) {
        let mut paths: Vec<(String, String)> = std::fs::read_dir(&self.midi_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| s.starts_with("midiC") && s.contains('D'))
            })
            .take(MAX_DEVICES)
            .map(|e| (e.path().to_string_lossy().into_owned(), "MIDI".into()))
            .collect();
        paths.sort();
        paths.extend(
            self.config
                .hid_devices
                .iter()
                .cloned()
                .map(|p| (p, "HID".into())),
        );
        paths.dedup();
        paths.truncate(MAX_DEVICES);
        self.readers
            .retain(|r| !r.absent && paths.iter().any(|(p, _)| *p == r.named));
        for (path, transport) in paths {
            if self.readers.iter().any(|r| r.named == path) {
                continue;
            }
            self.open(path, transport);
        }
        // A device that was unplugged and came back: try to reopen anything
        // whose handle was dropped, without losing its identity or mapping.
        for index in 0..self.readers.len() {
            // A raw HID reader opens through its own transport and keeps its
            // own reason when it cannot; this loop is for handles opened here.
            if self.readers[index].raw_intent
                || self.readers[index].file.is_some()
                || self.readers[index].refused
            {
                continue;
            }
            let path = self.readers[index].device.path.clone();
            match open_handle(&path, self.readers[index].writable) {
                Ok(file) => {
                    let reader = &mut self.readers[index];
                    reader.file = Some(file);
                    reader.midi = Default::default();
                    reader.hid = Default::default();
                    reader.msb.iter_mut().for_each(|m| *m = None);
                    reader.device.state = "connected".into();
                    // The device's lights are in an unknown state after a
                    // replug: re-send everything, and make the controls take
                    // over again.
                    if let Some(feedback) = reader.feedback.as_mut() {
                        feedback.reconnect();
                    }
                    reader.armed = false;
                }
                Err(e) => self.readers[index].device.state = e,
            }
        }
        // HID devices the kernel lists that Patch cannot read. Read from sysfs
        // and never opened: a vendor-HID controller — a Traktor Kontrol S2 Mk1
        // is one — has no transport here, and showing it as *detected* with the
        // reason beats showing nothing at all.
        //
        // One entry per piece of hardware, not per interface: a three-interface
        // USB receiver arrives as three hidraw nodes and is one device.
        //
        // Nothing is filtered out here. Linux cannot say which HID devices are
        // controllers — measured, not assumed: on a test machine the keyboard
        // and the mouse both report USB interface protocol 0 ("generic HID"), a
        // keyboard's key bitmap is not reachable from its own hidraw node, and
        // the leading usage page is vendor-defined `0xff00` for a keyboard, a
        // USB receiver and a pair of headphones alike. A list that guessed
        // would hide a real controller before it hid a keyboard, so every
        // device is listed with the honest reason and the human recognises
        // their own hardware by name. The refusal that matters — never opening
        // a keyboard or a mouse — stays on the evdev path, where capabilities
        // are real.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        self.detected = crate::identity::hidraw_devices(&self.hidraw_dir)
            .into_iter()
            .filter(|node| seen.insert(node.identity.device_key()))
            .map(|node| Device {
                path: node.identity.path.clone(),
                name: node.identity.name.clone(),
                transport: "HID".into(),
                state: "detected · HID device Patch cannot read yet".into(),
                events: 0,
                identity: node.identity.key(),
                identity_strength: node.identity.strength().label().into(),
                profile: "not opened".into(),
                matched: false,
                // Not opened, so nothing has been resolved to run.
                bindings: 0,
                outputs: 0,
                sent: 0,
                dropped: 0,
            })
            .collect();
        self.last_scan = Some(Instant::now());
    }

    /// Points HID detection at another tree (a test fixture).
    pub fn with_hidraw_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.hidraw_dir = dir.into();
        self
    }

    /// Open a device the user named in `hid_devices` — a hidraw path or a
    /// device identity — when a profile for it declares HID report fields.
    /// Nothing is opened speculatively: a listed device with no profile, or a
    /// profile with no fields, stays listed. Reports whether it took the device.
    fn try_open_raw(&mut self, named: &str) -> bool {
        let named = named.to_string();
        // A named entry is either the node itself or the device's stable
        // identity (`usb:17cc:1101`), which is what survives a reconnect.
        let Some(node) = crate::identity::hidraw_devices(&self.hidraw_dir)
            .into_iter()
            .find(|d| d.identity.path == named || d.identity.key() == named)
        else {
            return false;
        };
        let identity = node.identity;
        let matched = profile::select(&self.config.profiles, &identity);
        let Some(index) = matched.index() else {
            return false;
        };
        // Everything the reader needs, cloned out so the borrow of the config
        // ends before the reader is pushed.
        let (map, profile_name, binding_count, output_count, feedback) = {
            let profile = &self.config.profiles[index];
            (
                CompiledKeymap::new(profile.keymap.clone()),
                profile.name.clone(),
                profile.keymap.bindings.len(),
                profile.outputs.len(),
                // Without this a raw reader has no feedback at all, and every
                // LED the profile declares is silently never written.
                Feedback::new(profile).ok(),
            )
        };
        let fields = map.hid_fields();
        if fields.is_empty() {
            // No declarative fields: the profile is not one this transport can
            // drive, and opening it would report bindings that cannot fire.
            return false;
        }
        let windows = map.hid_windows();
        let opened =
            crate::hid::HidTransport::open(std::path::Path::new(&identity.path), fields, &windows);
        let (state, raw) = match opened {
            Ok(t) => {
                let calibration = t.calibration.clone();
                let state = match calibration.version {
                    Some(v) => format!("open · raw HID (device reports calibration {v})"),
                    None => "open · raw HID".to_string(),
                };
                (state, Some(t))
            }
            Err(why) => (format!("cannot open · {why}"), None),
        };
        self.readers.push(Reader {
            named,
            raw_intent: true,
            device: Device {
                path: identity.path.clone(),
                name: identity.name.clone(),
                transport: "HID".into(),
                state,
                events: 0,
                identity: identity.key(),
                identity_strength: identity.strength().label().into(),
                profile: profile_name,
                matched: true,
                bindings: binding_count,
                outputs: output_count,
                sent: 0,
                dropped: 0,
            },
            file: None,
            raw,
            writable: true,
            midi: Default::default(),
            hid: Default::default(),
            map,
            cc14: vec![],
            msb: vec![],
            feedback,
            takeover: SoftTakeover::new(),
            armed: false,
            refused: false,
            absent: false,
            waiting: vec![],
        });
        true
    }

    /// Identifies a device and binds its profile, then opens it.
    fn open(&mut self, path: String, transport: String) {
        // A `/dev/hidraw*` node, or a device identity such as `usb:17cc:1101`,
        // is opened report-level: it speaks a protocol the kernel leaves alone,
        // and a profile has to declare the fields to read.
        if transport == "HID" && self.try_open_raw(&path) {
            return;
        }
        // A device named by its identity is read report-level or not at all:
        // it has no event node to fall back to. Unplugged, it waits; plugged
        // in with no profile that reads its reports, it is refused with that
        // reason (a profile added later reloads the readers).
        if transport == "HID" && !path.starts_with('/') {
            let present = crate::identity::hidraw_devices(&self.hidraw_dir)
                .iter()
                .any(|d| d.identity.key() == path);
            let state = if present {
                "connected · no profile here reads its HID reports"
            } else {
                "not connected"
            };
            self.readers.push(Reader {
                named: path.clone(),
                device: Device {
                    path: path.clone(),
                    name: path.clone(),
                    transport,
                    state: state.into(),
                    events: 0,
                    identity: path.clone(),
                    identity_strength: if present { "refused" } else { "waiting" }.into(),
                    profile: "not opened".into(),
                    matched: false,
                    bindings: 0,
                    outputs: 0,
                    sent: 0,
                    dropped: 0,
                },
                file: None,
                raw: None,
                raw_intent: true,
                writable: false,
                midi: Default::default(),
                hid: Default::default(),
                map: CompiledKeymap::default(),
                cc14: vec![],
                msb: vec![],
                feedback: None,
                takeover: SoftTakeover::new(),
                armed: true,
                refused: present,
                absent: !present,
                waiting: vec![],
            });
            return;
        }
        let identity = if transport == "MIDI" {
            identity::midi_identity(&path)
        } else {
            match identity::may_open_hid(&path) {
                Ok(identity) => identity,
                // A keyboard or a mistyped path: reported, never opened.
                Err(why) => {
                    self.readers.push(Reader {
                        named: path.clone(),
                        device: Device {
                            path: path.clone(),
                            name: path.clone(),
                            transport,
                            state: why,
                            events: 0,
                            identity: path.clone(),
                            identity_strength: "refused".into(),
                            profile: "not opened".into(),
                            matched: false,
                            // Refused before anything was resolved.
                            bindings: 0,
                            outputs: 0,
                            sent: 0,
                            dropped: 0,
                        },
                        file: None,
                        raw: None,
                        raw_intent: false,
                        writable: false,
                        midi: Default::default(),
                        hid: Default::default(),
                        map: CompiledKeymap::default(),
                        cc14: vec![],
                        msb: vec![],
                        feedback: None,
                        takeover: SoftTakeover::new(),
                        armed: true,
                        refused: true,
                        absent: false,
                        waiting: vec![],
                    });
                    return;
                }
            }
        };
        let selection = self.resolve_profile(&identity);
        let profile_index = selection.index();
        let (map, cc14, feedback, outputs) = match profile_index {
            Some(index) => {
                let profile = &self.config.profiles[index];
                let feedback = Feedback::new(profile);
                (
                    CompiledKeymap::new(profile.keymap.clone()),
                    profile.fourteen_bit(),
                    feedback.ok(),
                    profile.outputs.len(),
                )
            }
            None => (self.legacy.clone(), vec![], None, 0),
        };
        // Computed above and discarded until now: the counts are what tell a user
        // whether a match means anything.
        let bindings = profile_index.map_or(0, |i| self.config.profiles[i].keymap.bindings.len());
        let writable = feedback.as_ref().is_some_and(|f| !f.is_empty());
        let device = Device {
            path: path.clone(),
            name: identity.name.clone(),
            transport: transport.clone(),
            state: "opening".into(),
            events: 0,
            identity: identity.key(),
            identity_strength: identity.strength().label().into(),
            profile: selection.describe(&self.config.profiles),
            matched: profile_index.is_some(),
            bindings,
            outputs,
            sent: 0,
            dropped: 0,
        };
        let mut reader = Reader {
            named: path.clone(),
            device,
            file: None,
            raw: None,
            raw_intent: false,
            writable,
            midi: Default::default(),
            hid: Default::default(),
            map,
            msb: vec![None; cc14.len()],
            cc14,
            feedback,
            takeover: SoftTakeover::new(),
            // Armed on the first authoritative state the host supplies, so
            // soft takeover knows what Patch is on before the fader moves.
            armed: false,
            refused: false,
            absent: false,
            waiting: vec![],
        };
        match open_handle(&path, writable) {
            Ok(file) => {
                reader.file = Some(file);
                reader.device.state = "connected".into();
            }
            Err(e) => reader.device.state = e,
        }
        self.readers.push(reader);
    }

    /// Writes feedback for every device that declares outputs, from the
    /// authoritative state the host supplies.
    ///
    /// Called from the control-plane thread, never from the audio callback.
    /// Writes are nonblocking: a device whose buffer is full has the message
    /// dropped and counted rather than stalling the worker.
    pub fn feedback(&mut self, state: &ControllerState, now_ms: u64) {
        self.state = state.clone();
        for reader in &mut self.readers {
            if reader.armed {
                rearm_on_external_change(reader, state);
            } else {
                arm_takeover(reader, state);
                reader.armed = true;
            }
            let Some(feedback) = reader.feedback.as_mut() else {
                continue;
            };
            if feedback.is_empty() {
                continue;
            }
            let messages = feedback.poll(state, now_ms);
            if messages.is_empty() {
                continue;
            }
            if reader.raw_intent {
                // A raw device's reports go out through its own transport.
                for message in messages {
                    let Some(transport) = reader.raw.as_mut() else {
                        break;
                    };
                    match transport.write_bytes(&message) {
                        Ok(()) => reader.device.sent = reader.device.sent.saturating_add(1),
                        Err(_) => {
                            reader.device.dropped = reader.device.dropped.saturating_add(1);
                            break;
                        }
                    }
                }
                continue;
            }
            let Some(file) = reader.file.as_mut() else {
                continue;
            };
            for message in messages {
                match file.write(&message) {
                    Ok(_) => {
                        reader.device.sent = reader.device.sent.saturating_add(1);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        reader.device.dropped = reader.device.dropped.saturating_add(1);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        reader.device.dropped = reader.device.dropped.saturating_add(1);
                    }
                    Err(_) => {
                        // The device went away mid-write; the next scan
                        // reopens it and re-sends its state.
                        reader.file = None;
                        reader.device.state = "disconnected".into();
                        feedback.reconnect();
                        break;
                    }
                }
            }
        }
    }

    /// Poll up to `timeout_ms`, then at most four fixed-size reads per device.
    /// The host callback must validate and execute commands outside audio RT.
    /// No unbounded input queue and no dependence on GUI frame scheduling.
    pub fn tick(
        &mut self,
        timeout_ms: i32,
        dispatch: &mut impl FnMut(Command) -> Result<(), String>,
    ) {
        if self
            .last_scan
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            self.scan();
        }
        let mut fds = [libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        }; MAX_DEVICES];
        for (fd, reader) in fds.iter_mut().zip(&self.readers) {
            fd.fd = reader
                .file
                .as_ref()
                .map(AsRawFd::as_raw_fd)
                .or_else(|| reader.raw.as_ref().map(|t| t.as_raw_fd()))
                .unwrap_or(-1);
        }
        // SAFETY: poll receives a valid writable array for the specified length.
        let ready = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                self.readers.len() as libc::nfds_t,
                timeout_ms.clamp(0, 50),
            )
        };
        if ready < 0 {
            return;
        }
        for index in 0..self.readers.len() {
            if fds[index].revents == 0 {
                continue;
            }
            let mut disconnected = false;
            if self.readers[index].raw.is_some() {
                let mut pending = std::mem::take(&mut self.pending);
                pending.clear();
                if let Some(t) = self.readers[index].raw.as_mut() {
                    for event in t.read_input(READ_BUDGET) {
                        if pending.len() >= EVENTS_PER_READ {
                            break;
                        }
                        pending.push(event);
                    }
                }
                for event in &pending {
                    self.handle_input(index, event, dispatch);
                }
                self.pending = pending;
                continue;
            }
            for _ in 0..READ_BUDGET {
                let mut buf = [0u8; 256];
                let Some(file) = self.readers[index].file.as_mut() else {
                    break;
                };
                match file.read(&mut buf) {
                    Ok(0) => {
                        disconnected = true;
                        break;
                    }
                    Ok(n) => {
                        let midi = self.readers[index].device.transport == "MIDI";
                        let mut pending = std::mem::take(&mut self.pending);
                        pending.clear();
                        {
                            let reader = &mut self.readers[index];
                            if midi {
                                let mut out = |event: InputEvent| {
                                    if pending.len() < EVENTS_PER_READ {
                                        pending.push(event);
                                    }
                                };
                                reader.midi.feed(&buf[..n], &mut out);
                            } else {
                                let mut out = |event: InputEvent| {
                                    if pending.len() < EVENTS_PER_READ {
                                        pending.push(event);
                                    }
                                };
                                reader.hid.feed(&buf[..n], &mut out);
                            }
                        }
                        for event in &pending {
                            self.handle_input(index, event, dispatch);
                        }
                        self.pending = pending;
                        if self.readers[index].hid.desynced {
                            self.readers[index].device.state =
                                "input overrun · reconnect device".into();
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected
                || fds[index].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            {
                let reader = &mut self.readers[index];
                reader.file = None;
                reader.device.state = "disconnected".into();
                if let Some(feedback) = reader.feedback.as_mut() {
                    feedback.reconnect();
                }
                reader.armed = false;
            }
        }

        // A relative HID control has no physical release report: its silence
        // ends the gesture. Advance that deadline on every tick, including
        // poll timeouts and ticks woken by a different device, or a jog scratch
        // can remain latched indefinitely after the wheel stops.
        for index in 0..self.readers.len() {
            let events = self.readers[index]
                .raw
                .as_mut()
                .map(crate::hid::HidTransport::quiet_releases)
                .unwrap_or_default();
            for event in &events {
                self.handle_input(index, event, dispatch);
            }
        }
    }

    /// One hardware event: assemble a 14-bit pair if the profile declares one,
    /// then map it, honour soft takeover, dispatch, and tell the feedback
    /// layer not to echo what this device just sent.
    fn handle_input(
        &mut self,
        index: usize,
        event: &InputEvent,
        dispatch: &mut impl FnMut(Command) -> Result<(), String>,
    ) {
        let observed_us = self.started.elapsed().as_micros() as u64;
        self.readers[index].device.events = self.readers[index].device.events.saturating_add(1);

        // A declared 14-bit pair consumes its MSB and assembles on its LSB.
        let event: InputEvent = match event {
            InputEvent::MidiCc { channel, cc, value } => {
                let (channel, cc, value) = (*channel, *cc, *value);
                let stored = {
                    let reader = &mut self.readers[index];
                    let msb_pair = reader
                        .cc14
                        .iter()
                        .position(|(c, m)| *c == channel && *m == cc);
                    let lsb_pair = reader
                        .cc14
                        .iter()
                        .position(|(c, m)| *c == channel && m.wrapping_add(32) == cc);
                    match (msb_pair, lsb_pair) {
                        (Some(pair), _) => {
                            reader.msb[pair] = Some(value);
                            None
                        }
                        (None, Some(pair)) => {
                            let msb = reader.msb[pair].unwrap_or(0);
                            Some(InputEvent::MidiCc14 {
                                channel,
                                cc: reader.cc14[pair].1,
                                value: ((msb as u16) << 7) | u16::from(value),
                            })
                        }
                        (None, None) => Some(InputEvent::MidiCc { channel, cc, value }),
                    }
                };
                let Some(event) = stored else {
                    // The MSB on its own means nothing until its pair arrives.
                    self.note(
                        index,
                        observed_us,
                        patch_core::midi::describe(&InputEvent::MidiCc { channel, cc, value }),
                        "14-bit high byte held".into(),
                        true,
                        true,
                        0,
                    );
                    return;
                };
                event
            }
            other => other.clone(),
        };

        let context = self.readers[index].context();
        // Soft takeover: a physical control only starts driving Patch once it
        // has caught up with the value Patch is already on. The target comes
        // from the last authoritative state the host supplied, so nothing here
        // keeps a rival copy of the mixer.
        let takeover = {
            let reader = &self.readers[index];
            match key_of_event(&event).and_then(|key| {
                reader
                    .map
                    .binding_for(context, &event)
                    .and_then(|(slot, binding)| {
                        reader
                            .map
                            .control_id(key)
                            .map(|control| (takeover_key(control, slot), binding))
                    })
            }) {
                Some((control, binding)) if binding.soft_takeover => {
                    hardware_unit(&event, binding).map(|hardware| (control, hardware))
                }
                _ => None,
            }
        };
        if let Some((control, hardware)) = takeover {
            // A control Patch has no value for was never armed, and an
            // unarmed control is live: arming against its own first value
            // would be a guess, not a takeover.
            let reader = &mut self.readers[index];
            // Compare what the *hardware* reports with what the table was
            // armed against; the target is already in the table.
            if reader.takeover.accept(control, hardware).is_none() {
                if !reader.waiting.contains(&control) {
                    reader.waiting.push(control);
                }
                self.note(
                    index,
                    observed_us,
                    patch_core::midi::describe(&event),
                    "waiting for the control to catch up".into(),
                    true,
                    true,
                    0,
                );
                return;
            }
            // It is live from here on; the armed target now follows the
            // hardware (see `SoftTakeover::accept`), so nothing re-arms it
            // while the user is moving it.
            reader.waiting.retain(|c| *c != control);
        }

        let command = self.readers[index].map.resolve(context, &event);
        let bound = command.is_some();
        let before = Instant::now();
        let (action, accepted) = match command {
            Some(command) => {
                let label = command.to_json();
                match dispatch(command) {
                    Ok(()) => {
                        // The hardware knows this control's state: do not echo
                        // it back. This is what makes a reflecting device
                        // harmless.
                        self.reflect(index, &event, context);
                        (label, true)
                    }
                    Err(e) => (format!("refused · {e}"), false),
                }
            }
            None => (
                patch_core::midi::trigger_json(&event)
                    .map(|t| format!("unbound · {t}"))
                    .unwrap_or_else(|| "unbound · configure axis limits in profile".into()),
                false,
            ),
        };
        self.note(
            index,
            observed_us,
            patch_core::midi::describe(&event),
            action,
            bound,
            accepted,
            before.elapsed().as_micros() as u64,
        );
    }

    /// Marks the state the event drives as hardware-caused, so feedback does
    /// not send it straight back.
    fn reflect(&mut self, index: usize, event: &InputEvent, context: &str) {
        let source = {
            let reader = &self.readers[index];
            reader
                .map
                .binding_for(context, event)
                .and_then(|(_, binding)| binding.source.as_deref())
                .and_then(|s| s.parse::<Source>().ok())
        };
        if let Some(source) = source
            && let Some(feedback) = self.readers[index].feedback.as_mut()
        {
            feedback.reflect(source);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn note(
        &mut self,
        index: usize,
        observed_us: u64,
        what: String,
        action: String,
        bound: bool,
        accepted: bool,
        dispatch_us: u64,
    ) {
        if self.snapshot.events.len() == MONITOR_LINES {
            self.snapshot.events.pop_back();
        }
        self.snapshot.events.push_front(MonitorLine {
            device: self.readers[index].device.path.clone(),
            what,
            action,
            bound,
            accepted,
            observed_us,
            dispatch_us,
        });
    }
}

impl Reader {
    /// The value Patch is currently on for a binding's source, in the 0…1
    /// space the hardware reports. `None` when the source is unknown, which
    /// leaves the control live rather than dead.
    fn takeover_target(&self, binding: &Binding, state: &ControllerState) -> Option<f64> {
        let source: Source = binding.source.as_deref()?.parse().ok()?;
        state
            .value(source)
            .map(|value| feedback::unit(value, binding.range))
    }
}

/// The takeover table's key for one control: its compiled id plus its slot
/// within that control's bindings, so two controls cannot collide.
fn takeover_key(control: u32, slot: usize) -> u32 {
    (control << 8) | (slot as u32 & 0xFF)
}

/// Re-arms a control whose value something *other than this controller*
/// changed — the AI DJ, the UI, a remote page — so it has to catch up again
/// instead of jumping.
///
/// A change this device caused is skipped: the controller's own movement
/// moves the state with it, and demanding a catch-up for that would make every
/// touch stutter. `Feedback::reflect` marks exactly those changes.
fn rearm_on_external_change(reader: &mut Reader, state: &ControllerState) {
    let mut buckets: Vec<(ControlKey, &Vec<Binding>)> = reader
        .map
        .controls
        .iter()
        .map(|(key, bindings)| (*key, bindings))
        .collect();
    buckets.sort_by_key(|(key, _)| *key);
    let mut rearms: Vec<(u32, f64)> = Vec::new();
    for (key, bindings) in buckets {
        for (slot, binding) in bindings.iter().enumerate() {
            if !binding.soft_takeover {
                continue;
            }
            let Some(source) = binding
                .source
                .as_deref()
                .and_then(|s| s.parse::<Source>().ok())
            else {
                continue;
            };
            let self_caused = reader
                .feedback
                .as_ref()
                .is_some_and(|f| f.is_reflected(source));
            if self_caused {
                continue;
            }
            let Some(control) = reader.map.control_id(key) else {
                continue;
            };
            let control = takeover_key(control, slot);
            let Some(target) = reader.takeover_target(binding, state) else {
                continue;
            };
            if let Some(armed) = reader.takeover.armed_target(control)
                && (armed - target).abs() > takeover::PICKUP_EPSILON
            {
                rearms.push((control, target));
            }
        }
    }
    for (control, target) in rearms {
        reader.takeover.arm(control, target);
    }
}

/// Opens a device. Writable when the profile declares outputs, because a
/// controller that is only read cannot light anything up.
fn open_handle(path: &str, writable: bool) -> Result<File, String> {
    let flags = libc::O_NONBLOCK | libc::O_CLOEXEC;
    if writable {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(e) => {
                // Read-only fallback: input still works, feedback is lost, and
                // the reason is what the device's state will show.
                return std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(flags)
                    .open(path)
                    .map_err(|_| e.to_string());
            }
        }
    }
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| e.to_string())
}

/// The hardware's own value for an absolute control, in 0…1 — the space soft
/// takeover compares in.
fn hardware_unit(event: &InputEvent, binding: &Binding) -> Option<f64> {
    match (event, &binding.on) {
        (InputEvent::MidiCc { value, .. }, Trigger::Cc { .. }) => Some(*value as f64 / 127.0),
        (InputEvent::MidiCc14 { value, .. }, Trigger::Cc14 { .. }) => Some(*value as f64 / 16383.0),
        (InputEvent::MidiPitchBend { value, .. }, Trigger::PitchBend { .. }) => {
            Some((f64::from(*value) + 8192.0) / 16384.0)
        }
        (InputEvent::Hid { value, .. }, Trigger::HidAxis { min, max, .. }) if max > min => Some(
            ((*value as f64 - f64::from(*min)) / (f64::from(*max) - f64::from(*min)))
                .clamp(0.0, 1.0),
        ),
        _ => None,
    }
}

/// Arms every soft-takeover control on a device with the value Patch is on,
/// so the physical control has to catch up before it can change anything.
///
/// The order is deterministic (the index buckets are sorted), so arming — and
/// therefore the first message a device sees — does not depend on hash order.
fn arm_takeover(reader: &mut Reader, state: &ControllerState) {
    let mut buckets: Vec<(ControlKey, &Vec<Binding>)> = reader
        .map
        .controls
        .iter()
        .map(|(key, bindings)| (*key, bindings))
        .collect();
    buckets.sort_by_key(|(key, _)| *key);
    let mut arming: Vec<(u32, f64)> = Vec::new();
    for (key, bindings) in buckets {
        for (slot, binding) in bindings.iter().enumerate() {
            if !binding.soft_takeover {
                continue;
            }
            let Some(control) = reader.map.control_id(key) else {
                continue;
            };
            if let Some(target) = reader.takeover_target(binding, state) {
                arming.push((takeover_key(control, slot), target));
            }
        }
    }
    for (control, target) in arming {
        reader.takeover.arm(control, target);
    }
}

#[cfg(test)]
mod tests {
    /// Against the real device and the real config, when both are present:
    /// `PATCH_S2_HARDWARE=1 cargo test --release -p patch-controller the_s2_is_taken -- --nocapture`
    #[test]
    fn the_s2_is_taken_from_the_real_config_when_it_is_there() {
        if std::env::var("PATCH_S2_HARDWARE").is_err() {
            return;
        }
        let config = crate::profile::Config::load().expect("the user's config loads");
        println!("hid_devices: {:?}", config.hid_devices);
        println!("profiles: {}", config.profiles.len());
        let mut controller =
            Controller::new(config, Keymap::default(), "/dev/snd").expect("controller");
        // One tick scans at once, with nothing to dispatch.
        controller.tick(0, &mut |_| Ok(()));
        for (i, r) in controller.readers.iter().enumerate() {
            println!(
                "reader {i}: named={:?} raw={} path={:?} state={:?}",
                r.named,
                r.raw.is_some(),
                r.device.path,
                r.device.state
            );
        }
        println!("detected: {}", controller.detected.len());
        for device in controller.snapshot().devices {
            if device.identity.contains("17cc") {
                println!(
                    "S2: state={:?} profile={:?} matched={} bindings={} path={:?}",
                    device.state, device.profile, device.matched, device.bindings, device.path
                );
            }
        }
    }

    use super::*;
    use std::io::Write;

    fn hid_packet(event_type: u16, code: u16, value: i32) -> Vec<u8> {
        // SAFETY: integer-only native event; initialize padding as well.
        let mut event: libc::input_event = unsafe { std::mem::zeroed() };
        event.type_ = event_type;
        event.code = code;
        event.value = value;
        unsafe {
            std::slice::from_raw_parts(
                (&event as *const libc::input_event).cast::<u8>(),
                std::mem::size_of_val(&event),
            )
        }
        .to_vec()
    }

    fn binding(context: &str, on: Trigger, action: serde_json::Value) -> Binding {
        Binding {
            context: context.into(),
            on,
            action,
            range: None,
            soft_takeover: false,
            source: None,
        }
    }

    #[test]
    fn raw_jog_release_is_dispatched_after_the_poll_times_out() {
        use std::os::unix::net::UnixStream;

        let field = |release| Trigger::HidField {
            report: 0x01,
            offset: 0,
            width: 1,
            mask: 0xFF,
            field: patch_core::input::HidFieldKind::Relative { step: 1.0, release },
        };
        let bindings = vec![
            binding(
                "",
                field(false),
                serde_json::json!({"op":"dj.scratch","deck":"a","delta":"{value}","velocity":0.0}),
            ),
            binding(
                "",
                field(true),
                serde_json::json!({"op":"dj.scratch_end","deck":"a"}),
            ),
        ];
        let map = CompiledKeymap::new(Keymap { bindings });
        let (reader_stream, mut writer) = UnixStream::pair().expect("test stream");
        reader_stream
            .set_nonblocking(true)
            .expect("nonblocking reader");
        let reader_fd = std::os::fd::OwnedFd::from(reader_stream);
        let raw = crate::hid::HidTransport::from_file_for_test(
            File::from(reader_fd),
            vec![(0x01, 0, 1, 0xFF, true)],
        );
        let mut controller = Controller::new(Config::default(), Keymap::default(), "/dev/null")
            .expect("controller");
        controller.last_scan = Some(Instant::now());
        controller.readers.push(Reader {
            device: Device {
                path: "test-hidraw".into(),
                name: "Test raw HID".into(),
                transport: "HID".into(),
                state: "open".into(),
                events: 0,
                identity: "test".into(),
                identity_strength: "test".into(),
                profile: "Test jog".into(),
                matched: true,
                bindings: 2,
                outputs: 0,
                sent: 0,
                dropped: 0,
            },
            named: "test-hidraw".into(),
            file: None,
            raw: Some(raw),
            raw_intent: true,
            writable: false,
            midi: Default::default(),
            hid: Default::default(),
            map,
            cc14: vec![],
            msb: vec![],
            feedback: None,
            takeover: SoftTakeover::new(),
            armed: true,
            refused: false,
            absent: false,
            waiting: vec![],
        });
        let mut commands = Vec::new();

        // Prime the counter, then move it once. The scratch starts on the
        // report; the matching scratch_end must arrive from a later poll timeout.
        writer.write_all(&[0x01, 0x00]).expect("prime the counter");
        controller.tick(50, &mut |command| {
            commands.push(command);
            Ok(())
        });
        writer.write_all(&[0x01, 0x01]).expect("move the jog");
        controller.tick(50, &mut |command| {
            commands.push(command);
            Ok(())
        });
        assert_eq!(commands.len(), 1, "one jog move starts one scratch");

        let deadline = Instant::now() + Duration::from_millis(800);
        while commands.len() < 2 && Instant::now() < deadline {
            controller.tick(50, &mut |command| {
                commands.push(command);
                Ok(())
            });
        }

        let actions: Vec<String> = commands.iter().map(Command::to_json).collect();
        assert_eq!(actions.len(), 2, "silence ends the scratch: {actions:?}");
        assert!(actions[0].contains("dj.scratch"), "{actions:?}");
        assert!(actions[1].contains("dj.scratch_end"), "{actions:?}");
    }

    fn profile(device: &str, bindings: Vec<Binding>) -> Profile {
        Profile {
            id: "test".into(),
            name: "Test controller".into(),
            manufacturer: "Test".into(),
            protocol: Protocol::Any,
            attribution: String::new(),
            version: 1,
            device: Some(device.into()),
            r#match: Criteria::default(),
            keymap: Keymap { bindings },
            outputs: vec![],
        }
    }

    /// A FIFO that stands in for a controller, so the whole path — open,
    /// parse, map, dispatch, feedback — runs without hardware.
    struct Fifo {
        dir: PathBuf,
        node: PathBuf,
    }

    impl Fifo {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("patch-controller-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let node = dir.join("midiC9D0");
            let cpath = std::ffi::CString::new(node.to_string_lossy().as_bytes()).unwrap();
            // SAFETY: valid nul-terminated path and owner-only FIFO mode.
            assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
            Self { dir, node }
        }

        fn path(&self) -> String {
            self.node.to_string_lossy().into_owned()
        }

        /// A handle to feed the controller input. Read-write so it opens
        /// whether or not the controller has already attached.
        fn writer(&self) -> File {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.node)
                .unwrap()
        }
    }

    impl Drop for Fifo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A dispatcher that records what it was asked to run.
    struct Recorder {
        seen: Vec<Command>,
        refuse: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                seen: vec![],
                refuse: false,
            }
        }

        fn dispatch(&mut self) -> impl FnMut(Command) -> Result<(), String> + '_ {
            move |command| {
                if self.refuse {
                    return Err("refused by the service".into());
                }
                self.seen.push(command);
                Ok(())
            }
        }
    }

    #[test]
    fn a_declared_fourteen_bit_pair_is_assembled_from_its_two_halves() {
        let fifo = Fifo::new("cc14");
        let mut controller = Controller::new(
            Config {
                profiles: vec![profile(
                    &fifo.path(),
                    vec![binding(
                        "midi",
                        Trigger::Cc14 {
                            channel: 0,
                            cc: 0x10,
                        },
                        serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                    )],
                )],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        let mut writer = fifo.writer();
        let mut recorder = Recorder::new();
        controller.tick(0, &mut recorder.dispatch());
        // The high byte alone is held, not dispatched.
        writer.write_all(&[0xB0, 0x10, 0x40]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        assert!(recorder.seen.is_empty());
        assert_eq!(
            controller.snapshot().events[0].action,
            "14-bit high byte held"
        );
        // The low byte completes the value: 0x40 << 7 | 0x00 = 8192.
        writer.write_all(&[0xB0, 0x30, 0x00]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        assert_eq!(recorder.seen.len(), 1);
        match recorder.seen[0] {
            Command::Crossfader { position } => {
                assert!((position - 8192.0 / 16383.0).abs() < 1e-6, "{position}")
            }
            ref other => panic!("{other:?}"),
        }
        // A control change that is not part of a declared pair is untouched.
        writer.write_all(&[0xB0, 0x07, 0x7F]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        assert!(
            controller.snapshot().events[0]
                .action
                .starts_with("unbound")
        );
    }

    #[test]
    fn soft_takeover_holds_a_fader_until_it_catches_up() {
        let fifo = Fifo::new("takeover");
        let mut take = binding(
            "midi",
            Trigger::cc(0, 7),
            serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
        );
        take.soft_takeover = true;
        take.source = Some("crossfader".into());
        let mut controller = Controller::new(
            Config {
                profiles: vec![profile(&fifo.path(), vec![take])],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        let mut writer = fifo.writer();
        let mut recorder = Recorder::new();
        controller.tick(0, &mut recorder.dispatch());

        // Patch's crossfader is at 0.5; the fader is parked at 0.
        let state = ControllerState {
            crossfader: 0.5,
            ..Default::default()
        };
        controller.feedback(&state, 0);

        for value in [0u8, 10, 30, 60] {
            writer.write_all(&[0xB0, 7, value]).unwrap();
            controller.tick(0, &mut recorder.dispatch());
        }
        assert!(
            recorder.seen.is_empty(),
            "nothing is dispatched while the fader is far from the target"
        );
        assert_eq!(
            controller.snapshot().events[0].action,
            "waiting for the control to catch up"
        );
        // Reaching it picks the control up; 64/127 ≈ 0.504, within half a step.
        writer.write_all(&[0xB0, 7, 64]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        assert_eq!(recorder.seen.len(), 1);
        // …and from then on it is live.
        writer.write_all(&[0xB0, 7, 100]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        assert_eq!(recorder.seen.len(), 2);
    }

    #[test]
    fn feedback_reaches_the_device_and_is_not_echoed_back() {
        let fifo = Fifo::new("feedback");
        let mut play = binding(
            "midi",
            Trigger::Note {
                channel: 0,
                note: 11,
                release: false,
            },
            serde_json::json!({"op":"dj.transport","deck":"a","action":"toggle"}),
        );
        play.source = Some("deck.a.play".into());
        let mut controller = Controller::new(
            Config {
                profiles: vec![Profile {
                    outputs: vec![Output {
                        target: OutputTarget::Note {
                            channel: 0,
                            note: 11,
                            on_value: 127,
                            off_value: 0,
                        },
                        source: "deck.a.play".into(),
                        range: None,
                        min_interval_ms: None,
                    }],
                    ..profile(&fifo.path(), vec![play])
                }],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        let mut recorder = Recorder::new();
        controller.tick(0, &mut recorder.dispatch());
        assert!(controller.snapshot().devices[0].state == "connected");
        // What the match would do. A matched profile with no bindings answers to
        // nothing, which is why the unit publishes these rather than only "matched".
        assert_eq!(controller.snapshot().devices[0].bindings, 1);
        assert_eq!(controller.snapshot().devices[0].outputs, 1);

        let mut state = ControllerState {
            decks: vec![DeckState {
                playing: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        controller.feedback(&state, 0);
        assert_eq!(controller.snapshot().devices[0].sent, 1);
        // Reading the FIFO back shows the light: our own bytes, no loop.
        let mut out = [0u8; 3];
        let mut back = File::open(&fifo.node).unwrap();
        back.read_exact(&mut out).unwrap();
        assert_eq!(out, [0x90, 11, 127]);
        // The same state again produces nothing.
        controller.feedback(&state, 20);
        assert_eq!(controller.snapshot().devices[0].sent, 1);
        // A pause darkens it.
        state.decks[0].playing = false;
        controller.feedback(&state, 40);
        assert_eq!(controller.snapshot().devices[0].sent, 2);
    }

    #[test]
    fn an_ambiguous_profile_is_reported_rather_than_applied() {
        let fifo = Fifo::new("ambiguous");
        let by_path = profile(&fifo.path(), vec![]);
        let mut by_name = profile(&fifo.path(), vec![]);
        by_name.id = "by-name".into();
        by_name.name = "By name".into();
        by_name.device = None;
        by_name.r#match = Criteria {
            names: vec!["controller".into()],
            ..Default::default()
        };
        let mut also_by_name = by_name.clone();
        also_by_name.id = "also-by-name".into();
        also_by_name.r#match = Criteria {
            names: vec!["patch".into()],
            ..Default::default()
        };
        // The device as Patch would identify it: an ALSA node whose card
        // reports no USB id, so only the path and a name are available.
        let device = Identity {
            path: fifo.path(),
            transport: "MIDI".into(),
            name: "Patch test controller".into(),
            ..Default::default()
        };

        // An explicit path beats a name: the user's own statement wins.
        let controller = Controller::new(
            Config {
                profiles: vec![by_path, by_name.clone(), also_by_name.clone()],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        assert_eq!(
            controller.resolve_profile(&device),
            Selection::One {
                index: 0,
                evidence: Evidence::Exact
            }
        );

        // With no path named, two equally weak name matches are ambiguous:
        // Patch reports them rather than loading one of them.
        let controller = Controller::new(
            Config {
                profiles: vec![by_name, also_by_name],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        match controller.resolve_profile(&device) {
            Selection::Ambiguous { indexes, evidence } => {
                assert_eq!(indexes, vec![0, 1]);
                assert_eq!(evidence, Evidence::Named);
            }
            other => panic!("{other:?}"),
        }
        // A device whose profile is not there yet is still usable: no profile
        // is not the same as a refusal.
        let controller =
            Controller::new(Config::default(), Keymap::default(), fifo.dir.clone()).unwrap();
        assert_eq!(controller.resolve_profile(&device), Selection::None);
    }

    #[test]
    fn hid_repeats_and_overrun_still_fail_closed() {
        let mut parser = HidParser::default();
        let bytes = hid_packet(1, 304, 1);
        let mut events = vec![];
        parser.feed(&bytes[..3], &mut |e| events.push(e));
        assert!(events.is_empty());
        parser.feed(&bytes[3..], &mut |e| events.push(e));
        parser.feed(&hid_packet(1, 304, 2), &mut |e| events.push(e));
        parser.feed(&hid_packet(1, 304, 0), &mut |e| events.push(e));
        assert_eq!(events.len(), 2);
        parser.feed(&hid_packet(0, 3, 0), &mut |e| events.push(e));
        parser.feed(&hid_packet(1, 304, 1), &mut |e| events.push(e));
        assert!(parser.desynced);
        assert_eq!(events.len(), 2, "no stale actions after overrun");
    }

    #[test]
    fn a_hid_device_patch_cannot_read_is_listed_once_per_device() {
        // The S2 Mk1's situation: the kernel lists a HID device, no MIDI node
        // exists, and the vendor's report protocol is not implemented. It has to
        // be visible with the reason rather than absent — and once, not once per
        // interface, since its two interfaces are one piece of hardware.
        let fixture = std::env::temp_dir().join("patch-hidraw-fixture-lib");
        let _ = std::fs::remove_dir_all(&fixture);
        crate::identity::fixture_hidraw(&fixture);
        let dir = std::env::temp_dir().join("patch-hidraw-empty-midi");
        std::fs::create_dir_all(&dir).unwrap();
        let mut controller = Controller::new(Config::default(), Keymap::default(), dir.clone())
            .unwrap()
            .with_hidraw_dir(&fixture);
        let mut recorder = Recorder::new();
        controller.tick(0, &mut recorder.dispatch());
        let devices = controller.snapshot().devices;
        assert_eq!(devices.len(), 2, "{devices:?}");
        let controller_device = &devices[0];
        assert_eq!(controller_device.name, "Traktor Kontrol S2 Mk1");
        assert_eq!(controller_device.transport, "HID");
        assert_eq!(controller_device.identity, "usb:17cc:1101");
        assert!(
            controller_device.state.contains("cannot read yet"),
            "the state has to say why it cannot be used: {controller_device:?}"
        );
        assert!(
            !controller_device.matched,
            "nothing may claim a profile it cannot drive"
        );
        // Nothing was opened, so nothing can have been written to it.
        assert_eq!(
            (
                controller_device.sent,
                controller_device.dropped,
                controller_device.events
            ),
            (0, 0, 0)
        );
        // The keyboard is listed too: this list says what the kernel reports,
        // and Linux cannot tell a keyboard from a controller (measured). It is
        // still never opened — the evdev path refuses it.
        assert_eq!(devices[1].name, "AT Translated Set 2 keyboard");
        assert!(!devices[1].matched);
        let _ = std::fs::remove_dir_all(&fixture);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_command_is_visible_and_not_accepted() {
        let fifo = Fifo::new("refused");
        let mut controller = Controller::new(
            Config {
                profiles: vec![profile(
                    &fifo.path(),
                    vec![binding(
                        "midi",
                        Trigger::cc(0, 7),
                        serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                    )],
                )],
                ..Default::default()
            },
            Keymap::default(),
            fifo.dir.clone(),
        )
        .unwrap();
        let mut writer = fifo.writer();
        let mut recorder = Recorder::new();
        recorder.refuse = true;
        controller.tick(0, &mut recorder.dispatch());
        writer.write_all(&[0xB0, 7, 127]).unwrap();
        controller.tick(0, &mut recorder.dispatch());
        let line = &controller.snapshot().events[0];
        assert!(line.bound);
        assert!(!line.accepted);
        assert!(line.action.contains("refused"), "{}", line.action);
    }

    #[test]
    fn a_refused_hid_path_is_reported_and_never_opened() {
        let dir = std::env::temp_dir().join(format!("patch-ctl-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A path that is not an event device: nothing real is touched, and the
        // reason is what the Controller unit shows.
        let mut controller = Controller::new(
            Config {
                hid_devices: vec!["/dev/input/by-id/does-not-exist-event-joystick".into()],
                ..Default::default()
            },
            Keymap::default(),
            dir.clone(),
        )
        .unwrap()
        // Detection reads sysfs, so point it at this test's empty directory: a
        // Controller must not inherit whatever HID devices the machine running
        // the tests happens to have.
        .with_hidraw_dir(&dir);
        controller.tick(0, &mut |_| Ok(()));
        let devices = controller.snapshot().devices;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].identity_strength, "refused");
        assert!(
            devices[0].state.contains("not an event device"),
            "{:?}",
            devices[0]
        );
        assert!(!devices[0].matched, "a refused device has no profile");
        assert!(!controller.has_devices(), "nothing open: the service loop rests");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_named_controller_that_is_unplugged_waits_and_is_found_when_plugged_in() {
        let dir = std::env::temp_dir().join(format!("patch-ctl-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Saved by its identity when it was set up, then unplugged.
        let mut controller = Controller::new(
            Config { hid_devices: vec!["usb:17cc:1101".into()], ..Default::default() },
            Keymap::default(),
            dir.clone(),
        )
        .unwrap()
        .with_hidraw_dir(&dir);
        controller.tick(0, &mut |_| Ok(()));
        let devices = controller.snapshot().devices;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].state, "not connected", "{:?}", devices[0]);
        assert_ne!(devices[0].identity_strength, "refused", "unplugged is not refused");
        assert!(!controller.has_devices(), "nothing open: the service loop rests");
        // Plugged in: the next scan finds it. With no profile that reads its
        // reports it is not opened (so no real node is touched here), and says so.
        crate::identity::fixture_hidraw(&dir);
        controller.last_scan = None;
        controller.tick(0, &mut |_| Ok(()));
        let named: Vec<_> =
            controller.snapshot().devices.into_iter().filter(|d| d.identity == "usb:17cc:1101" && d.path == "usb:17cc:1101").collect();
        assert_eq!(named.len(), 1, "{named:?}");
        assert!(named[0].state.contains("connected · no profile"), "{:?}", named[0]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
