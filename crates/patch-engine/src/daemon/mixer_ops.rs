//! Mixer requests: EQ (channels, applications, master), FX, mutes and the master level.

use super::*;

pub(super) fn op_mute(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let c = channel(p, req)?;
        p.set_mute(
            c,
            req["muted"]
                .as_bool()
                .ok_or("'muted' must be true or false")?,
        );
        Ok(json!("ok"))
}

pub(super) fn op_eq(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let c = channel(p, req)?;
        let mut e = p.eq_of(c);
        for (key, band) in [
            ("low_db", &mut e.low),
            ("mid_db", &mut e.mid),
            ("high_db", &mut e.high),
        ] {
            match req.get(key) {
                Some(Value::Null) => *band = 0.0,
                Some(v) => {
                    *band = crate::eq::band_gain(
                        v.as_f64()
                            .ok_or_else(|| format!("'{key}' must be dB or null"))?
                            as f32,
                    )
                }
                None => {}
            }
        }
        if let Some(f) = req.get("filter") {
            e.filter = f.as_f64().ok_or("'filter' must be −1 … 1")? as f32;
        }
        match due_at() {
            Some(at) => {
                // Only what this request names: a step of one knob's
                // ramp queues one change, not four.
                for (key, kind, v) in [
                    ("low_db", 1, e.low),
                    ("mid_db", 2, e.mid),
                    ("high_db", 3, e.high),
                    ("filter", 4, e.filter),
                ] {
                    if req.get(key).is_some() && !p.post_glide(at, due_glide(), c, kind, v) {
                        p.set_eq(c, e);
                        break;
                    }
                }
            }
            None => p.set_eq(c, e),
        }
        Ok(json!("ok"))
}

pub(super) fn op_eq_apps(s: &Shared, _req: &Value) -> Result<Value, String> {
        let explicit = s.app_eq.lock().unwrap().clone();
        let links = s.links.lock().unwrap();
        let mut assigned = links.stream_lanes.assigned.clone();
        for (app, channel) in explicit {
            if !links.stream_apps.values().any(|name| name.eq_ignore_ascii_case(&app)) {
                assigned.insert(app, channel);
            }
        }
        let p = &s.params;
        let streams: Vec<_> = links.stream_lanes.assigned.iter().filter_map(|(key, ch)| {
            let c = p.channel(ch)?;
            let age = p.stream_signal_age(c).or_else(|| links.stream_lanes.heard.contains(key).then_some(u32::MAX));
            Some((key.clone(), links.browser_helper_candidates.contains(key), age,
                !p.stream_heard_within(c, crate::stream_lanes::QUIET_HOLD_SECONDS),
                p.stream_touched(c) || links.stream_lanes.touched.contains(key)))
        }).collect();
        let hidden = crate::stream_lanes::hidden_helpers(&streams);
        let labels = crate::stream_lanes::display_labels(&links.stream_labels, &hidden);
        // Inventory is maintained by the graph keeper. No Pulse subprocess
        // or heavy query is allowed on this frequently polled request.
        Ok(json!({"assigned": assigned, "labels": labels, "hidden": hidden, "playing": []}))
}

pub(super) fn op_master_eq(s: &Shared, req: &Value) -> Result<Value, String> {
        if req["all"].as_bool() == Some(true) {
            // Fixed targets, and every application that has its own EQ.
            let mut apps: Vec<String> = s.app_eq.lock().unwrap().keys().map(|a| format!("app:{a}")).collect();
            apps.extend(s.links.lock().unwrap().stream_lanes.assigned.keys().map(|a| format!("app:{a}")));
            // The mics that have a channel, as the Mixer shows them (read
            // before the EQ lock: one lock at a time).
            let mics: Vec<String> = s.mics.lock().unwrap().keys().cloned().collect();
            let all = s.eq.lock().unwrap();
            let mut out = serde_json::Map::new();
            for (t, _) in patch_core::eq::TARGETS {
                out.insert(t.to_string(), master_eq_json(&all.get(t).cloned().unwrap_or_default()));
            }
            for a in apps {
                out.insert(a.clone(), master_eq_json(&all.get(&a).cloned().unwrap_or_default()));
            }
            for ch in mics {
                out.insert(ch.clone(), master_eq_json(&all.get(&ch).cloned().unwrap_or_default()));
            }
            return Ok(Value::Object(out));
        }
        let t = eq_target(req["target"].as_str())?;
        match req.get("state") {
            Some(v) => {
                let st: patch_core::eq::EqState = serde_json::from_value(v.clone()).map_err(|e| format!("EQ state: {e}"))?;
                Ok(set_eq_state(s, &t, st))
            }
            None => Ok(master_eq_json(&s.eq.lock().unwrap().get(&t).cloned().unwrap_or_default())),
        }
}

