//! Device identity: what a connected controller *is*, stable across
//! reconnects and independent of its display name.
//!
//! A device path is not an identity. ALSA renumbers cards across reboots
//! (`/dev/snd/midiC0D0` today, `midiC2D0` tomorrow) and a USB device can move
//! to another bus. What is stable is the USB vendor/product pair and the
//! device's serial, plus (for ALSA) the card's `id` string, which the kernel
//! derives from the hardware rather than the enumeration order.
//!
//! Everything here is read from `/sys`, so it works with no hardware present
//! and is fully testable: [`Identity::from_parts`] is the pure core, and the
//! sysfs readers are thin wrappers that parse files.
//!
//! # Safety: not opening unrelated input devices
//!
//! `/dev/input/event*` includes the keyboard the user is typing on and the
//! mouse they are pointing with. [`is_keyboard_like`] refuses a device whose
//! evdev capabilities include alphabetic keys, and [`is_pointer_like`]
//! refuses one that reports relative axes *and* pointer buttons. A refused
//! device is only opened when the user names it explicitly in the profile,
//! which is the one case where the intent is unambiguous.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Everything Patch knows about one physical controller.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    /// The path Patch opens today. Shown to the user; never the only key.
    pub path: String,
    /// "MIDI" or "HID".
    pub transport: String,
    /// The hardware's own name (`/sys/.../name`, or the ALSA card's longname).
    pub name: String,
    /// USB vendor id, when the device is on a USB bus.
    pub vendor_id: Option<u16>,
    /// USB product id, when the device is on a USB bus.
    pub product_id: Option<u16>,
    /// The device's serial, when it reports one.
    pub serial: Option<String>,
    /// The ALSA card's stable `id` (`/sys/class/sound/cardN/id`), e.g.
    /// `XONEK2`. Rarely present for class-compliant MIDI.
    pub card_id: Option<String>,
    /// The USB interface number, when the device is on a USB bus.
    pub interface: Option<u8>,
    /// The evdev node behind a HID path, e.g. `event7`. `/dev/input/by-id`
    /// paths move; this plus the name is what survives a reconnect.
    pub event: Option<String>,
}

impl Identity {
    /// The stable key for "the same device", used for restart-safe bookkeeping
    /// and to notice that two paths are one controller. Falls back through
    /// progressively weaker evidence and ends at the path, so it is always
    /// defined — but callers that need certainty should check which parts are
    /// present, not just that a key exists.
    pub fn key(&self) -> String {
        if let (Some(v), Some(p), Some(serial)) = (self.vendor_id, self.product_id, &self.serial) {
            return format!("usb:{v:04x}:{p:04x}:{serial}");
        }
        if let (Some(v), Some(p)) = (self.vendor_id, self.product_id) {
            // No serial: include the interface so a two-interface device does
            // not collapse into one.
            return match self.interface {
                Some(i) => format!("usb:{v:04x}:{p:04x}:{i}"),
                None => format!("usb:{v:04x}:{p:04x}"),
            };
        }
        if let Some(card) = &self.card_id {
            return format!("alsa:{card}");
        }
        format!("{}:{}", self.transport.to_lowercase(), self.path)
    }

    /// The key for "the same piece of hardware", ignoring which interface a
    /// path reaches it through. A controller exposes several HID interfaces and
    /// so does a keyboard (a vendor interface beside its own), so grouping by
    /// this is what keeps one keyboard from arriving as three devices — and
    /// what lets an interface with no keyboard personality be judged as part of
    /// the keyboard it belongs to.
    pub fn device_key(&self) -> String {
        match (self.vendor_id, self.product_id) {
            (Some(v), Some(p)) => match &self.serial {
                Some(serial) => format!("usb:{v:04x}:{p:04x}:{serial}"),
                None => format!("usb:{v:04x}:{p:04x}"),
            },
            _ if self.card_id.is_some() => {
                format!("alsa:{}", self.card_id.as_deref().unwrap_or_default())
            }
            _ => format!("{}:{}", self.transport.to_lowercase(), self.path),
        }
    }

