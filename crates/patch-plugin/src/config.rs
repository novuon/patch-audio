//! The rack: which plugins Patch knows, in signal order, and how each one
//! is set. `plugins.json` is written by the windows and the CLI; the engine
//! reads it at start and again on `plugins sync`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The most plugins a rack holds (the engine's chain is preallocated).
pub const MAX_SLOTS: usize = 16;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RackConfig {
    pub slots: Vec<Slot>,
}

/// One plugin in the rack. `id` is stable for the slot's life: it names the
/// slot's window (`plugin` for 1, `plugin#2`, …) and its engine instance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Slot {
    pub id: u32,
    pub plugin: PathBuf,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    /// In the signal path (loaded in the plugin host).
    pub active: bool,
    /// Where in the mix it runs: the master, one channel, a bus.
    #[serde(default)]
    pub place: Place,
    /// What opening the plugin shows: Patch's face or the plugin's own.
    #[serde(default)]
    pub view: View,
    /// The plugin's own editor's size on top of the screen's scale.
    #[serde(default = "unzoomed")]
    pub zoom: f32,
    #[serde(default)]
    pub parameters: Vec<SavedParameter>,
}

fn unzoomed() -> f32 {
    1.0
}

/// The sizes the plugin's own editor offers.
pub const ZOOMS: [f32; 3] = [1.0, 1.5, 2.0];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Patch,
    Original,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedParameter {
    pub id: u32,
    pub value: f64,
}

/// Where a plugin runs: on the whole mix (after the master EQ), on one
/// engine channel (its input, after a mic's voice processing, before its
/// EQ, fader and sends), or on a bus (AUX: after its level, before it
/// leaves; FX: after its effect, before the return fader). Written as
/// `"master"`, `"aux-1"`, `"fx-2"`, or the channel's engine name
/// (`"mic-1"`, `"app-2"`, `"deck-a"`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Place {
    #[default]
    Master,
    /// AUX 1 or 2.
    Aux(u8),
    /// FX 1 or 2.
    Fx(u8),
    Channel(String),
}

impl From<String> for Place {
    fn from(text: String) -> Self {
        Place::parse(&text)
    }
}

impl From<Place> for String {
    fn from(place: Place) -> Self {
        place.key()
    }
}

/// Insert points: the master, the four buses, and this many channels at
/// once.
pub const CHANNEL_POINTS: usize = 6;
pub const POINTS: usize = 5 + CHANNEL_POINTS;

impl Place {
    pub fn parse(text: &str) -> Place {
        let text = text.trim().to_lowercase();
        match text.as_str() {
            "" | "master" | "main" => Place::Master,
            "aux-1" | "aux1" => Place::Aux(1),
            "aux-2" | "aux2" => Place::Aux(2),
            "fx-1" | "fx1" => Place::Fx(1),
            "fx-2" | "fx2" => Place::Fx(2),
            _ => Place::Channel(text),
        }
    }

    pub fn key(&self) -> String {
        match self {
            Place::Master => "master".into(),
            Place::Aux(n) => format!("aux-{n}"),
            Place::Fx(n) => format!("fx-{n}"),
            Place::Channel(name) => name.clone(),
        }
    }

    /// As the Mixer names it ("Mic 2", "AUX 1").
    pub fn label(&self) -> String {
        match self {
            Place::Master => "Master".into(),
            Place::Aux(n) => format!("AUX {n}"),
            Place::Fx(n) => format!("FX {n}"),
            Place::Channel(name) => channel_label(name),
        }
    }

    /// The engine's bus index (AUX 1, 2, FX 1, 2: 0–3).
    pub fn bus(&self) -> Option<usize> {
        match self {
            Place::Aux(n @ 1..=2) => Some(usize::from(*n) - 1),
            Place::Fx(n @ 1..=2) => Some(usize::from(*n) + 1),
            _ => None,
        }
    }
}

/// An engine channel's name as people read it.
pub fn channel_label(name: &str) -> String {
    let word = |w: &str| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    };
    match name {
        "bus" => "Apps".into(),
        _ => name.split('-').map(|w| if w.len() == 1 { w.to_uppercase() } else { word(w) }).collect::<Vec<String>>().join(" "),
    }
}