pub(super) fn op_fx(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let c = channel(p, req)?;
        let mut f = p.fx_of(c);
        let kind = |k: &str| {
            crate::fx::Kind::parse(k).ok_or_else(|| {
                let all = crate::fx::Kind::ALL.map(crate::fx::Kind::name).join(", ");
                format!("no effect '{k}' ({all})")
            })
        };
        if let Some(chain) = req["chain"].as_array() {
            let kinds = chain
                .iter()
                .map(|k| kind(k.as_str().unwrap_or_default()))
                .collect::<Result<Vec<_>, _>>()?;
            if kinds.is_empty() || kinds.len() > crate::fx::SLOTS {
                return Err(format!("a chain is 1 … {} effects", crate::fx::SLOTS));
            }
            f.set_kinds(&kinds);
        } else if let Some(k) = req["kind"].as_str() {
            f.set_kinds(&[kind(k)?]);
        }
        if let Some(on) = req["on"].as_bool() {
            f.on = on;
        }
        if let Some(b) = req["beats"].as_f64() {
            f.beats = b as f32;
        }
        if let Some(d) = req["depth"].as_f64() {
            f.depth = d as f32;
        }
        p.set_fx(c, f);
        Ok(fx_json(p, c))
}

pub(super) fn op_master(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        if req.get("db").is_some() {
            p.set_master(db_to_linear(number(req, "db")?.clamp(-120.0, 12.0)));
        }
        if let Some(m) = req["mute"].as_bool() {
            p.master_mute.store(m, Ordering::Relaxed);
        }
        Ok(json!("ok"))
}

/// A bus by name ("aux1", "aux2", "fx1", "fx2") or number.
fn bus_index(req: &Value) -> Result<usize, String> {
    match &req["bus"] {
        Value::String(n) => crate::BUS_NAMES.iter().position(|b| b == n),
        Value::Number(n) => n.as_u64().map(|n| n as usize).filter(|n| *n < crate::BUSES),
        _ => None,
    }
    .ok_or_else(|| "bus is aux1, aux2, fx1 or fx2".into())
}

/// A send's slot: a channel, or the DJ Booth ("dj").
fn send_slot(p: &crate::Params, req: &Value) -> Result<usize, String> {
    if req["channel"] == "dj" {
        return Ok(crate::DJ_SEND);
    }
    channel(p, req)
}

pub(super) fn op_send(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    let slot = send_slot(p, req)?;
    let bus = bus_index(req)?;
    match req.get("db") {
        Some(Value::Null) => p.set_send(slot, bus, 0.0),
        Some(v) => {
            let db = v.as_f64().ok_or("db is a number or null (off)")? as f32;
            p.set_send(slot, bus, if db <= -60.0 { 0.0 } else { crate::db_to_linear(db.min(6.0)) });
        }
        None => {}
    }
    if let Some(pre) = req["pre"].as_bool() {
        p.set_send_pre(slot, bus, pre);
    }
    Ok(json!("ok"))
}

/// Solo-in-place. `channel` is an engine channel, or `"dj"` for the Booth.
pub(super) fn op_solo(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    let on = req["solo"].as_bool().ok_or("solo is true or false")?;
    if req["channel"] == "dj" {
        p.set_dj_solo(on);
        return Ok(json!("ok"));
    }
    p.set_solo(channel(p, req)?, on);
    Ok(json!("ok"))
}

pub(super) fn op_bus(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    let bus = bus_index(req)?;
    // The bus fader is −60…+6 dB. +6 is about 2×, inside the linear cap.
    let db = req["db"].as_f64().map(|db| crate::db_to_linear((db as f32).clamp(-60.0, 6.0)));
    p.set_bus(bus, db, req["mute"].as_bool());
    Ok(json!("ok"))
}

pub(super) fn op_aux_fx(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    let set = |key: &str, a: &std::sync::atomic::AtomicU32, lo: f32, hi: f32| {
        if let Some(v) = req[key].as_f64() {
            a.store((v as f32).clamp(lo, hi).to_bits(), Ordering::Relaxed);
        }
    };
    set("reverb_size", &p.reverb_size, 0.0, 1.0);
    set("delay_ms", &p.delay_ms, 1.0, 2000.0);
    set("delay_feedback", &p.delay_feedback, 0.0, 0.9);
    Ok(json!("ok"))
}

/// A slot's sends for status: per bus its level (dB, null when off) and
/// whether it's pre-fader.
pub(super) fn sends_json(p: &crate::Params, slot: usize) -> Value {
    (0..crate::BUSES)
        .map(|b| {
            let g = p.send_of(slot, b);
            json!({"db": (g > 0.0).then(|| crate::linear_to_db(g)), "pre": p.send_pre_of(slot, b)})
        })
        .collect()
}

/// The buses and the returns' effects, for status.
pub(super) fn buses_json(p: &crate::Params) -> Value {
    let load = |a: &std::sync::atomic::AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
    json!({
        "buses": (0..crate::BUSES).map(|b| {
            let (g, mute) = p.bus_of(b);
            json!({"name": crate::BUS_NAMES[b], "gain_db": crate::linear_to_db(g), "mute": mute})
        }).collect::<Vec<_>>(),
        "dj_sends": sends_json(p, crate::DJ_SEND),
        "aux_fx": {
            "reverb_size": load(&p.reverb_size),
            "delay_ms": load(&p.delay_ms),
            "delay_feedback": load(&p.delay_feedback),
        },
    })
}

pub(super) fn mixer_json(m: &crate::session::Mixer) -> Value {
    json!({
        "xfader": m.xfader,
        "faders": m.faders,
        "faders4": m.faders4(),
        "trims4_db": m.trims4(),
        "gains4_db": m.gains4_db(),
        "auto_gain": m.auto_gain,
        "trims_db": m.trims_db,
        "gains_db": m.gains_db(),
        "assign": m.assign.map(|a| a.id()),
        "curve": m.curve.id(),
        "ai_on_faders": m.ai_on_faders(),
    })
}