    /// How strong the evidence behind [`Identity::key`] is. A profile match
    /// that rests on a path is not the same claim as one that rests on a
    /// vendor/product pair.
    pub fn strength(&self) -> Strength {
        match (self.vendor_id, self.product_id, self.serial.as_deref()) {
            (Some(_), Some(_), Some(_)) => Strength::Serial,
            (Some(_), Some(_), _) => Strength::VendorProduct,
            _ if self.card_id.is_some() => Strength::CardId,
            _ => Strength::PathOnly,
        }
    }

    /// A short description for the Controller surface.
    pub fn detail(&self) -> String {
        let mut parts = vec![self.key()];
        if let Some(i) = self.interface {
            parts.push(format!("interface {i}"));
        }
        if let Some(e) = &self.event {
            parts.push(e.clone());
        }
        parts.join(" · ")
    }

    /// Parses `1234` / `0x1234` into a USB id.
    pub fn parse_usb_id(text: &str) -> Option<u16> {
        let t = text.trim();
        let t = t
            .strip_prefix("0x")
            .or_else(|| t.strip_prefix("0X"))
            .unwrap_or(t);
        u16::from_str_radix(t, 16).ok()
    }
}

/// How the identity was established.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// Only the device path: the weakest link, and the reason a profile
    /// should also match on something else.
    PathOnly,
    /// The ALSA card id.
    CardId,
    /// USB vendor and product ids.
    VendorProduct,
    /// Vendor, product and serial.
    Serial,
}

impl Strength {
    pub fn label(self) -> &'static str {
        match self {
            Strength::PathOnly => "path only",
            Strength::CardId => "card id",
            Strength::VendorProduct => "vendor/product",
            Strength::Serial => "serial",
        }
    }
}

/// The interesting files in a sysfs directory, read once.
fn sysfs(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only the small attribute files; skip subdirectories and symlinks.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            out.insert(name, text.trim().to_string());
        }
    }
    out
}

/// Resolves a symlink to its target, as a path, without requiring it to exist
/// beyond one level (sysfs links point outside the directory's tree).
fn link_target(path: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(path).ok()?;
    Some(if target.is_absolute() {
        target
    } else {
        path.parent()?.join(target)
    })
}

/// Walks up from `start` looking for a USB device directory (one with
/// `idVendor`), because the sound card's node points at the *interface*, not
/// the device.
fn usb_device_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    let mut hops = 0;
    while let Some(current) = dir {
        if current.join("idVendor").is_file() {
            return Some(current);
        }
        if hops > 8 {
            return None;
        }
        hops += 1;
        dir = current.parent().map(Path::to_path_buf);
    }
    None
}

/// The USB interface number from a path like `…/1-2:1.0` or from
/// `bInterfaceNumber`.
fn interface_number(dir: &Path) -> Option<u8> {
    if let Ok(text) = std::fs::read_to_string(dir.join("bInterfaceNumber")) {
        return u8::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok();
    }
    // A sysfs USB node is named BUS-PORT:CONFIG.INTERFACE, e.g. 1-2:1.4 —
    // config 1, interface 4.
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let suffix = name.rsplit_once(':')?.1;
    let mut parts = suffix.split('.');
    let config = parts.next()?;
    parts.next().unwrap_or(config).parse().ok()
}

/// Reads `idVendor`/`idProduct`/`serial`/`manufacturer` from a USB device dir.
fn usb_fields(dir: &Path, identity: &mut Identity) {
    let attrs = sysfs(dir);
    identity.vendor_id = attrs
        .get("idVendor")
        .and_then(|v| Identity::parse_usb_id(v));
    identity.product_id = attrs
        .get("idProduct")
        .and_then(|v| Identity::parse_usb_id(v));
    identity.serial = attrs.get("serial").filter(|s| !s.is_empty()).cloned();
    if identity.name.is_empty()
        && let Some(product) = attrs.get("product")
    {
        identity.name = product.clone();
    }
}