/// The single-insert file of 0.7.8 (Box Of Bass only).
#[derive(Deserialize)]
struct Legacy {
    enabled: bool,
    plugin: PathBuf,
    #[serde(default)]
    parameters: Vec<SavedParameter>,
}

impl RackConfig {
    pub fn slot(&self, id: u32) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    pub fn slot_mut(&mut self, id: u32) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    /// The lowest unused id: a removed slot's window number is reused, as
    /// the other multi-window Units do.
    pub fn next_id(&self) -> u32 {
        (1..).find(|n| self.slot(*n).is_none()).unwrap_or(1)
    }

    pub fn active(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter().filter(|s| s.active)
    }

    /// The insert point each active plugin runs at, in rack order: 0 the
    /// master, 1–4 AUX 1, AUX 2, FX 1, FX 2, then one per channel in the
    /// order the rack first names it. A channel past the last point is
    /// refused (its reason instead of a point).
    pub fn points(&self) -> Vec<(u32, Result<usize, String>)> {
        let mut channels: Vec<&str> = Vec::new();
        self.active()
            .map(|slot| {
                let point = match &slot.place {
                    Place::Master => Ok(0),
                    Place::Channel(name) => match channels.iter().position(|c| c == name) {
                        Some(at) => Ok(5 + at),
                        None if channels.len() < CHANNEL_POINTS => {
                            channels.push(name);
                            Ok(5 + channels.len() - 1)
                        }
                        None => Err(format!("{CHANNEL_POINTS} channels already have plugins")),
                    },
                    place => place.bus().map(|b| 1 + b).ok_or_else(|| format!("no bus {}", place.key())),
                };
                (slot.id, point)
            })
            .collect()
    }

