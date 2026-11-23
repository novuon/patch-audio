//! Properties passed through pactl's module argument parser and then its
//! nested property-list parser. Command::args does not remove either layer.

/// Quote one value for PulseAudio/PipeWire's argument parser.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A complete module argument, with both parsing layers escaped.
pub fn sink_properties(description: &str, passive: bool, sound_device: bool) -> String {
    let mut properties = format!("device.description={}", quote(description));
    if passive {
        properties.push_str(" node.passive=true");
    }
    if sound_device {
        properties.push_str(" device.class=sound");
    }
    format!("sink_properties={}", quote(&properties))
}

/// Labels for Patch-owned endpoints; internal node names remain stable.
pub fn description(name: &str) -> Option<String> {
    Some(match name {
        "patch" => "Patch Output".into(),
        "patch.input" => "Patch Input".into(),
        "patch.program" => "Patch Program".into(),
        "patch.system-sounds" => "Patch System Sounds".into(),
        "patch.aux1" => "Patch AUX 1".into(),
        "patch.aux2" => "Patch AUX 2".into(),
        name => {
            if let Some(deck) = name.strip_prefix("patch.deck-") {
                if !matches!(deck, "a" | "b" | "c" | "d") {
                    return None;
                }
                format!("Patch Deck {}", deck.to_uppercase())
            } else if let Some(number) = name.strip_prefix("patch.app-") {
                number.parse::<u32>().ok()?;
                format!("Patch App {number}")
            } else {
                let number = name.strip_prefix("patch.stream-")?;
                number.parse::<u32>().ok()?;
                format!("Patch Stream {number}")
            }
        }
    })
}