/// Identity of an ALSA rawmidi node (`/dev/snd/midiC2D0`).
///
/// The card's `id` is the kernel's own stable name for the hardware, and the
/// USB attributes come from the `device` link, which for a class-compliant
/// MIDI interface points at the USB interface.
pub fn midi_identity(path: &str) -> Identity {
    let mut identity = Identity {
        path: path.to_string(),
        transport: "MIDI".into(),
        ..Default::default()
    };
    let node = Path::new(path);
    let Some(file) = node.file_name().and_then(|n| n.to_str()) else {
        return identity;
    };
    let Some(card) = file
        .strip_prefix("midiC")
        .and_then(|rest| rest.split('D').next())
    else {
        return identity;
    };
    let card_dir = PathBuf::from("/sys/class/sound").join(format!("card{card}"));
    if let Ok(id) = std::fs::read_to_string(card_dir.join("id")) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            identity.card_id = Some(id);
        }
    }
    if let Ok(longname) = std::fs::read_to_string(card_dir.join("longname")) {
        identity.name = longname.trim().to_string();
    }
    // The rawmidi node knows its own device; the card's does too. Prefer the
    // node's, then fall back to the card's.
    let device =
        link_target(&card_dir.join(file)).or_else(|| link_target(&card_dir.join("device")));
    if let Some(device) = device {
        if let Some(usb) = usb_device_dir(&device) {
            usb_fields(&usb, &mut identity);
        }
        identity.interface = interface_number(&device);
    }
    if identity.name.is_empty() {
        identity.name = identity.card_id.clone().unwrap_or_else(|| file.into());
    }
    identity
}

/// A HID device found through `/sys/class/hidraw`.
///
/// Read from sysfs only and never opened. A vendor-HID controller speaks a
/// protocol Patch has no transport for yet, and it must still show up as
/// *detected* rather than invisible — so this is an identity, and nothing more.
///
/// It deliberately does not try to say whether the device is a controller. Of
/// the HID devices on a test machine, the keyboard and the mouse both report
/// USB interface protocol 0 ("generic HID"), a keyboard's key bitmap is not
/// reachable from its own hidraw node, and the leading HID usage page is
/// vendor-defined `0xff00` for a keyboard, a USB receiver and a pair of
/// headphones alike. Linux's HID metadata cannot tell a controller from a
/// headset. The evdev path can, because evdev carries real capabilities, and
/// that is where refusing to open a keyboard or a mouse belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidrawDevice {
    pub identity: Identity,
}

/// Reads `HID_ID`'s zero-padded 16-bit fields: `0003:000017CC:00001101` is bus
/// 3, vendor `0x17cc`, product `0x1101`. Only the last four digits are the id,
/// so a bus prefix can never be read as part of it.
fn hid_id_field(text: &str) -> Option<u16> {
    let text = text.trim();
    let tail = &text[text.len().saturating_sub(4)..];
    u16::from_str_radix(tail, 16).ok()
}

/// Every HID device the kernel exposes, read from `root` (`/sys/class/hidraw`).
///
/// A device with no vendor/product pair (a Bluetooth or I2C HID device) is
/// still returned: its name and path are the only identity it has, and the
/// strength ladder says so.
pub fn hidraw_devices(root: &Path) -> Vec<HidrawDevice> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut nodes: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with("hidraw"))
        .collect();
    nodes.sort();
    for node in nodes {
        let device_dir = root.join(&node).join("device");
        let mut identity = Identity {
            path: format!("/dev/{node}"),
            transport: "HID".into(),
            ..Default::default()
        };
        let mut uevent_vendor = None;
        let mut uevent_product = None;
        if let Ok(uevent) = std::fs::read_to_string(device_dir.join("uevent")) {
            for line in uevent.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                match key.trim() {
                    "HID_NAME" => identity.name = value.trim().to_string(),
                    "HID_ID" => {
                        let parts: Vec<&str> = value.trim().split(':').collect();
                        if parts.len() == 3 {
                            uevent_vendor = hid_id_field(parts[1]);
                            uevent_product = hid_id_field(parts[2]);
                        }
                    }
                    _ => {}
                }
            }
        }
        // The USB device directory is the better source when it exists (it also
        // carries the serial and the manufacturer); the uevent is the fallback,
        // since sysfs does not always expose the parent for a HID node.
        if let Some(usb) = usb_device_dir(&device_dir) {
            usb_fields(&usb, &mut identity);
            identity.interface = interface_number(&device_dir);
        }
        if identity.vendor_id.is_none() {
            identity.vendor_id = uevent_vendor;
        }
        if identity.product_id.is_none() {
            identity.product_id = uevent_product;
        }
        found.push(HidrawDevice { identity });
    }
    found
}

