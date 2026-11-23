//! Keeps the engine wired into the graph, from its own process.
//!
//! * Channel sources: `bus` is the Patch bus (null sink `patch`, where
//!   applications routed through Patch play); `deck-a` through `deck-d` are the
//!   decks' own virtual soundcards (null sinks `patch.deck-a` / `-b`). Each
//!   sink's monitor feeds the engine's input pair in the same graph cycle.
//! * Program output goes to the output device: the system default (following
//!   the system bar), or one chosen in Patch until the default changes.
//! * The old bus loopback (`patch.loopback`) is removed only once the engine
//!   is linked, and put back when the engine stops, so audio never stops.

use std::collections::HashSet;
use std::process::Command;

pub const ENGINE: &str = "patch.engine";
pub const BUS: &str = "patch";
/// The program as the engine made it, for sample-accurate recording: a
/// virtual *source* fed from the engine in the same cycle, before the system
/// volume, at the graph rate in 32-bit float, before any device conversion
/// (Bluetooth codecs etc.). A source, not a sink: it never shows up as an
/// output the system could switch to. Desktop-audio recorders are moved
/// here ([`route_capture_apps`]).
pub const PROGRAM: &str = "patch.program";
/// Configurable input mix exposed to recording and calling applications.
pub const INPUT: &str = "patch.input";
/// The Mixer's AUX 1 and 2 as virtual sources (recordable, streamable),
/// while Patch is the output selected in the bar.
pub const AUX: [&str; 2] = ["patch.aux1", "patch.aux2"];

/// Processes the engine has run, by command (the Performance Unit shows them).
pub static SPAWNED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::LazyLock::new(Default::default);

/// The DJ mixer's LINE inputs (deck channel → application), kept across
/// engine restarts.
fn deck_lines_file() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("deck-lines.json")
}

/// The output device behind Patch, kept across engine restarts.
fn output_file() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("output-device")
}

fn selection_file() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("output-selection")
}

/// The bar's choice, kept across logins (the runtime file is not): "patch"
/// for the full console, or a device. The installer starts it at "patch".
fn saved_selection_file() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omarchy-patch").join("main-output")
}

fn save_selection(sink: &str) {
    let f = saved_selection_file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    let _ = std::fs::write(f, sink);
}

/// A new session starts from the saved choice; a session from before it
/// was saved keeps what it has. "patch" only while Patch is the system's
/// audio: without it the bar picks devices.
fn restore_selection() {
    let saved = saved_selection_file();
    match std::fs::read_to_string(selection_file()) {
        Ok(now) => {
            if !saved.exists() && !now.trim().is_empty() {
                save_selection(now.trim());
            }
        }
        Err(_) => {
            if let Ok(choice) = std::fs::read_to_string(&saved)
                && !choice.trim().is_empty()
                && (choice.trim() != BUS || patch_core::setup::load().system_audio)
            {
                let _ = std::fs::write(selection_file(), choice.trim());
            }
        }
    }
}

fn validate_output(sink: &str, all: &[(String, u32)]) -> Result<(), String> {
    if sink == BUS || (!sink.starts_with(BUS) && all.iter().any(|(n, _)| n == sink)) {
        Ok(())
    } else {
        Err(format!("'{sink}' is not an available playback destination"))
    }
}

fn stream_destination(app: &str, apps: &std::collections::BTreeMap<String, String>) -> String {
    if let Some(deck) = app.strip_prefix("Patch Deck ")
        && matches!(deck, "A" | "B" | "C" | "D")
    {
        return format!("patch.deck-{}", deck.to_lowercase());
    }
    apps.iter()
        .find(|(_, name)| app.eq_ignore_ascii_case(name))
        .map(|(ch, _)| format!("patch.{ch}"))
        .unwrap_or_else(|| BUS.to_string())
}

/// Applications recording the Patch input mix (when it is the selected
/// input) go to `patch.input`; recorders of desktop audio go to the program.
/// True while an application records the input.
pub(crate) fn route_capture_apps() -> bool {
    route_captures(true)
}

/// Every capture stream not Patch's own: onto the program when it records
/// the bus's monitor, and (with `apps`) onto `patch.input` otherwise.
///
/// "Desktop audio" is the default sink's monitor (Omarchy's screen
/// recorder, OBS, gpu-screen-recorder's `default_output`). With Patch as the
/// default sink that is `patch.monitor`: what apps play *into* Patch, without
/// the decks, microphones, EQ, FX or faders, and silent in a DJ set. What
/// you hear is the program.
fn route_captures(apps: bool) -> bool {
    let captures: Vec<serde_json::Value> = run(&["pactl", "-f", "json", "list", "source-outputs"])
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let sources: Vec<serde_json::Value> = run(&["pactl", "-f", "json", "list", "sources"])
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let index_of = |name: &str| {
        sources
            .iter()
            .find(|source| source["name"] == name)
            .map(|source| &source["index"])
    };
    let desktop = index_of(&format!("{BUS}.monitor"));
    let program = index_of(PROGRAM);
    let target = index_of(INPUT);
    let mut active = false;
    for capture in captures {
        let node = capture["properties"]["node.name"].as_str().unwrap_or("");
        let app = capture["properties"]["application.name"]
            .as_str()
            .unwrap_or("");
        let own = node.starts_with("patch")
            || node.starts_with("Patch meter")
            || app.starts_with("Patch ");
        // A loopback too (Omarchy's recorder mixes desktop and microphone
        // through one); Patch's own loopbacks never read the bus monitor
        // while the engine runs.
        if !own
            && program.is_some()
            && desktop.is_some_and(|d| capture["source"] == *d)
            && let Some(index) = capture["index"].as_u64()
        {
            if run(&["pactl", "move-source-output", &index.to_string(), PROGRAM]).is_none() {
                // A stream that may not be moved (gpu-screen-recorder's):
                // its own links then.
                relink_to_program(node, &links());
            }
            continue;
        }
        let Some(target) = target.filter(|_| apps) else {
            continue;
        };
        let source = sources
            .iter()
            .find(|source| source["index"] == capture["source"]);
        if own
            || node.contains("loopback-")
            || source.is_some_and(|source| source["name"].as_str().is_some_and(chosen_source))
        {
            continue;
        }
        let on_input = capture["source"] == *target;
        // The source the application asked for (Patch AUX 2 picked in a
        // browser, a microphone chosen in a call): its choice stands. Only
        // the default follows the input mix.
        if !on_input && picked_source(&capture["properties"]) {
            continue;
        }
        active |= capture["corked"].as_bool() != Some(true);
        if on_input {
            continue;
        }
        if let Some(index) = capture["index"].as_u64() {
            let _ = run(&["pactl", "move-source-output", &index.to_string(), INPUT]);
        }
    }
    active
}

/// The system default output, followed only once it has held.
///
/// A WirePlumber restart (by hand, or after a PipeWire update) drops every
/// device and passes the default through whatever comes back first before
/// it puts "patch" back. Followed at once, that flicker became the user's
/// choice in the bar: the selection and the remembered output moved to
/// another device (2026-11-22). A real switch in the bar holds, and lands
/// after SETTLE. The engine's first look is its start, followed at once.
/// Without system audio the default is the output switch itself: no wait.
#[derive(Default)]
struct DefaultFollow {
    started: bool,
    pending: Option<(String, std::time::Instant)>,
}

impl DefaultFollow {
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

    fn started(&self) -> bool {
        self.started
    }

    /// Once per tick. `seen`: a new default (not Patch, not the one
    /// followed), or None. True when `seen` is to be followed now.
    fn settled(&mut self, seen: Option<&str>, now: std::time::Instant, wait: std::time::Duration) -> bool {
        let first = !std::mem::replace(&mut self.started, true);
        let Some(seen) = seen else {
            self.pending = None;
            return false;
        };
        if first || wait.is_zero() {
            self.pending = None;
            return true;
        }
        match &self.pending {
            Some((p, since)) if p == seen => {
                let held = now.duration_since(*since) >= wait;
                if held {
                    self.pending = None;
                }
                held
            }
            _ => {
                self.pending = Some((seen.to_string(), now));
                false
            }
        }
    }
}

/// Whether a capture stream named its source (`target.object`, or the older
/// `node.target`/`target.node`) rather than following the default source.
fn picked_source(props: &serde_json::Value) -> bool {
    ["target.object", "target.node", "node.target"].iter().any(|key| {
        props[*key]
            .as_str()
            .is_some_and(|t| !t.is_empty() && t != "-1" && t != "@DEFAULT_SOURCE@")
    })
}

/// Sources an application records on purpose, never moved onto the input
/// mix: monitors, the program, and the Mixer's AUX sends (Patch AUX 1 picked
/// as a browser's or a call's microphone).
fn chosen_source(name: &str) -> bool {
    name.ends_with(".monitor") || name == PROGRAM || AUX.contains(&name)
}

/// A capture node's links from the bus's monitor, onto the program's
/// matching side: the new one first, so the stream is never unlinked (the
/// session manager would link it again to what it asked for).
fn relink_to_program(node: &str, have: &HashSet<(String, String)>) {
    for (from, to) in program_relinks(node, have) {
        let _ = run(&["pw-link", &to.0, &from.1]);
        let _ = run(&["pw-link", "-d", &from.0, &from.1]);
    }
}

/// (old link, new link) for each of `node`'s ports fed by the bus monitor.
fn program_relinks(
    node: &str,
    have: &HashSet<(String, String)>,
) -> Vec<((String, String), (String, String))> {
    let monitor = format!("{BUS}:monitor_");
    let mut out: Vec<_> = have
        .iter()
        .filter(|(_, to)| to.split_once(':').is_some_and(|(n, _)| n == node))
        .filter_map(|(from, to)| {
            let side = from.strip_prefix(&monitor)?;
            Some((
                (from.clone(), to.clone()),
                (format!("{PROGRAM}:capture_{side}"), to.clone()),
            ))
        })
        .collect();
    out.sort();
    out
}

/// Whether the bus's monitor carries the `patch` sink's volume and mute
/// (PipeWire's monitor.channel-volumes, set at runtime). Off while the
/// engine applies the system volume after the recording tap; on for
/// anything else that plays the monitor. True when PipeWire took it.
pub fn set_monitor_volumes(on: bool) -> bool {
    let props = format!("{{ params = [ \"monitor.channel-volumes\" {on} ] }}");
    // pw-cli exits 0 on errors too: it echoes the params when it set them.
    run(&["pw-cli", "s", BUS, "Props", &props])
        .is_some_and(|o| o.contains("monitor.channel-volumes"))
}

