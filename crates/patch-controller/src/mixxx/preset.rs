//! The Mixxx preset model: the part of a mapping file this importer reads.
//!
//! Only the declarative structure is modelled — `<info>`, `<controller>`,
//! `<controls><control>` and `<outputs><output>` with their options. Script
//! files are recorded as names so the report can say why a mapping needs a
//! scripting layer Patch does not have, and are not read.

use super::xml::Node;

/// Which direction a control moves something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// A relative-encoder encoding, as Mixxx's option name implies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeHint {
    /// `diff`, `selectknob`, `hercjog`: two's complement.
    TwosComplement,
    /// `rot64`, `rot64fast`, `spread64`: offset binary.
    OffsetBinary,
    /// `hercjogfast`: two's complement at three times the rate.
    TwosComplementFast,
}

impl RelativeHint {
    /// The Patch `relative` spelling, and the multiplier it implies.
    pub fn patch(self) -> (&'static str, f64) {
        match self {
            RelativeHint::TwosComplement => ("twos_complement", 1.0),
            RelativeHint::OffsetBinary => ("offset_binary", 1.0),
            RelativeHint::TwosComplementFast => ("twos_complement", 3.0),
        }
    }
}

/// What a status byte addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Note,
    Cc,
    PitchBend,
    Program,
    /// Anything else (aftertouch, system messages): reported, not translated.
    Other(u8),
}

/// One `<control>` or `<output>` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct MixxxControl {
    /// The raw status byte, e.g. `0xb0`.
    pub status: u8,
    /// The data byte: a note number or a controller number.
    pub midino: u8,
    /// Mixxx's control group, e.g. `[Channel1]`.
    pub group: String,
    /// Mixxx's control-object name, e.g. `play`.
    pub key: String,
    pub description: String,
    /// `<options>` child names, lowercased.
    pub options: Vec<String>,
}

impl MixxxControl {
    pub fn channel(&self) -> u8 {
        self.status & 0x0F
    }

    pub fn kind(&self) -> Kind {
        match self.status & 0xF0 {
            0x90 => Kind::Note,
            0x80 => Kind::Note,
            0xB0 => Kind::Cc,
            0xE0 => Kind::PitchBend,
            0xC0 => Kind::Program,
            other => Kind::Other(other),
        }
    }

    pub fn has(&self, option: &str) -> bool {
        self.options.iter().any(|name| name == option)
    }

    /// A MIDI mapping entry that Mixxx hands to JavaScript.
    pub fn is_script(&self) -> bool {
        self.has("script-binding")
    }

    pub fn relative(&self) -> Option<RelativeHint> {
        for (option, hint) in [
            ("diff", RelativeHint::TwosComplement),
            ("selectknob", RelativeHint::TwosComplement),
            ("hercjog", RelativeHint::TwosComplement),
            ("hercjogfast", RelativeHint::TwosComplementFast),
            ("rot64", RelativeHint::OffsetBinary),
            ("rot64invert", RelativeHint::OffsetBinary),
            ("rot64fast", RelativeHint::OffsetBinary),
            ("spread64", RelativeHint::OffsetBinary),
        ] {
            if self.has(option) {
                return Some(hint);
            }
        }
        None
    }

    /// Which half of a 14-bit pair this entry is, when it declares one.
    pub fn fourteen_bit(&self) -> Option<Direction> {
        if self.has("fourteen-bit-msb") {
            Some(Direction::Up)
        } else if self.has("fourteen-bit-lsb") {
            Some(Direction::Down)
        } else {
            None
        }
    }

    pub fn soft_takeover(&self) -> bool {
        self.has("soft-takeover")
    }

    pub fn invert(&self) -> bool {
        self.has("invert")
    }

