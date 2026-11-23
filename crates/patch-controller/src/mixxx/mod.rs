//! Mixxx mapping import.
//!
//! Reads a Mixxx controller preset (`.midi.xml`) and translates the part of it
//! that is declarative into a native Patch profile. Nothing here is bundled
//! with Patch and nothing Mixxx wrote is copied: this reads a file the user
//! obtained themselves, at their request, on their machine, and reports
//! exactly what it could and could not translate. See
//! `docs/controllers-licensing.md`.
//!
//! The scope is the honest one measured from Mixxx's shipped corpus: of 17,295
//! control entries, **11,588 (67 %) carry a `script-binding` option** and are
//! therefore outside a declarative translation, and the remaining 5,707
//! entries reference 1,452 distinct control-object names, of which Patch has a
//! canonical command for a subset. (Read the option names case-insensitively:
//! Mixxx lowercases them before comparing, and mappings in the wild spell them
//! both ways — `Script-Binding` in the Xone K2 file.) Everything else is
//! *reported*, never silently dropped.

pub mod preset;
pub mod translate;
pub mod xml;

pub use preset::{Direction, MixxxControl, MixxxMapping, RelativeHint};
pub use translate::{Import, Report, Unsupported};

/// Imports a Mixxx mapping from its XML text.
pub fn import(text: &str) -> Result<Import, String> {
    let root = xml::parse(text)?;
    let mapping = preset::read(&root)?;
    Ok(translate::translate(&mapping))
}

/// Translates a Mixxx preset, taking the transport from the file's own name.
///
/// Mixxx names its presets `*.midi.xml`, `*.hid.xml` and `*.bulk.xml`, and its own
/// loader reads the transport from that name rather than from the document — which
/// is the only way to tell, because a modern preset's root is `MixxxControllerPreset`
/// whatever the transport is. Importing a controller's own HID mapping without this
/// produced a profile that claimed MIDI, and so could never match the device the
/// mapping was written for.
pub fn import_named(text: &str, name: &str) -> Result<Import, String> {
    let transport = transport_of(name);
    if transport == Some(Transport::Bulk) {
        return Err(format!(
            "`{name}` is a Mixxx USB-bulk preset, and Patch has no bulk transport, so \
             nothing here could drive it"
        ));
    }
    let root = xml::parse(text)?;
    let mapping = preset::read(&root)?;
    let mut imported = translate::translate(&mapping);
    // The name wins where it says something: a legacy `<MixxxHIDPreset>` root is
    // already read as HID by the translation, and a name that says MIDI is the only
    // signal a modern preset carries.
    match transport {
        Some(Transport::Hid) => imported.profile.protocol = crate::profile::Protocol::Hid,
        Some(Transport::Midi) => imported.profile.protocol = crate::profile::Protocol::Midi,
        Some(Transport::Bulk) | None => {}
    }
    // A HID preset names no `<controller>`, so its criterion fell back to the
    // preset's own name, which carries the vendor and therefore does not appear in
    // the device name. Mixxx names such a file after the device it drives, so the
    // file's own name is a second criterion that does match — trusted only when the
    // name follows Mixxx's own convention, which is also what decided the transport
    // above.
    if transport.is_some()
        && mapping.device_id.is_empty()
        && let Some(device) = device_name_from_file(name)
        && !imported.profile.r#match.names.contains(&device)
    {
        imported.profile.r#match.names.push(device.clone());
        imported.report.warn(&format!(
            "the preset names no device, so the file's own name ('{device}') was added as a \
             second match criterion; add a vendor/product id before relying on this"
        ));
    }
    // And without the generation token, which a device does not report.
    if mapping.device_id.is_empty()
        && let Some(short) = without_generation(&imported.profile.name)
        && !imported.profile.r#match.names.contains(&short)
    {
        imported.profile.r#match.names.push(short.clone());
        imported.report.warn(&format!(
            "the preset's own name ends in a generation token, which hardware leaves out, so \
             '{short}' was added as a further criterion"
        ));
    }
    Ok(imported)
}