pub(crate) fn run(args: &[&str]) -> Option<String> {
    let key = format!("{} {}", args[0], args.get(1).unwrap_or(&""));
    *SPAWNED.lock().unwrap().entry(key).or_default() += 1;
    let out = Command::new(args[0]).args(&args[1..]).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The program's links to a device: each playback port from the engine
/// output for its position (FL, FR, FC, LFE, SL, SR, RL, RR, MONO; see
/// surround.rs for what each carries, as PipeWire upmixes an app), and
/// which pairs carry the decoded surround. Positions Patch doesn't make
/// (FLC, TFL, …) stay unlinked.
pub fn program_ports(ports: &[String], node: &str) -> (Vec<(String, String)>, u8) {
    let prefix = format!("{node}:playback_");
    let mut out = vec![];
    let mut positions = vec![];
    for p in ports {
        let Some(pos) = p.strip_prefix(&prefix) else {
            continue;
        };
        positions.push(pos);
        if ["FL", "FR", "FC", "LFE", "SL", "SR", "RL", "RR", "MONO"].contains(&pos) {
            out.push((format!("{ENGINE}:out_{pos}"), p.clone()));
        }
    }
    // Pro Audio interfaces often expose AUX0..AUXn rather than speaker
    // positions. A simple Main mix uses only their first pair; additional
    // ports remain unassigned instead of receiving duplicate program audio.
    if out.is_empty() {
        let mut aux: Vec<_> = ports
            .iter()
            .filter_map(|p| {
                p.strip_prefix(&prefix)?
                    .strip_prefix("AUX")?
                    .parse::<u32>()
                    .ok()
                    .map(|n| (n, p))
            })
            .collect();
        aux.sort_by_key(|(n, _)| *n);
        match aux.as_slice() {
            [(_, mono)] => out.push((format!("{ENGINE}:out_MONO"), (*mono).clone())),
            [(_, left), (_, right), ..] => {
                out.push((format!("{ENGINE}:out_FL"), (*left).clone()));
                out.push((format!("{ENGINE}:out_FR"), (*right).clone()));
            }
            _ => {}
        }
    }
    if out.is_empty() {
        let audio: Vec<_> = ports
            .iter()
            .filter(|p| p.starts_with(&format!("{node}:")))
            .collect();
        match audio.as_slice() {
            [mono] => out.push((format!("{ENGINE}:out_MONO"), (*mono).clone())),
            [left, right, ..] => {
                out.push((format!("{ENGINE}:out_FL"), (*left).clone()));
                out.push((format!("{ENGINE}:out_FR"), (*right).clone()));
            }
            _ => {}
        }
    }
    (out, crate::surround::psd_for(&positions))
}

/// A sink's volume, as Pulse raw values per channel (65536 = 100 %), and
/// its mute.
pub fn sink_volume(sink: &str) -> Option<(Vec<u32>, bool)> {
    let raw = parse_volume(&run(&["pactl", "get-sink-volume", sink])?);
    let mute = run(&["pactl", "get-sink-mute", sink]).is_some_and(|m| m.contains("yes"));
    (!raw.is_empty()).then_some((raw, mute))
}

/// `Volume: front-left: 65536 / 100% / 0.00 dB, front-right: …` → raw values.
fn parse_volume(text: &str) -> Vec<u32> {
    text.lines()
        .next()
        .unwrap_or("")
        .split(',')
        .filter_map(|part| {
            part.split('/')
                .next()?
                .split_whitespace()
                .last()?
                .parse()
                .ok()
        })
        .collect()
}

/// `to` at `from`'s volume (its channels' mean on every channel) and mute.
fn copy_volume(from: &str, to: &str) {
    if let Some((raw, mute)) = sink_volume(from) {
        let mean = raw.iter().map(|v| *v as u64).sum::<u64>() / raw.len() as u64;
        let _ = run(&["pactl", "set-sink-volume", to, &mean.to_string()]);
        let _ = run(&["pactl", "set-sink-mute", to, if mute { "1" } else { "0" }]);
    }
}

/// The bus's volume, linear per side (Pulse volumes are cubic: 65536 =
/// unity), and its mute.
pub fn bus_volume() -> Option<([f32; 2], bool)> {
    let (raw, mute) = sink_volume(BUS)?;
    let lin = |v: u32| (v as f32 / 65536.0).powi(3);
    let l = raw.first().copied().map_or(1.0, lin);
    let r = raw.get(1).copied().map_or(l, lin);
    Some(([l, r], mute))
}

/// Before a device goes to full scale under system audio, the engine must
/// already apply the system volume to what leaves for it. The volume
/// thread does this too, but up to two seconds later (it notices the setup
/// change on its own file check): until then decks and app channels played
/// at unity into a device at 100 %, a full-scale blast on every takeover.
/// Here it happens first, so the level only ever drops on the way. False:
/// the volume could not be read, and the device must keep its own volume.
fn engage_system_volume(params: Option<&std::sync::Arc<crate::Params>>) -> bool {
    use std::sync::atomic::Ordering;
    let Some(p) = params else { return false };
    if p.system_post.load(Ordering::Relaxed) {
        return true;
    }
    let Some((v, mute)) = bus_volume() else { return false };
    p.set_system(v[0], v[1], mute);
    // After the tap, ramped. The bus monitor still carries the volume
    // until the volume thread claims it: apps are quieter for that moment,
    // never louder.
    p.system_post.store(true, Ordering::Relaxed);
    true
}

fn full_volume(sink: &str) {
    let _ = run(&["pactl", "set-sink-volume", sink, "100%"]);
    let _ = run(&["pactl", "set-sink-mute", sink, "0"]);
}

/// `to` at the *quieter* of `from`'s and `to`'s volumes (each channel's mean),
/// with `from`'s mute. Taking a device over must never *raise* the system
/// volume: Patch drives a device at full scale, so that device's 100 % is not
/// the level the user hears, and adopting it as the system volume jumped the
/// output to 0 dB (a blast the first time an output was picked in the bar).
fn copy_volume_quieter(from: &str, to: &str) {
    if let (Some((raw, mute)), Some((cur, _))) = (sink_volume(from), sink_volume(to)) {
        let mean = |v: &[u32]| {
            if v.is_empty() {
                0
            } else {
                v.iter().map(|x| *x as u64).sum::<u64>() / v.len() as u64
            }
        };
        let quietest = mean(&raw).min(mean(&cur));
        let _ = run(&["pactl", "set-sink-volume", to, &quietest.to_string()]);
        let _ = run(&["pactl", "set-sink-mute", to, if mute { "1" } else { "0" }]);
    }
}

/// The sink whose monitor feeds channel `name`.
pub fn source_sink(name: &str) -> Option<String> {
    match name {
        "bus" => Some(BUS.to_string()),
        n if n.starts_with("deck-") || n.starts_with("app-") || n.starts_with("stream-") => Some(format!("patch.{n}")),
        _ => None,
    }
}

/// An application's playback stream (a PipeWire/Pulse sink input).
#[derive(Clone, Debug)]
pub struct AppStream {
    pub index: u32,
    /// The sink it plays to (by name).
    pub sink: String,
    pub app: String,
    pub key: String,
    pub label: String,
    pub browser_helper_candidate: bool,
    pub native: bool,
    pub gain: f32,
    pub muted: bool,
}

/// Playing application streams, from `pactl -f json`.
pub fn app_streams() -> Vec<AppStream> {
    let sinks: std::collections::HashMap<u64, String> =
        run(&["pactl", "-f", "json", "list", "short", "sinks"])
            .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| Some((v["index"].as_u64()?, v["name"].as_str()?.to_string())))
            .collect();
    run(&["pactl", "-f", "json", "list", "sink-inputs"])
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| {
            let props = &v["properties"];
            let app = props["application.name"]
                .as_str()
                .or(props["application.process.binary"].as_str())?
                .to_string();
            let client = v["client"].as_u64().or_else(|| v["client"].as_str()?.parse().ok()).filter(|n| *n != u32::MAX as u64).map(|n| n as u32)
                .or_else(|| props["client.id"].as_str().and_then(|s| s.parse().ok()));
            let process = props["application.process.id"].as_str().unwrap_or("");
            let serial = v["index"].as_u64()? as u32;
            let media = props["media.name"].as_str().unwrap_or("");
            let role = props["media.role"].as_str().unwrap_or("").to_ascii_lowercase();
            let node = props["node.name"].as_str().unwrap_or("");
            let native = !matches!(role.as_str(), "event" | "notification" | "alert") && !node.starts_with("patch.")
                && !app.eq_ignore_ascii_case("paplay") && !node.eq_ignore_ascii_case("paplay")
                && app != "Patch Player" && !app.starts_with("Patch Deck ");
            let generic = media.is_empty() || media == app || matches!(media, "Playback" | "AudioStream");
            let binary = props["application.process.binary"].as_str().unwrap_or("").to_ascii_lowercase();
            let browser_helper_candidate = generic && matches!(binary.as_str(), "chromium" | "chromium-browser" | "google-chrome" | "chrome" | "brave" | "brave-browser" | "microsoft-edge" | "msedge");
            let label = if generic { app.clone() } else { format!("{app} · {media}") };
            Some(AppStream {
                key: patch_core::stream::key(serial, client, process), label, native, browser_helper_candidate,
                gain: {
                    let values: Vec<f32> = v["volume"].as_object().into_iter().flat_map(|m| m.values())
                        .filter_map(|c| c["value"].as_f64().map(|v| v as f32 / 65536.0)).collect();
                    if values.is_empty() { 1.0 } else { (values.iter().sum::<f32>() / values.len() as f32).powi(3) }
                },
                muted: v["mute"].as_bool().unwrap_or(false),
                index: v["index"].as_u64()? as u32,
                sink: sinks.get(&v["sink"].as_u64()?).cloned().unwrap_or_default(),
                app,
            })
        })
        // Not Patch's own streams (exact names only: an app may well have
        // "patch" in its name).
        .filter(|s| {
            s.app == "Patch Player"
                || s.app.starts_with("Patch Deck ")
                || !(s.app.eq_ignore_ascii_case("omarchy-patch")
                    || s.app.eq_ignore_ascii_case("patch")
                    || s.app.starts_with("Patch "))
        })
        .collect()
}

/// Does stream `s` belong to application `app` (name or binary, any case)?
pub fn is_app(s: &AppStream, app: &str) -> bool {
    s.app.eq_ignore_ascii_case(app)
}

fn description(sink: &str) -> String {
    patch_core::pulse::description(sink).unwrap_or_else(|| sink.into())
}

/// Existing links as (output port, input port), from `pw-link -l`.
fn links() -> HashSet<(String, String)> {
    let mut set = HashSet::new();
    let Some(text) = run(&["pw-link", "-l"]) else {
        return set;
    };
    let mut current = String::new();
    for line in text.lines() {
        if let Some(to) = line.trim_start().strip_prefix("|-> ") {
            set.insert((current.clone(), to.trim().to_string()));
        } else if !line.starts_with(' ') {
            current = line.trim().to_string();
        }
    }
    set
}