    /// Add a plugin at the end of the chain, off until switched on.
    pub fn add(&mut self, plugin: &Path, info: &crate::BundleInfo) -> Result<u32> {
        anyhow::ensure!(self.slots.len() < MAX_SLOTS, "the rack holds {MAX_SLOTS} plugins");
        let id = self.next_id();
        self.slots.push(Slot {
            id,
            plugin: plugin.to_path_buf(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            active: false,
            view: View::Patch,
            zoom: 1.0,
            place: Place::Master,
            parameters: Vec::new(),
        });
        Ok(id)
    }

    /// Move a slot by `by` places in the chain (clamped).
    pub fn shift(&mut self, id: u32, by: i32) -> bool {
        let Some(at) = self.slots.iter().position(|s| s.id == id) else {
            return false;
        };
        let to = (at as i32 + by).clamp(0, self.slots.len() as i32 - 1) as usize;
        if to == at {
            return false;
        }
        let slot = self.slots.remove(at);
        self.slots.insert(to, slot);
        true
    }

    /// Record one parameter's value; false when it is unchanged.
    pub fn set_parameter(&mut self, id: u32, parameter: u32, value: f64) -> bool {
        let Some(slot) = self.slot_mut(id) else { return false };
        match slot.parameters.iter_mut().find(|p| p.id == parameter) {
            Some(saved) if (saved.value - value).abs() <= 1e-6 => false,
            Some(saved) => {
                saved.value = value;
                true
            }
            None => {
                slot.parameters.push(SavedParameter { id: parameter, value });
                true
            }
        }
    }
}

pub fn config_path() -> PathBuf {
    // Another file (tests: never the user's rack).
    if let Some(path) = std::env::var_os("PATCH_PLUGINS_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("omarchy-patch/plugins.json")
}

/// Parse a rack file, current or 0.7.8's single insert.
pub fn parse(bytes: &[u8]) -> Result<RackConfig> {
    if let Ok(rack) = serde_json::from_slice::<RackConfig>(bytes) {
        return Ok(rack);
    }
    let legacy: Legacy = serde_json::from_slice(bytes).context("not a plugin rack")?;
    let info = crate::scan::bundle_info(&legacy.plugin);
    Ok(RackConfig {
        slots: vec![Slot {
            id: 1,
            name: info.name,
            vendor: info.vendor,
            version: info.version,
            plugin: legacy.plugin,
            active: legacy.enabled,
            view: View::Patch,
            zoom: 1.0,
            place: Place::Master,
            parameters: legacy.parameters,
        }],
    })
}

pub fn load_config() -> Result<Option<RackConfig>> {
    let path = config_path();
    match std::fs::read(&path) {
        Ok(bytes) => parse(&bytes)
            .map(Some)
            .with_context(|| format!("parse plugin rack {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read plugin rack {}", path.display())),
    }
}

/// The rack, or an empty one when there is no file yet.
pub fn load_or_default() -> Result<RackConfig> {
    Ok(load_config()?.unwrap_or_default())
}

pub fn save_config(config: &RackConfig) -> Result<()> {
    let path = config_path();
    let parent = path.parent().context("plugin rack path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let staged = path.with_extension("json.new");
    std::fs::write(&staged, serde_json::to_vec_pretty(config)?)?;
    std::fs::rename(staged, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_single_insert_file_becomes_slot_one() {
        let rack = parse(
            br#"{"enabled":true,"plugin":"/nowhere/Box Of Bass.vst3","parameters":[{"id":7,"value":0.25}]}"#,
        )
        .unwrap();
        assert_eq!(rack.slots.len(), 1);
        let slot = &rack.slots[0];
        assert_eq!((slot.id, slot.active, slot.name.as_str()), (1, true, "Box Of Bass"));
        assert_eq!(slot.parameters, [SavedParameter { id: 7, value: 0.25 }]);
    }

    #[test]
    fn ids_reuse_the_lowest_free_number_and_moves_clamp() {
        let info = crate::BundleInfo::default();
        let mut rack = RackConfig::default();
        let a = rack.add(Path::new("/a.vst3"), &info).unwrap();
        let b = rack.add(Path::new("/b.vst3"), &info).unwrap();
        let c = rack.add(Path::new("/c.vst3"), &info).unwrap();
        assert_eq!((a, b, c), (1, 2, 3));
        rack.slots.retain(|s| s.id != 2);
        assert_eq!(rack.next_id(), 2);
        assert!(rack.shift(3, -5));
        assert_eq!(rack.slots.iter().map(|s| s.id).collect::<Vec<_>>(), [3, 1]);
        assert!(!rack.shift(3, -1));
        assert!(rack.set_parameter(1, 9, 0.5));
        assert!(!rack.set_parameter(1, 9, 0.5));
    }

    #[test]
    fn places_read_and_write_as_words_and_map_to_points() {
        let info = crate::BundleInfo::default();
        let mut rack = RackConfig::default();
        for _ in 0..10 {
            rack.add(Path::new("/p.vst3"), &info).unwrap();
        }
        let places = ["master", "mic-1", "fx-1", "mic-2", "mic-1", "aux-2", "app-1", "deck-a", "mic-9", "mic-10"];
        for (slot, place) in rack.slots.iter_mut().zip(places) {
            slot.active = true;
            slot.place = Place::parse(place);
        }
        rack.slots[9].active = true;
        let json = serde_json::to_string(&rack).unwrap();
        assert!(json.contains(r#""place":"mic-1""#) && json.contains(r#""place":"fx-1""#), "{json}");
        assert_eq!(serde_json::from_str::<RackConfig>(&json).unwrap(), rack);
        // A file from before places: the master.
        let old: Slot = serde_json::from_str(r#"{"id":1,"plugin":"/p.vst3","name":"P","active":true}"#).unwrap();
        assert_eq!(old.place, Place::Master);
        let points: Vec<Result<usize, String>> = rack.points().into_iter().map(|(_, p)| p).collect();
        assert_eq!(&points[..9], &[Ok(0), Ok(5), Ok(3), Ok(6), Ok(5), Ok(2), Ok(7), Ok(8), Ok(9)]);
        assert_eq!(points[9], Ok(10));
        rack.slots[0].place = Place::parse("mic-11");
        assert!(rack.points()[0].1.is_ok() && rack.points()[9].1.is_err(), "the seventh channel is refused");
        assert_eq!(Place::parse("mic-12").label(), "Mic 12");
        assert_eq!(Place::parse("deck-a").label(), "Deck A");
        assert_eq!(Place::Fx(2).bus(), Some(3));
    }
}
