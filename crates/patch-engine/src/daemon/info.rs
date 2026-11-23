//! Engine information requests: status, meters, clock, latency, analyzer.

use super::*;

pub(super) fn op_status(s: &Shared, _req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        // Take an owned graph snapshot and release the route lock before
        // reading other shared state. Mic assignment updates `mics` and then
        // `links`; keeping this guard until the response is built made status
        // acquire them in the opposite order and could deadlock every control
        // request after a mic change.
        let (
            link_target,
            explicit_target,
            capture_active,
            cue_target,
            main_ports,
            cue_ports,
            links_active,
            aux_routes,
        ) = {
            let links = s.links.lock().unwrap();
            (
                links.target.clone(),
                links.explicit(),
                links.capture_active,
                links.cue_target.clone(),
                links.main_ports(),
                links
                    .output_routing
                    .cue
                    .as_ref()
                    .filter(|a| Some(&a.device) == links.cue_target.as_ref())
                    .map(|a| a.ports.clone()),
                links.active,
                links.aux_routes(),
            )
        };
        let (avg, max) = *s.cost.lock().unwrap();
        let rate = p.last_rate.load(Ordering::Relaxed).max(1) as f64;
        let budget = p.last_frames.load(Ordering::Relaxed) as f64 / rate * 1e9;
        let load = |ns: u64| {
            if budget > 0.0 {
                ns as f64 / budget
            } else {
                0.0
            }
        };
        let sched = data_thread_sched();
        let (ch_active, program_active) = activity(s);
        let mut leases = s.module_leases.lock().unwrap();
        leases.retain(|_, at| at.elapsed() < Duration::from_secs(3));
        let visible_modules: Vec<&str> = ["pattern", "tidal", "sequencer", "dj"].into_iter()
            .filter(|name| leases.keys().any(|(n, _)| n == name)
                || p.channel(name).is_some_and(|c| ch_active[c])
                || (*name == "pattern" && s.pattern.playing())
                || (*name == "tidal" && s.tidal.playing())
                || (*name == "sequencer" && p.sequencer.playing.load(Ordering::Relaxed))
                || (*name == "dj" && p.decks.iter().any(|d| d.active.load(Ordering::Relaxed))))
            .collect();
        drop(leases);
        let dj_group = {
            let m = &s.session.lock().unwrap().mixer;
            json!({"gain_db": m.group_db, "mute": m.group_mute, "solo": p.dj_solo()})
        };
        let channel_status = (0..p.channels()).map(|c| json!({
                "name": p.names[c],
                "gain_db": linear_to_db(p.gain_of(c)),
                "mute": p.is_muted(c),
                "solo": p.solo_of(c),
                "pan": load_pan(p, c),
                "trim_db": linear_to_db(p.trim_of(c)),
                "sends": sends_json(p, c),
                "fx": fx_json(p, c),
                "eq": eq_json(p, c),
                "pfl": p.pfl_of(c),
                "voice": p.voice_of(c).on,
                "active": ch_active.get(c).copied().unwrap_or(false),
            })).collect::<Vec<_>>();
        let out_delay = f64::from_bits(p.out_delay.load(Ordering::Relaxed));
        let output_delay_ms = (out_delay > 0.0).then(|| out_delay * 1000.0 / rate);
        // Since the engine started: callbacks over their cycle, and cost
        // percentiles (upper bounds, powers of two of ns), cycles after an
        // xrun PipeWire reported and wake-ups over two cycles late
        // (`dropped` counts telemetry ring drops, not xruns).
        let timing = {
            let (cycles, [p50, p95, p99, p999]) = p.cost_percentiles();
            json!({
                "overruns": p.overruns.load(Ordering::Relaxed),
                "cycles": cycles,
                "cost_percentiles_ns": {"p50": p50, "p95": p95, "p99": p99, "p99_9": p999},
                "xruns": p.xruns.load(Ordering::Relaxed),
                "gaps": p.gaps.load(Ordering::Relaxed),
            })
        };
        let mut status = json!({
            "realtime": sched.is_some_and(|(p, _)| p != "normal"),
            "program_active": program_active,
            "visible_modules": visible_modules,
            "sched": sched.map(|(p, prio)| format!("{p} {prio}")),
            "cost_avg_ns": avg,
            "cost_max_ns": max,
            "timing": timing,
            "load_avg": load(avg),
            "load_max": load(max),
            "pid": std::process::id(),
            "build": crate::client::build_stamp(),
            "version": patch_core::VERSION,
            "backend": backend_once(),
            "kernels": kernels_json(),
            "rate": p.last_rate.load(Ordering::Relaxed),
            "quantum": p.last_frames.load(Ordering::Relaxed),
            // The output device's delay to the DAC as its driver tracks
            // it (ms, smoothed; null: the driver reports none).
            "output_delay_ms": output_delay_ms,
            "cycles": p.cycles.load(Ordering::Relaxed),
            "dropped": p.meters.dropped.load(Ordering::Relaxed) + p.program.dropped.load(Ordering::Relaxed),
            // Why the DJ session is not on disk, while it isn't.
            "session_save_error": crate::session::save_error(),
            "master_db": linear_to_db(p.master()),
            "master_mute": p.master_mute.load(Ordering::Relaxed),
            "system_post": p.system_post.load(Ordering::Relaxed),
            "system_volume": system_volume_json(p),
            "graph_always": s.always.load(Ordering::Relaxed),
            "graph_reason": drive_reason(s),
            "output": link_target,
            "output_chosen": explicit_target,
            "capture_active": capture_active,
            "input_main": p.input_main.load(Ordering::Relaxed),
            "cue_output": cue_target,
            "output_ports": main_ports,
            "cue_ports": cue_ports,
            "dj_group": dj_group,
            "cue_mix": p.cue().0,
            "cue_level_db": linear_to_db(p.cue().1),
            "linked": links_active,
            "channels": channel_status,
            // The Mixer's buses (masters, returns, the DJ Booth's sends,
            // the returns' effects) and where AUX 1 and 2 play.
            "mixer_buses": buses_json(p),
            "aux_routes": aux_routes,
        });
        // Keep the detailed arrays outside the large status macro. This
        // read stays in-process; device descriptions belong to the mics op.
        {
            let mics = s.mics.lock().unwrap();
            status["mics"] = json!(mics.iter().map(|(channel, mic)| json!({
                "channel": channel,
                "source": mic.source,
                "monitor": mic.monitor,
                "input_send": mic.input_send,
                "remote": patch_core::mic::remote_source(&mic.source),
                "voice": mic.voice,
                "meter": p.channel(channel).and_then(|c| p.mic_meter[c].read()),
            })).collect::<Vec<_>>());
        }
        // Playback strips whose audio moved to a mic lane.
        status["remote_lanes"] = json!(s.links.lock().unwrap().remote_lanes());
        status["stretch"] = json!(crate::deck::stretch::ENGINE);
        Ok(status)
}