/// Identity of a Linux evdev node or a `/dev/input/by-id` link.
/// Returns `None` for a path that is not an event device, so a mistyped
/// profile entry is reported rather than opened.
pub fn hid_identity(path: &str) -> Option<Identity> {
    let node = Path::new(path);
    // /dev/input/by-id/usb-…-event-joystick → /dev/input/event7
    let resolved = std::fs::canonicalize(node).ok()?;
    let event = resolved.file_name()?.to_str()?.to_string();
    if !event.starts_with("event") {
        return None;
    }
    let mut identity = Identity {
        path: path.to_string(),
        transport: "HID".into(),
        event: Some(event.clone()),
        ..Default::default()
    };
    let device_dir = PathBuf::from("/sys/class/input")
        .join(&event)
        .join("device");
    if let Ok(name) = std::fs::read_to_string(device_dir.join("name")) {
        identity.name = name.trim().to_string();
    }
    if let Some(usb) = usb_device_dir(&device_dir) {
        usb_fields(&usb, &mut identity);
        identity.interface = interface_number(&device_dir);
    }
    if identity.serial.is_none()
        && let Ok(uniq) = std::fs::read_to_string(device_dir.join("uniq"))
    {
        let uniq = uniq.trim();
        if !uniq.is_empty() {
            identity.serial = Some(uniq.to_string());
        }
    }
    Some(identity)
}

/// A sysfs tree shaped like the kernel's HID listing, built by tests: a
/// controller with two HID interfaces, and a keyboard whose second interface is
/// vendor-defined and carries no keyboard capability of its own.
#[cfg(test)]
pub(crate) fn fixture_hidraw(root: &Path) {
    fn write(path: std::path::PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let s2 = "DRIVER=hid-generic\nHID_ID=0003:000017CC:00001101\n";
    write(
        root.join("hidraw0/device/uevent"),
        &format!("{s2}HID_NAME=Traktor Kontrol S2 Mk1\n"),
    );
    write(
        root.join("hidraw1/device/uevent"),
        &format!("{s2}HID_NAME=Traktor Kontrol S2 Mk1\n"),
    );
    let keyboard = "DRIVER=hid-generic\nHID_ID=0003:0000046D:0000C31C\n";
    write(
        root.join("hidraw2/device/uevent"),
        &format!("{keyboard}HID_NAME=AT Translated Set 2 keyboard\n"),
    );
    write(
        root.join("hidraw3/device/uevent"),
        &format!("{keyboard}HID_NAME=USB Receiver\n"),
    );
    // The kernel writes a capability bitmap most-significant word first, so bit
    // 30 (KEY_A) lives in the last word.
    write(
        root.join("hidraw2/device/input/input9/capabilities/key"),
        "00000000 40000000\n",
    );
}

/// Capability bits an evdev device reports, as space-separated hex words, in
/// the kernel's sysfs order — **most significant word first**, so the words
/// are reversed here and bit `n` is then simply bit `n`.
fn capability_bits(device_dir: &Path, which: &str) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(device_dir.join("capabilities").join(which)) else {
        return vec![];
    };
    let mut words: Vec<u32> = text
        .split([' ', ',', '\n'])
        .filter_map(|token| {
            let token = token.trim();
            if token.is_empty() {
                return None;
            }
            let token = token
                .strip_prefix("0x")
                .or_else(|| token.strip_prefix("0X"))
                .unwrap_or(token);
            u32::from_str_radix(token, 16).ok()
        })
        .collect();
    words.reverse();
    words
}

