//! Native Mic orchestration; audio processing remains in the shared engine.
use super::*;
use std::io::Write as _;

thread_local! {
    /// When a voice control last changed (the UI thread's).
    static LAST_EDIT: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

impl AudioApp {
    /// Engine requests from a mic control, off the UI thread (some ask
    /// PipeWire, ~10 ms); the Inspector reads the result on its next poll.
    pub(super) fn engine_bg(&self, req: serde_json::Value) {
        std::thread::spawn(move || {
            if let Err(e) = patch_engine::client::call(&req) {
                // `eprintln!` panics when the terminal that launched Patch
                // has closed its stderr pipe. Release builds abort on panic,
                // taking every Unit window down with this background request.
                let _ = writeln!(std::io::stderr().lock(), "patch: {e}");
            }
        });
    }

    /// A slider's engine request: only the latest value per control is sent,
    /// one at a time. A drag moves ~100 times a second; a thread per move
    /// queued requests behind each other and the engine lagged the hand.
    pub(super) fn engine_latest(&self, key: String, req: serde_json::Value) {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        type Pending = Mutex<(HashMap<String, serde_json::Value>, bool)>;
        static PENDING: OnceLock<Pending> = OnceLock::new();
        let pending = PENDING.get_or_init(Default::default);
        let mut state = pending.lock().unwrap_or_else(|e| e.into_inner());
        state.0.insert(key, req);
        if state.1 {
            return;
        }
        state.1 = true;
        drop(state);
        std::thread::spawn(move || loop {
            let next = {
                let mut state = pending.lock().unwrap_or_else(|e| e.into_inner());
                let key = state.0.keys().next().cloned();
                match key.and_then(|k| state.0.remove(&k)) {
                    Some(req) => req,
                    None => {
                        state.1 = false;
                        return;
                    }
                }
            };
            if let Err(e) = patch_engine::client::call(&next) {
                let _ = writeln!(std::io::stderr().lock(), "patch: {e}");
            }
        });
    }

    /// A microphone on the first free mic channel: the system's default
    /// input (muted at first: the engine's rule; headphones on, then up).
    pub(super) fn add_mic(&self) {
        let used: Vec<String> = self
            .provider
            .borrow()
            .strips()
            .iter()
            .filter_map(|s| s.node.strip_prefix("engine.").map(String::from))
            .collect();
        let Some(ch) = (1..=16)
            .map(|i| format!("mic-{i}"))
            .find(|c| !used.contains(c))
        else {
            return;
        };
        *self.mic_pending.borrow_mut() = Some(format!("engine.{ch}"));
        std::thread::spawn(move || {
            // An input no mic uses yet: the system's default if it's free,
            // else the first free one (a second + Mic took the default
            // again: two mics on one input).
            let info =
                patch_engine::client::call(&serde_json::json!({"op": "mics"})).unwrap_or_default();
            let taken: Vec<String> = info["mics"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["source"].as_str().map(String::from))
                .collect();
            let all: Vec<String> = info["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["name"].as_str().map(String::from))
                .collect();
            let default = std::process::Command::new("pactl")
                .arg("get-default-source")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty() && !s.ends_with(".monitor"));
            let source = default
                .filter(|d| {
                    !taken
                        .iter()
                        .any(|used| patch_core::mic::sources_overlap(used, d))
                        && all.contains(d)
                })
                .or_else(|| {
                    all.into_iter().find(|s| {
                        !taken
                            .iter()
                            .any(|used| patch_core::mic::sources_overlap(used, s))
                    })
                });
            match source {
                Some(src) => {
                    let _ = patch_engine::client::call(
                        &serde_json::json!({"op": "mic", "channel": ch, "source": src, "monitor": false}),
                    );
                }
                None => {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "patch: no free microphone input to add"
                    );
                }
            }
        });
    }

    /// A remote voice on the first free mic lane: the far end of a call or an
    /// X Space playing into Patch (the stream playing now), through the voice
    /// chain and kept out of Patch Input (the engine's mix-minus default).
    pub(super) fn add_remote(&self, role: &str) {
        let used: Vec<String> = self
            .provider
            .borrow()
            .strips()
            .iter()
            .filter_map(|s| s.node.strip_prefix("engine.").map(String::from))
            .collect();
        let Some(ch) = (1..=16).map(|i| format!("mic-{i}")).find(|c| !used.contains(c)) else {
            *self.toast.borrow_mut() = "All sixteen input lanes are in use".into();
            self.push_toast();
            return;
        };
        // The engine picks the stream playing now (it knows which plays).
        *self.mic_pending.borrow_mut() = Some(format!("engine.{ch}"));
        self.mic_polls.request("remote-add", serde_json::json!({"op": "mic_remote", "channel": ch, "role": role}));
    }

    pub(super) fn mic_source(&self, strip: &str, source: &str) {
        let Some(ch) = strip.strip_prefix("engine.") else {
            return;
        };
        self.engine_bg(serde_json::json!({"op": "mic", "channel": ch, "source": source}));
    }

    pub(super) fn mic_remove(&self, strip: &str) {
        let Some(ch) = strip.strip_prefix("engine.") else {
            return;
        };
        self.engine_bg(serde_json::json!({"op": "mic", "channel": ch, "source": null}));
        self.select("master");
    }

    pub(super) fn mic_step(&self, direction: i32) {
        let mut lanes: Vec<String> = self
            .provider
            .borrow()
            .strips()
            .iter()
            .filter(|s| s.node.starts_with("engine.mic-"))
            .map(|s| s.node.to_string())
            .collect();
        lanes.sort_by_key(|s| {
            s.trim_start_matches("engine.mic-")
                .parse::<u8>()
                .unwrap_or(0)
        });
        if lanes.is_empty() {
            return;
        }
        let selected = self.selected.borrow().clone();
        let index = lanes.iter().position(|s| *s == selected).map_or(0, |i| {
            (i as i32 + direction).rem_euclid(lanes.len() as i32) as usize
        });
        self.select(&lanes[index]);
    }

    pub(super) fn mic_record(&self, processed: bool) {
        if self.rec_busy.get() || !self.rec_snapshot.borrow().is_null() {
            *self.toast.borrow_mut() =
                "Recorder is already active; stop it before starting a voice take".into();
            self.push_toast();
            return;
        }
        let ch = self
            .selected
            .borrow()
            .trim_start_matches("engine.")
            .to_string();
        let source = if processed {
            patch_engine::links::INPUT.to_string()
        } else {
            self.mic_info.borrow()["mics"]
                .as_array()
                .and_then(|ms| ms.iter().find(|m| m["channel"] == ch))
                .and_then(|m| m["source"].as_str())
                .unwrap_or("")
                .to_string()
        };
        if source.is_empty() || source.contains(':') {
            *self.toast.borrow_mut() =
                "Raw per-port recording is unavailable; use Processed take for the capture mix"
                    .into();
            self.push_toast();
            return;
        }
        let label = if processed {
            "Mic processed"
        } else {
            "Mic original"
        };
        let path =
            Self::rec_dir().join(format!("{label} {}.wav", timestamp_utc().replace(':', "")));
        self.queue_record(serde_json::json!({"op":"start", "source":format!("device:{source}"), "label":label, "float":true, "rate":48000, "path":path}));
    }

    pub(super) fn mic_compare(&self, path: &str) {
        if self.rec_snapshot.borrow()["path"].as_str() == Some(path) {
            *self.toast.borrow_mut() = "Stop and save the take before comparing".into();
            self.push_toast();
            return;
        }
        let ch = self
            .selected
            .borrow()
            .trim_start_matches("engine.")
            .to_string();
        self.mic_polls.request_with("mic-compare", serde_json::json!({"path":path, "channel":ch}), |request| {
            let path = request["path"].as_str().ok_or("no original take")?;
            let samples = patch_analysis::mic::decode_short(std::path::Path::new(path))?;
            let profile: patch_core::mic::Profile = serde_json::from_value(patch_engine::client::call(&serde_json::json!({"op":"mic_profile", "action":"snapshot", "channel":request["channel"]}))?).map_err(|e| e.to_string())?;
            let dir = crate::dj::music_dir().join("Patch Recordings");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
            let output = dir.join(format!("Mic comparison {id}.wav"));
            patch_record::mic::render(&samples, &profile, &output)?;
            Ok(serde_json::json!({"original":path, "processed":output}))
        });
    }

    pub(super) fn mic_analyze(&self, path: &str) {
        if self.rec_snapshot.borrow()["path"].as_str() == Some(path) {
            *self.toast.borrow_mut() = "Stop and save the take before analyzing".into();
            self.push_toast();
            return;
        }
        let ch = self
            .selected
            .borrow()
            .trim_start_matches("engine.")
            .to_string();
        let source = self.mic_info.borrow()["mics"]
            .as_array()
            .and_then(|ms| ms.iter().find(|m| m["channel"] == ch))
            .and_then(|m| m["source"].as_str())
            .unwrap_or("")
            .to_string();
        self.mic_polls.request_with("mic-analysis", serde_json::json!({"path":path, "channel":ch, "source":source}), |request| {
            let samples = patch_analysis::mic::decode_short(std::path::Path::new(request["path"].as_str().ok_or("no take path")?))?;
            let analysis = patch_analysis::mic::analyze(&samples, 48000)?;
            Ok(serde_json::json!({"analysis":analysis, "channel":request["channel"], "source":request["source"], "path":request["path"]}))
        });
    }

    pub(super) fn mic_apply_calibration(&self) {
        let Some(record) = self.mic_calibration.borrow().clone() else {
            return;
        };
        let ch = self
            .selected
            .borrow()
            .trim_start_matches("engine.")
            .to_string();
        let source = self.mic_info.borrow()["mics"]
            .as_array()
            .and_then(|ms| ms.iter().find(|m| m["channel"] == ch))
            .and_then(|m| m["source"].as_str())
            .unwrap_or("")
            .to_string();
        if record["channel"] != ch || record["source"] != source {
            *self.toast.borrow_mut() =
                "Input changed since analysis; analyze a take from the current input".into();
            self.push_toast();
            return;
        }
        self.mic_polls.request_with("mic-edit", record, move |record| {
            let analysis: patch_analysis::mic::Calibration = serde_json::from_value(record["analysis"].clone()).map_err(|e| e.to_string())?;
            if analysis.active_fraction == 0.0 || analysis.clipped_samples > 0 { return Err("record an unclipped voice sample before applying recommendations".into()); }
            let name = format!("Calibration {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos());
            let voice = patch_core::mic::Settings { on:true, hpf_hz:analysis.recommended_hpf_hz, gate_db:analysis.recommended_gate_db, deess:analysis.recommended_deess, comp:analysis.recommended_compression, ..patch_core::mic::Settings::OFF };
            let profile = patch_core::mic::Profile::new(&name, voice, patch_core::eq::EqState::default());
            patch_engine::client::call(&serde_json::json!({"op":"mic_profile", "action":"import", "profile":profile}))?;
            patch_engine::client::call(&serde_json::json!({"op":"mic_profile", "channel":ch, "name":name, "expected_source":source, "preserve_eq":true}))
        });
    }

    pub(super) fn mic_profile(&self, strip: &str, action: &str, name: &str) {
        let Some(ch) = strip.strip_prefix("engine.") else {
            return;
        };
        if !patch_core::mic::channel(ch) {
            return;
        }
        if action == "eq" {
            for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                if w.get_surface() == "eq" {
                    w.global::<crate::ui::EqState>().set_target(ch.into());
                }
            }
            return;
        }
        self.mic_polls.request(
            "mic-edit",
            serde_json::json!({"op":"mic_profile", "channel":ch, "action":action, "name":name}),
        );
    }

    /// A voice stage by its row: on/off and amount (0…1) to its setting.
    pub(super) fn voice_set(&self, strip: &str, stage: &str, on: bool, amount: f32) {
        let Some(ch) = strip.strip_prefix("engine.") else {
            return;
        };
        if matches!(stage, "Monitor" | "Send" | "MixMinus") {
            let (op, key) = if stage == "Monitor" { ("mic_monitor", "monitor") } else { ("mic_send", "input_send") };
            // Call far end (mix-minus) on is the input send off.
            let on = if stage == "MixMinus" { !on } else { on };
            // Shown at once (the engine confirms on the next poll).
            if let Some(m) = self.mic_info.borrow_mut()["mics"]
                .as_array_mut()
                .and_then(|ms| ms.iter_mut().find(|m| m["channel"] == ch))
            {
                m[key] = serde_json::json!(on);
            }
            self.engine_bg(serde_json::json!({"op": op, "channel": ch, "on": on}));
            self.rebuild_selection();
            return;
        }
        let a = amount.clamp(0.0, 1.0);
        let mut req = serde_json::json!({"op": "voice", "channel": ch, "on": true});
        match stage {
            "Input trim" => {
                req["input_gain_db"] = serde_json::json!(if on { -24.0 + 48.0 * a } else { 0.0 })
            }
            "Output trim" => {
                req["output_gain_db"] = serde_json::json!(if on { -24.0 + 48.0 * a } else { 0.0 })
            }
            "Bypass" => req["on"] = serde_json::json!(on),
            "High-pass" => req["hpf"] = serde_json::json!(if on { 40.0 + 160.0 * a } else { 0.0 }),
            "Noise gate" => {
                req["gate"] = if on {
                    serde_json::json!(-70.0 + 40.0 * a)
                } else {
                    serde_json::Value::Null
                }
            }
            "Band suppression" => {
                req["noise"] = if on {
                    serde_json::json!(-70.0 + 40.0 * a)
                } else {
                    serde_json::Value::Null
                }
            }
            "Expander" => {
                req["expander"] = if on {
                    serde_json::json!(-70.0 + 40.0 * a)
                } else {
                    serde_json::Value::Null
                }
            }
            "De-esser" => req["deess"] = serde_json::json!(if on { a.max(0.05) } else { 0.0 }),
            "Compressor" => req["comp"] = serde_json::json!(if on { a.max(0.05) } else { 0.0 }),
            _ => return,
        }
        // Shown at once (the engine confirms on the next poll).
        if let Some(m) = self.mic_info.borrow_mut()["mics"]
            .as_array_mut()
            .and_then(|ms| ms.iter_mut().find(|m| m["channel"] == ch))
        {
            let v = &mut m["voice"];
            match stage {
                "Input trim" => v["input_gain_db"] = req["input_gain_db"].clone(),
                "Output trim" => v["output_gain_db"] = req["output_gain_db"].clone(),
                "Bypass" => v["on"] = req["on"].clone(),
                "High-pass" => v["hpf_hz"] = req["hpf"].clone(),
                "Noise gate" => v["gate_db"] = req["gate"].clone(),
                "Band suppression" => v["noise_db"] = req["noise"].clone(),
                "Expander" => v["expander_db"] = req["expander"].clone(),
                "De-esser" => v["deess"] = req["deess"].clone(),
                _ => v["comp"] = req["comp"].clone(),
            }
        }
        self.engine_latest(format!("{ch}/{stage}"), req);
        LAST_EDIT.set(Some(std::time::Instant::now()));
        self.rebuild_selection();
    }

    /// While a mic strip is selected: its details, once a second.
    pub(super) fn poll_mics(&self) {
        if patch_host::shot::active() && !super::live_shots() {
            return;
        }
        if let Some(reply) = self.mic_polls.take("mic-history") {
            let value = reply.result.unwrap_or_default();
            let selected = self
                .selected
                .borrow()
                .trim_start_matches("engine.")
                .to_string();
            if !value.is_null() && value["channel"] != selected {
                return;
            }
            let points: Vec<f32> = value["peaks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    let peak = v.as_f64().unwrap_or(0.0).max(1e-6);
                    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0) as f32
                })
                .collect();
            let model = ModelRc::new(VecModel::from(points));
            for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                w.global::<AudioState>().set_mic_history(model.clone());
                w.global::<AudioState>().set_mic_history_span(
                    format!(
                        "{:.0} ms · engine cycles →",
                        value["span_ms"].as_f64().unwrap_or(0.0)
                    )
                    .into(),
                );
            }
        }
        if let Some(reply) = self.mic_polls.take("mic-compare") {
            match reply.result {
                Ok(record) => {
                    for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                        w.global::<AudioState>()
                            .set_mic_comparison(record["processed"].as_str().unwrap_or("").into());
                    }
                }
                Err(e) => {
                    *self.toast.borrow_mut() = e;
                    self.push_toast();
                }
            }
        }
        if let Some(reply) = self.mic_polls.take("mic-analysis") {
            let text = match reply.result {
                Ok(record) => {
                    let a = &record["analysis"];
                    let text = format!(
                        "Peak {:.1} dBFS · RMS {:.1} dBFS · headroom {:.1} dB · clips {}\nSuggested HPF {:.0} Hz · gate {} · de-esser {:.0}% · compression {:.0}%\n{}",
                        a["peak_dbfs"].as_f64().unwrap_or(-120.0),
                        a["rms_dbfs"].as_f64().unwrap_or(-120.0),
                        a["headroom_db"].as_f64().unwrap_or(0.0),
                        a["clipped_samples"],
                        a["recommended_hpf_hz"].as_f64().unwrap_or(60.0),
                        a["recommended_gate_db"],
                        a["recommended_deess"].as_f64().unwrap_or(0.0) * 100.0,
                        a["recommended_compression"].as_f64().unwrap_or(0.0) * 100.0,
                        a["warnings"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    *self.mic_calibration.borrow_mut() = Some(record);
                    text
                }
                Err(e) => {
                    *self.mic_calibration.borrow_mut() = None;
                    e
                }
            };
            for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                w.global::<AudioState>()
                    .set_mic_analysis(text.clone().into());
                w.global::<AudioState>().set_mic_calibration_ready(
                    self.mic_calibration.borrow().as_ref().is_some_and(|v| {
                        v["analysis"]["clipped_samples"] == 0
                            && v["analysis"]["active_fraction"].as_f64().unwrap_or(0.0) > 0.0
                    }),
                );
            }
        }
        if let Some(reply) = self.mic_polls.take("remote-add") {
            *self.toast.borrow_mut() = match reply.result {
                Ok(v) => format!("Added {}", v["description"].as_str().unwrap_or("the voice")),
                Err(e) => {
                    *self.mic_pending.borrow_mut() = None;
                    e
                }
            };
            self.push_toast();
        }
        if let Some(reply) = self.mic_polls.take("mic-edit") {
            *self.toast.borrow_mut() = match reply.result {
                Ok(_) => "Microphone profile updated".into(),
                Err(e) => e,
            };
            self.push_toast();
        }
        let pending = self.mic_pending.borrow().clone();
        if let Some(id) = pending
            && self.provider.borrow().strips().iter().any(|s| s.node == id)
        {
            *self.mic_pending.borrow_mut() = None;
            self.select(&id);
        }
        if !self.selected.borrow().starts_with("engine.mic-") {
            return;
        }
        // A reply asked before the last edit reached the engine would show
        // the old setting for a poll: the slider jumped back on release.
        let settled = LAST_EDIT.get().is_none_or(|at| at.elapsed().as_millis() > 500);
        if let Some(r) = self.mic_polls.take("mics")
            && let Ok(v) = r.result
            && settled
        {
            *self.mic_info.borrow_mut() = v;
            self.rebuild_selection();
        }
        self.mic_polls
            .request("mics", serde_json::json!({"op": "mics"}));
        let channel = self
            .selected
            .borrow()
            .trim_start_matches("engine.")
            .to_string();
        self.mic_polls.request(
            "mic-history",
            serde_json::json!({"op":"mic_history", "channel":channel}),
        );
    }
}