/// The device name a Mixxx preset's own file name carries.
///
/// `Traktor_Kontrol_S2_Mk1.hid.xml` names the Traktor Kontrol S2 Mk1, which is what
/// a Linux device reports. `None` when there is nothing usable to take.
/// The name a preset uses with the model-generation token removed.
///
/// Mixxx names a preset by its generation — "Traktor Kontrol S2 Mk1" — while the
/// hardware reports itself without one: the USB product id carries the generation
/// instead, so the device calls itself "Traktor Kontrol S2". The name criterion is a
/// substring test against the device's own name, so the preset's name alone never
/// matches this device; the shortened form does, and it is added alongside rather
/// than instead.
fn without_generation(name: &str) -> Option<String> {
    let words: Vec<&str> = name.split_whitespace().collect();
    let last = words.last()?;
    let lower = last.to_ascii_lowercase();
    let generation = lower.is_ascii()
        && lower.starts_with("mk")
        && lower.len() > 2
        && lower[2..].chars().all(|c| c.is_ascii_alphanumeric());
    if !generation {
        return None;
    }
    let shortened = words[..words.len() - 1].join(" ");
    (!shortened.is_empty()).then_some(shortened)
}

fn device_name_from_file(name: &str) -> Option<String> {
    // The caller passes whatever it has: the CLI has a path, a test may have only a
    // file name. Start from the file's own name either way, or a criterion would be
    // built out of a directory.
    let name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name);
    let lower = name.to_ascii_lowercase();
    // ASCII lowercasing preserves byte lengths, so the stem can be sliced from the
    // original.
    let stem = [".hid.xml", ".midi.xml", ".bulk.xml", ".xml"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(|s| &name[..s.len()]))?;
    let words = stem.replace('_', " ");
    let words = words.trim();
    (!words.is_empty()).then(|| words.to_string())
}

/// The transport a preset's own name declares, as Mixxx's loader reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    Midi,
    Hid,
    Bulk,
}

fn transport_of(name: &str) -> Option<Transport> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".hid.xml") {
        Some(Transport::Hid)
    } else if lower.ends_with(".midi.xml") {
        Some(Transport::Midi)
    } else if lower.ends_with(".bulk.xml") {
        Some(Transport::Bulk)
    } else {
        None
    }
}

#[cfg(test)]
#[test]
fn an_imported_hid_preset_matches_the_device_it_was_named_after() {
    // The shape of the Traktor Kontrol S2 Mk1's own preset: modern root, no
    // `<controller>`, no controls, all JavaScript.
    let preset = r#"<MixxxControllerPreset schemaVersion="1">
        <info><name>Native Instruments Traktor Kontrol S2 Mk1</name>
          <author>leifhelm</author></info>
        <scriptfiles><file functionprefix="S2">s2.js</file></scriptfiles>
      </MixxxControllerPreset>"#;
    // A path, as the CLI passes it: a criterion built from the directory instead
    // of the device name is the bug this test exists to catch.
    let imported = import_named(
        preset,
        "/home/someone/mx_xml/Traktor_Kontrol_S2_Mk1.hid.xml",
    )
    .unwrap();
    assert_eq!(imported.profile.protocol, crate::profile::Protocol::Hid);
    assert!(
        imported
            .profile
            .r#match
            .names
            .contains(&"Traktor Kontrol S2 Mk1".to_string()),
        "the file's own name is a criterion: {:?}",
        imported.profile.r#match.names
    );
    // What a Linux device actually reports.
    let device = crate::identity::Identity {
        path: "/dev/hidraw3".into(),
        transport: "HID".into(),
        name: "Traktor Kontrol S2 Mk1".into(),
        ..Default::default()
    };
    let selection = crate::profile::select(std::slice::from_ref(&imported.profile), &device);
    assert!(
        matches!(selection, crate::profile::Selection::One { .. }),
        "the imported profile matches its own device: {selection:?}"
    );
    // A file name that follows no convention adds nothing — but the preset's own
    // name still ends in a generation token, so the shortened form is added. Two
    // criteria, and neither of them is the file's name.
    let plain = import_named(preset, "some-preset.xml").unwrap();
    assert_eq!(
        plain.profile.r#match.names.len(),
        2,
        "{:?}",
        plain.profile.r#match.names
    );
    assert!(
        plain
            .profile
            .r#match
            .names
            .contains(&"Native Instruments Traktor Kontrol S2".to_string())
    );
    assert!(
        !plain
            .profile
            .r#match
            .names
            .iter()
            .any(|n| n == "some-preset"),
        "an unremarkable file name is not a criterion: {:?}",
        plain.profile.r#match.names
    );
    // The stem is taken at the *last* extension, whatever the transport spelling.
    assert_eq!(
        device_name_from_file("Pioneer_CDJ-2000.midi.xml").unwrap(),
        "Pioneer CDJ-2000"
    );
    assert_eq!(device_name_from_file(".xml"), None);
    assert_eq!(device_name_from_file("no-extension"), None);
}