fn output_ports(node: &str) -> Vec<String> {
    run(&["pw-link", "-o"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|p| {
            (p == &node || p.strip_prefix(node).is_some_and(|r| r.starts_with(':')))
                && !p.contains(":monitor_")
        })
        .map(String::from)
        .collect()
}

/// The lane a remote voice plays in now: a stream's lane by its key (none
/// once the stream has ended), or an application channel in use.
fn remote_lane(
    source: &str,
    lanes: &crate::stream_lanes::Lanes,
    apps: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if source.starts_with("stream:") {
        // The stream, or the one that took over from it (a reopening).
        lanes.current(source).and_then(|key| lanes.assigned.get(key)).cloned()
    } else {
        apps.contains_key(source).then(|| source.to_string())
    }
}

/// A lane's soundcard monitor onto a mic channel's inputs.
fn remote_links(sink: &str, channel: &str) -> Vec<(String, String)> {
    ["FL", "FR"]
        .map(|side| (format!("{sink}:monitor_{side}"), format!("{ENGINE}:in_{channel}_{side}")))
        .to_vec()
}

/// A capture source's ports onto a channel's two inputs: its left and right,
/// or its one port (a mono mic) onto both.
fn mic_links(ports: &[String], channel: &str) -> Vec<(String, String)> {
    let to = |side: &str| format!("{ENGINE}:in_{channel}_{side}");
    let side = |s: &str| ports.iter().find(|p| p.ends_with(&format!("_{s}")));
    match (side("FL"), side("FR")) {
        (Some(l), Some(r)) => vec![(l.clone(), to("FL")), (r.clone(), to("FR"))],
        _ if ports.len() >= 2 => vec![(ports[0].clone(), to("FL")), (ports[1].clone(), to("FR"))],
        _ => ports
            .first()
            .map(|p| vec![(p.clone(), to("FL")), (p.clone(), to("FR"))])
            .unwrap_or_default(),
    }
}

fn input_ports(node: &str) -> Vec<String> {
    run(&["pw-link", "-i"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|p| p.strip_prefix(node).is_some_and(|r| r.starts_with(':')))
        .map(String::from)
        .collect()
}

/// The output a plain session would fall back to when the remembered one is
/// gone: the live device PipeWire ranks highest (`priority.session`), the same
/// rule the session itself uses to elect a default output. Patch's own sinks are
/// not candidates - the bus, the event-sound sink and the decks' soundcards.
fn best_output() -> Option<String> {
    let text = run(&["pactl", "list", "sinks"]).unwrap_or_default();
    let mut name: Option<String> = None;
    let mut best: Option<(i64, String)> = None;
    // A device that names no priority (a virtual or unusual sink) still
    // plays: the first one, when no device ranks itself. Without it Patch
    // took the default output and sent it nowhere.
    let mut first: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(n) = t.strip_prefix("Name: ") {
            name = Some(n.to_string());
            if first.is_none() && !n.starts_with(BUS) {
                first = Some(n.to_string());
            }
        } else if let Some(p) = t.strip_prefix("priority.session = ") {
            let prio = p.trim().trim_matches('"').parse::<i64>().unwrap_or(0);
            if let Some(n) = name.clone()
                && !n.starts_with(BUS)
                && best.as_ref().is_none_or(|(b, _)| prio > *b)
            {
                best = Some((prio, n));
            }
        }
    }
    best.map(|(_, n)| n).or(first)
}

fn sinks() -> Vec<(String, u32)> {
    listed_sinks().unwrap_or_default()
}

/// The sinks, or None when pactl failed (it times out under memory
/// pressure): an empty answer then is no answer.
fn listed_sinks() -> Option<Vec<(String, u32)>> {
    // "index  name  driver  format  state"; the format carries the rate.
    Some(parse_sinks(&run(&["pactl", "list", "short", "sinks"])?))
}

fn parse_sinks(out: &str) -> Vec<(String, u32)> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let name = f.get(1)?.to_string();
            let rate = f
                .get(3)
                .and_then(|s| {
                    s.split_whitespace()
                        .find_map(|w| w.strip_suffix("Hz")?.parse().ok())
                })
                .unwrap_or(48_000);
            Some((name, rate))
        })
        .collect()
}

fn modules_with(arg: &str) -> Vec<String> {
    run(&["pactl", "list", "short", "modules"])
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(arg))
        .filter_map(|l| l.split('\t').next().map(String::from))
        .collect()
}

/// Patch's sinks listed twice: unload every module but the first that
/// makes one (only when the listing shows a name twice: no pactl call on a
/// normal pass). True when something was unloaded.
fn drop_duplicate_sinks(all: &[(String, u32)]) -> bool {
    let mut dropped = false;
    for name in [BUS, "patch.system-sounds"] {
        if all.iter().filter(|(n, _)| n == name).count() > 1 {
            for m in modules_with(&format!("sink_name={name} ")).iter().skip(1) {
                dropped |= run(&["pactl", "unload-module", m]).is_some();
            }
        }
    }
    dropped
}

fn loopback_modules() -> Vec<String> {
    run(&["pactl", "list", "short", "modules"])
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("module-loopback") && l.contains(&format!("source={BUS}.monitor")))
        .filter_map(|l| l.split('\t').next().map(String::from))
        .collect()
}

pub struct Links {
    pub params: Option<std::sync::Arc<crate::Params>>,
    /// Whether the AUX virtual sources exist (None: not checked yet).
    aux_sources: Option<bool>,
    channels: Vec<String>,
    /// App channels in use: channel ("app-1") → application name. Their
    /// sinks exist only while assigned; the app's streams are kept there.
    pub apps: std::collections::BTreeMap<String, String>,
    pub stream_lanes: crate::stream_lanes::Lanes,
    pub streams_enabled: bool,
    pub stream_labels: std::collections::BTreeMap<String, String>,
    pub browser_helper_candidates: std::collections::BTreeSet<String>,
    pub stream_apps: std::collections::BTreeMap<String, String>,
    /// Per application, its last lane settings (see [`crate::stream_lanes::Profile`]).
    pub stream_profiles: std::collections::BTreeMap<String, crate::stream_lanes::Profile>,
    /// Per application, the stream whose settings it keeps (this pass).
    pub stream_representatives: std::collections::BTreeMap<String, String>,
    stream_normalized: HashSet<String>,
    pub stream_arrivals: Vec<(String, String, String)>,
    /// Handovers made lately: (old key, new key, when). Undone when both
    /// streams play (two sources after all, not a reopened one).
    recent_swaps: Vec<(String, String, std::time::Instant)>,
    observe_streams_until: Option<std::time::Instant>,
    /// When each stream was first heard (a handover is judged for 2 s after).
    first_heard: std::collections::BTreeMap<String, std::time::Instant>,
    /// Microphone channels in use: channel ("mic-1") → capture source (its
    /// PipeWire node name). Linked only while assigned: an unused mic stays
    /// closed (no privacy indicator, no device kept awake).
    pub mics: std::collections::BTreeMap<String, String>,
    /// The decks' soundcards are wanted: a stream player plays into them.
    /// Local files play inside the engine and need none, so the service
    /// starts without them (the minimal Patch); a player asks for them
    /// (`deck_sinks`), and an engine that finds them keeps them.
    pub decks_wanted: Option<bool>,
    /// The DJ mixer's input selectors on LINE: deck channel ("deck-a") →
    /// application name. That app's streams play into the deck's soundcard
    /// and through the channel's trim, EQ, filter, fader and crossfader.
    /// Kept across engine restarts (a runtime file).
    pub deck_lines: std::collections::BTreeMap<String, String>,
    /// Which decks' `line` flags this engine has set (to notice a load
    /// clearing one).
    line_applied: [bool; crate::MAX_DECKS],
    /// Applications playing now that a LINE input can take, by name.
    pub line_sources: Vec<String>,
    /// Headphones: the device the cue output plays on (none until chosen).
    pub cue_target: Option<String>,
    pub output_routing: crate::output_routes::Routing,
    pub capture_active: bool,
    /// Output chosen in Patch; cleared when the system default changes.
    explicit: Option<String>,
    last_default: Option<String>,
    /// A new system default, followed once it has held (see DefaultFollow).
    default_follow: DefaultFollow,
    pub target: Option<String>,
    /// The engine is linked to the output; the loopback is gone.
    pub active: bool,
    /// The setup (system audio), read again only when its file changes.
    setup: patch_core::setup::Watched,
    /// System audio made the Patch bus the default output: put the device
    /// back when it's switched off.
    owns_default: bool,
    /// Passes still to make soon: the bar sets the default output first and
    /// moves the apps just after (moves wake nobody), so look again.
    pub recheck: u8,
    /// The outputs drive the graph (see `set_drive`), and how they're
    /// linked now (None: not yet).
    drive: bool,
    out_mode: Option<bool>,
    device_properties_checked: bool,
    device_properties_retry: Option<std::time::Instant>,
    /// The output device's decoded surround pairs (surround.rs), shared
    /// with the engine's audio thread.
    pub psd: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Handover successor: outputs join the graph only after this, once
    /// the node already runs cycles (a node that joins and feeds a sink in
    /// the same cycle makes that sink skip one cycle — a 10 ms gap).
    outputs_after: Option<std::time::Instant>,
}

impl Links {
    /// Whether the engine's outputs drive the graph (a deck plays): active
    /// links to the devices; else passive, and an idle device suspends.
    /// The `idler` sets it. PipeWire fixes a link's passivity when it's
    /// made, so a change relinks the outputs (in silence, or as a deck
    /// starts: a few ms, as the device waking up).
    pub fn set_drive(&mut self, on: bool) {
        if self.drive != on {
            self.drive = on;
            self.tick();
        }
    }