pub(super) fn op_meters(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let since = req["since"].as_u64().unwrap_or(0);
        // Packed levels: a fraction of the JSON cost on both sides.
        let packed = req["packed"].as_bool() == Some(true);
        let h = s.history.lock().unwrap();
        let latest = h.back().map(|(q, _)| *q).unwrap_or(0);
        let n = p.channels();
        let blocks: Vec<Value> = h
            .iter()
            .filter(|(q, _)| *q > since)
            .rev()
            .take(MAX_REPLY_BLOCKS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(q, b)| {
                if packed {
                    // Levels as centi-dB, then gains ×8000, as i16:
                    // [ch L,R …, abs L,R …, out L,R] and [gain L,R …].
                    let cdb = |x: f32| (linear_to_db(x) * 100.0).round().clamp(-32768.0, 32767.0) as i16;
                    let lv: Vec<i16> = b.ch_peak[..n].iter().chain(&b.ch_abs[..n]).flat_map(|s| [cdb(s[0]), cdb(s[1])]).chain([cdb(b.out_peak[0]), cdb(b.out_peak[1])]).collect();
                    let g: Vec<i16> = b.ch_gain[..n].iter().flat_map(|s| s.map(|x| (x * 8000.0).round().clamp(-32768.0, 32767.0) as i16)).collect();
                    return json!({
                        "seq": q,
                        "nsec": b.stamp.nsec,
                        "rate": b.stamp.rate,
                        "frames": b.stamp.frames,
                        "lv": patch_core::pack::pack_i16(&lv),
                        // The DJ Booth's sum: peak L, R, then mean |x| L, R (dB).
                        "dj": [linear_to_db(b.dj_peak[0]), linear_to_db(b.dj_peak[1]), linear_to_db(b.dj_abs[0]), linear_to_db(b.dj_abs[1])],
                        // The Mixer's buses (AUX 1, 2, FX 1, 2) as they leave: peak L, R (dB).
                        "bus": b.bus_peak.iter().map(|s| [linear_to_db(s[0]), linear_to_db(s[1])]).collect::<Vec<_>>(),
                        "g": patch_core::pack::pack_i16(&g),
                        "decks": (0..p.decks.len()).map(|d| {
                            let pos = b.deck_pos[d];
                            if pos.is_nan() { Value::Null } else { json!([pos / b.stamp.rate.max(1) as f64, b.deck_step[d]]) }
                        }).collect::<Vec<_>>(),
                    });
                }
                json!({
                    "seq": q,
                    "pos": b.stamp.sample_pos,
                    "nsec": b.stamp.nsec,
                    "rate": b.stamp.rate,
                    "frames": b.stamp.frames,
                    "ch": b.ch_peak[..n].iter().map(|s| [linear_to_db(s[0]), linear_to_db(s[1])]).collect::<Vec<_>>(),
                    // Mean |x| per channel (dB re full scale), for channel VUs.
                    "abs": b.ch_abs[..n].iter().map(|s| [linear_to_db(s[0]), linear_to_db(s[1])]).collect::<Vec<_>>(),
                    // Fader gain per channel in this cycle (linear).
                    "gain": b.ch_gain[..n].iter().map(|s| [s[0], s[1]]).collect::<Vec<_>>(),
                    "out": [linear_to_db(b.out_peak[0]), linear_to_db(b.out_peak[1])],
                    "dj": [linear_to_db(b.dj_peak[0]), linear_to_db(b.dj_peak[1]), linear_to_db(b.dj_abs[0]), linear_to_db(b.dj_abs[1])],
                    "bus": b.bus_peak.iter().map(|s| [linear_to_db(s[0]), linear_to_db(s[1])]).collect::<Vec<_>>(),
                    // Native decks: [seconds, step] (null: no track).
                    "decks": (0..p.decks.len()).map(|d| {
                        let pos = b.deck_pos[d];
                        if pos.is_nan() { Value::Null } else { json!([pos / b.stamp.rate.max(1) as f64, b.deck_step[d]]) }
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        drop(h);
        let a = s.analysis.snapshot.lock().unwrap().clone();
        // The DJ mixer as it is now: views follow crossfades per frame.
        let mixer = {
            let m = &s.session.lock().unwrap().mixer;
            json!({"xfader": m.xfader, "faders": m.faders})
        };
        Ok(json!({
            "mixer": mixer,
            "seq": latest,
            "channels": p.names,
            "blocks": blocks,
            "vu": a.vu,
            "loudness": { "m": a.momentary, "s": a.short_term, "i": a.integrated, "lra": a.range, "tp_max": a.true_peak_max },
            "spectrum": if packed { Value::Null } else { json!(a.spectrum) },
            "spectrum16": if packed { json!(patch_core::pack::pack_i16(&a.spectrum.iter().map(|x| (x * 100.0).round().clamp(-32768.0, 32767.0) as i16).collect::<Vec<_>>())) } else { Value::Null },
        }))
}

pub(super) fn op_clock(s: &Shared, _req: &Value) -> Result<Value, String> {
        let last = s.history.lock().unwrap().back().map(|(_, b)| *b);
        Ok(match last {
            Some(b) => json!({
                "position": b.stamp.sample_pos,
                "nsec": b.stamp.nsec,
                "rate": b.stamp.rate,
                "frames": b.stamp.frames,
                "now_ns": crate::now_ns(),
            }),
            None => json!({"now_ns": crate::now_ns()}),
        })
}

pub(super) fn op_latency(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    match req["do"].as_str() {
        None => Ok(json!({
            "measure": *s.measure.lock().unwrap(),
            "output": s.links.lock().unwrap().target.clone(),
            "saved": crate::latency::load_all(),
        })),
        Some("measure") => {
            let output = s
                .links
                .lock()
                .unwrap()
                .target
                .clone()
                .ok_or("no output device to measure")?;
            let mic = match req["mic"].as_str() {
                Some(m) => m.to_string(),
                None => crate::latency::choose_mic()?,
            };
            let rate = p.last_rate.load(Ordering::Relaxed);
            if rate == 0 {
                return Err("the engine isn't running a graph yet".into());
            }
            p.latency.arm(rate)?;
            *s.measure.lock().unwrap() = Measure {
                phase: "measuring",
                mic: Some(mic.clone()),
                output: Some(output.clone()),
                ..Default::default()
            };
            let (params, measure, reply_mic) =
                (s.params.clone(), s.measure.clone(), mic.clone());
            std::thread::Builder::new()
                .name("patch-latency".into())
                .spawn(move || {
                    let r = crate::latency::run(&params, &mic, &output, rate);
                    let mut m = measure.lock().unwrap();
                    match r {
                        Ok(res) => {
                            m.phase = "done";
                            m.result = Some(res);
                        }
                        Err(e) => {
                            m.phase = "failed";
                            m.error = Some(e);
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(
                json!({"mic": reply_mic, "seconds": crate::latency::Plan::new(rate).len as f64 / rate as f64 + 0.6}),
            )
        }
        Some("forget") => {
            let output = s
                .links
                .lock()
                .unwrap()
                .target
                .clone()
                .ok_or("no output device")?;
            crate::latency::forget(&output)?;
            Ok(json!("ok"))
        }
        Some(other) => Err(format!("latency: unknown '{other}'")),
    }
}

pub(super) fn op_analyzer(s: &Shared, req: &Value) -> Result<Value, String> {
        let a = s.analysis.analyzer(req["reset"].as_bool().unwrap_or(false));
        // The wire carries finite numbers only.
        let f = |x: f64| {
            if x.is_finite() {
                (x * 100.0).round() / 100.0
            } else {
                -140.0
            }
        };
        let r = |x: f32| (x * 10.0).round() / 10.0;
        let st = &a.stereo;
        Ok(json!({
            "rate": a.rate,
            "correlation": (st.correlation * 1000.0).round() / 1000.0,
            "balance_db": f(st.balance_db),
            "width_db": f(st.width_db),
            "rms": [f(st.rms_dbfs[0]), f(st.rms_dbfs[1])],
            "peak": [f(st.peak_dbfs[0]), f(st.peak_dbfs[1])],
            "crest": [f(st.crest_db[0]), f(st.crest_db[1])],
            "dc": [f(st.dc_dbfs[0]), f(st.dc_dbfs[1])],
            "mid": f(st.mid_dbfs),
            "side": f(st.side_dbfs),
            "bands": a.rta.centres.iter().map(|c| c.round()).collect::<Vec<_>>(),
            "fast": a.rta.fast.iter().copied().map(r).collect::<Vec<_>>(),
            "slow": a.rta.slow.iter().copied().map(r).collect::<Vec<_>>(),
            "hold": a.rta.peak.iter().copied().map(r).collect::<Vec<_>>(),
            // x, y pairs as i16 (1 = 10000), packed: JSON floats cost ~4 ms here.
            "scope16": patch_core::pack::pack_i16(&a.scope.iter().flat_map(|p| [(p[0] * 1e4).round().clamp(-32768.0, 32767.0) as i16, (p[1] * 1e4).round().clamp(-32768.0, 32767.0) as i16]).collect::<Vec<_>>()),
        }))
}