#[cfg(test)]
#[test]
fn the_s2_mk1_preset_matches_the_s2_mk1_as_the_hardware_reports_itself() {
    // Mixxx's preset for this controller, and the values the device actually reports
    // (read from /sys on the machine this was built on: HID_NAME without a generation
    // token, USB 17cc:1101, serial 522EB6F6).
    let preset = r#"<MixxxControllerPreset schemaVersion="1">
        <info><name>Native Instruments Traktor Kontrol S2 Mk1</name>
          <author>leifhelm</author></info>
        <scriptfiles><file functionprefix="S2">s2.js</file></scriptfiles>
      </MixxxControllerPreset>"#;
    let imported = import_named(preset, "Traktor_Kontrol_S2_Mk1.hid.xml").unwrap();
    assert_eq!(imported.profile.protocol, crate::profile::Protocol::Hid);
    assert!(
        imported
            .profile
            .r#match
            .names
            .contains(&"Native Instruments Traktor Kontrol S2".to_string()),
        "the generation token is dropped for matching: {:?}",
        imported.profile.r#match.names
    );
    let device = crate::identity::Identity {
        path: "/dev/hidraw6".into(),
        transport: "HID".into(),
        name: "Native Instruments Traktor Kontrol S2".into(),
        vendor_id: Some(0x17cc),
        product_id: Some(0x1101),
        serial: Some("522EB6F6".into()),
        ..Default::default()
    };
    let selection = crate::profile::select(std::slice::from_ref(&imported.profile), &device);
    assert!(
        matches!(selection, crate::profile::Selection::One { .. }),
        "an imported preset attaches to this controller: {selection:?}"
    );
    // The generation token alone is still not a criterion, and a name with no
    // generation token is left exactly as it is.
    assert_eq!(
        without_generation("Traktor Kontrol S2 Mk1").unwrap(),
        "Traktor Kontrol S2"
    );
    assert_eq!(
        without_generation("Traktor Kontrol S2 MK3").unwrap(),
        "Traktor Kontrol S2"
    );
    assert_eq!(without_generation("Traktor Kontrol S2"), None);
    assert_eq!(without_generation("Pioneer DJ DDJ-SX2"), None);
}

#[cfg(test)]
#[test]
fn a_preset_name_decides_the_transport_where_the_document_cannot() {
    // What the Traktor Kontrol S2 Mk1's own mapping looks like: the modern root,
    // scripts and no controls, so only the name can say it is HID.
    let modern = r#"<MixxxControllerPreset schemaVersion="1">
        <info><name>S2</name></info>
        <controller id="Traktor Kontrol S2 Mk1"/>
        <scriptfiles><file functionprefix="S2">s2.js</file></scriptfiles>
      </MixxxControllerPreset>"#;
    let hid = import_named(modern, "Traktor_Kontrol_S2_Mk1.hid.xml").unwrap();
    assert_eq!(hid.profile.protocol, crate::profile::Protocol::Hid);
    // The same document under a MIDI name is a MIDI profile.
    let midi = import_named(modern, "Whatever.midi.xml").unwrap();
    assert_eq!(midi.profile.protocol, crate::profile::Protocol::Midi);
    // A legacy HID root still reads as HID with no name to help.
    let legacy = import_named(
        r#"<MixxxHIDPreset><info><name>Pad</name></info></MixxxHIDPreset>"#,
        "pad.xml",
    )
    .unwrap();
    assert_eq!(legacy.profile.protocol, crate::profile::Protocol::Hid);
    // And a name that says nothing leaves the document's own answer alone.
    let plain = import_named(
        r#"<MixxxMIDIPreset><info><name>Deck</name></info></MixxxMIDIPreset>"#,
        "deck.xml",
    )
    .unwrap();
    assert_eq!(plain.profile.protocol, crate::profile::Protocol::Midi);
    // A bulk preset is refused rather than labelled as something it is not.
    let err = import_named(modern, "Something.bulk.xml").unwrap_err();
    assert!(err.contains("no bulk transport"), "{err}");
}