    pub fn new(channels: &[String]) -> Self {
        restore_selection();
        let delay = std::env::var("PATCH_ENGINE_OUTPUT_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);
        let outputs_after = std::env::var_os("PATCH_ENGINE_SILENT")
            .map(|_| std::time::Instant::now() + std::time::Duration::from_millis(delay));
        Links {
            params: None,
            channels: channels.to_vec(),
            apps: Default::default(),
            stream_lanes: std::fs::read(patch_core::ipc::runtime_dir().join("stream-lanes.json"))
                .ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            streams_enabled: std::env::var_os("PATCH_ENGINE_SILENT").is_none(),
            stream_labels: Default::default(),
            browser_helper_candidates: Default::default(),
            stream_apps: Default::default(),
            stream_profiles: crate::stream_lanes::load_profiles(),
            stream_representatives: Default::default(),
            stream_normalized: Default::default(),
            stream_arrivals: Vec::new(),
            recent_swaps: Vec::new(),
            observe_streams_until: None,
            first_heard: Default::default(),
            mics: Default::default(),
            cue_target: load_cue_target(),
            aux_sources: None,
            output_routing: crate::output_routes::Routing::load(),
            capture_active: false,
            explicit: None,
            last_default: None,
            default_follow: DefaultFollow::default(),
            target: None,
            active: false,
            outputs_after,
            decks_wanted: None,
            deck_lines: std::fs::read(deck_lines_file())
                .ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            line_applied: [false; crate::MAX_DECKS],
            line_sources: Vec::new(),
            setup: Default::default(),
            owns_default: false,
            recheck: 0,
            drive: true,
            out_mode: None,
            device_properties_checked: false,
            device_properties_retry: None,
            psd: Default::default(),
        }
    }

    fn stream_sink(&self, stream: &AppStream) -> Option<String> {
        self.line_of(&stream.app).map(|ch| format!("patch.{ch}"))
            .or_else(|| self.stream_lanes.assigned.get(&stream.key).map(|ch| format!("patch.{ch}")))
    }

    /// The deck channel whose LINE input takes application `app`.
    fn line_of(&self, app: &str) -> Option<&str> {
        self.deck_lines.iter().find(|(_, a)| a.eq_ignore_ascii_case(app)).map(|(ch, _)| ch.as_str())
    }

    /// Puts application `app` on deck channel `deck`'s LINE input (None:
    /// the channel back on its deck). An app is on one LINE at a time.
    pub fn set_deck_line(&mut self, deck: &str, app: Option<String>) -> Result<(), String> {
        if !matches!(deck, "deck-a" | "deck-b" | "deck-c" | "deck-d") {
            return Err(format!("'{deck}' is not a deck channel"));
        }
        match app.filter(|a| !a.trim().is_empty()) {
            Some(app) => {
                self.deck_lines.retain(|_, a| !a.eq_ignore_ascii_case(&app));
                self.deck_lines.insert(deck.to_string(), app);
            }
            None => {
                self.deck_lines.remove(deck);
            }
        }
        self.save_deck_lines();
        self.sync_deck_lines();
        self.recheck = self.recheck.max(2);
        Ok(())
    }

    fn save_deck_lines(&self) {
        let path = deck_lines_file();
        if let Ok(bytes) = serde_json::to_vec(&self.deck_lines) {
            let temp = path.with_extension("json.new");
            if std::fs::write(&temp, bytes).is_ok() { let _ = std::fs::rename(temp, path); }
        }
    }

    /// The decks' `line` flags from `deck_lines`; a LINE a track load
    /// switched off (the worker clears the flag) leaves the map.
    fn sync_deck_lines(&mut self) {
        let Some(p) = self.params.clone() else { return };
        let mut changed = false;
        for (d, letter) in ["a", "b", "c", "d"].iter().enumerate() {
            let name = format!("deck-{letter}");
            let Some(sh) = p.channel(&name).and_then(|c| p.deck_of[c]).and_then(|d| p.decks.get(d)) else { continue };
            let on = sh.line.load(std::sync::atomic::Ordering::Acquire);
            if self.line_applied[d] && !on && self.deck_lines.contains_key(&name) {
                // A track was loaded onto the deck: it is the source again.
                self.deck_lines.remove(&name);
                changed = true;
            }
            let want = self.deck_lines.contains_key(&name);
            if want != on {
                sh.line.store(want, std::sync::atomic::Ordering::Release);
            }
            self.line_applied[d] = want;
        }
        if changed {
            self.save_deck_lines();
            self.recheck = self.recheck.max(2);
        }
    }

    fn normalize_native(&mut self, st: &AppStream, target: &str) {
        if !target.starts_with("patch.stream-") || self.stream_normalized.contains(&st.key) { return; }
        let volume = (st.gain - 1.0).abs() < 0.001 || run(&["pactl", "set-sink-input-volume", &st.index.to_string(), "65536"]).is_some();
        let mute = !st.muted || run(&["pactl", "set-sink-input-mute", &st.index.to_string(), "0"]).is_some();
        if volume && mute { self.stream_normalized.insert(st.key.clone()); }
        else { self.recheck = self.recheck.max(2); }
    }

    /// A browser moves its sound to a new playback stream of the same
    /// process (Chromium reopens them all when a page starts an
    /// echo-cancelling capture) and keeps the old one open, quiet. The new
    /// stream takes the old one's lane, so its strip, fader, sends and a
    /// Remote voice stay with the sound; see [`crate::stream_lanes::handover`].
    fn follow_handovers(&mut self, heard_before: &std::collections::BTreeSet<String>) {
        let Some(p) = self.params.clone() else { return };
        let lane = |lanes: &crate::stream_lanes::Lanes, key: &str| lanes.assigned.get(key).and_then(|ch| p.channel(ch));
        // A redirected stream's audio reaches its mic input directly.
        // Read that input's raw signal when evaluating either sibling.
        let remote = self.remote_lanes();
        let heard = |lanes: &crate::stream_lanes::Lanes, key: &str, seconds| {
            lanes.assigned.get(key).is_some_and(|channel| {
                if let Some(mic) = remote.get(channel).and_then(|mic| p.channel(mic)) {
                    p.mic_meter[mic].heard_within(seconds)
                } else {
                    p.channel(channel).is_some_and(|c| p.stream_heard_within(c, seconds))
                }
            })
        };
        // Two sources after all: both play once the streams have moved.
        let mut undo = vec![];
        self.recent_swaps.retain(|(old, new, at)| {
            let age = at.elapsed();
            if age < std::time::Duration::from_secs(2) { return true; }
            if age > std::time::Duration::from_secs(6) { return false; }
            let both = [old, new].iter().all(|k| heard(&self.stream_lanes, k, 0.3));
            if both { undo.push((old.clone(), new.clone())); }
            !both
        });
        for (old, new) in undo {
            self.stream_lanes.unswap(&old, &new);
            self.set_lane_owner(&p, &old);
            self.set_lane_owner(&p, &new);
            self.recheck = self.recheck.max(2);
        }
        let bound: std::collections::BTreeSet<String> = self.mics.values()
            .filter_map(|src| self.stream_lanes.current(src).map(String::from)).collect();
        let keys: Vec<String> = self.stream_lanes.assigned.keys().cloned().collect();
        self.first_heard.retain(|k, _| self.stream_lanes.assigned.contains_key(k));
        // Heard before this pass (or before an engine upgrade: the set is
        // kept): not new, never a handover's successor.
        let long_ago = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now);
        for key in &keys {
            if !self.first_heard.contains_key(key) && heard(&self.stream_lanes, key, 0.5) {
                let at = if heard_before.contains(key) || self.stream_lanes.inherited(key) {
                    long_ago
                } else {
                    std::time::Instant::now()
                };
                self.first_heard.insert(key.clone(), at);
            }
        }
        let mut taken = std::collections::BTreeSet::new();
        for new in &keys {
            let Some(cn) = lane(&self.stream_lanes, new) else { continue };
            let first_heard = self.first_heard.get(new).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2))
                && heard(&self.stream_lanes, new, 0.5);
            if !first_heard { continue; }
            let app = self.stream_apps.get(new);
            let old = keys.iter().find(|old| {
                *old != new && !taken.contains(*old)
                    && crate::stream_lanes::handover(
                        crate::stream_lanes::process(old) == crate::stream_lanes::process(new)
                            && app.is_some() && self.stream_apps.get(*old) == app,
                        first_heard,
                        p.stream_touched(cn),
                        heard_before.contains(*old) || self.stream_lanes.touched.contains(*old) || bound.contains(*old),
                        heard(&self.stream_lanes, old, 3.0),
                        heard(&self.stream_lanes, old, 0.4),
                    )
            }).cloned();
            let Some(old) = old else { continue };
            if let Some((_, b)) = self.stream_lanes.swap(&old, new) {
                // Never judged again: the old stream is now on the other lane.
                self.first_heard.insert(new.clone(), long_ago);
                taken.insert(old.clone());
                self.set_lane_owner(&p, &old);
                self.set_lane_owner(&p, new);
                // The lane the quiet stream lands on: its recent signal was
                // the new stream's, so it waits hidden, not as a fresh strip.
                if let Some(c) = p.channel(&b) { p.forget_stream_signal(c); }
                self.recent_swaps.push((old, new.clone(), std::time::Instant::now()));
                self.recheck = self.recheck.max(2);
            }
        }
    }

    fn set_lane_owner(&self, p: &crate::Params, key: &str) {
        if let Some(c) = self.stream_lanes.assigned.get(key).and_then(|ch| p.channel(ch)) {
            *p.stream_owners[c].lock().unwrap() = Some(key.to_string());
        }
    }

    /// Lanes taken by a Remote voice: lane ("stream-3", "app-1") → mic lane.
    pub fn remote_lanes(&self) -> std::collections::BTreeMap<String, String> {
        self.mics.iter()
            .filter_map(|(mic, src)| remote_lane(src, &self.stream_lanes, &self.apps).map(|lane| (lane, mic.clone())))
            .collect()
    }

    fn stream_channel_used(&self, channel: &str) -> bool {
        self.stream_lanes.assigned.values().any(|ch| ch == channel)
    }

    fn reconcile_streams(&mut self, streams: &[AppStream], all: &mut Vec<(String, u32)>) {
        let system_audio = self.setup.get().system_audio;
        self.sync_deck_lines();
        let mut sources: Vec<String> = streams.iter().filter(|st| st.native && st.sink != "patch.system-sounds")
            .map(|st| st.app.clone()).collect();
        sources.sort_by_key(|a| a.to_lowercase());
        sources.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        self.line_sources = sources;
        let eligible: Vec<_> = streams.iter().filter(|st| st.native && st.sink != "patch.system-sounds" &&
            self.line_of(&st.app).is_none() &&
            (system_audio || st.sink == BUS || st.sink.starts_with("patch.stream-")
                || st.sink.starts_with("patch.app-") || st.sink.starts_with("patch.deck-"))).collect();
        let heard_before = self.stream_lanes.heard.clone();
        let touched_before = self.stream_lanes.touched.clone();
        if let Some(p) = &self.params {
            for (key, channel) in &self.stream_lanes.assigned {
                let Some(c) = p.channel(channel) else { continue };
                if p.stream_has_signal(c) { self.stream_lanes.heard.insert(key.clone()); }
                if p.stream_touched(c) { self.stream_lanes.touched.insert(key.clone()); }
            }
            // Each application's settings, from the stream it was last heard
            // on, before a departed stream's lane is reset below.
            let lanes: Vec<_> = self.stream_lanes.assigned.iter().filter_map(|(key, channel)| {
                let c = p.channel(channel)?;
                Some((self.stream_apps.get(key)?.as_str(), key.as_str(), p.stream_signal_age(c), p.stream_touched(c)))
            }).collect();
            self.stream_representatives = crate::stream_lanes::representatives(lanes.into_iter());
            let mut changed = false;
            for (app, key) in &self.stream_representatives {
                let Some(c) = self.stream_lanes.assigned.get(key).and_then(|ch| p.channel(ch)) else { continue };
                let mut profile = crate::stream_lanes::Profile::of(p, c);
                profile.eq8 = self.stream_profiles.get(app).and_then(|old| old.eq8.clone());
                if self.stream_profiles.get(app) != Some(&profile) {
                    self.stream_profiles.insert(app.clone(), profile);
                    changed = true;
                }
            }
            if changed { crate::stream_lanes::save_profiles(&self.stream_profiles); }
        }
        self.browser_helper_candidates = eligible.iter().filter(|s| s.browser_helper_candidate).map(|s| s.key.clone()).collect();
        let before = self.stream_lanes.assigned.clone();
        let released = self.stream_lanes.reconcile(eligible.iter().map(|s| (s.key.clone(), s.label.clone())));
        self.stream_arrivals = eligible.iter().filter(|st| !self.stream_apps.contains_key(&st.key))
            .filter_map(|st| self.stream_lanes.assigned.get(&st.key).map(|ch| (st.key.clone(), st.app.clone(), ch.clone()))).collect();
        // A browser can open the replacement before it starts producing
        // audio. That change alone emits no graph event.
        if eligible.iter().any(|arrival| {
            let pid = crate::stream_lanes::process(&arrival.key);
            !pid.is_empty()
                && !before.contains_key(&arrival.key)
                && eligible.iter().any(|sibling| {
                    before.contains_key(&sibling.key)
                        && sibling.app == arrival.app
                        && crate::stream_lanes::process(&sibling.key) == pid
                })
        }) {
            self.observe_streams_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(8));
        }
        self.stream_labels = eligible.iter().map(|s| (s.key.clone(), s.label.clone())).collect();
        self.stream_apps = eligible.iter().map(|s| (s.key.clone(), s.app.clone())).collect();
        self.stream_normalized.retain(|key| self.stream_lanes.assigned.contains_key(key));
        // A reused lane starts with an empty sink, not the previous caller's
        // queued audio. Its RT filter/FX state is reset by an epoch as well.
        for channel in &released {
            let sink = format!("patch.{channel}");
            for module in modules_with(&format!("sink_name={sink} ")) {
                let _ = run(&["pactl", "unload-module", &module]);
            }
            all.retain(|(name, _)| name != &sink);
        }
        if let Some(p) = &self.params {
            for ch in &released {
                if let Some(c) = p.channel(ch) {
                    *p.stream_owners[c].lock().unwrap() = None;
                }
            }
            for (key, _) in self.stream_lanes.assigned.iter().filter(|(key, _)| {
                !before.contains_key(*key) && self.stream_lanes.inherited(key)
            }) {
                self.set_lane_owner(p, key);
            }
            // A returning stream keeps the reserved lane's settings. Fresh
            // streams reset their lanes before the graph connects them.
            let reset = self.stream_lanes.assigned.iter().filter_map(|(key, ch)| {
                (!before.contains_key(key) && !self.stream_lanes.inherited(key)).then_some(ch)
            });
            for ch in reset {
                if let Some(c) = p.channel(ch) {
                    let mut owner = p.stream_owners[c].lock().unwrap();
                    p.reset_stream_channel(c);
                    *owner = None;
                    if let Some(st) = eligible.iter().find(|st| !before.contains_key(&st.key)
                        && self.stream_lanes.assigned.get(&st.key) == Some(ch))
                    {
                    let donor = self.apps.iter().find(|(_, app)| app.eq_ignore_ascii_case(&st.app))
                        .and_then(|(name, _)| p.channel(name));
                    p.set_gain(c, st.gain * donor.map_or(1.0, |d| p.gain_of(d)));
                    p.set_mute(c, st.muted || donor.is_some_and(|d| p.is_muted(d)));
                    // The application's own lane settings, once it has some,
                    // win over a legacy application channel's.
                    if let Some(profile) = self.stream_profiles.get(&st.app) {
                        profile.apply(p, c, st.gain);
                    }
                    if let Some(d) = donor {
                        p.set_eq(c, p.eq_of(d));
                        p.set_trim(c, p.trim_of(d));
                        p.set_pan(c, f32::from_bits(p.pan[d].load(std::sync::atomic::Ordering::Relaxed)));
                        p.set_pfl(c, p.pfl_of(d));
                        p.set_solo(c, p.solo_of(d));
                        p.set_fx(c, p.fx_of(d));
                        for b in 0..crate::BUSES { p.set_send(c, b, p.send_of(d, b)); p.set_send_pre(c, b, p.send_pre_of(d, b)); }
                    }
                        *owner = Some(st.key.clone());
                    }
                }
            }
        }
        self.follow_handovers(&heard_before);
        if let Some(p) = &self.params {
            for (ch, app) in &self.apps {
                if eligible.iter().any(|st| st.app.eq_ignore_ascii_case(app))
                    && eligible.iter().filter(|st| st.app.eq_ignore_ascii_case(app)).all(|st| self.stream_lanes.assigned.contains_key(&st.key))
                    && let Some(c) = p.channel(ch)
                { p.set_solo(c, false); }
            }
        }
        if before != self.stream_lanes.assigned || heard_before != self.stream_lanes.heard
            || touched_before != self.stream_lanes.touched
        {
            self.save_stream_lanes();
        }
    }

    fn save_stream_lanes(&self) {
        let path = patch_core::ipc::runtime_dir().join("stream-lanes.json");
        if let Ok(bytes) = serde_json::to_vec(&self.stream_lanes) {
            let temp = path.with_extension("json.new");
            if std::fs::write(&temp, bytes).is_ok() { let _ = std::fs::rename(temp, path); }
        }
    }

    /// Before a handover: which lanes were heard or adjusted since the last
    /// graph pass, so the successor shows the same strips.
    pub fn hand_over_stream_lanes(&mut self) {
        if let Some(p) = &self.params {
            for (key, channel) in &self.stream_lanes.assigned {
                let Some(c) = p.channel(channel) else { continue };
                if p.stream_has_signal(c) { self.stream_lanes.heard.insert(key.clone()); }
                if p.stream_touched(c) { self.stream_lanes.touched.insert(key.clone()); }
            }
        }
        self.save_stream_lanes();
    }

    /// Headphones on `sink` (None: off). Remembered across restarts.
    pub fn set_cue(&mut self, sink: Option<String>) {
        save_cue_target(sink.as_deref());
        self.cue_target = sink;
    }

    pub fn professional(&self) -> bool {
        std::fs::read_to_string(selection_file()).is_ok_and(|s| s.trim() == BUS)
    }

    /// The Mixer's AUX 1 and 2 while Patch is the output in the bar: each a
    /// virtual source (recordable, streamable), plus the physical ports
    /// assigned to it. Otherwise neither exists. Never a port Main or Cue
    /// plays on.
    fn aux_links(
        &mut self,
        professional: bool,
        have: &std::collections::HashSet<(String, String)>,
        main: &[(String, String)],
        cue: &[(String, String)],
    ) {
        if self.aux_sources.is_none() {
            let sources = run(&["pactl", "list", "short", "sources"]).unwrap_or_default();
            self.aux_sources = Some(AUX.iter().all(|a| sources.lines().any(|l| l.split('\t').nth(1) == Some(*a))));
        }
        if professional && self.aux_sources != Some(true) {
            for source in AUX {
                let _ = run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    "media.class=Audio/Source/Virtual",
                    &format!("sink_name={source}"),
                    &patch_core::pulse::sink_properties(&description(source), true, false),
                    "format=float32le",
                    "channels=2",
                    "channel_map=front-left,front-right",
                ]);
            }
            self.aux_sources = Some(true);
        } else if !professional && self.aux_sources == Some(true) {
            for source in AUX {
                for m in modules_with(&format!("sink_name={source}")) {
                    let _ = run(&["pactl", "unload-module", &m]);
                }
            }
            self.aux_sources = Some(false);
        }
        let mut wanted: Vec<(String, String)> = vec![];
        if professional {
            for (i, source) in AUX.iter().enumerate() {
                let bus = format!("aux{}", i + 1);
                for side in ["FL", "FR"] {
                    wanted.push((format!("{ENGINE}:{bus}_{side}"), format!("{source}:input_{side}")));
                }
                if let Some(a) = self.output_routing.aux(&bus)
                    && crate::output_routes::validate(&a.ports, &input_ports(&a.device)).is_ok()
                {
                    wanted.extend(
                        crate::output_routes::links(&a.ports, &bus)
                            .into_iter()
                            .filter(|(_, to)| !main.iter().chain(cue).any(|(_, p)| p == to)),
                    );
                }
            }
        }
        for (from, to) in &wanted {
            if !have.contains(&(from.clone(), to.clone())) {
                let _ = run(&["pw-link", from, to]);
            }
        }
        for (from, to) in have {
            if (from.starts_with(&format!("{ENGINE}:aux1_")) || from.starts_with(&format!("{ENGINE}:aux2_")))
                && !wanted.contains(&(from.clone(), to.clone()))
            {
                let _ = run(&["pw-link", "-d", from, to]);
            }
        }
    }

    /// Where AUX 1 and 2 play, for status: {"aux1": {device, ports} | null, …}.
    /// `"silent": true` when the route shares Main's ports: its links are
    /// skipped (no doubled signal), so it plays nothing.
    pub fn aux_routes(&self) -> serde_json::Value {
        let main = self.main_ports().unwrap_or_default();
        let route = |r: &Option<_>| {
            let mut v = serde_json::to_value(r).unwrap_or(serde_json::Value::Null);
            let shared = v["ports"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p.as_str().is_some_and(|p| main.iter().any(|m| m == p))));
            if shared {
                v["silent"] = true.into();
            }
            v
        };
        serde_json::json!({
            "aux1": route(&self.output_routing.aux1),
            "aux2": route(&self.output_routing.aux2),
        })
    }

    /// AUX 1 or 2 back to its virtual source only (no physical ports).
    pub fn clear_aux(&mut self, bus: &str) -> Result<(), String> {
        let slot = self.output_routing.aux_mut(bus).ok_or("unknown output bus")?;
        *slot = None;
        self.output_routing.save()?;
        self.tick();
        Ok(())
    }

    pub fn main_ports(&self) -> Option<Vec<String>> {
        self.professional()
            .then(|| {
                self.target
                    .as_ref()
                    .and_then(|d| self.output_routing.main.get(d).cloned())
            })
            .flatten()
    }

    /// Assign the Main or existing independent Cue bus to actual device ports.
    pub fn route_ports(&mut self, bus: &str, sink: &str, ports: Vec<String>) -> Result<(), String> {
        if !self.professional() {
            return Err("select Patch in the bar before assigning individual ports".into());
        }
        validate_output(sink, &sinks())?;
        if sink == BUS {
            return Err("choose physical output ports".into());
        }
        crate::output_routes::validate(&ports, &input_ports(sink))?;
        let previous = (
            self.output_routing.clone(),
            self.explicit.clone(),
            self.cue_target.clone(),
        );
        match bus {
            "main" => {
                if self
                    .output_routing
                    .cue
                    .as_ref()
                    .is_some_and(|c| c.ports.iter().any(|p| ports.contains(p)))
                {
                    return Err("Main and Cue must use different physical ports".into());
                }
                self.output_routing.main.insert(sink.into(), ports.clone());
                self.explicit = Some(sink.into());
            }
            "cue" => {
                if ports.len() != 2 {
                    return Err("Cue needs a stereo pair; Main also supports mono".into());
                }
                let main = self
                    .main_ports()
                    .map(|p| crate::output_routes::links(&p, "out"))
                    .unwrap_or_else(|| {
                        self.target
                            .as_ref()
                            .map(|d| program_ports(&input_ports(d), d).0)
                            .unwrap_or_default()
                    });
                if main.iter().any(|(_, p)| ports.contains(p)) {
                    return Err("Main and Cue must use different physical ports".into());
                }
                self.output_routing.cue = Some(crate::output_routes::Assignment {
                    device: sink.into(),
                    ports: ports.clone(),
                });
                self.cue_target = Some(sink.into());
            }
            "aux1" | "aux2" => {
                // Its own ports: none of Main's, Cue's or the other AUX's.
                let main = self
                    .main_ports()
                    .map(|p| crate::output_routes::links(&p, "out"))
                    .unwrap_or_default();
                let other = if bus == "aux1" { "aux2" } else { "aux1" };
                let taken = main.iter().any(|(_, p)| ports.contains(p))
                    || self.output_routing.cue.as_ref().is_some_and(|c| c.ports.iter().any(|p| ports.contains(p)))
                    || self.output_routing.aux(other).is_some_and(|a| a.ports.iter().any(|p| ports.contains(p)));
                if taken {
                    return Err("AUX outputs need ports of their own (not Main's, Cue's or the other AUX's)".into());
                }
                if let Some(slot) = self.output_routing.aux_mut(bus) {
                    *slot = Some(crate::output_routes::Assignment { device: sink.into(), ports: ports.clone() });
                }
            }
            _ => return Err("unknown output bus".into()),
        }
        self.tick();
        let wanted = crate::output_routes::links(&ports, match bus {
            "main" => "out",
            "cue" => "cue",
            other => other,
        });
        let connected = links();
        if wanted.iter().any(|link| !connected.contains(link)) {
            (self.output_routing, self.explicit, self.cue_target) = previous;
            self.tick();
            return Err("could not connect the selected physical ports".into());
        }
        if let Err(error) = self.output_routing.save() {
            (self.output_routing, self.explicit, self.cue_target) = previous;
            self.tick();
            return Err(error);
        }
        if bus == "cue" {
            save_cue_target(Some(sink));
        }
        Ok(())
    }

    /// The output chosen in Patch (None: following the system default).
    pub fn explicit(&self) -> Option<String> {
        self.explicit.clone()
    }

    /// Send the program to `sink` (None: follow the system default).
    pub fn set_output(&mut self, sink: Option<String>) -> Result<(), String> {
        if let Some(name) = sink.as_deref() {
            if name.starts_with(BUS) {
                return Err("choose a hardware destination, not a Patch input bus".into());
            }
            validate_output(name, &sinks())?;
        }
        self.explicit = sink;
        Ok(())
    }

    /// Select a destination without changing the bus default or moving apps.
    /// "patch" exposes professional routing, retaining the current hardware.
    pub fn select_output(&mut self, sink: &str) -> Result<(), String> {
        if !self.setup.get().system_audio {
            // The bar uses this command in both modes. With system audio off,
            // Patch does not own the default sink, so selecting an output
            // should perform the normal system output switch. The next tick
            // follows that default and moves Patch's program with it.
            if sink == BUS {
                return Err("Patch is not the system output while system audio is off".into());
            }
            validate_output(sink, &sinks())?;
            run(&["pactl", "set-default-sink", sink])
                .ok_or_else(|| format!("could not select output '{sink}'"))?;
            self.explicit = None;
            std::fs::write(selection_file(), sink).map_err(|e| e.to_string())?;
            save_selection(sink);
            self.tick();
            return Ok(());
        }
        let all = sinks();
        validate_output(sink, &all)?;
        if sink != BUS && input_ports(sink).is_empty() {
            return Err(format!("'{sink}' has no playback ports"));
        }
        let previous = (self.last_default.clone(), self.explicit.clone());
        let previous_selection = std::fs::read(selection_file()).ok();
        std::fs::write(selection_file(), sink).map_err(|e| e.to_string())?;
        if sink != BUS {
            self.last_default = Some(sink.to_string());
            self.explicit = Some(sink.to_string());
        }
        self.tick();
        if sink != BUS && self.target.as_deref() != Some(sink) {
            (self.last_default, self.explicit) = previous;
            if let Some(selection) = previous_selection {
                let _ = std::fs::write(selection_file(), selection);
            } else {
                let _ = std::fs::remove_file(selection_file());
            }
            self.tick();
            return Err(format!("could not link mixer output to '{sink}'"));
        }
        std::fs::write(selection_file(), sink).map_err(|e| e.to_string())?;
        save_selection(sink);
        Ok(())
    }

    /// One pass: create missing sinks, follow the default output, add
    /// missing links and remove stale ones. Cheap enough for every 500 ms.
    /// Still connecting (outputs wait for the inputs; not on an output
    /// yet): the keeper checks often until done.
    pub fn settling(&self) -> bool {
        !self.active
            || self.observe_streams_until.is_some_and(|end| std::time::Instant::now() < end)
            || self.recent_swaps.iter().any(|(_, _, at)| at.elapsed().as_secs() < 6)
            || self
                .outputs_after
                .is_some_and(|t| std::time::Instant::now() < t)
    }

    pub fn tick(&mut self) {
        // No listing, no pass: read as "no sinks", it loaded a second
        // `patch` sink (apps on it went unheard) and forgot the chosen
        // output. Two quick passes try again (as after an output switch).
        let Some(mut all) = listed_sinks() else {
            self.recheck = self.recheck.max(2);
            return;
        };
        // A second `patch` (or System sounds) sink from such a pass before
        // this fix: apps on it were unheard. The first one stays.
        if drop_duplicate_sinks(&all) {
            self.recheck = self.recheck.max(2);
            return;
        }
        // Retained Pulse modules need a one-time label/property migration.
        // Wait for handover_complete so the retiring engine is finished.
        let now = std::time::Instant::now();
        if !self.device_properties_checked
            && self.streams_enabled
            && !self.drive
            && self.outputs_after.is_none_or(|t| now >= t)
            && self.device_properties_retry.is_none_or(|t| now >= t)
        {
            match crate::device_properties::repair(&mut run) {
                Ok(Some(changed)) => {
                    self.device_properties_checked = true;
                    if changed {
                        // A recreated bus defaults to applying its volume to
                        // its monitor. Reclaim it immediately when the engine
                        // applies that volume at its output, or apps are
                        // attenuated twice until a later volume event.
                        if let Some(p) = &self.params
                            && p.system_post.load(std::sync::atomic::Ordering::Relaxed)
                            && !set_monitor_volumes(false)
                        {
                            // Fall back to the bus's volume, applied once.
                            p.system_post.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        // The saved sink IDs are stale after module reloads.
                        self.recheck = self.recheck.max(2);
                        return;
                    }
                }
                Ok(None) => {
                    self.device_properties_retry = Some(now + std::time::Duration::from_secs(5));
                }
                Err(e) => {
                    eprintln!("patch device properties: {e}");
                    self.device_properties_retry = Some(now + std::time::Duration::from_secs(30));
                }
            }
        }
        let default = run(&["pactl", "get-default-sink"]).map(|s| s.trim().to_string());
        let capture_active = route_captures(
            patch_core::ipc::runtime_dir()
                .join("input-selection")
                .exists(),
        );
        self.capture_active = capture_active;
        let changed = default
            .clone()
            .filter(|d| !d.starts_with(BUS))
            .filter(|d| self.last_default.as_deref() != Some(d.as_str()));
        let first = !self.default_follow.started();
        // With system audio Patch owns the default ("patch"): another one is
        // a flicker until it holds. Without, the default is the switch.
        let wait = if self.setup.get().system_audio { DefaultFollow::SETTLE } else { std::time::Duration::ZERO };
        let follow = self.default_follow.settled(changed.as_deref(), std::time::Instant::now(), wait);
        if let Some(d) = changed.filter(|_| follow) {
            if !first {
                self.explicit = None; // the system bar switched output
            }
            self.last_default = Some(d.clone());
            // The device the session starts on is no choice: a saved
            // "patch" (the full console) stays selected.
            if !(first && self.professional()) {
                let _ = std::fs::write(selection_file(), &d);
            }
            // The output device behind Patch, remembered across restarts (in
            // system audio the default output is Patch itself).
            if std::fs::read_to_string(output_file())
                .ok()
                .as_deref()
                .map(str::trim)
                != Some(d.as_str())
            {
                let _ = std::fs::write(output_file(), &d);
            }
        }
        // A device that is gone cannot hold the program. A session without Patch
        // moves to the next output by PipeWire's own ranking and Patch does the
        // same: the sound always plays somewhere, and a gapless handover - whose
        // successor links this same device - can always link instead of failing
        // on a name that no longer exists. What is remembered stays: it is where
        // the sound returns to when the device is back.
        let live = |n: &str| all.iter().any(|(s, _)| s == n);
        if self.explicit.as_deref().is_some_and(|e| !live(e)) {
            self.explicit = None;
        }
        if self.last_default.as_deref().is_some_and(|d| !live(d)) {
            self.last_default = None;
        }
        if self.last_default.is_none() {
            self.last_default = std::fs::read_to_string(output_file())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| live(s))
                .or_else(best_output);
        }
        // One stream listing serves both passes below that move streams
        // (`app_streams` is two fork+execs); skipped when neither can act.
        let streams = if self.setup.get().system_audio
            || self.owns_default
            || !self.apps.is_empty()
            || !self.stream_lanes.assigned.is_empty()
            || !self.deck_lines.is_empty()
        {
            app_streams()
        } else {
            Vec::new()
        };
        // A silent handover successor must use the retiring engine's
        // existing inputs until the crossfade is complete. Moving live
        // browser streams away earlier would cut the retiring mix's input.
        if self.streams_enabled { self.reconcile_streams(&streams, &mut all); }
        let streams_moved = self.system_audio(&all, default.as_deref(), &streams);
        let target = self.explicit.clone().or_else(|| self.last_default.clone());
        let rate = target
            .as_ref()
            .and_then(|t| all.iter().find(|(n, _)| n == t))
            .map(|(_, r)| *r)
            .unwrap_or(48_000);

        // The engine owns the decks' soundcards. The bus belongs to the
        // Router: Patch creates it when a stream is routed through it and
        // removes it on restore; the engine only listens to it.
        let graph_rate = run(&["pw-metadata", "-n", "settings", "0", "clock.rate"])
            .and_then(|s| {
                s.split("value:'")
                    .nth(1)
                    .and_then(|v| v.split('\'').next())
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(rate);
        // Both listings once per pass: the loop reads the same inventories
        // for either source, and each is one fork+exec less (~5 ms) per
        // pass. Sources and modules this pass cannot change under it: the
        // replacements below only touch each source's own entries.
        let sources = run(&["pactl", "list", "short", "sources"]).unwrap_or_default();
        let modules = run(&["pactl", "list", "short", "modules"]).unwrap_or_default();
        for source in [PROGRAM, INPUT] {
            // Program: a virtual source (an older engine made it a sink: replace).
            // …or one made before it was passive: replace it once. Both
            // checks read the same listings for both sources, so the two
            // fork+execs happen once per pass, not once per source.
            let passive = modules
                .lines()
                .any(|l| l.contains(&format!("sink_name={source}")) && l.contains("node.passive"));
            let program_is_source = passive
                && sources
                    .lines()
                    .any(|l| l.split('\t').nth(1) == Some(source));
            if !program_is_source {
                if all.iter().any(|(n, _)| n == source)
                    || sources
                        .lines()
                        .any(|l| l.split('\t').nth(1) == Some(source))
                {
                    for m in modules_with(&format!("sink_name={source}")) {
                        let _ = run(&["pactl", "unload-module", &m]);
                    }
                }
                let _ = run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    "media.class=Audio/Source/Virtual",
                    &format!("sink_name={source}"),
                    &patch_core::pulse::sink_properties(&description(source), true, false),
                    "format=float32le",
                    &format!("rate={graph_rate}"),
                    "channels=2",
                    "channel_map=front-left,front-right",
                ]);
            }
        }
        // Deck soundcards: kept if they exist (a handover), else only on request.
        if self.decks_wanted.is_none() {
            self.decks_wanted = Some(all.iter().any(|(n, _)| n.starts_with("patch.deck-")));
        }
        let decks = self.decks_wanted == Some(true);
        // App channels have a sink only while an app is assigned to them.
        let owned = self
            .channels
            .iter()
            .filter(|c| !c.starts_with("app-") || self.apps.contains_key(*c))
            .filter(|c| !c.starts_with("stream-") || self.stream_channel_used(c))
            .filter(|c| !c.starts_with("deck-") || decks || self.deck_lines.contains_key(*c))
            .filter_map(|c| source_sink(c))
            .filter(|s| s != BUS);
        for sink in owned {
            if !all.iter().any(|(n, _)| *n == sink) {
                let _ = run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    &format!("sink_name={sink}"),
                    &patch_core::pulse::sink_properties(&description(&sink), false, false),
                    "format=float32le",
                    &format!("rate={rate}"),
                    "channels=2",
                    "channel_map=front-left,front-right",
                ]);
            }
        }

        // Released deck soundcards go.
        if self.decks_wanted == Some(false) {
            for (name, _) in all.iter().filter(|(n, _)| n.starts_with("patch.deck-")
                && !self.deck_lines.contains_key(&n["patch.".len()..])) {
                for m in modules_with(&format!("sink_name={name} ")) {
                    let _ = run(&["pactl", "unload-module", &m]);
                }
            }
        }
        // Unassigned app sinks go (their streams were moved back first).
        for (name, _) in &all {
            if let Some(ch) = name
                .strip_prefix("patch.")
                .filter(|c| c.starts_with("app-") || c.starts_with("stream-"))
                && !self.apps.contains_key(ch) && !self.stream_channel_used(ch)
            {
                for m in modules_with(&format!("sink_name={name} ")) {
                    let _ = run(&["pactl", "unload-module", &m]);
                }
            }
        }
        // Assigned apps' streams play into their own channel (again after
        // the app restarts, or when something else moved them). The
        // system-audio pass already enforced every destination from this
        // same listing; re-moving its streams would only repeat its moves.
        if !streams_moved {
            for st in &streams {
                let target = self.stream_sink(st).or_else(|| self.apps.iter()
                    .find(|(_, app)| is_app(st, app)).map(|(ch, _)| format!("patch.{ch}")));
                if let Some(sink) = target && st.sink != sink && all.iter().any(|(n, _)| *n == sink)
                    && run(&["pactl", "move-sink-input", &st.index.to_string(), &sink]).is_some()
                { self.recheck = self.recheck.max(2); self.normalize_native(st, &sink); }
            }
        }
        for st in &streams {
            if let Some(target) = self.stream_sink(st) && st.sink == target { self.normalize_native(st, &target); }
        }
        let have = links();
        // A link is passive (it doesn't keep the graph running) when it's
        // made while the engine node is passive (PipeWire ≥ 1.2: node.passive
        // at link time; there's no per-link setting). An app playing into
        // Patch runs the graph by its own stream either way.
        let link = |from: &str, to: &str| {
            if !have.contains(&(from.to_string(), to.to_string())) {
                let _ = run(&["pw-link", from, to]);
            }
        };
        // Outputs: made again when the engine changes between driving the
        // graph (a deck plays: active, they wake the device) and not.
        let relink = self.out_mode != Some(self.drive);
        if relink {
            for (from, to) in &have {
                if (from.starts_with(&format!("{ENGINE}:out_"))
                    || from.starts_with(&format!("{ENGINE}:cue_")))
                    && !to.starts_with(&format!("{PROGRAM}:"))
                {
                    let _ = run(&["pw-link", "-d", from, to]);
                }
            }
            self.out_mode = Some(self.drive);
        }
        let drive = self.drive;
        let have_out = if relink { links() } else { have.clone() };
        let _ = drive;
        let link_out = |from: &str, to: &str| {
            if !have_out.contains(&(from.to_string(), to.to_string())) {
                run(&["pw-link", from, to]).is_some()
            } else {
                true
            }
        };
        // Lanes a mic lane takes as a remote voice: they play through the
        // mic's voice chain instead of their own strip, never both.
        let remote: std::collections::BTreeMap<String, String> = self
            .mics
            .iter()
            .filter_map(|(mic, src)| {
                remote_lane(src, &self.stream_lanes, &self.apps).map(|lane| (lane, mic.clone()))
            })
            .collect();
        for c in &self.channels {
            // Only soundcards that exist (unassigned app channels and the
            // bus have none until they are used).
            if let Some(sink) = source_sink(c).filter(|k| all.iter().any(|(n, _)| n == k)) {
                for side in ["FL", "FR"] {
                    let (from, to) = (format!("{sink}:monitor_{side}"), format!("{ENGINE}:in_{c}_{side}"));
                    if remote.contains_key(c) {
                        if have.contains(&(from.clone(), to.clone())) {
                            let _ = run(&["pw-link", "-d", &from, &to]);
                        }
                    } else {
                        link(&from, &to);
                    }
                }
            }
        }
        // Microphones: their source into their channel; a mic channel
        // without one has nothing linked (removed: the mic closes).
        for c in self.channels.iter().filter(|c| c.starts_with("mic-")) {
            let monitor = self.params.as_ref().is_none_or(|p| {
                p.channel(c)
                    .is_some_and(|i| p.input_monitor[i].load(std::sync::atomic::Ordering::Relaxed))
            });
            let capture = capture_active
                && self
                    .params
                    .as_ref()
                    .is_none_or(|p| !p.input_main.load(std::sync::atomic::Ordering::Relaxed));
            let want = match self.mics.get(c) {
                // A remote voice: its lane's soundcard, always (no device
                // opens; the voice was playing before it came here).
                Some(src) if patch_core::mic::remote_source(src) => {
                    remote_lane(src, &self.stream_lanes, &self.apps)
                        .and_then(|lane| source_sink(&lane))
                        .filter(|sink| all.iter().any(|(n, _)| n == sink))
                        .map(|sink| remote_links(&sink, c))
                        .unwrap_or_default()
                }
                Some(src) if monitor || capture => mic_links(&output_ports(src), c),
                _ => vec![],
            };
            let into = format!("{ENGINE}:in_{c}_");
            for (from, to) in &have {
                if to.starts_with(&into) && !want.iter().any(|(f, t)| f == from && t == to) {
                    let _ = run(&["pw-link", "-d", from, to]);
                }
            }
            for (from, to) in &want {
                link(from, to);
            }
        }
        if self
            .outputs_after
            .is_some_and(|t| std::time::Instant::now() < t)
        {
            return; // inputs first; outputs on a later pass
        }
        // The program source is passive itself (made so): a virtual
        // source, a clock driver, it kept the graph and the sound card
        // running in silence. A recorder's own stream runs the graph.
        // Fed from the engine's recording pair, before the system volume
        // (the speakers' `out_` pair has it): a recording never follows the
        // volume or mute of what you listen on.
        for side in ["FL", "FR"] {
            let out = format!("{ENGINE}:out_{side}");
            let to = format!("{PROGRAM}:input_{side}");
            if have.contains(&(out.clone(), to.clone())) {
                let _ = run(&["pw-link", "-d", &out, &to]);
            }
            link(&format!("{ENGINE}:program_{side}"), &to);
        }
        for side in ["FL", "FR"] {
            link(
                &format!("{ENGINE}:capture_{side}"),
                &format!("{INPUT}:input_{side}"),
            );
        }
        // Explicit port assignments permit independent Main and Cue pairs
        // on the same device. Overlapping destinations remain forbidden.
        let professional = self.professional();
        let (wanted, psd) = target
            .as_ref()
            .map(|d| {
                if professional && let Some(ports) = self.output_routing.main.get(d) {
                    return (
                        if crate::output_routes::validate(ports, &input_ports(d)).is_ok() {
                            crate::output_routes::links(ports, "out")
                        } else {
                            vec![]
                        },
                        0,
                    );
                }
                program_ports(&input_ports(d), d)
            })
            .unwrap_or_default();
        let cue_links = if professional {
            if let Some(assignment) = self
                .output_routing
                .cue
                .as_ref()
                .filter(|a| Some(&a.device) == self.cue_target.as_ref())
            {
                if crate::output_routes::validate(
                    &assignment.ports,
                    &input_ports(&assignment.device),
                )
                .is_ok()
                {
                    crate::output_routes::links(&assignment.ports, "cue")
                } else {
                    vec![]
                }
            } else {
                self.cue_target
                    .as_ref()
                    .filter(|d| Some(*d) != target.as_ref())
                    .map(|d| {
                        let ports = input_ports(d);
                        let program = program_ports(&ports, d).0;
                        let selected: Vec<_> =
                            program.into_iter().take(2).map(|(_, to)| to).collect();
                        crate::output_routes::links(&selected, "cue")
                    })
                    .unwrap_or_default()
            }
        } else {
            vec![]
        };
        let cue_links: Vec<_> = cue_links
            .into_iter()
            .filter(|(_, to)| !wanted.iter().any(|(_, p)| p == to))
            .collect();
        for (from, to) in &cue_links {
            link_out(from, to);
        }
        for (from, to) in &have {
            if from.starts_with(&format!("{ENGINE}:cue_"))
                && !cue_links.contains(&(from.clone(), to.clone()))
            {
                let _ = run(&["pw-link", "-d", from, to]);
            }
        }
        self.aux_links(professional, &have, &wanted, &cue_links);
        let Some(target) = target else { return };
        if wanted.is_empty() && professional && self.output_routing.main.contains_key(&target) {
            for (from, to) in &have {
                if from.starts_with(&format!("{ENGINE}:out_"))
                    && !to.starts_with(&format!("{PROGRAM}:"))
                {
                    let _ = run(&["pw-link", "-d", from, to]);
                }
            }
            self.active = false;
        }
        if !wanted.is_empty() {
            // System audio carries the volume on the bus and applies it once, as
            // the program leaves for the device; the device under it plays at
            // full scale (see the takeover in `system_audio`). A device Patch
            // starts using *later* - the bar picked it, or the remembered one was
            // gone - is set the same way here. Otherwise its own volume multiplies
            // with the system one and the level can only be lowered, never raised,
            // with the control the user has: measured, a -33 dBFS tone through a
            // 20% device under an 8% bus was inaudible at the speakers and no
            // volume press helped. Only once the takeover has given the bus the
            // device's volume (`owns_default`): on the pass that makes the bus
            // it is still at 100 %, and the device went to full scale before
            // its volume was copied - the user's 35 % became 100 % on install.
            if self.setup.get().system_audio
                && self.owns_default
                && self.target.as_deref() != Some(target.as_str())
                && engage_system_volume(self.params.as_ref())
            {
                full_volume(&target);
            }
            self.psd.store(psd, std::sync::atomic::Ordering::Relaxed);
            // Keep the old destination if any new link fails. Never cut the
            // working path merely because a device was listed but vanished.
            let connected = wanted
                .iter()
                .fold(true, |ok, (from, to)| link_out(from, to) && ok);
            if !connected {
                // Roll back only links introduced by this attempt. A partial
                // stereo connection must not remain beside the old output.
                for (from, to) in &wanted {
                    if !have_out.contains(&(from.clone(), to.clone())) {
                        let _ = run(&["pw-link", "-d", from, to]);
                    }
                }
                return;
            }
            // Unlink the program from any other output.
            for (from, to) in &have {
                let ours = wanted.iter().any(|(f, t)| f == from && t == to);
                if from.starts_with(&format!("{ENGINE}:out_"))
                    && !ours
                    && !to.starts_with(&format!("{PROGRAM}:"))
                {
                    let _ = run(&["pw-link", "-d", from, to]);
                }
            }
            // Engine is on the output now: a loopback would double the bus.
            for m in loopback_modules() {
                let _ = run(&["pactl", "unload-module", &m]);
            }
            self.active = true;
            let _ = std::fs::write(output_file(), &target);
            self.target = Some(target);
        }
    }

    /// System audio: the Patch bus is the default output and every app
    /// plays into it. The bar changes the engine's destination directly;
    /// external changes to the default are also followed, then repaired so
    /// the default and the apps return to the bus. Switched off: the device
    /// is the default again, with the apps on it. `streams` is the pass's
    /// one stream listing (two fork+execs). True when this pass moved every
    /// stream to its destination: the caller need not do it again.
    fn system_audio(
        &mut self,
        all: &[(String, u32)],
        default: Option<&str>,
        streams: &[AppStream],
    ) -> bool {
        let on = self.setup.get().system_audio;
        let bus = all.iter().any(|(n, _)| n == BUS);
        if on {
            if !bus {
                let _ = run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    &format!("sink_name={BUS}"),
                    // The shell needs device.class=sound to expose volume controls.
                    &patch_core::pulse::sink_properties(&description(BUS), false, true),
                    "format=float32le",
                    "channels=2",
                    "channel_map=front-left,front-right",
                ]);
                return false; // linked on the next pass
            }
            if let Some(hw) = default.filter(|d| *d != BUS) {
                // Taking over from the device: its volume becomes the
                // system volume (the bus's, which the engine follows), the
                // device plays at full scale under it: the same loudness,
                // and the bar shows the same level. An output picked in the
                // bar later goes to full scale the same way. Never louder,
                // though: a device Patch already drives is at full scale on
                // purpose (see `copy_volume_quieter`).
                if !self.owns_default {
                    copy_volume_quieter(hw, BUS);
                }
                if engage_system_volume(self.params.as_ref()) {
                    full_volume(hw);
                }
                let _ = run(&["pactl", "set-default-sink", BUS]);
                self.recheck = 2;
            }
            self.owns_default = true;
            // Apps on a device go to the bus (the decks, app channels and
            // Patch's own streams keep their soundcards).
            // The mixer consolidates event streams on this sink. Pulling
            // them back to the bus fights its graph watcher and repeatedly
            // moves a short notification between two playback paths.
            for st in streams.iter().filter(|st| st.sink != "patch.system-sounds") {
                let target = self.stream_sink(st).unwrap_or_else(|| stream_destination(&st.app, &self.apps));
                if st.sink != target && all.iter().any(|(n, _)| *n == target)
                    && run(&["pactl", "move-sink-input", &st.index.to_string(), &target]).is_some()
                { self.recheck = self.recheck.max(2); self.normalize_native(st, &target); }
            }
            true
        } else {
            if self.owns_default {
                self.owns_default = false;
                if default == Some(BUS)
                    && let Some(hw) = self.last_default.clone()
                {
                    // The system volume goes back to the device.
                    copy_volume(BUS, &hw);
                    full_volume(BUS);
                    let _ = run(&["pactl", "set-default-sink", &hw]);
                    for st in streams.iter().filter(|s| s.sink == BUS) {
                        let _ = run(&["pactl", "move-sink-input", &st.index.to_string(), &hw]);
                    }
                }
                // The device carries the volume again: the engine back to
                // unity, now (also when it engaged the volume itself and the
                // volume thread never claimed it: off within its 2 s check).
                if let Some(p) = &self.params {
                    p.system_post.store(false, std::sync::atomic::Ordering::Relaxed);
                    p.set_system(1.0, 1.0, false);
                }
            }
            false
        }
    }

    /// Hand the bus back to a plain loopback so audio keeps playing without
    /// the engine.
    pub fn restore_loopback(&self) {
        // The loopback plays the monitor straight to the device: it must
        // carry the system volume again first (never full volume).
        set_monitor_volumes(true);
        let Some(target) = self.target.clone().or_else(|| self.last_default.clone()) else {
            return;
        };
        if loopback_modules().is_empty() {
            let _ = run(&[
                "pactl",
                "load-module",
                "module-loopback",
                &format!("source={BUS}.monitor"),
                &format!("sink={target}"),
                "latency_msec=20",
                // Passive, both sides: it runs when an app plays into the
                // bus, and an idle device suspends (active, it kept the
                // card running and cost 1.5 % CPU in silence).
                "sink_input_properties=node.name=patch.loopback media.name=Patch node.passive=true",
                "source_output_properties=node.passive=true",
            ]);
        }
    }
}

