//! Inputs and outputs: microphones and their profiles, voice, the input mix, output routes.

use super::*;

pub(super) fn op_mic_history(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let name = req["channel"].as_str().ok_or("no microphone lane")?;
        if !patch_core::mic::channel(name) { return Err("expected mic-1 through mic-16".into()); }
        let c = p.channel(name).ok_or("no microphone lane")?;
        let mut history = p.mic_meter[c].history().unwrap_or(Value::Null);
        if !history.is_null() { history["channel"] = json!(name); }
        Ok(history)
}

pub(super) fn op_mic_profile(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let action = req["action"].as_str().unwrap_or("apply");
        if action == "import" {
            let profile: patch_core::mic::Profile = serde_json::from_value(req["profile"].clone()).map_err(|e| e.to_string())?;
            crate::mic_profiles::save(&profile, req["replace"].as_bool().unwrap_or(false))?;
            return Ok(json!(profile));
        }
        if action == "export" {
            return Ok(json!(crate::mic_profiles::find(req["name"].as_str().ok_or("no profile name")?)?));
        }
        let name = req["channel"].as_str().ok_or("no channel")?;
        if !patch_core::mic::channel(name) { return Err("expected mic-1 through mic-16".into()); }
        let c = p.channel(name).ok_or("microphone lane unavailable")?;
        let eq = s.eq.lock().unwrap().get(name).cloned().unwrap_or_default();
        let mut mics = s.mics.lock().unwrap();
        let mic = mics.get_mut(name).ok_or("assign a microphone first")?;
        if let Some(expected) = req["expected_source"].as_str()
            && mic.source != expected { return Err("microphone source changed since calibration".into()); }
        let current = patch_core::mic::Profile::new("Previous settings", mic.voice, eq);
        if action == "snapshot" { return Ok(json!(current)); }
        if action == "save" {
            let profile = patch_core::mic::Profile::new(req["name"].as_str().ok_or("no profile name")?, current.voice, current.eq);
            crate::mic_profiles::save(&profile, req["replace"].as_bool().unwrap_or(false))?;
            return Ok(json!(profile));
        }
        let mut profile = match action {
            "apply" => crate::mic_profiles::find(req["name"].as_str().ok_or("no profile name")?)?,
            "restore" => mic.previous_profile.clone().ok_or("no previous microphone settings")?,
            _ => return Err("unknown microphone profile action".into()),
        };
        if req["preserve_eq"].as_bool() == Some(true) { profile.eq = current.eq.clone(); }
        mic.previous_profile = Some(current);
        mic.voice = profile.voice;
        p.set_voice(c, mic.voice);
        save_mics(&mics);
        drop(mics);
        set_eq_state(s, name, profile.eq.clone());
        Ok(json!(profile))
}

