//! The native Patch controller profile: what a device is, how to recognise
//! it, what its controls do, and what it should light up.
//!
//! A profile is an external JSON file (`~/.config/omarchy-patch/controllers.json`)
//! — never a compiled-in controller implementation. JSON is Patch's existing
//! convention for every user-editable file it has (`dj-keys.json`,
//! `controllers.json`, `eq.json`) and `serde_json` is already a dependency of
//! this crate, so a mapping needs no new runtime, no parser dependency and no
//! schema translation layer. TOML and YAML were the alternatives; TOML is a
//! workspace dependency but would give two formats for controller data, and
//! YAML would add a dependency for nothing this format needs.
//!
//! # Shape
//!
//! ```json
//! {
//!   "hid_devices": ["/dev/input/by-id/usb-XONE_K2-event-joystick"],
//!   "profiles": [{
//!     "id": "allen-heath-xone-k2",
//!     "name": "Allen & Heath Xone K2",
//!     "manufacturer": "Allen & Heath",
//!     "protocol": "midi",
//!     "match": { "names": ["XONE:K2"], "vendor_id": 4668, "product_id": 1 },
//!     "keymap": { "bindings": [ … ] },
//!     "outputs": [ … ]
//!   }]
//! }
//! ```
//!
//! The older shape — a profile with only `"device"` and `"keymap"` — still
//! loads and still matches, by exact path. A profile must say how to
//! recognise its device; one with neither a path nor any match criterion is
//! rejected rather than left to match everything.

use crate::identity::Identity;
use patch_core::input::Keymap;
use serde::{Deserialize, Serialize};

/// Bounds. Declared, not discovered: a profile file is user input and these
/// keep a mistake from becoming an allocation or a poll storm.
pub const MAX_DEVICES: usize = 32;
pub const MAX_PROFILES: usize = 64;
pub const MAX_BINDINGS: usize = 1024;
pub const MAX_OUTPUTS: usize = 256;
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// HID is opt-in, and naming a device here is the user's explicit
    /// decision: an `/dev/input/by-id/*-event-*` path for an evdev device (it
    /// survives a reconnect), a `/dev/hidraw*` node, or a device identity such
    /// as `usb:17cc:1101` for a controller that speaks a raw HID protocol.
    /// A raw device is opened only when a profile for it declares report
    /// fields to read, so nothing is opened speculatively.
    #[serde(default)]
    pub hid_devices: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

/// The shape of the identity string the service publishes for a device
/// (`usb:17cc:1101`): a lowercase transport prefix, then a non-empty body, with
/// nothing that could make it a path.
pub fn is_device_identity(s: &str) -> bool {
    let Some((prefix, body)) = s.split_once(':') else {
        return false;
    };
    !prefix.is_empty()
        && !body.is_empty()
        && prefix.chars().all(|c| c.is_ascii_lowercase())
        && !s.contains(' ')
        && !s.contains('/')
}

impl Config {
    pub fn path() -> std::path::PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".config")
            });
        base.join("omarchy-patch/controllers.json")
    }

    pub fn load() -> Result<Self, String> {
        let path = Self::path();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_CONFIG_BYTES as u64) {
            return Err("controller config exceeds 1 MiB".into());
        }
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() <= MAX_CONFIG_BYTES => {
                let config: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                config.validate()?;
                Ok(config)
            }
            Ok(_) => Err("controller config exceeds 1 MiB".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.hid_devices.len() > MAX_DEVICES || self.profiles.len() > MAX_PROFILES {
            return Err(format!(
                "at most {MAX_DEVICES} HID devices and {MAX_PROFILES} profiles"
            ));
        }
        for path in &self.hid_devices {
            if path.contains("..") {
                return Err("a HID device entry may not contain ..".into());
            }
            let evdev = path.starts_with("/dev/input/");
            let hidraw = path.starts_with("/dev/hidraw");
            if !(evdev || hidraw || is_device_identity(path)) {
                return Err(format!(
                    "'{path}' is not a device Patch can open: name an /dev/input or \
                     /dev/hidraw path, or a device identity such as usb:17cc:1101"
                ));
            }
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            profile.validate()?;
            if let Some(device) = &profile.device
                && self.profiles[..i]
                    .iter()
                    .any(|p| p.device.as_deref() == Some(device.as_str()))
            {
                return Err(format!("duplicate profile for {device}"));
            }
        }
        Ok(())
    }

    /// Profiles that name a device path, in file order.
    pub fn profile_for_path(&self, path: &str) -> Option<usize> {
        self.profiles
            .iter()
            .position(|p| p.device.as_deref() == Some(path))
    }
}