    /// The deck letter a group addresses: `[Channel1]` is deck `a`.
    /// Groups beyond four channels have no Patch deck.
    pub fn deck(&self) -> Option<&'static str> {
        let index = self.group.strip_prefix("[Channel")?.strip_suffix(']')?;
        match index {
            "1" => Some("a"),
            "2" => Some("b"),
            "3" => Some("c"),
            "4" => Some("d"),
            _ => None,
        }
    }

    /// Whether the group is the mixer's rather than a deck's.
    pub fn is_master(&self) -> bool {
        self.group == "[Master]"
    }

    /// The trigger JSON this entry becomes.
    pub fn trigger(&self) -> Option<String> {
        let channel = self.channel();
        match self.kind() {
            Kind::Note => Some(format!(
                "{{\"kind\":\"note\",\"channel\":{channel},\"note\":{}}}",
                self.midino
            )),
            Kind::Cc => match self.relative() {
                Some(hint) => {
                    let (mode, step) = hint.patch();
                    Some(format!(
                        "{{\"kind\":\"cc\",\"channel\":{channel},\"cc\":{},\"relative\":\"{mode}\",\"step\":{step}}}",
                        self.midino
                    ))
                }
                None => Some(format!(
                    "{{\"kind\":\"cc\",\"channel\":{channel},\"cc\":{}}}",
                    self.midino
                )),
            },
            Kind::PitchBend => Some(format!("{{\"kind\":\"pitch_bend\",\"channel\":{channel}}}")),
            Kind::Program => Some(format!(
                "{{\"kind\":\"program\",\"channel\":{channel},\"program\":{}}}",
                self.midino
            )),
            Kind::Other(_) => None,
        }
    }
}

/// A whole preset, reduced to what an importer can use.
#[derive(Clone, Debug, PartialEq)]
pub struct MixxxMapping {
    /// The root element's name, which is also the dialect: `MixxxMIDIPreset`,
    /// `MixxxControllerPreset`, `MixxxHIDPreset`.
    pub root: String,
    pub name: String,
    pub author: String,
    pub description: String,
    pub forum: Option<String>,
    pub wiki: Option<String>,
    /// The `<controller id="…">`, which is what Mixxx matches a device by.
    pub device_id: String,
    /// Script file names, in order. Recorded, never read.
    pub scripts: Vec<String>,
    pub controls: Vec<MixxxControl>,
    pub outputs: Vec<MixxxControl>,
    /// Entries that carried no status byte or no data byte, and so name no
    /// control at all. Counted rather than dropped: the report has to add up.
    pub skipped: usize,
    /// The same, for `<output>` entries.
    pub skipped_outputs: usize,
}

impl MixxxMapping {
    /// Whether the preset addresses USB HID rather than MIDI.
    pub fn is_hid(&self) -> bool {
        self.root.contains("HID")
    }

    /// A one-line attribution, preserving what the mapping file says about
    /// itself. Patch does not claim authorship of an imported mapping.
    pub fn attribution(&self) -> String {
        let mut text = String::new();
        if !self.name.is_empty() {
            text.push_str(&self.name);
        }
        if !self.author.is_empty() {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            text.push_str("author ");
            text.push_str(&self.author);
        }
        if let Some(forum) = &self.forum {
            text.push_str(" · ");
            text.push_str(forum);
        }
        text
    }
}

fn hex(text: &str) -> Option<u8> {
    let text = text.trim();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u8::from_str_radix(text, 16).ok()
}

