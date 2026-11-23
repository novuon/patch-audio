//! Microphones and inputs: saved mic setups, the capture sources on offer, choosing an input.

use super::*;

pub(super) fn monitor_default() -> bool {
    true
}

pub(super) fn send_default() -> bool {
    true
}

pub(super) fn mics_path() -> std::path::PathBuf {
    crate::session::state_dir().join("mics.json")
}

pub(super) fn load_mics() -> std::collections::BTreeMap<String, MicSetup> {
    std::fs::read_to_string(mics_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub(super) fn save_mics(map: &std::collections::BTreeMap<String, MicSetup>) {
    if let Ok(t) = serde_json::to_string_pretty(map) {
        let _ = std::fs::create_dir_all(crate::session::state_dir());
        let _ = std::fs::write(mics_path(), t);
    }
}

fn pulse_objects(kind: &str) -> Vec<Value> {
    crate::links::run(&["pactl", "-f", "json", "list", kind])
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Capture sources to choose from (no monitors): (node name, description,
/// channels).
pub(super) fn capture_sources() -> Vec<Value> {
    capture_choices(&pulse_objects("sources"))
}

fn capture_choices(sources: &[Value]) -> Vec<Value> {
    let ports = crate::links::run(&["pw-link", "-o"]).unwrap_or_default();
    sources
        .iter()
        .filter(|v| v["monitor_of_sink"].is_null() || v["monitor_of_sink"].as_str() == Some("n/a"))
        .filter(|v| !v["name"].as_str().unwrap_or("").ends_with(".monitor"))
        .flat_map(|v| {
            let name = v["name"].as_str().unwrap_or("");
            // Not Patch's own sources (its program, the latency probe).
            if name.is_empty() || name.starts_with("patch") {
                return vec![];
            }
            let description = v["description"].as_str().unwrap_or(name);
            let channels = v["channel_map"]
                .as_str()
                .map(|m| m.split(',').count())
                .unwrap_or(0);
            let inputs: Vec<_> = ports
                .lines()
                .map(str::trim)
                .filter(|p| p.strip_prefix(name).is_some_and(|r| r.starts_with(':')))
                .collect();
            let mut choices = vec![];
            if channels <= 2 || inputs.is_empty() {
                choices
                    .push(json!({"name": name, "description": description, "channels": channels}));
            }
            if channels >= 2 {
                choices.extend(inputs.iter().enumerate().map(|(i, port)| {
                    json!({
                        "name": port, "description": format!("{description} · Input {}", i + 1),
                        "channels": 1, "device": name
                    })
                }));
            }
            choices
        })
        .collect()
}

/// Other applications' playback a mic lane can take as a remote voice (a
/// call's or an X Space's far end): each playing stream, by its lane key,
/// and each application channel in use. Browsers name every stream
/// "Playback", so the lane number and whether it plays tell them apart.
pub(super) fn remote_sources(s: &Shared) -> Vec<Value> {
    let p = &*s.params;
    let links = s.links.lock().unwrap();
    // A binding follows a reopened stream to its successor.
    let taken: Vec<String> = links.mics.values()
        .map(|src| links.stream_lanes.current(src).unwrap_or(src).to_string()).collect();
    // Patch's own players (Patch Player, Patch Preview) are no far end.
    let own = |app: &str| app.starts_with("Patch ");
    let mut out: Vec<Value> = links
        .stream_lanes
        .assigned
        .iter()
        .filter(|(key, _)| !links.stream_labels.get(*key).is_some_and(|l| own(l)))
        .map(|(key, lane)| {
            let label = links.stream_labels.get(key).cloned().unwrap_or_else(|| "Application".into());
            let n = lane.trim_start_matches("stream-");
            let playing = p.channel(lane).is_some_and(|c| p.stream_heard_within(c, 3.0));
            let state = if taken.contains(key) { " · in a mic lane" } else if playing { " · playing" } else { " · quiet" };
            json!({"name": key, "description": format!("{label} · stream {n}{state}"), "channels": 2,
                "remote": true, "playing": playing, "lane": lane})
        })
        .collect();
    // Playing first: the stream you are listening to is the one you want.
    out.sort_by_key(|v| (v["playing"] != true, v["lane"].as_str().map(|l| l.trim_start_matches("stream-").parse::<u32>().unwrap_or(0))));
    out.extend(links.apps.iter().filter(|(_, app)| !own(app)).map(|(channel, app)| {
        json!({"name": channel, "description": format!("{app} · application channel"), "channels": 2,
            "remote": true, "lane": channel})
    }));
    out
}

/// A mic lane's source as it plays now: a Remote voice's stream followed
/// to the one that took over from it (a reopened browser stream).
pub(super) fn source_now(s: &Shared, source: &str) -> String {
    s.links.lock().unwrap().stream_lanes.current(source).unwrap_or(source).to_string()
}

/// The AUX buses a voice-source lane is sent to that its own application
/// process records (a Chromium voice sent to AUX 1 while Chromium's call
/// takes AUX 1 as its microphone: the call hears itself). ["AUX 1", …].
fn send_loops(s: &Shared, channel: &str, source: &str, captures: &[Value], aux_sources: &[(usize, u64)]) -> Vec<String> {
    let Some(c) = s.params.channel(channel) else { return vec![] };
    if !source.starts_with("stream:") || s.params.mix_minus(c) { return vec![] }
    let process = crate::stream_lanes::process(source);
    aux_sources.iter().filter(|(b, index)| {
        s.params.send_of(c, *b) > 0.0 && captures.iter().any(|o| {
            o["source"].as_u64() == Some(*index)
                && o["properties"]["application.process.id"].as_str() == Some(process)
        })
    }).map(|(b, _)| format!("AUX {}", b + 1)).collect()
}

pub(super) fn mics_json(s: &Shared) -> Value {
    let pulse_sources = pulse_objects("sources");
    let mut sources = capture_choices(&pulse_sources);
    sources.extend(remote_sources(s));
    let mics: Vec<(String, MicSetup)> = s.mics.lock().unwrap().iter().map(|(c, m)| (c.clone(), m.clone())).collect();
    let now: std::collections::BTreeMap<String, String> = mics.iter().map(|(c, m)| (c.clone(), source_now(s, &m.source))).collect();
    // Loops need the captures only while a voice-source lane exists.
    let voice_lanes = mics.iter().any(|(_, m)| m.input_send && m.source.starts_with("stream:"));
    let (captures, aux_sources): (Vec<Value>, Vec<(usize, u64)>) = if voice_lanes {
        let aux = crate::links::AUX.iter().enumerate()
            .filter_map(|(b, name)| pulse_sources.iter().find(|v| v["name"] == *name).and_then(|v| v["index"].as_u64()).map(|i| (b, i)))
            .collect();
        (pulse_objects("source-outputs"), aux)
    } else {
        (vec![], vec![])
    };
    let desc = |n: &str| {
        sources
            .iter()
            .find(|v| v["name"] == n)
            .and_then(|v| v["description"].as_str().map(String::from))
            .unwrap_or_else(|| n.to_string())
    };
    json!(mics.iter().map(|(ch, m)| json!({"channel": ch, "source": now[ch], "description": desc(&now[ch]), "monitor": m.monitor, "input_send": m.input_send, "remote": patch_core::mic::remote_source(&m.source), "loops": send_loops(s, ch, &now[ch], &captures, &aux_sources), "present": sources.iter().any(|v| v["name"] == now[ch].as_str()), "voice": m.voice, "meter": s.params.channel(ch).and_then(|c| s.params.mic_meter[c].read())})).collect::<Vec<_>>())
}

pub(super) fn select_input(s: &Shared, name: &str) -> Result<Value, String> {
    if name != crate::links::INPUT {
        let sources = capture_sources();
        let whole_device = sources.iter().any(|source| source["name"] == name);
        let selected: Vec<String> = sources
            .into_iter()
            .filter(|source| source["name"] == name || (!whole_device && source["device"] == name))
            .filter_map(|source| source["name"].as_str().map(String::from))
            .collect();
        if selected.is_empty() || selected.len() > 16 {
            return Err("choose an available input with at most sixteen channels".into());
        }
        std::fs::write(crate::session::state_dir().join("input-main"), "false")
            .map_err(|e| e.to_string())?;
        s.params.input_main.store(false, Ordering::Relaxed);
        let mut mics = s.mics.lock().unwrap();
        // Remote voices (a call's far end) keep their lanes: choosing your
        // microphone never drops the guest.
        let mut selected = selected.iter();
        for i in 1..=16 {
            let channel = format!("mic-{i}");
            if mics.get(&channel).is_some_and(|m| patch_core::mic::remote_source(&m.source)) {
                continue;
            }
            let c = s
                .params
                .channel(&channel)
                .ok_or("input channel unavailable")?;
            if let Some(source) = selected.next() {
                let fresh = !mics.contains_key(&channel);
                let mic = mics.entry(channel).or_insert(MicSetup {
                    source: source.clone(),
                    monitor: false,
                    voice: VoiceSetup::OFF,
                    previous_profile: None,
                    input_send: true,
                });
                mic.source = source.clone();
                mic.monitor = false;
                s.params.input_monitor[c].store(false, Ordering::Relaxed);
                s.params.set_voice(c, mic.voice);
                if fresh {
                    s.params.set_mute(c, false);
                }
            } else {
                mics.remove(&channel);
                s.params.set_voice(c, VoiceSetup::OFF);
            }
        }
        save_mics(&mics);
        let assignments = mics
            .iter()
            .map(|(channel, mic)| (channel.clone(), mic.source.clone()))
            .collect();
        drop(mics);
        let mut links = s.links.lock().unwrap();
        links.mics = assignments;
        links.tick();
    }
    crate::links::run(&["pactl", "set-default-source", crate::links::INPUT])
        .ok_or("could not select the Patch input mix")?;
    std::fs::write(patch_core::ipc::runtime_dir().join("input-selection"), name)
        .map_err(|e| e.to_string())?;
    crate::links::route_capture_apps();
    Ok(json!("ok"))
}