pub(super) fn op_mic(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let name = req["channel"].as_str().ok_or("no channel")?;
        let c = p.channel(name).filter(|_| name.starts_with("mic-")).ok_or_else(|| format!("'{name}' is no microphone channel (mic-1 through mic-16)"))?;
        let source = req["source"].as_str().map(String::from);
        // A remote voice: a stream or application channel playing now, and
        // in one lane only (checked before the mic lock: links, then mics).
        if let Some(src) = source.as_deref().filter(|s| patch_core::mic::remote_source(s)) {
            let links = s.links.lock().unwrap();
            if links.stream_lanes.current(src).is_none() && !links.apps.contains_key(src) {
                return Err("that application isn't playing into Patch any more: choose a playing stream".into());
            }
            let now = links.stream_lanes.current(src).unwrap_or(src);
            if links.mics.iter().any(|(ch, other)| ch != name && links.stream_lanes.current(other).unwrap_or(other) == now) {
                return Err("that stream already feeds another mic lane".into());
            }
        }
        let mut remote_eq = None;
        {
            let mut mics = s.mics.lock().unwrap();
            match source.as_deref() {
                Some(src) => {
                    if src.starts_with("patch") {
                        return Err("a Patch capture mix cannot feed its own input channel".into());
                    }
                    let remote = patch_core::mic::remote_source(src);
                    let fresh = !mics.contains_key(name);
                    let was_remote = mics.get(name).is_some_and(|m| patch_core::mic::remote_source(&m.source));
                    let m = mics.entry(name.to_string()).or_insert(MicSetup {
                        source: src.to_string(),
                        monitor: true,
                        voice: VoiceSetup::default(),
                        previous_profile: None,
                        input_send: true,
                    });
                    m.source = src.to_string();
                    if remote && !was_remote {
                        // A guest on a broadcast console: heard in the program
                        // at once (it was audible before), out of Patch Input
                        // (mix-minus), with the Remote voice profile.
                        m.monitor = true;
                        m.input_send = false;
                        if let Ok(profile) = crate::mic_profiles::find(patch_core::mic::REMOTE_PROFILE) {
                            m.voice = profile.voice;
                            remote_eq = Some(profile.eq);
                        }
                    } else if !remote && was_remote {
                        m.input_send = true;
                        m.monitor = false;
                    }
                    if let Some(monitor) = req["monitor"].as_bool() {
                        m.monitor = monitor;
                    }
                    if remote && let Some(input_send) = req["input_send"].as_bool() {
                        m.input_send = input_send;
                    }
                    p.input_monitor[c].store(m.monitor, Ordering::Relaxed);
                    p.input_send[c].store(m.input_send, Ordering::Relaxed);
                    p.set_voice(c, m.voice);
                    if fresh && !remote {
                        p.set_mute(c, true);
                    } else if fresh {
                        p.set_mute(c, false);
                    }
                }
                None => {
                    mics.remove(name);
                    p.set_voice(c, crate::voice::Settings::OFF);
                    p.input_send[c].store(true, Ordering::Relaxed);
                }
            }
            save_mics(&mics);
        }
        if let Some(eq) = remote_eq {
            set_eq_state(s, name, eq);
        }
        {
            let mut links = s.links.lock().unwrap();
            match source {
                Some(src) => {
                    links.mics.insert(name.to_string(), src);
                }
                None => {
                    links.mics.remove(name);
                }
            }
            links.recheck = 2;
        }
        Ok(mics_json(s))
}

pub(super) fn op_voice(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let name = req["channel"].as_str().ok_or("no channel")?;
        let c = p.channel(name).ok_or_else(|| format!("no channel '{name}'"))?;
        let mut mics = s.mics.lock().unwrap();
        let m = mics.get_mut(name).ok_or_else(|| format!("no microphone on '{name}'"))?;
        let mut proposed = m.voice;
        let v = &mut proposed;
        if let Some(on) = req["on"].as_bool() {
            v.on = on;
        }
        if let Some(h) = req["hpf"].as_f64() {
            v.hpf_hz = (h as f32).clamp(0.0, 300.0);
        }
        if req.get("gate").is_some() {
            v.gate_db = req["gate"].as_f64().map(|g| (g as f32).clamp(-80.0, -20.0));
        }
        if let Some(d) = req["deess"].as_f64() {
            v.deess = (d as f32).clamp(0.0, 1.0);
        }
        if let Some(k) = req["comp"].as_f64() {
            v.comp = (k as f32).clamp(0.0, 1.0);
        }
        if let Some(value) = req.get("expander") {
            v.expander_db = if value.is_null() { None } else {
                Some(value.as_f64().ok_or("expander must be dBFS or null")? as f32)
            };
        }
        if let Some(value) = req.get("noise") {
            v.noise_db = if value.is_null() { None } else { Some(value.as_f64().ok_or("noise must be dBFS or null")? as f32) };
        }
        for (key, target) in [("input_gain_db", &mut v.input_gain_db), ("output_gain_db", &mut v.output_gain_db)] {
            if let Some(value) = req.get(key) { *target = value.as_f64().ok_or("gain must be numeric dB")? as f32; }
        }
        v.validate()?;
        m.voice = *v;
        p.set_voice(c, m.voice);
        let out = json!(m.voice);
        save_mics(&mics);
        Ok(out)
}

