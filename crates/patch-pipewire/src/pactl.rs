//! Thin, typed wrapper over `pactl -f json` and friends.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default)]
pub struct SinkInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub state: String,
    pub monitor: String,
    pub channels: u16,
    pub channel_map: Vec<String>,
    pub rate: u32,
    pub format: String,
    pub volume_db: f32,
    pub mute: bool,
    pub bus: String,
    pub api: String,
    pub codec: String,
    pub form_factor: String,
    pub active_port: String,
    pub vendor_id: String,
    pub product_id: String,
    pub serial: String,
    pub node_name: String,
}

impl SinkInfo {
    /// What the output is, as a mixer's output picker labels it.
    pub fn kind(&self) -> &'static str {
        let port = format!("{} {}", self.active_port, self.name).to_lowercase();
        if self.bus == "bluetooth" || self.api == "bluez5" {
            "Bluetooth"
        } else if port.contains("hdmi") || port.contains("displayport") {
            "HDMI"
        } else if port.contains("iec958") {
            "S/PDIF"
        } else if self.bus == "usb" {
            "USB"
        } else if self.bus == "pci" {
            "Built-in"
        } else {
            ""
        }
    }

    /// The format it runs at: "48 kHz · 2 ch", plus the codec over Bluetooth.
    pub fn format_detail(&self) -> String {
        let mut parts = vec![];
        if self.rate > 0 {
            let khz = self.rate as f64 / 1000.0;
            parts.push(if self.rate.is_multiple_of(1000) { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") });
        }
        if self.channels > 0 {
            parts.push(format!("{} ch", self.channels));
        }
        if !self.codec.is_empty() {
            parts.push(self.codec.to_uppercase());
        }
        parts.join(" · ")
    }

    pub fn output_identity(&self) -> patch_tune::DeviceIdentity {
        let layout = self.channel_map.clone();
        let has_hardware_key = !self.serial.is_empty()
            || (!self.vendor_id.is_empty() && !self.product_id.is_empty())
            || !self.node_name.is_empty();
        let id = if has_hardware_key {
            patch_tune::stable_identity_id(&[
                &self.api,
                &self.bus,
                &self.vendor_id,
                &self.product_id,
                &self.serial,
                &self.node_name,
                &layout.join(","),
            ])
        } else {
            String::new()
        };
        patch_tune::DeviceIdentity {
            stable_id: id,
            backend: (!self.api.is_empty()).then(|| self.api.clone()),
            bus: (!self.bus.is_empty()).then(|| self.bus.clone()),
            vendor_id: (!self.vendor_id.is_empty()).then(|| self.vendor_id.clone()),
            product_id: (!self.product_id.is_empty()).then(|| self.product_id.clone()),
            serial: (!self.serial.is_empty()).then(|| self.serial.clone()),
            node_name: (!self.node_name.is_empty()).then(|| self.node_name.clone()),
            channel_layout: layout,
            components: Vec::new(),
        }
    }
    pub fn bus_label(&self) -> String {
        let mut parts = vec![match self.bus.as_str() {
            "bluetooth" => "Bluetooth".to_string(),
            "pci" => "PCI".to_string(),
            "usb" => "USB".to_string(),
            "" => self.api.clone(),
            b => b.to_string(),
        }];
        if !self.form_factor.is_empty() {
            parts.push(self.form_factor.clone());
        }
        if !self.codec.is_empty() {
            parts.push(self.codec.to_uppercase());
        }
        parts.push(self.format.clone());
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }

    pub fn port_label(&self) -> String {
        match self.active_port.as_str() {
            "" => "Output".into(),
            p => p.replace("-output", "").replace('-', " "),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub index: u32,
    /// The audio connection, shared by streams from the same client.
    pub client_id: Option<u32>,
    pub process_id: String,
    pub sink: u32,
    pub app_name: String,
    pub app_id: String,
    pub node_name: String,
    pub binary: String,
    pub channels: u16,
    pub volume_db: f32,
    pub mute: bool,
    pub system_sound: bool,

}

#[derive(Debug, Clone)]
pub struct InputInfo {
    pub name: String,
    pub description: String,
    pub channels: u16,
    pub channel_map: Vec<String>,
    pub rate: u32,
}

pub fn channel_short(c: &str) -> String {
    match c {
        "front-left" => "L".into(),
        "front-right" => "R".into(),
        "mono" => "M".into(),
        "front-center" => "C".into(),
        "lfe" => "LFE".into(),
        "rear-left" => "RL".into(),
        "rear-right" => "RR".into(),
        "side-left" => "SL".into(),
        "side-right" => "SR".into(),
        other if other.starts_with("aux") => other.to_ascii_uppercase(),
        other => other
            .split('-')
            .map(|w| w.chars().next().unwrap_or('?').to_ascii_uppercase())
            .collect(),
    }
}

/// dB → absolute Pulse volume integer (65536 = 0 dB). Pulse volumes are
/// cubic: raw = 65536 · (10^(dB/20))^(1/3) = 65536 · 10^(dB/60).
pub fn pa_volume(db: f32) -> u32 {
    if db <= -120.0 {
        0
    } else {
        (65536.0 * 10f64.powf(db as f64 / 60.0)).round() as u32
    }
}

/// Sink-volume controls span silence through 0 dB. Ordinary channel gain
/// controls use a narrower floor and must not share this clamp.
pub fn clamp_volume_db(db: f32) -> f32 {
    db.clamp(-120.0, 0.0)
}

/// `pactl` translates its text, and some JSON fields, through gettext.
/// Omarchy 4.5 runs the session in the user's language. `LANGUAGE` wins
/// over `LC_ALL` for gettext, so both are pinned to C: "Server Name:",
/// " dB" and port availability stay the strings this file matches.
fn command(args: &[&str]) -> Command {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]).env("LC_ALL", "C").env("LANGUAGE", "C");
    cmd
}

fn run(args: &[&str]) -> Option<String> {
    let out = command(args).stderr(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_ok(args: &[&str]) -> Result<String, String> {
    let out = command(args)
        .output()
        .map_err(|e| format!("{}: {e}", args[0]))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn json(args: &[&str]) -> Vec<Value> {
    let mut full = vec!["pactl", "-f", "json"];
    full.extend_from_slice(args);
    run(&full)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// The system-sounds loopback's capture side, passive (also its mark: an
/// older, active loopback is replaced once).
const PASSIVE_CAPTURE: &str = "source_output_properties=node.passive=true";

fn system_sounds_loopbacks_in(modules: &str) -> Vec<(u64, String, bool)> {
    modules
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let index = f.next()?.parse().ok()?;
            (f.next()? == "module-loopback").then_some(())?;
            let args = f.next()?;
            args.contains("source=patch.system-sounds.monitor").then_some(())?;
            let sink = args.split(' ').find_map(|a| a.strip_prefix("sink="))?;
            Some((index, sink.to_string(), args.contains(PASSIVE_CAPTURE)))
        })
        .collect()
}

/// `pactl list short sinks` lists `name` ("index  name  driver  …").
fn short_sinks_have(out: &str, name: &str) -> bool {
    out.lines().any(|l| l.split('\t').nth(1) == Some(name))
}

fn prop(v: &Value, key: &str) -> String {
    v["properties"][key]
        .as_str()
        .filter(|s| *s != "(null)")
        .unwrap_or("")
        .to_string()
}

fn playback_client(v: &Value) -> Option<u32> {
    v["client"]
        .as_u64()
        .or_else(|| v["client"].as_str()?.parse().ok())
        .and_then(|n| u32::try_from(n).ok()).filter(|n| *n != u32::MAX)
        .or_else(|| prop(v, "client.id").parse().ok())
}

/// "Server Name: PulseAudio (on PipeWire 1.6.9)". English only; [`command`]
/// forces that locale, because the label is translated.
fn pipewire_version(info: &str) -> Option<String> {
    let line = info.lines().find(|l| l.starts_with("Server Name:"))?;
    let v = line
        .rsplit("PipeWire ")
        .next()?
        .trim_end_matches(')')
        .trim()
        .to_string();
    line.contains("PipeWire").then_some(v)
}

fn source_output_name(v: &Value) -> String {
    let node = prop(v, "node.name");
    if prop(v, "application.name") == "Patch meter" {
        let media = prop(v, "media.name");
        if !media.is_empty() {
            return media;
        }
    }
    node
}

/// Pulse's JSON omits `monitor_of_sink` for some PipeWire monitors (notably
/// Bluetooth sink monitors). Use the other fields PipeWire exposes so an
/// output's loopback is never presented as a microphone input.
///
/// The English "Monitor of " prefix is only a last resort: PipeWire writes
/// that description in the server's language, which `LC_ALL` on `pactl`
/// cannot change. The `.monitor` name and `device.class` do not depend on it.
fn is_sink_monitor(v: &Value) -> bool {
    let monitor_of_sink = &v["monitor_of_sink"];
    let pulse_monitor = monitor_of_sink
        .as_u64()
        .is_some_and(|n| n != u32::MAX as u64)
        || monitor_of_sink
            .as_str()
            .is_some_and(|s| s != "4294967295" && !s.is_empty());
    let named_monitor = |s: &str| s.ends_with(".monitor");
    pulse_monitor
        || v["monitor_source"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        || v["properties"]["device.class"].as_str() == Some("monitor")
        || v["name"].as_str().is_some_and(named_monitor)
        || v["properties"]["node.name"].as_str().is_some_and(named_monitor)
        || v["description"]
            .as_str()
            .is_some_and(|s| s.starts_with("Monitor of "))
}

/// The playback half of a sink-side filter: a filter-chain or loopback whose
/// other half is a sink, such as Asahi's "MacBook Speakers" DSP, whose
/// `effect_output.j514-convolver` stream plays into the hidden raw speakers.
/// It is part of the output device, after Patch, not an application: as a
/// strip it showed up as "effect_out… 6ch" and its fader turned the
/// speakers down.
fn is_sink_filter_output(v: &Value, sink_groups: &HashSet<String>) -> bool {
    let group = prop(v, "node.link-group");
    !group.is_empty() && sink_groups.contains(&group)
}

/// Omarchy sends short event sounds with paplay; other desktops tag these
/// streams with media.role=event. Keep them out of the app list and collect
/// them in Patch's persistent System sounds sink.
///
/// The player has to be recognised by name, not by binary: `paplay` is a
/// symlink to `pacat` on Arch, so `application.process.binary` reads `pacat`
/// while `application.name` and `node.name` keep the name it was called as —
/// matched on the binary alone, every event sound played to the hardware and
/// stayed audible with the System sounds strip muted. The file it plays has
/// to look like a sound theme (or be unknown); a player reading from Music
/// stays an application stream. Omarchy's own hooks play their sounds with
/// `mpv` from the freedesktop theme, which the same rule covers.
fn is_event_sound(v: &Value) -> bool {
    // A role is the desktop saying what the sound is: a UI event, a
    // notification or an alert. Any of them belongs in the System sounds
    // strip whatever plays it (the mixer used to move only `event` ones, so
    // a browser's or a notification daemon's sound stayed an application
    // stream and played on with the strip muted).
    let role = prop(v, "media.role");
    if role.eq_ignore_ascii_case("event")
        || role.eq_ignore_ascii_case("notification")
        || role.eq_ignore_ascii_case("alert")
    {
        return true;
    }
    let hint = {
        let filename = prop(v, "media.filename");
        // paplay reports the path it was handed in media.name, not
        // media.filename.
        if filename.is_empty() {
            prop(v, "media.name")
        } else {
            filename
        }
    };
    let sounds_file = hint.is_empty()
        || hint.starts_with("/usr/share/sounds/")
        || hint.contains("/share/sounds/")
        || hint.contains("/share/omarchy/")
        // A UI sound rendered to a *transient* path: that is where tools put the
        // sounds they play with paplay as they go. Measured on this machine:
        // herdr's notification sound is `/tmp/herdr-sound-<pid>-<n>.mp3`, which
        // matched nothing here, stayed on the bus and played straight through the
        // System sounds strip's mute. A player reading a real track (from the
        // user's own files, or a URL) stays an application stream.
        || hint.starts_with("/tmp/")
        || hint.starts_with("/var/tmp/")
        || hint.starts_with("/run/");
    let player = ["application.name", "node.name", "application.process.binary"].iter().any(|key| {
        matches!(
            prop(v, key).rsplit('/').next().unwrap_or_default().to_ascii_lowercase().as_str(),
            "paplay" | "pacat" | "pw-play" | "pw-cat" | "canberra-gtk-play" | "mpv"
        )
    });
    player && sounds_file
}

/// Average volume in dB over channels; "-inf dB" → -120. `None`: the listing
/// carried no volume at all. A stream or sink caught mid-creation appears
/// without one, and reading that as 0 dB put full scale on its strip's fader
/// until the next read: the fader slammed to the top and back.
fn volume_db(v: &Value) -> Option<f32> {
    let map = v["volume"].as_object()?;
    let vals: Vec<f32> = map
        .values()
        .filter_map(|c| c["db"].as_str())
        .map(|d| {
            d.trim_end_matches(" dB")
                .trim()
                .parse::<f32>()
                .unwrap_or(-120.0)
        })
        .collect();
    (!vals.is_empty()).then(|| vals.iter().sum::<f32>() / vals.len() as f32)
}

fn spec(v: &Value) -> (String, u16, u32) {
    // "s16le 2ch 44100Hz"
    let s = v["sample_specification"].as_str().unwrap_or("");
    let mut it = s.split_whitespace();
    let fmt = it.next().unwrap_or("").to_string();
    let ch = it
        .next()
        .and_then(|c| c.trim_end_matches("ch").parse().ok())
        .unwrap_or(2);
    let rate = it
        .next()
        .and_then(|r| r.trim_end_matches("Hz").parse().ok())
        .unwrap_or(48_000);
    (fmt, ch, rate)
}

pub struct Pactl {
    /// node.name → UTF-8 description from pw-dump (pactl's JSON mangles
    /// non-ASCII names such as "User’s AirPods Max" into "(null)").
    names: RefCell<HashMap<String, String>>,

}

impl Pactl {
    pub fn new() -> Self {
        Pactl {
            names: RefCell::default(),
        }
    }

    pub fn server_version(&self) -> Option<String> {
        pipewire_version(&run(&["pactl", "info"])?)
    }

    pub fn default_sink(&self) -> Option<String> {
        run(&["pactl", "get-default-sink"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Whether the sink `name` exists; None when the server didn't answer.
    /// A failed listing reads as no sinks at all: creating one on that
    /// loaded a second `patch.system-sounds` when pactl timed out under
    /// memory pressure.
    pub fn sink_exists(&self, name: &str) -> Option<bool> {
        run(&["pactl", "list", "short", "sinks"]).map(|out| short_sinks_have(&out, name))
    }

    /// (clock.rate, effective quantum) from the settings metadata.
    pub fn clock(&self) -> Option<(u32, u32)> {
        let out = run(&["pw-metadata", "-n", "settings"])?;
        let get = |k: &str| {
            out.lines()
                .find(|l| l.contains(&format!("key:'{k}'")))
                .and_then(|l| l.split("value:'").nth(1))
                .and_then(|v| v.split('\'').next())
                .and_then(|v| v.parse::<u32>().ok())
        };
        let rate = get("clock.rate")?;
        let forced = get("clock.force-quantum").unwrap_or(0);
        let quantum = if forced > 0 {
            forced
        } else {
            get("clock.quantum")?
        };
        Some((rate, quantum))
    }

    pub fn sinks(&self) -> Vec<SinkInfo> {
        json(&["list", "sinks"])
            .iter()
            .map(|v| {
                let (format, channels, rate) = spec(v);
                SinkInfo {
                    index: v["index"].as_u64().unwrap_or(0) as u32,
                    name: v["name"].as_str().unwrap_or("").to_string(),
                    description: v["description"]
                        .as_str()
                        .filter(|s| *s != "(null)")
                        .unwrap_or("")
                        .to_string(),
                    state: v["state"].as_str().unwrap_or("").to_string(),
                    monitor: v["monitor_source"].as_str().unwrap_or("").to_string(),
                    channels,
                    channel_map: v["channel_map"]
                        .as_str()
                        .unwrap_or("")
                        .split(',')
                        .map(String::from)
                        .collect(),
                    rate,
                    format,
                    // NAN: the listing had no volume (see `volume_db`);
                    // `apply_graph` keeps the last one for the node.
                    volume_db: volume_db(v).unwrap_or(f32::NAN),
                    mute: v["mute"].as_bool().unwrap_or(false),
                    bus: prop(v, "device.bus"),
                    api: prop(v, "device.api"),
                    codec: prop(v, "api.bluez5.codec"),
                    form_factor: prop(v, "device.form_factor"),
                    active_port: v["active_port"].as_str().unwrap_or("").to_string(),
                    vendor_id: prop(v, "device.vendor.id"),
                    product_id: prop(v, "device.product.id"),
                    serial: prop(v, "device.serial"),
                    node_name: prop(v, "node.name"),
                }
            })
            .collect()
    }

    pub fn playback_ports(&self) -> Vec<String> {
        run(&["pw-link","-i"]).unwrap_or_default().lines().map(str::trim).map(String::from).collect()
    }

    pub fn inputs(&self) -> Vec<InputInfo> {
        json(&["list", "sources"]).iter()
            .filter(|v| !is_sink_monitor(v))
            .filter(|v| !v["name"].as_str().unwrap_or("").starts_with("patch"))
            .map(|v| {
                let (_, channels, rate) = spec(v);
                let name = v["name"].as_str().unwrap_or("").to_string();
                let description = v["description"]
                    .as_str()
                    .filter(|s| !s.is_empty() && *s != "(null)")
                    .unwrap_or(&name)
                    .to_string();
                InputInfo {
                    name,
                    description,
                    channels, rate,
                    channel_map: v["channel_map"].as_str().unwrap_or("").split(',').map(String::from).collect(),
                }
            }).collect()
    }

    pub fn streams(&self) -> Vec<StreamInfo> {
        let inputs = json(&["list", "sink-inputs"]);
        // Sinks list only when a stream is half of a link group (rare: the
        // poll stays one pactl call on most machines).
        let sink_groups: HashSet<String> = if inputs.iter().any(|v| !prop(v, "node.link-group").is_empty()) {
            json(&["list", "sinks"]).iter().map(|s| prop(s, "node.link-group")).filter(|g| !g.is_empty()).collect()
        } else {
            HashSet::new()
        };
        inputs
            .iter()
            // Ignore Patch's own loopbacks: they are internal buses.
            .filter(|v| {
                prop(v, "node.name") != "patch.loopback"
                    && prop(v, "node.name") != "patch.system-sounds.loopback"
            })
            .filter(|v| !is_sink_filter_output(v, &sink_groups))
            .map(|v| {
                let (_, channels, _) = spec(v);
                StreamInfo {
                    index: v["index"].as_u64().unwrap_or(0) as u32,
                    client_id: playback_client(v),
                    process_id: prop(v, "application.process.id"),
                    sink: v["sink"].as_u64().unwrap_or(0) as u32,
                    app_name: prop(v, "application.name"),
                    app_id: prop(v, "application.id"),
                    node_name: prop(v, "node.name"),
                    binary: prop(v, "application.process.binary"),
                    channels,
                    volume_db: volume_db(v).unwrap_or(f32::NAN),
                    mute: v["mute"].as_bool().unwrap_or(false),
                    system_sound: is_event_sound(v),
                    }
            })
            .collect()
    }

    /// Sink-input index of the Patch loopback, if the bus exists.
    pub fn bus_loopback_index(&self) -> Option<u32> {
        json(&["list", "sink-inputs"])
            .iter()
            .find(|v| prop(v, "node.name") == "patch.loopback")
            .and_then(|v| v["index"].as_u64())
            .map(|i| i as u32)
    }

    /// The sink the Patch loopback currently plays into (it can be moved).
    pub fn bus_loopback_target(&self) -> Option<String> {
        self.loopback_target("patch.loopback")
    }

    pub fn loopback_target(&self, node_name: &str) -> Option<String> {
        let inputs = json(&["list", "sink-inputs"]);
        let sink = inputs.iter().find(|v| prop(v, "node.name") == node_name)?["sink"].as_u64()?;
        json(&["list", "sinks"])
            .iter()
            .find(|s| s["index"].as_u64() == Some(sink))
            .and_then(|s| s["name"].as_str())
            .map(String::from)
    }

    /// Nothing to persist: the loopback stream's own sink is the source of truth.
    pub fn set_bus_target(&self, _sink: &str) -> Result<(), String> {
        Ok(())
    }

    /// UTF-8 name for a node, preferring pw-dump over pactl's mangled one.
    pub fn describe(&self, node_name: &str, fallback: &str) -> String {
        if let Some(n) = self.names.borrow().get(node_name) {
            return n.clone();
        }
        if let Some(dump) = run(&["pw-dump"]).and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            let mut names = self.names.borrow_mut();
            for o in dump.as_array().into_iter().flatten() {
                let p = &o["info"]["props"];
                if let (Some(n), Some(d)) =
                    (p["node.name"].as_str(), p["node.description"].as_str())
                {
                    names.insert(n.to_string(), d.to_string());
                }
            }
        }
        // Cache misses too: this runs per frame, pw-dump takes ~12 ms.
        let name = self
            .names
            .borrow()
            .get(node_name)
            .cloned()
            .unwrap_or_else(|| {
                if fallback.is_empty() {
                    node_name.to_string()
                } else {
                    fallback.to_string()
                }
            });
        self.names
            .borrow_mut()
            .insert(node_name.to_string(), name.clone());
        name
    }

    /// Absolute volume. Never pass "-3dB": pactl reads a leading sign as a
    /// relative step, which turned every fader drag into a fade to silence.
    pub fn set_stream_volume(&self, index: u32, db: f32) {
        let _ = run(&[
            "pactl",
            "set-sink-input-volume",
            &index.to_string(),
            &pa_volume(db).to_string(),
        ]);
    }

    pub fn set_stream_mute(&self, index: u32, mute: bool) {
        let _ = run(&[
            "pactl",
            "set-sink-input-mute",
            &index.to_string(),
            if mute { "1" } else { "0" },
        ]);
    }

    pub fn set_sink_volume(&self, sink: &str, db: f32) {
        let _ = run(&["pactl", "set-sink-volume", sink, &pa_volume(db).to_string()]);
    }

    pub fn set_sink_mute(&self, sink: &str, mute: bool) {
        let _ = run(&["pactl", "set-sink-mute", sink, if mute { "1" } else { "0" }]);
    }

    pub fn set_source_mute(&self, source: &str, mute: bool) {
        let _ = run(&[
            "pactl",
            "set-source-mute",
            source,
            if mute { "1" } else { "0" },
        ]);
    }

    pub fn set_default_sink(&self, sink: &str) -> Result<(), String> {
        run_ok(&["pactl", "set-default-sink", sink]).map(|_| ())
    }

    /// Make a source the default input.
    pub fn set_default_source(&self, source: &str) -> Result<(), String> {
        run_ok(&["pactl", "set-default-source", source]).map(|_| ())
    }

    /// Point a capture stream at another source, named (as `pactl` takes it).
    pub fn move_source_output_to(&self, index: u32, source: &str) -> Result<(), String> {
        run_ok(&[
            "pactl",
            "move-source-output",
            &index.to_string(),
            source,
        ])
        .map(|_| ())
    }

    pub fn move_stream(&self, index: u32, sink: &str) -> Result<(), String> {
        run_ok(&["pactl", "move-sink-input", &index.to_string(), sink]).map(|_| ())
    }

    /// Patch's own capture streams (source-outputs): index, node name, and the
    /// source index each is reading from.
    pub fn source_outputs(&self) -> Vec<(u32, String, u32)> {
        json(&["list", "source-outputs"])
            .iter()
            .map(|v| {
                (
                    v["index"].as_u64().unwrap_or(0) as u32,
                    source_output_name(v),
                    v["source"].as_u64().unwrap_or(0) as u32,
                )
            })
            .collect()
    }

    /// Source indices by PulseAudio name, including sink monitors.
    pub fn source_indexes(&self) -> HashMap<String, u32> {
        json(&["list", "sources"])
            .iter()
            .filter_map(|v| Some((v["name"].as_str()?.to_string(), v["index"].as_u64()? as u32)))
            .collect()
    }

    /// Point a capture stream at another source.
    pub fn move_source_output(&self, index: u32, source: u32) -> Result<(), String> {
        run_ok(&[
            "pactl",
            "move-source-output",
            &index.to_string(),
            &source.to_string(),
        ])
        .map(|_| ())
    }

    /// Which of Patch's own capture streams read from the wrong source, and
    /// where each of them belongs: `(stream, source)`. An input switch in the
    /// bar runs Omarchy's helper, which moves *every* source-output to the new
    /// microphone - Patch's included - after which the meters showed the
    /// microphone and the system-sounds loopback carried it into the mix.
    /// Split out so it can be tested without a sound server.
    pub fn capture_fixups(streams: &[(u32, String, u32)], want: &[(&str, u32)]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for (id, name, source) in streams {
            for (prefix, wanted) in want {
                if name.starts_with(prefix) && source != wanted {
                    out.push((*id, *wanted));
                }
            }
        }
        out
    }

    /// Null sink "Patch" plus a loopback from its monitor to `target`, both
    /// loaded into the audio server so they outlive this process.
    /// The bus runs in float32 at the target's own rate, so Patch adds no
    /// resampling and no truncation of its own.
    pub fn create_bus(&self, name: &str, target: &str, rate: u32) -> Result<(), String> {
        self.create_null_sink(name, rate)?;
        self.create_loopback(name, target)
    }

    /// The bus sink alone (the engine carries it to the output).
    pub fn create_null_sink(&self, name: &str, rate: u32) -> Result<(), String> {
        run_ok(&[
            "pactl",
            "load-module",
            "module-null-sink",
            &format!("sink_name={name}"),
            &patch_core::pulse::sink_properties(
                &patch_core::pulse::description(name).unwrap_or_else(|| name.into()),
                false,
                name == "patch",
            ),
            "format=float32le",
            &format!("rate={rate}"),
            "channels=2",
            "channel_map=front-left,front-right",
        ])
        .map(|_| ())
    }

    pub fn create_system_sounds_sink(&self, rate: u32) -> Result<(), String> {
        run_ok(&[
            "pactl",
            "load-module",
            "module-null-sink",
            "sink_name=patch.system-sounds",
            &patch_core::pulse::sink_properties("Patch System Sounds", false, false),
            "format=float32le",
            &format!("rate={rate}"),
            "channels=2",
            "channel_map=front-left,front-right",
        ])
        .map(|_| ())
    }

    /// Only the loopback half of the bus (the engine's fallback).
    pub fn create_loopback(&self, name: &str, target: &str) -> Result<(), String> {
        // The engine may have left the monitor free of the sink's volume
        // (it applies it after its recording tap): a loopback must carry it.
        let _ = run_ok(&[
            "pw-cli",
            "s",
            name,
            "Props",
            "{ params = [ \"monitor.channel-volumes\" true ] }",
        ]);
        run_ok(&[
            "pactl",
            "load-module",
            "module-loopback",
            &format!("source={name}.monitor"),
            &format!("sink={target}"),
            "latency_msec=20",
            "sink_input_properties=node.name=patch.loopback media.name=Patch",
        ])
        .map(|_| ())
    }

    pub fn create_system_sounds_loopback(&self, target: &str) -> Result<(), String> {
        let _ = run_ok(&[
            "pw-cli",
            "s",
            "patch.system-sounds",
            "Props",
            "{ params = [ \"monitor.channel-volumes\" true ] }",
        ]);
        run_ok(&[
            "pactl",
            "load-module",
            "module-loopback",
            "source=patch.system-sounds.monitor",
            &format!("sink={target}"),
            "latency_msec=20",
            // Quoted: unquoted, the module took only the first property.
            // Passive both ways: the loopback carries a system sound when
            // one plays, and never keeps the graph and the sound card
            // running in silence (it did, from the first window on).
            "sink_input_properties=\"node.name=patch.system-sounds.loopback node.passive=true\"",
            PASSIVE_CAPTURE,
        ])
        .map(|_| ())
    }

    /// Our system-sounds loopback modules: (index, target sink, passive),
    /// from the loaded modules themselves. (The sink-input can't be found
    /// by the node.name we ask for: PipeWire names it input.loopback-…, and
    /// looking for it that way reloaded a loopback on every graph event.)
    pub fn system_sounds_loopbacks(&self) -> Vec<(u64, String, bool)> {
        system_sounds_loopbacks_in(&run(&["pactl", "list", "modules", "short"]).unwrap_or_default())
    }

    pub fn unload_module(&self, index: u64) {
        let _ = run_ok(&["pactl", "unload-module", &index.to_string()]);
    }

    pub fn remove_bus(&self, name: &str) -> Result<(), String> {
        for m in json(&["list", "modules"]) {
            let arg = m["argument"].as_str().unwrap_or("");
            let is_ours = (m["name"] == "module-loopback"
                && arg.contains(&format!("source={name}.monitor")))
                || (m["name"] == "module-null-sink" && arg.contains(&format!("sink_name={name}")));
            if is_ours && let Some(i) = m["index"].as_u64() {
                run_ok(&["pactl", "unload-module", &i.to_string()])?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sink_is_found_by_its_exact_name() {
        let out = "198\talsa_output.pci-0000_00_05.0.analog-stereo\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
                   220\tpatch\tPipeWire\tfloat32le 2ch 48000Hz\tRUNNING\n\
                   363\tpatch.system-sounds\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n";
        assert!(short_sinks_have(out, "patch"));
        assert!(short_sinks_have(out, "patch.system-sounds"));
        assert!(!short_sinks_have(out, "patch.system"));
        assert!(!short_sinks_have("", "patch"));
    }

    #[test]
    fn an_active_system_sounds_loopback_is_told_apart() {
        // As `pactl list modules short` prints them, before and after.
        let out = "536870922\tmodule-loopback\tsource=patch.system-sounds.monitor sink=patch latency_msec=20 sink_input_properties=node.name=patch.system-sounds.loopback media.name=System sounds\t\n\
                   536870930\tmodule-loopback\tsource=patch.system-sounds.monitor sink=patch latency_msec=20 sink_input_properties=\"node.name=patch.system-sounds.loopback node.passive=true\" source_output_properties=node.passive=true\t\n\
                   536870931\tmodule-loopback\tsource=patch.monitor sink=alsa_output.x latency_msec=20\t\n";
        assert_eq!(
            system_sounds_loopbacks_in(out),
            vec![(536870922, "patch".to_string(), false), (536870930, "patch".to_string(), true)]
        );
    }

    #[test]
    fn playback_client_accepts_pulse_numeric_and_string_indexes() {
        assert_eq!(playback_client(&serde_json::json!({"client":685670,"properties":{"client.id":"83"}})), Some(685670));
        assert_eq!(playback_client(&serde_json::json!({"client":"685670"})), Some(685670));
        assert_eq!(playback_client(&serde_json::json!({"properties":{"client.id":"83"}})), Some(83));
        assert_eq!(playback_client(&serde_json::json!({})), None);
    }

    /// The Mixer's output picker: what each kind of sink is called, from
    /// real sinks' properties (Bluetooth headphones, HDMI, USB, onboard).
    #[test]
    fn output_kind_and_format() {
        let sink = |name: &str, bus: &str, api: &str, port: &str| SinkInfo {
            name: name.into(),
            bus: bus.into(),
            api: api.into(),
            active_port: port.into(),
            rate: 48000,
            channels: 2,
            ..Default::default()
        };
        let mut bt = sink("bluez_output.90_9C_4A_E1_A7_68.1", "bluetooth", "bluez5", "headphone-output");
        bt.rate = 44100;
        bt.codec = "aac".into();
        assert_eq!(bt.kind(), "Bluetooth");
        assert_eq!(bt.format_detail(), "44.1 kHz · 2 ch · AAC");
        assert_eq!(sink("alsa_output.pci-0000_02_00.1.hdmi-stereo", "pci", "alsa", "hdmi-output-0").kind(), "HDMI");
        let usb = sink("alsa_output.usb-Focusrite_Scarlett_2i2_USB-00.HiFi__Line__sink", "usb", "alsa", "[Out] Line");
        assert_eq!(usb.kind(), "USB");
        assert_eq!(usb.format_detail(), "48 kHz · 2 ch");
        assert_eq!(sink("alsa_output.pci-0000_80_1f.3.analog-stereo", "pci", "alsa", "analog-output-lineout").kind(), "Built-in");
        assert_eq!(sink("patch", "", "", "").kind(), "");
    }

    #[test]
    fn a_sink_filters_output_is_not_an_application() {
        use serde_json::json;
        // The live shape on an M3 Pro (Asahi): the speakers' DSP.
        let dsp = json!({"properties":{"node.name":"effect_output.j514-convolver",
            "node.link-group":"filter-chain-1036-19","node.virtual":"true"}});
        let groups: HashSet<String> = ["filter-chain-1036-19".to_string()].into();
        assert!(is_sink_filter_output(&dsp, &groups));
        // A loopback from a microphone has no sink half: it stays a stream.
        let mic = json!({"properties":{"node.name":"output.loopback-1-2","node.link-group":"loopback-1-2"}});
        assert!(!is_sink_filter_output(&mic, &groups));
        let app = json!({"properties":{"node.name":"alsa_playback.cliamp"}});
        assert!(!is_sink_filter_output(&app, &groups));
    }

    #[test]
    fn event_sounds_do_not_become_application_strips() {
        use serde_json::json;
        assert!(is_event_sound(
            &json!({"properties":{"media.role":"event","application.process.binary":"anything"}})
        ));
        // Notifications and alerts are system sounds too, whoever plays them.
        assert!(is_event_sound(
            &json!({"properties":{"media.role":"Notification","application.name":"Some App"}})
        ));
        assert!(is_event_sound(
            &json!({"properties":{"media.role":"alert","node.name":"alarm"}})
        ));
        assert!(is_event_sound(
            &json!({"properties":{"application.process.binary":"/usr/bin/paplay","media.filename":"/usr/share/sounds/freedesktop/stereo/message.oga"}})
        ));
        assert!(is_event_sound(
            &json!({"properties":{"application.process.binary":"paplay"}})
        ));
        // The live shape on Arch: paplay is a symlink to pacat, so the binary
        // reads pacat; the path it plays arrives in media.name.
        assert!(is_event_sound(&json!({"properties":{
            "application.name":"paplay","application.process.binary":"pacat","node.name":"paplay",
            "media.name":"/usr/share/sounds/alsa/Front_Center.wav"}})));
        // Omarchy's own hooks play their sounds with mpv from the theme.
        assert!(is_event_sound(&json!({"properties":{
            "node.name":"mpv","media.filename":"/usr/share/sounds/freedesktop/stereo/dialog-warning.oga"}})));
        assert!(is_event_sound(&json!({"properties":{
            "node.name":"pw-play","media.filename":"/usr/share/sounds/freedesktop/stereo/message.oga"}})));
        // A tool's UI sound rendered to a transient path (measured: herdr plays
        // its notification sound with paplay from /tmp).
        assert!(is_event_sound(&json!({"properties":{
            "application.name":"paplay","application.process.binary":"pacat",
            "media.name":"/tmp/herdr-sound-275024-11.mp3"}})));
        for props in [
            json!({"application.process.binary":"paplay","media.filename":"/home/user/Music/song.wav"}),
            json!({"application.process.binary":"player","media.filename":"/usr/share/sounds/test.wav"}),
            // A media player on a real track is an application stream.
            json!({"node.name":"mpv","media.filename":"/home/user/Music/song.flac"}),
            json!({"node.name":"pw-play","media.filename":"/home/user/Music/song.flac"}),
        ] {
            assert!(!is_event_sound(&json!({"properties":props})));
        }
    }

    #[test]
    fn a_monitor_is_not_an_input_when_its_description_is_translated() {
        use serde_json::json;
        assert!(is_sink_monitor(&json!({
            "name": "bluez_output.AA.monitor",
            "description": "Moniteur de Headphones",
            "properties": {}
        })));
        assert!(is_sink_monitor(&json!({
            "name": "alsa_output.pci.analog-stereo",
            "description": "Monitor de Speakers",
            "properties": { "node.name": "alsa_output.pci.analog-stereo.monitor", "device.class": "monitor" }
        })));
        assert!(is_sink_monitor(&json!({
            "name": "odd",
            "description": "Monitor of Speakers",
            "properties": {}
        })));
        assert!(!is_sink_monitor(&json!({
            "name": "alsa_input.pci.analog-stereo",
            "description": "Built-in Audio",
            "properties": { "device.class": "sound" }
        })));
    }

    #[test]
    fn the_server_line_is_the_english_one() {
        assert_eq!(
            pipewire_version("Server String: PulseAudio\nServer Name: PulseAudio (on PipeWire 1.6.9)\n"),
            Some("1.6.9".into())
        );
        // A translated label does not parse; `command` pins pactl to C so
        // this is not what a localized session returns.
        assert_eq!(pipewire_version("Nom du serveur : PulseAudio (on PipeWire 1.6.9)\n"), None);
    }

    #[test]
    fn volumes_are_absolute_and_cubic() {
        assert_eq!(pa_volume(0.0), 65536);
        // pactl reports 58982 as −2.75 dB
        assert!((pa_volume(-2.745) as i64 - 58982).abs() <= 2);
        assert_eq!(pa_volume(-60.0), 6554);
        assert_eq!(pa_volume(clamp_volume_db(-120.0)), 0);
        assert_eq!(pa_volume(clamp_volume_db(-130.0)), 0);
        assert_eq!(clamp_volume_db(6.0), 0.0);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::{Pactl, source_output_name};

    #[test]
    fn patch_meter_capture_name_uses_its_unique_media_label() {
        let v = serde_json::json!({
            "properties": {
                "application.name": "Patch meter",
                "node.name": "Patch meter",
                "media.name": "Patch meter stream-434421"
            }
        });
        assert_eq!(source_output_name(&v), "Patch meter stream-434421");
    }

    #[test]
    fn loopback_capture_keeps_its_node_name() {
        let v = serde_json::json!({
            "properties": {
                "application.name": "",
                "node.name": "input.loopback-1350-13",
                "media.name": "loopback-1350-13 input"
            }
        });
        assert_eq!(source_output_name(&v), "input.loopback-1350-13");
    }

    /// `omarchy-audio-input-set-default` (what the bar's audio menu runs when
    /// you pick an input) moves *every* capture stream to the new microphone
    /// so recording apps follow it. Patch's own captures are in that list, so
    /// they are moved back - but nothing else is touched, least of all a
    /// user's own recording app.
    #[test]
    fn only_patch_captures_are_put_back_and_only_when_moved() {
        let streams = vec![
            (106487u32, "input.loopback-1375-13".to_string(), 126u32), // stolen: a microphone
            (127332, "Patch meter bus".to_string(), 100154),            // still on the bus
            (127333, "Patch meter system-sounds".to_string(), 126),     // stolen with other captures
            (5, "Spotify".to_string(), 126),                           // an app, not ours
            (6, "input.loopback-999-1".to_string(), 9064),             // already right
        ];
        let want = [
            ("input.loopback-", 9064u32),
            ("Patch meter bus", 100154u32),
            ("Patch meter system-sounds", 555u32),
        ];
        assert_eq!(Pactl::capture_fixups(&streams, &want), vec![(106487, 9064), (127333, 555)]);
    }
}