fn read_control(node: &Node) -> Option<MixxxControl> {
    let status = hex(node.text_of("status")?)?;
    let midino = hex(node.text_of("midino").unwrap_or("0"))?;
    let options = node
        .child("options")
        .map(|options| {
            options
                .children
                .iter()
                .map(|child| child.name.to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    Some(MixxxControl {
        status,
        midino,
        group: node.text_of("group").unwrap_or("").trim().to_string(),
        key: node.text_of("key").unwrap_or("").trim().to_string(),
        description: node.text_of("description").unwrap_or("").trim().to_string(),
        options,
    })
}

/// The usable entries, and how many named no control at all.
/// Every element with this name, in document order.
fn collect<'a>(node: &'a Node, name: &str, out: &mut Vec<&'a Node>) {
    for child in &node.children {
        if child.name == name {
            out.push(child);
        }
        collect(child, name, out);
    }
}

/// The usable entries of one container, and how many named nothing at all.
fn read_entries(container: Option<&Node>, entry: &str) -> (Vec<MixxxControl>, usize) {
    let Some(container) = container else {
        return (Vec::new(), 0);
    };
    let mut kept = Vec::new();
    let mut skipped = 0;
    for node in container.children_named(entry) {
        match read_control(node) {
            Some(control) => kept.push(control),
            None => skipped += 1,
        }
    }
    (kept, skipped)
}

/// Reads a preset from a parsed document.
pub fn read(root: &Node) -> Result<MixxxMapping, String> {
    const ROOTS: [&str; 3] = ["MixxxMIDIPreset", "MixxxControllerPreset", "MixxxHIDPreset"];
    if !ROOTS.contains(&root.name.as_str()) {
        return Err(format!(
            "not a Mixxx controller preset: root element is '{}', expected one of {}",
            root.name,
            ROOTS.join(", ")
        ));
    }
    let info = root.child("info");
    let text = |name: &str| {
        info.and_then(|info| info.text_of(name))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let controller = root.child("controller");
    let scripts = controller
        .and_then(|c| c.child("scriptfiles"))
        .map(|files| {
            files
                .children_named("file")
                .filter_map(|file| file.attribute("filename"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // Every block, in document order: a mapping may carry more than one
    // <controls>/<outputs> pair (Behringer_BCD3000_Advanced.midi.xml does), and
    // reading only the first would lose entries without saying so.
    let mut blocks = Vec::new();
    collect(root, "controls", &mut blocks);
    let mut controls = Vec::new();
    let mut skipped = 0usize;
    for block in &blocks {
        let (found, more) = read_entries(Some(block), "control");
        controls.extend(found);
        skipped += more;
    }
    let mut blocks = Vec::new();
    collect(root, "outputs", &mut blocks);
    let mut outputs = Vec::new();
    let mut skipped_outputs = 0usize;
    for block in &blocks {
        let (found, more) = read_entries(Some(block), "output");
        outputs.extend(found);
        skipped_outputs += more;
    }
    Ok(MixxxMapping {
        root: root.name.clone(),
        name: text("name"),
        author: text("author"),
        description: text("description"),
        forum: info
            .and_then(|info| info.text_of("forums"))
            .map(str::to_string),
        wiki: info
            .and_then(|info| info.text_of("wiki"))
            .map(str::to_string),
        device_id: controller
            .and_then(|c| c.attribute("id"))
            .unwrap_or("")
            .to_string(),
        scripts,
        controls,
        outputs,
        skipped,
        skipped_outputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(text: &str) -> MixxxMapping {
        read(&super::super::xml::parse(text).unwrap()).unwrap()
    }

    #[test]
    fn a_preset_head_and_its_controls_are_read() {
        let mapping = preset(
            r#"<MixxxControllerPreset schemaVersion="1" mixxxVersion="2.6">
              <info>
                <name>Pioneer DDJ-FLX4</name>
                <author>Robert904</author>
                <description>Controller Midi Mapping for the Pioneer DDJ-FLX4</description>
                <forums>https://example.invalid/t</forums>
              </info>
              <controller id="DDJ-FLX4">
                <scriptfiles>
                  <file filename="Pioneer-DDJ-FLX4-script.js" functionprefix="DDJFLX4"/>
                </scriptfiles>
                <controls>
                  <control>
                    <status>0x90</status><midino>0x0B</midino>
                    <group>[Channel1]</group><key>play</key>
                    <options><normal/></options>
                  </control>
                  <control>
                    <status>0xB0</status><midino>0x13</midino>
                    <group>[Master]</group><key>crossfader</key>
                    <options><script-binding/><soft-takeover/></options>
                  </control>
                </controls>
                <outputs>
                  <output>
                    <status>0x90</status><midino>0x0B</midino>
                    <group>[Channel1]</group><key>play_indicator</key>
                  </output>
                </outputs>
              </controller>
            </MixxxControllerPreset>"#,
        );
        assert_eq!(mapping.name, "Pioneer DDJ-FLX4");
        assert_eq!(mapping.author, "Robert904");
        assert_eq!(mapping.device_id, "DDJ-FLX4");
        assert_eq!(mapping.scripts, ["Pioneer-DDJ-FLX4-script.js"]);
        assert_eq!(mapping.controls.len(), 2);
        assert_eq!(mapping.outputs.len(), 1);
        assert!(!mapping.is_hid());
        assert!(mapping.attribution().contains("Robert904"));

        let play = &mapping.controls[0];
        assert_eq!(play.kind(), Kind::Note);
        assert_eq!(play.channel(), 0);
        assert_eq!(play.deck(), Some("a"));
        assert!(!play.is_script());
        assert_eq!(
            play.trigger().unwrap(),
            r#"{"kind":"note","channel":0,"note":11}"#
        );

        let crossfader = &mapping.controls[1];
        assert!(crossfader.is_script());
        assert!(crossfader.soft_takeover());
        assert!(crossfader.is_master());
        assert_eq!(crossfader.deck(), None);
        assert_eq!(
            crossfader.trigger().unwrap(),
            r#"{"kind":"cc","channel":0,"cc":19}"#
        );
    }

    #[test]
    fn options_become_patch_options_with_their_implications() {
        let mapping = preset(
            r#"<MixxxMIDIPreset schemaVersion="1">
              <controller id="X"><controls>
                <control><status>0xB0</status><midino>0x20</midino>
                  <group>[Channel1]</group><key>rate</key>
                  <options><diff/></options></control>
                <control><status>0xB0</status><midino>0x21</midino>
                  <group>[Channel1]</group><key>volume</key>
                  <options><rot64/></options></control>
                <control><status>0xB0</status><midino>0x22</midino>
                  <group>[Channel2]</group><key>jog</key>
                  <options><hercjogfast/></options></control>
                <control><status>0xB0</status><midino>0x10</midino>
                  <group>[Channel1]</group><key>pregain</key>
                  <options><fourteen-bit-msb/><invert/></options></control>
                <control><status>0xB0</status><midino>0x30</midino>
                  <group>[Channel1]</group><key>pregain</key>
                  <options><fourteen-bit-lsb/></options></control>
                <control><status>0xA0</status><midino>0x10</midino>
                  <group>[Channel1]</group><key>volume</key></control>
              </controls></controller>
            </MixxxMIDIPreset>"#,
        );
        let by = |midino: u8| {
            mapping
                .controls
                .iter()
                .find(|c| c.midino == midino)
                .expect("control")
        };
        let (mode, step) = by(0x20).relative().unwrap().patch();
        assert_eq!((mode, step), ("twos_complement", 1.0));
        let (mode, step) = by(0x21).relative().unwrap().patch();
        assert_eq!((mode, step), ("offset_binary", 1.0));
        let (mode, step) = by(0x22).relative().unwrap().patch();
        assert_eq!((mode, step), ("twos_complement", 3.0), "hercjogfast is 3×");
        // The relative spelling reaches the trigger JSON.
        let trigger = by(0x22).trigger().unwrap();
        assert!(
            trigger.contains("\"relative\":\"twos_complement\""),
            "{trigger}"
        );
        assert!(trigger.contains("\"step\":3"), "{trigger}");

        assert_eq!(by(0x10).fourteen_bit(), Some(Direction::Up));
        assert!(by(0x10).invert());
        assert_eq!(by(0x30).fourteen_bit(), Some(Direction::Down));
        // Aftertouch is recognised as unsupported rather than mistranslated.
        assert_eq!(by(0x10).kind(), Kind::Cc);
        assert_eq!(mapping.controls[5].kind(), Kind::Other(0xA0));
        assert_eq!(mapping.controls[5].trigger(), None);
    }

    #[test]
    fn a_deck_group_beyond_four_has_no_patch_deck() {
        let mapping = preset(
            r#"<MixxxMIDIPreset schemaVersion="1"><controller id="X"><controls>
              <control><status>0x90</status><midino>0x01</midino>
                <group>[Channel5]</group><key>play</key></control>
              <control><status>0x90</status><midino>0x02</midino>
                <group>[Sampler1]</group><key>play</key></control>
            </controls></controller></MixxxMIDIPreset>"#,
        );
        assert_eq!(mapping.controls[0].deck(), None);
        assert_eq!(mapping.controls[1].deck(), None);
    }

    #[test]
    fn the_root_element_selects_the_dialect_and_a_foreign_file_is_refused() {
        let hid =
            preset(r#"<MixxxHIDPreset schemaVersion="1"><controller id="X"/></MixxxHIDPreset>"#);
        assert!(hid.is_hid());
        let error = read(&super::super::xml::parse("<html><body/></html>").unwrap()).unwrap_err();
        assert!(error.contains("not a Mixxx controller preset"), "{error}");
    }

    #[test]
    fn a_control_without_a_status_or_midino_is_skipped_rather_than_guessed() {
        let mapping = preset(
            r#"<MixxxMIDIPreset schemaVersion="1"><controller id="X"><controls>
              <control><group>[Channel1]</group><key>play</key></control>
              <control><status>0x90</status><group>[Channel1]</group><key>play</key></control>
              <control><status>0x90</status><midino>0x01</midino><group>[Channel1]</group></control>
            </controls></controller></MixxxMIDIPreset>"#,
        );
        // The first has no status; the second has no midino; only the third is
        // a usable entry. None is invented from a default.
        assert_eq!(mapping.controls.len(), 2);
        assert_eq!(mapping.controls[1].key, "");
    }
}