fn cue_file() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omarchy-patch").join("headphones")
}

fn load_cue_target() -> Option<String> {
    std::fs::read_to_string(cue_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_cue_target(sink: Option<&str>) {
    let f = cue_file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    let _ = std::fs::write(f, sink.unwrap_or(""));
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn sinks_parse_names_and_rates() {
        let out = "198\talsa_output.pci-0000_00_05.0.analog-stereo\tPipeWire\ts16le 2ch 44100Hz\tRUNNING\n\
                   220\tpatch\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n";
        assert_eq!(
            parse_sinks(out),
            vec![
                ("alsa_output.pci-0000_00_05.0.analog-stereo".to_string(), 44_100),
                ("patch".to_string(), 48_000),
            ]
        );
        assert!(parse_sinks("").is_empty());
    }

    #[test]
    fn an_unmovable_recorder_is_relinked_from_the_bus_monitor_to_the_program() {
        let have: HashSet<(String, String)> = [
            ("patch:monitor_FL", "gsr-default_output:input_FL"),
            ("patch:monitor_FR", "gsr-default_output:input_FR"),
            ("patch:monitor_FL", "patch.engine:in_bus_FL"),
            ("patch:monitor_FL", "Patch meter:input_FL"),
            ("alsa_input.mic:capture_FL", "gsr-default_output:input_FL"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            program_relinks("gsr-default_output", &have),
            vec![
                (
                    pair("patch:monitor_FL", "gsr-default_output:input_FL"),
                    pair("patch.program:capture_FL", "gsr-default_output:input_FL")
                ),
                (
                    pair("patch:monitor_FR", "gsr-default_output:input_FR"),
                    pair("patch.program:capture_FR", "gsr-default_output:input_FR")
                ),
            ]
        );
        assert!(program_relinks("Patch meter", &HashSet::new()).is_empty());
    }

    #[test]
    fn unnamed_audio_ports_and_aux_stereo_inputs_do_not_require_a_vendor_rule() {
        let ports = vec!["generic:audio_1".into(), "generic:audio_2".into()];
        assert_eq!(program_ports(&ports, "generic").0.len(), 2);
        assert_eq!(
            mic_links(&ports, "mic-1"),
            vec![
                (ports[0].clone(), format!("{ENGINE}:in_mic-1_FL")),
                (ports[1].clone(), format!("{ENGINE}:in_mic-1_FR"))
            ]
        );
    }

    #[test]
    fn aux_interfaces_use_only_the_first_actual_pair_in_numeric_order() {
        let ports = ["AUX10", "AUX1", "AUX0", "AUX2"].map(|p| format!("interface:playback_{p}"));
        let (links, psd) = program_ports(&ports, "interface");
        assert_eq!(psd, 0);
        assert_eq!(
            links,
            vec![
                (format!("{ENGINE}:out_FL"), "interface:playback_AUX0".into()),
                (format!("{ENGINE}:out_FR"), "interface:playback_AUX1".into())
            ]
        );
        let mono = ["interface:playback_AUX0".into()];
        assert_eq!(
            program_ports(&mono, "interface").0,
            vec![(format!("{ENGINE}:out_MONO"), mono[0].clone())]
        );
    }

    #[test]
    fn destinations_keep_player_and_apps_in_the_mixer_and_decks_in_the_booth() {
        let apps = std::collections::BTreeMap::from([("app-1".into(), "Browser".into())]);
        assert_eq!(stream_destination("Patch Player", &apps), BUS);
        assert_eq!(stream_destination("browser", &apps), "patch.app-1");
        assert_eq!(stream_destination("Music", &apps), BUS);
        for deck in ["A", "B", "C", "D"] {
            assert_eq!(
                stream_destination(&format!("Patch Deck {deck}"), &apps),
                format!("patch.deck-{}", deck.to_lowercase())
            );
        }
    }

    #[test]
    fn output_choices_cannot_feed_the_mix_back_into_its_inputs() {
        let sinks = vec![
            ("alsa_output.test".into(), 48000),
            ("patch.app-1".into(), 48000),
        ];
        assert!(validate_output(BUS, &sinks).is_ok());
        assert!(validate_output("alsa_output.test", &sinks).is_ok());
        assert!(validate_output("missing", &sinks).is_err());
        assert!(validate_output("patch.app-1", &sinks).is_err());
        assert!(validate_output(PROGRAM, &sinks).is_err());
    }

    #[test]
    fn sixteen_mono_ports_link_independently_to_sixteen_input_channels() {
        for i in 1..=16 {
            let port = format!("card:capture_AUX{}", i - 1);
            assert_eq!(
                mic_links(std::slice::from_ref(&port), &format!("mic-{i}")),
                vec![
                    (port.clone(), format!("{ENGINE}:in_mic-{i}_FL")),
                    (port, format!("{ENGINE}:in_mic-{i}_FR"))
                ]
            );
        }
    }

    #[test]
    fn reads_pactl_volumes_and_sides() {
        let two = "Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 32768 /  50% / -18.06 dB\n        balance 0.00\n";
        assert_eq!(parse_volume(two), vec![65536, 32768]);
        // A MacBook's speakers: six channels.
        let six = "Volume: front-left: 45000 /  69% / -9.80 dB,   front-right: 45000 /  69% / -9.80 dB,   side-left: 45000 /  69% / -9.80 dB,   side-right: 45000 /  69% / -9.80 dB,   rear-left: 45000 /  69% / -9.80 dB,   rear-right: 45000 /  69% / -9.80 dB\n";
        assert_eq!(parse_volume(six).len(), 6);
        // A MacBook's six speakers: each from its own output, copies.
        let ports: Vec<String> = ["FL", "FR", "SL", "SR", "RL", "RR"]
            .iter()
            .map(|p| format!("mac:playback_{p}"))
            .collect();
        let (links, psd) = program_ports(&ports, "mac");
        assert_eq!(links.len(), 6);
        assert!(links.contains(&(format!("{ENGINE}:out_SL"), "mac:playback_SL".into())));
        assert_eq!(psd, 0);
        let mono = vec!["usb:playback_MONO".to_string()];
        assert_eq!(
            program_ports(&mono, "usb").0,
            vec![(
                format!("{ENGINE}:out_MONO"),
                "usb:playback_MONO".to_string()
            )]
        );
        // 5.1: centre and LFE from their outputs, the rears decoded.
        let av: Vec<String> = ["FL", "FR", "FC", "LFE", "RL", "RR"]
            .iter()
            .map(|p| format!("av:playback_{p}"))
            .collect();
        let (links, psd) = program_ports(&av, "av");
        assert!(links.contains(&(format!("{ENGINE}:out_LFE"), "av:playback_LFE".into())));
        assert_eq!(psd, crate::surround::REAR_PSD);
    }

    #[test]
    fn a_remote_voice_takes_its_lane_into_the_mic_lane() {
        let mut streams = crate::stream_lanes::Lanes::default();
        streams.assigned.insert("stream:1:2:3".to_string(), "stream-4".to_string());
        let apps = std::collections::BTreeMap::from([("app-1".to_string(), "Teams".to_string())]);
        assert_eq!(remote_lane("stream:1:2:3", &streams, &apps).as_deref(), Some("stream-4"));
        assert_eq!(remote_lane("app-1", &streams, &apps).as_deref(), Some("app-1"));
        // Ended stream, unassigned app channel: nothing to take.
        assert_eq!(remote_lane("stream:9:9:9", &streams, &apps), None);
        assert_eq!(remote_lane("app-2", &streams, &apps), None);
        assert_eq!(
            remote_links("patch.stream-4", "mic-3"),
            vec![
                ("patch.stream-4:monitor_FL".to_string(), format!("{ENGINE}:in_mic-3_FL")),
                ("patch.stream-4:monitor_FR".to_string(), format!("{ENGINE}:in_mic-3_FR")),
            ]
        );
    }

    #[test]
    fn aux_sources_chosen_by_an_app_stay_put() {
        // Chromium on Patch AUX 1 (a mic test, a call) stays on the send.
        assert!(chosen_source("patch.aux1"));
        assert!(chosen_source("patch.aux2"));
        assert!(chosen_source(PROGRAM));
        assert!(chosen_source("patch.monitor"));
        // A hardware microphone still goes onto the input mix.
        assert!(!chosen_source("alsa_input.usb-BLUE_BLUE_USB_Audio_2.0_0000-00.analog-stereo"));
    }

    #[test]
    fn a_picked_source_is_the_apps_choice() {
        use serde_json::json;
        // Chromium with Patch AUX 1 (or a hardware mic) chosen by name.
        assert!(picked_source(&json!({"target.object": "patch.aux1"})));
        assert!(picked_source(&json!({"node.target": "alsa_input.usb-x"})));
        // "Default": no target, so it follows the Patch input mix.
        assert!(!picked_source(&json!({"node.name": "Chromium input"})));
        assert!(!picked_source(&json!({"target.object": "@DEFAULT_SOURCE@"})));
        assert!(!picked_source(&json!({"target.object": "-1"})));
    }
}

#[cfg(test)]
mod default_follow_tests {
    use super::DefaultFollow;
    use std::time::{Duration, Instant};
    const W: Duration = DefaultFollow::SETTLE;

    #[test]
    fn the_first_look_is_followed_at_once() {
        let mut f = DefaultFollow::default();
        assert!(f.settled(Some("speakers"), Instant::now(), W));
    }

    #[test]
    fn a_flicker_back_to_patch_is_never_followed() {
        let mut f = DefaultFollow::default();
        let t = Instant::now();
        f.settled(None, t, W); // started, on Patch
        assert!(!f.settled(Some("hdmi"), t + Duration::from_millis(200), W));
        assert!(!f.settled(Some("scarlett"), t + Duration::from_millis(900), W));
        assert!(!f.settled(None, t + Duration::from_secs(2), W)); // "patch" again
        assert!(!f.settled(Some("scarlett"), t + Duration::from_secs(4), W), "the wait starts over");
    }

    #[test]
    fn without_system_audio_the_default_is_followed_at_once() {
        let mut f = DefaultFollow::default();
        let t = Instant::now();
        f.settled(None, t, W);
        assert!(f.settled(Some("headphones"), t, Duration::ZERO));
    }

    #[test]
    fn a_real_switch_lands_once_it_holds() {
        let mut f = DefaultFollow::default();
        let t = Instant::now();
        f.settled(None, t, W);
        assert!(!f.settled(Some("headphones"), t + Duration::from_secs(1), W));
        assert!(!f.settled(Some("headphones"), t + Duration::from_secs(3), W));
        assert!(f.settled(Some("headphones"), t + Duration::from_secs(4), W));
    }
}