pub(super) fn op_mic_monitor(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let channel = req["channel"].as_str().ok_or("no input channel")?;
        let on = req["on"].as_bool().ok_or("no monitoring setting")?;
        let c = p.channel(channel).filter(|_| channel.starts_with("mic-")).ok_or("unknown input channel")?;
        let mut mics = s.mics.lock().unwrap();
        let mic = mics.get_mut(channel).ok_or("input channel has no source")?;
        mic.monitor = on;
        p.input_monitor[c].store(on, Ordering::Relaxed);
        save_mics(&mics);
        drop(mics);
        crate::events::kick();
        Ok(mics_json(s))
}

/// An application's voice on a mic lane, the engine choosing which: the
/// stream playing now that no lane has taken, else the only free one.
/// `role`: "far_end" (default) — a call's far end, mix-minus: out of Patch
/// Input and every send, so the call never hears itself; "voice" — a voice
/// source: through the voice chain like any mic, into Patch Input and
/// sendable (a Chromium voice as another browser's microphone via AUX 1).
pub(super) fn op_mic_remote(s: &Shared, req: &Value) -> Result<Value, String> {
    let channel = req["channel"].as_str().ok_or("no input channel")?;
    let voice = match req["role"].as_str().unwrap_or("far_end") {
        "far_end" => false,
        "voice" => true,
        _ => return Err("role is far_end or voice".into()),
    };
    let taken: Vec<String> = {
        let links = s.links.lock().unwrap();
        links.mics.values().map(|src| links.stream_lanes.current(src).unwrap_or(src).to_string()).collect()
    };
    let free: Vec<Value> = remote_sources(s)
        .into_iter()
        .filter(|r| r["name"].as_str().is_some_and(|n| !taken.iter().any(|t| t == n)))
        .collect();
    let pick = free
        .iter()
        .find(|r| r["playing"] == true)
        .or_else(|| (free.len() == 1).then(|| &free[0]))
        .cloned()
        .ok_or_else(|| {
            if free.is_empty() {
                "Nothing plays into Patch: start the call or the Space first".to_string()
            } else {
                "Several apps play into Patch and none is playing now: choose the remote voice in the lane's Input list".to_string()
            }
        })?;
    op_mic(s, &json!({"channel": channel, "source": pick["name"], "input_send": voice}))?;
    Ok(pick)
}

/// An input lane in or out of the Patch Input mix (out: mix-minus, for a
/// remote voice, so a call never hears itself back).
pub(super) fn op_mic_send(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    let channel = req["channel"].as_str().ok_or("no input channel")?;
    let on = req["on"].as_bool().ok_or("no input mix setting")?;
    let c = p.channel(channel).filter(|_| patch_core::mic::channel(channel)).ok_or("unknown input channel")?;
    let mut mics = s.mics.lock().unwrap();
    let mic = mics.get_mut(channel).ok_or("input channel has no source")?;
    mic.input_send = on;
    p.input_send[c].store(on, Ordering::Relaxed);
    save_mics(&mics);
    drop(mics);
    crate::events::kick();
    Ok(mics_json(s))
}

pub(super) fn op_input_mix(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        if let Some(main) = req["main"].as_bool() {
            std::fs::write(crate::session::state_dir().join("input-main"), main.to_string()).map_err(|e| e.to_string())?;
            p.input_main.store(main, Ordering::Relaxed);
            crate::events::kick();
        }
        Ok(json!({"main": p.input_main.load(Ordering::Relaxed)}))
}

pub(super) fn op_output_route(s: &Shared, req: &Value) -> Result<Value, String> {
        let bus = req["bus"].as_str().ok_or("no output bus")?;
        let sink = req["sink"].as_str().ok_or("no output device")?;
        let ports = req["ports"].as_array().ok_or("no physical ports")?.iter()
            .map(|p| p.as_str().map(String::from).ok_or("invalid port"))
            .collect::<Result<Vec<_>,_>>()?;
        s.links.lock().unwrap().route_ports(bus,sink,ports)?;
        Ok(json!("ok"))
}

pub(super) fn op_output_select(s: &Shared, req: &Value) -> Result<Value, String> {
        let sink = req["sink"].as_str().ok_or("no output destination")?;
        s.links.lock().unwrap().select_output(sink)?;
        Ok(json!("ok"))
}