fn bit(words: &[u32], index: u32) -> bool {
    let word = (index / 32) as usize;
    words
        .get(word)
        .is_some_and(|w| w & (1 << (index % 32)) != 0)
}

/// Linux input-event-codes: KEY_ESC = 1, KEY_A = 30 … KEY_Z = 44.
const KEY_ESC: u32 = 1;
const KEY_A: u32 = 30;
const KEY_Z: u32 = 44;

/// Whether an evdev device looks like a keyboard: it reports the letter keys.
///
/// A controller's transport buttons use `BTN_*`/`KEY_*` codes well below the
/// alphabet, so a device that reports the letters is a typing device and is
/// not opened unless the user named it.
pub fn is_keyboard_like(device_dir: &Path) -> bool {
    let key = capability_bits(device_dir, "key");
    bit(&key, KEY_ESC) || (KEY_A..=KEY_Z).any(|code| bit(&key, code))
}

/// Whether an evdev device looks like a mouse: relative axes *and* pointer
/// buttons (button 1 = left click).
pub fn is_pointer_like(device_dir: &Path) -> bool {
    let rel = capability_bits(device_dir, "rel");
    let key = capability_bits(device_dir, "key");
    !rel.is_empty() && bit(&key, 0x110)
}

/// Whether a HID path may be opened. A keyboard or mouse may not; it is
/// refused and reported, never silently ignored.
pub fn may_open_hid(path: &str) -> Result<Identity, String> {
    let identity = hid_identity(path).ok_or_else(|| format!("{path} is not an event device"))?;
    let event = identity
        .event
        .clone()
        .ok_or_else(|| format!("{path} has no event node"))?;
    let device_dir = PathBuf::from("/sys/class/input").join(event).join("device");
    if is_keyboard_like(&device_dir) {
        return Err(format!("{path} is a keyboard; controllers only"));
    }
    if is_pointer_like(&device_dir) {
        return Err(format!("{path} is a pointing device; controllers only"));
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidraw_nodes_are_identified_and_grouped_into_devices() {
        let root = std::env::temp_dir().join("patch-hidraw-fixture-unit");
        let _ = std::fs::remove_dir_all(&root);
        fixture_hidraw(&root);
        let found = hidraw_devices(&root);
        assert_eq!(found.len(), 4, "{found:?}");
        // The controller: vendor and product from the kernel's HID_ID, and a
        // name a person would recognise.
        let controller = &found[0];
        assert_eq!(controller.identity.path, "/dev/hidraw0");
        assert_eq!(controller.identity.name, "Traktor Kontrol S2 Mk1");
        assert_eq!(controller.identity.vendor_id, Some(0x17cc));
        assert_eq!(controller.identity.product_id, Some(0x1101));
        assert_eq!(controller.identity.strength().label(), "vendor/product");
        // Two interfaces, one piece of hardware.
        assert_eq!(
            found[0].identity.device_key(),
            found[1].identity.device_key()
        );
        // A different device is a different key, even with the same name.
        assert_ne!(
            found[1].identity.device_key(),
            found[2].identity.device_key()
        );
        assert_eq!(found[2].identity.name, "AT Translated Set 2 keyboard");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn keys_prefer_serial_then_vendor_product_then_card_then_path() {
        let with_serial = Identity {
            path: "/dev/snd/midiC9D0".into(),
            transport: "MIDI".into(),
            vendor_id: Some(0x17cc),
            product_id: Some(0x1720),
            serial: Some("ABC123".into()),
            ..Default::default()
        };
        assert_eq!(with_serial.key(), "usb:17cc:1720:ABC123");
        assert_eq!(with_serial.strength(), Strength::Serial);

        let no_serial = Identity {
            serial: None,
            interface: Some(4),
            ..with_serial.clone()
        };
        assert_eq!(no_serial.key(), "usb:17cc:1720:4");
        assert_eq!(no_serial.strength(), Strength::VendorProduct);

        let card_only = Identity {
            vendor_id: None,
            product_id: None,
            card_id: Some("XONEK2".into()),
            ..no_serial.clone()
        };
        assert_eq!(card_only.key(), "alsa:XONEK2");
        assert_eq!(card_only.strength(), Strength::CardId);

        let path_only = Identity {
            card_id: None,
            ..card_only
        };
        assert_eq!(path_only.key(), "midi:/dev/snd/midiC9D0");
        assert_eq!(path_only.strength(), Strength::PathOnly);
    }

    #[test]
    fn usb_ids_parse_in_decimal_hex_and_with_whitespace() {
        assert_eq!(Identity::parse_usb_id("1234\n"), Some(0x1234));
        assert_eq!(Identity::parse_usb_id("0x17cc"), Some(0x17cc));
        assert_eq!(Identity::parse_usb_id("17CC"), Some(0x17cc));
        assert_eq!(Identity::parse_usb_id("zz"), None);
    }

    #[test]
    fn interface_number_comes_from_attributes_or_the_node_name() {
        let dir = std::env::temp_dir().join(format!("patch-ctl-if-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bInterfaceNumber"), "03\n").unwrap();
        assert_eq!(interface_number(&dir), Some(3));
        std::fs::remove_file(dir.join("bInterfaceNumber")).unwrap();
        assert_eq!(interface_number(&dir), None);
        // …and from the sysfs link name when the attribute is absent.
        assert_eq!(
            interface_number(Path::new("/sys/whatever/1-2:1.4")),
            Some(4)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_keyboard_is_refused_and_a_pad_is_not() {
        let dir = std::env::temp_dir().join(format!("patch-ctl-hid-{}", std::process::id()));
        let caps = dir.join("capabilities");
        std::fs::create_dir_all(&caps).unwrap();
        // Kernel order: the last word holds bits 0-31. KEY_ESC (1) and
        // KEY_A (30) are both in it, so this is a keyboard.
        std::fs::write(caps.join("key"), "1 0 0 0 40000002\n").unwrap();
        assert!(is_keyboard_like(&dir));
        // A DJ pad: absolute axes, and only button codes above the alphabet.
        std::fs::write(caps.join("key"), "0 0 0 0 0\n").unwrap();
        std::fs::write(caps.join("abs"), "3\n").unwrap();
        assert!(!is_keyboard_like(&dir));
        assert!(!is_pointer_like(&dir));
        // A mouse: relative axes plus BTN_LEFT (272 = word 8, bit 16).
        std::fs::write(caps.join("rel"), "3\n").unwrap();
        std::fs::write(caps.join("key"), "0 10000 0 0 0 0 0 0 0 0\n").unwrap();
        assert!(is_pointer_like(&dir));
        // …but a wheel-only axis device is not a mouse.
        std::fs::write(caps.join("key"), "0 0 0 0 0\n").unwrap();
        assert!(!is_pointer_like(&dir));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_mistyped_hid_path_is_reported_not_opened() {
        let err = may_open_hid("/dev/input/does-not-exist").unwrap_err();
        assert!(err.contains("not an event device"), "{err}");
    }

    #[test]
    fn identity_round_trips_as_json_for_the_monitor() {
        let identity = Identity {
            path: "/dev/input/by-id/usb-XONE_K2-event-joystick".into(),
            transport: "HID".into(),
            name: "XONE:K2".into(),
            vendor_id: Some(0x1a70),
            product_id: Some(0x0001),
            card_id: None,
            serial: Some("K2-1".into()),
            interface: None,
            event: Some("event7".into()),
        };
        let text = serde_json::to_string(&identity).unwrap();
        assert_eq!(serde_json::from_str::<Identity>(&text).unwrap(), identity);
        assert!(identity.detail().contains("event7"));
    }
}