/// Which transport a profile is for. `any` is the default so a profile that
/// only lists controls works on either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Any,
    Midi,
    Hid,
}

impl Protocol {
    pub fn accepts(self, transport: &str) -> bool {
        match self {
            Protocol::Any => true,
            Protocol::Midi => transport.eq_ignore_ascii_case("midi"),
            Protocol::Hid => transport.eq_ignore_ascii_case("hid"),
        }
    }
}

/// How to recognise the hardware a profile is for. Every criterion that is
/// present must hold; a profile with none is rejected.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criteria {
    /// Case-insensitive substrings of the device name. `"XONE:K2"` matches
    /// "Allen & Heath XONE:K2".
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub vendor_id: Option<u16>,
    #[serde(default)]
    pub product_id: Option<u16>,
    /// The device's serial, when it reports one: the strongest criterion for
    /// telling two identical models apart.
    #[serde(default)]
    pub serial: Option<String>,
    /// The ALSA card's stable `id`.
    #[serde(default)]
    pub card_id: Option<String>,
    /// The evdev node, e.g. `event7`.
    #[serde(default)]
    pub event: Option<String>,
    /// The USB interface number.
    #[serde(default)]
    pub interface: Option<u8>,
}

impl Criteria {
    fn is_empty(&self) -> bool {
        self.names.is_empty()
            && self.vendor_id.is_none()
            && self.product_id.is_none()
            && self.serial.is_none()
            && self.card_id.is_none()
            && self.event.is_none()
            && self.interface.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// A stable slug, for referring to the profile (diagnostics, feedback
    /// state, a future editor).
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub protocol: Protocol,
    /// Where an imported profile came from, in the mapping file's own words:
    /// its preset name, author and forum link. Patch preserves this and does
    /// not claim authorship of a mapping it translated. See
    /// `docs/controllers-licensing.md`.
    #[serde(default)]
    pub attribution: String,
    /// The profile format version, so a later change can migrate rather than
    /// guess.
    #[serde(default = "current_version")]
    pub version: u32,
    /// An exact device path. The oldest form, kept because it works and user
    /// files use it.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub r#match: Criteria,
    pub keymap: Keymap,
    /// What this controller's LEDs, rings and displays follow.
    #[serde(default)]
    pub outputs: Vec<crate::feedback::Output>,
}

pub fn current_version() -> u32 {
    1
}

impl Profile {
    /// What to call it in the surface and in diagnostics.
    pub fn label(&self) -> String {
        match (self.manufacturer.is_empty(), self.name.is_empty()) {
            (false, false) => format!("{} {}", self.manufacturer, self.name),
            (_, false) => self.name.clone(),
            (false, true) => self.manufacturer.clone(),
            (true, true) => match &self.device {
                Some(device) => device.clone(),
                None => self.id.clone(),
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let id = if self.id.is_empty() {
            self.label()
        } else {
            self.id.clone()
        };
        if self.version == 0 || self.version > current_version() {
            let current = current_version();
            return Err(format!(
                "{id}: profile version {} is newer than {current}",
                self.version
            ));
        }
        if self.device.is_none() && self.r#match.is_empty() {
            return Err(format!(
                "{id}: a profile must name a device path or match on the hardware"
            ));
        }
        if self.device.as_deref().is_some_and(|d| d.is_empty()) {
            return Err(format!("{id}: empty device path"));
        }
        if self.keymap.bindings.len() > MAX_BINDINGS {
            return Err(format!("{id}: at most {MAX_BINDINGS} bindings per device"));
        }
        for binding in &self.keymap.bindings {
            use patch_core::input::Trigger;
            if !matches!(binding.context.as_str(), "" | "midi" | "hid") {
                return Err(format!(
                    "{id}: device profile context must be empty, midi or hid"
                ));
            }
            if binding
                .range
                .is_some_and(|(lo, hi)| !lo.is_finite() || !hi.is_finite())
            {
                return Err(format!("{id}: controller ranges must be finite"));
            }
            if binding.soft_takeover {
                match binding.source.as_deref() {
                    None => {
                        return Err(format!(
                            "{id}: a soft-takeover control must name the state it drives"
                        ));
                    }
                    Some(source) => {
                        if source.parse::<crate::feedback::Source>().is_err() {
                            return Err(format!("{id}: unknown soft-takeover source '{source}'"));
                        }
                        if matches!(
                            binding.on,
                            Trigger::Cc {
                                relative: Some(_),
                                ..
                            }
                        ) {
                            return Err(format!("{id}: a relative encoder cannot soft-take-over"));
                        }
                    }
                }
            }
            match binding.on {
                Trigger::Note { channel, note, .. } if channel > 15 || note > 127 => {
                    return Err(format!("{id}: invalid MIDI note/channel"));
                }
                Trigger::Cc { channel, cc, .. } if channel > 15 || cc > 127 => {
                    return Err(format!("{id}: invalid MIDI CC/channel"));
                }
                Trigger::Cc { step, .. } if !step.is_finite() => {
                    return Err(format!("{id}: encoder step must be finite"));
                }
                Trigger::Cc14 { channel, cc } if channel > 15 || cc > 111 => {
                    // The LSB is cc + 32 and must stay inside the 7-bit field.
                    return Err(format!("{id}: 14-bit CC must be 0…111"));
                }
                Trigger::PitchBend { channel } if channel > 15 => {
                    return Err(format!("{id}: invalid MIDI channel"));
                }
                Trigger::Program { channel, program } if channel > 15 || program > 127 => {
                    return Err(format!("{id}: invalid MIDI program/channel"));
                }
                Trigger::HidAxis { min, max, .. } if max <= min => {
                    return Err(format!("{id}: HID axis maximum must exceed minimum"));
                }
                Trigger::HidRelative { step, .. } if !step.is_finite() || step == 0.0 => {
                    return Err(format!("{id}: HID encoder step must be finite and nonzero"));
                }
                Trigger::Key { .. } => {
                    return Err(format!("{id}: keyboard bindings belong in dj-keys.json"));
                }
                _ => {}
            }
        }
        if self.outputs.len() > MAX_OUTPUTS {
            return Err(format!("{id}: at most {MAX_OUTPUTS} outputs per device"));
        }
        for output in &self.outputs {
            output.validate().map_err(|e| format!("{id}: {e}"))?;
        }
        Ok(())
    }

    /// The 14-bit control pairs this profile's bindings declare, so the reader
    /// assembles exactly those and never invents a pair the hardware does not
    /// send.
    pub fn fourteen_bit(&self) -> Vec<(u8, u8)> {
        use patch_core::input::Trigger;
        self.keymap
            .bindings
            .iter()
            .filter_map(|b| match b.on {
                Trigger::Cc14 { channel, cc } => Some((channel, cc)),
                _ => None,
            })
            .collect()
    }
}

/// How well an identity matched a profile, strongest first.
/// Declared weakest first, so `Ord` *is* strength and a sort by it puts the
/// best evidence first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// A name substring only: enough to suggest, not enough to be sure.
    Named,
    /// A vendor/product or serial or card-id criterion.
    Identified,
    /// The device path, or the evdev node.
    Exact,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    /// Exactly one profile, with the evidence it rested on.
    One { index: usize, evidence: Evidence },
    /// Several profiles matched equally well: ask, do not guess.
    Ambiguous {
        indexes: Vec<usize>,
        evidence: Evidence,
    },
    /// Nothing matched. The device is still usable — unbound, or on the
    /// legacy keyboard map.
    None,
}

impl Selection {
    pub fn index(&self) -> Option<usize> {
        match self {
            Selection::One { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// A sentence for the Controller surface, so an ambiguous or missing
    /// profile is visible rather than silent.
    pub fn describe(&self, profiles: &[Profile]) -> String {
        match self {
            Selection::None => "no profile matches".into(),
            Selection::One { index, evidence } => format!(
                "{} · matched by {}",
                profiles[*index].label(),
                evidence_label(*evidence)
            ),
            Selection::Ambiguous { indexes, evidence } => format!(
                "{} profiles match by {} ({}) · choose one",
                indexes.len(),
                evidence_label(*evidence),
                indexes
                    .iter()
                    .map(|i| profiles[*i].label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn evidence_label(evidence: Evidence) -> &'static str {
    match evidence {
        Evidence::Exact => "path",
        Evidence::Identified => "hardware id",
        Evidence::Named => "name",
    }
}

/// How well one profile matches one device.
///
/// Every criterion present must hold; the verdict is the strongest criterion
/// that contributed. A profile for the wrong transport never matches.
pub fn evidence(profile: &Profile, device: &Identity) -> Option<Evidence> {
    if !profile.protocol.accepts(&device.transport) {
        return None;
    }
    let criteria = &profile.r#match;
    let mut best: Option<Evidence> = None;
    let mut note = |e: Evidence| {
        best = Some(match best {
            Some(previous) if previous > e => previous,
            _ => e,
        });
    };

    if profile.device.as_deref() == Some(device.path.as_str()) {
        note(Evidence::Exact);
    }
    if let (Some(want), Some(have)) = (&criteria.event, &device.event)
        && want == have
    {
        note(Evidence::Exact);
    }
    if let (Some(want), Some(have)) = (criteria.vendor_id, device.vendor_id)
        && want == have
    {
        note(Evidence::Identified);
    } else if criteria.vendor_id.is_some() {
        // A vendor criterion the device does not satisfy can never be the
        // right profile, even if the name looks similar.
        return None;
    }
    if let (Some(want), Some(have)) = (criteria.product_id, device.product_id)
        && want == have
    {
        note(Evidence::Identified);
    } else if criteria.product_id.is_some() {
        return None;
    }
    if let (Some(want), Some(have)) = (&criteria.serial, &device.serial)
        && want == have
    {
        note(Evidence::Identified);
    } else if criteria.serial.is_some() {
        return None;
    }
    if let (Some(want), Some(have)) = (&criteria.card_id, &device.card_id)
        && want.eq_ignore_ascii_case(have)
    {
        note(Evidence::Identified);
    } else if criteria.card_id.is_some() {
        return None;
    }
    if let Some(want) = criteria.interface {
        if device.interface != Some(want) {
            return None;
        }
        note(Evidence::Identified);
    }
    if !criteria.names.is_empty() {
        let name = device.name.to_lowercase();
        if criteria
            .names
            .iter()
            .any(|want| name.contains(&want.to_lowercase()))
        {
            note(Evidence::Named);
        } else {
            return None;
        }
    }
    best
}

/// Picks the profile for a device: the best evidence, and only when exactly
/// one profile has it.
///
/// This is where Patch refuses to do what Mixxx lets the user do by hand:
/// nothing is loaded on a weak or shared match, so a wrong preset can never
/// move a fader nobody touched.
pub fn select(profiles: &[Profile], device: &Identity) -> Selection {
    let mut scored: Vec<(usize, Evidence)> = profiles
        .iter()
        .enumerate()
        .filter_map(|(i, p)| evidence(p, device).map(|e| (i, e)))
        .collect();
    if scored.is_empty() {
        return Selection::None;
    }
    // Strongest evidence first, then file order: a later profile may override
    // an earlier one only by matching better, never by being listed later.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let best = scored[0].1;
    let indexes: Vec<usize> = scored
        .iter()
        .take_while(|(_, e)| *e == best)
        .map(|(i, _)| *i)
        .collect();
    match indexes.len() {
        0 => Selection::None,
        1 => Selection::One {
            index: indexes[0],
            evidence: best,
        },
        _ => Selection::Ambiguous {
            indexes,
            evidence: best,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xone() -> Identity {
        Identity {
            path: "/dev/snd/midiC1D0".into(),
            transport: "MIDI".into(),
            name: "Allen & Heath XONE:K2".into(),
            vendor_id: Some(0x1234),
            product_id: Some(0x0001),
            card_id: Some("XONEK2".into()),
            ..Default::default()
        }
    }

    fn profile(id: &str, criteria: Criteria) -> Profile {
        Profile {
            id: id.into(),
            name: id.into(),
            manufacturer: String::new(),
            protocol: Protocol::Any,
            attribution: String::new(),
            version: 1,
            device: None,
            r#match: criteria,
            keymap: Keymap::default(),
            outputs: vec![],
        }
    }

    fn names(list: &[&str]) -> Criteria {
        Criteria {
            names: list.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_profile_must_say_how_to_recognise_its_device() {
        let err = profile("x", Criteria::default()).validate().unwrap_err();
        assert!(err.contains("must name a device path"), "{err}");
        let mut with_path = profile("x", Criteria::default());
        with_path.device = Some("/dev/snd/midiC1D0".into());
        assert!(with_path.validate().is_ok());
        assert!(profile("x", names(&["XONE"])).validate().is_ok());
    }

    #[test]
    fn a_newer_profile_version_is_refused_rather_than_guessed() {
        let mut p = profile("x", names(&["XONE"]));
        p.version = current_version() + 1;
        assert!(p.validate().unwrap_err().contains("newer than"));
        p.version = 0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn fourteen_bit_pairs_are_taken_from_the_bindings() {
        use patch_core::input::{Binding, Trigger};
        let mut p = profile("x", names(&["XONE"]));
        p.keymap.bindings = vec![
            Binding {
                context: "midi".into(),
                on: Trigger::Cc14 {
                    channel: 2,
                    cc: 0x10,
                },
                action: serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                range: None,
                soft_takeover: false,
                source: None,
            },
            Binding {
                context: "midi".into(),
                on: Trigger::cc(2, 0x30),
                action: serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                range: None,
                soft_takeover: false,
                source: None,
            },
        ];
        assert_eq!(p.fourteen_bit(), vec![(2, 0x10)]);
        assert!(p.validate().is_ok());
        // The LSB must stay inside the 7-bit field, or the pair is unusable.
        p.keymap.bindings[0].on = Trigger::Cc14 {
            channel: 2,
            cc: 0x70,
        };
        assert!(p.validate().unwrap_err().contains("0…111"));
    }

    #[test]
    fn identity_criteria_pick_the_exact_profile() {
        let profiles = vec![
            profile("generic-xone", names(&["xone"])),
            profile(
                "xone-k2",
                Criteria {
                    vendor_id: Some(0x1234),
                    product_id: Some(0x0001),
                    ..Default::default()
                },
            ),
        ];
        // The vendor/product profile is stronger than the name-only one.
        assert_eq!(
            select(&profiles, &xone()),
            Selection::One {
                index: 1,
                evidence: Evidence::Identified
            }
        );
        // A name-only device still gets the name-only profile.
        let guess = Identity {
            vendor_id: None,
            product_id: None,
            ..xone()
        };
        assert_eq!(
            select(&profiles, &guess),
            Selection::One {
                index: 0,
                evidence: Evidence::Named
            }
        );
    }

    #[test]
    fn a_vendor_criterion_that_does_not_hold_never_matches() {
        let wrong = Identity {
            vendor_id: Some(0x9999),
            product_id: Some(0x9999),
            ..xone()
        };
        let profiles = vec![profile(
            "others",
            Criteria {
                vendor_id: Some(0x1234),
                product_id: Some(0x0001),
                names: vec!["xone".into()],
                ..Default::default()
            },
        )];
        assert_eq!(select(&profiles, &wrong), Selection::None);
    }

    #[test]
    fn two_equally_good_profiles_are_reported_not_guessed() {
        let profiles = vec![
            profile("one", names(&["xone"])),
            profile("two", names(&["k2"])),
        ];
        assert_eq!(
            select(&profiles, &xone()),
            Selection::Ambiguous {
                indexes: vec![0, 1],
                evidence: Evidence::Named
            }
        );
        assert!(select(&profiles, &xone()).index().is_none());
        assert!(
            select(&profiles, &xone())
                .describe(&profiles)
                .contains("choose one")
        );
    }

    #[test]
    fn an_exact_path_beats_a_name_and_an_empty_list_says_so() {
        let mut exact = profile("exact", Criteria::default());
        exact.device = Some("/dev/snd/midiC1D0".into());
        let profiles = vec![profile("by-name", names(&["xone"])), exact];
        assert_eq!(
            select(&profiles, &xone()),
            Selection::One {
                index: 1,
                evidence: Evidence::Exact
            }
        );
        assert_eq!(select(&[], &xone()), Selection::None);
        assert_eq!(
            select(&profiles, &xone()).describe(&profiles),
            "exact · matched by path"
        );
    }

    #[test]
    fn a_midi_profile_does_not_match_a_hid_device() {
        let mut midi_only = profile("midi", names(&["xone"]));
        midi_only.protocol = Protocol::Midi;
        let hid = Identity {
            transport: "HID".into(),
            name: "XONE:K2".into(),
            vendor_id: None,
            product_id: None,
            card_id: None,
            path: "/dev/input/by-id/usb-XONE_K2-event-joystick".into(),
            event: Some("event7".into()),
            ..Default::default()
        };
        assert_eq!(select(&[midi_only], &hid), Selection::None);
        assert_eq!(
            select(&[profile("any", names(&["xone"]))], &hid).index(),
            Some(0)
        );
    }

    #[test]
    fn a_two_interface_device_is_told_apart_by_interface_or_serial() {
        let dev = |interface: u8, serial: Option<&str>| Identity {
            path: format!("/dev/snd/midiC{interface}D0"),
            transport: "MIDI".into(),
            name: "XONE:K2".into(),
            vendor_id: Some(0x1234),
            product_id: Some(0x0001),
            serial: serial.map(String::from),
            interface: Some(interface),
            ..Default::default()
        };
        let left = profile(
            "left",
            Criteria {
                interface: Some(0),
                ..Default::default()
            },
        );
        let right = profile(
            "right",
            Criteria {
                interface: Some(1),
                ..Default::default()
            },
        );
        let profiles = vec![left, right];
        assert_eq!(select(&profiles, &dev(0, None)).index(), Some(0));
        assert_eq!(select(&profiles, &dev(1, None)).index(), Some(1));
        // A serial criterion pins one unit even when the interface differs.
        let serialised = vec![profile(
            "studio",
            Criteria {
                serial: Some("K2-A".into()),
                ..Default::default()
            },
        )];
        assert_eq!(select(&serialised, &dev(3, Some("K2-A"))).index(), Some(0));
        assert_eq!(select(&serialised, &dev(3, Some("K2-B"))), Selection::None);
    }

    #[test]
    fn the_old_two_field_profile_still_loads_and_matches() {
        let config: Config = serde_json::from_str(
            r#"{"profiles":[{"device":"/dev/snd/midiC1D0",
                "keymap":{"bindings":[{"context":"midi",
                  "on":{"kind":"cc","channel":0,"cc":7},
                  "action":{"op":"dj.crossfader","position":"{value}"},
                  "range":[0,1]}]}}]}"#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.profile_for_path("/dev/snd/midiC1D0"), Some(0));
        assert_eq!(
            select(&config.profiles, &xone()).index(),
            Some(0),
            "an exact path is the strongest evidence"
        );
    }

    #[test]
    fn unknown_fields_are_refused_so_a_typo_is_not_silently_ignored() {
        assert!(
            serde_json::from_str::<Config>(r#"{"hid_device":[]}"#).is_err(),
            "the old typo is still reported"
        );
        assert!(
            serde_json::from_str::<Profile>(
                r#"{"device":"/dev/snd/midiC0D0","keymap":{"bindings":[]},"output":[]}"#
            )
            .is_err()
        );
    }
}
