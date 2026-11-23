//! Deck requests and their JSON, channel and number fields, and the stem bridge.

use super::*;

/// A channel's EQ for status: band dB (null = kill) and filter knob.
pub(super) fn eq_json(p: &Params, c: usize) -> Value {
    let e = p.eq_of(c);
    let db = |g: f32| if g > 0.0 { Some(linear_to_db(g)) } else { None };
    json!({ "low_db": db(e.low), "mid_db": db(e.mid), "high_db": db(e.high), "filter": e.filter })
}

/// Deck by letter ("a" = the first native deck).
pub(super) fn deck_index(p: &Params, v: &Value) -> Result<usize, String> {
    let d = v["deck"].as_str().ok_or("no deck")?;
    let i = match d {
        "a" => 0,
        "b" => 1,
        "c" => 2,
        "d" => 3,
        _ => return Err("deck must be a, b, c, or d".into()),
    };
    if i < p.decks.len() {
        Ok(i)
    } else {
        Err(format!("no deck {d}"))
    }
}

/// The settings each deck's sample maker used last.
static SAMPLE_SETTINGS: Mutex<Vec<crate::deck_sample::Settings>> = Mutex::new(Vec::new());

/// A deck's sample maker (see [`crate::deck_sample`]): what it would cut
/// and what was made from the track (`status`), or a new sample (`save`).
pub(super) fn op_deck_sample(s: &Shared, req: &Value) -> Result<Value, String> {
    use crate::deck_sample as ds;
    let p = &*s.params;
    let d = deck_index(p, req)?;
    let st = p.decks[d].status.lock().unwrap().clone();
    if !st.loaded || st.path.is_empty() {
        return Err("load a track to make a sample from it".into());
    }
    let track = st.path.to_string();
    let was = {
        let mut all = SAMPLE_SETTINGS.lock().unwrap();
        if all.len() < p.decks.len() {
            all.resize(p.decks.len(), ds::Settings::default());
        }
        all[d].clone()
    };
    let default_name = ds::default_name(&st.title, &track);
    let grid = st.grid.map(|g| ds::Grid { bpm: g.bpm, offset: g.offset, downbeat: g.downbeat.unwrap_or(0) });
    let looped = st.loop_in.zip(st.loop_out).filter(|(a, b)| b > a);
    let loop_beats = looped.and_then(|(a, b)| grid.map(|g| ((b - a) * g.bpm / 60.0 * 1000.0).round() / 1000.0));
    let made = |m: &[ds::Made]| -> Vec<Value> {
        m.iter()
            .map(|m| json!({"name": m.name, "n": m.n, "files": m.files.len(), "label": m.label, "code": m.code, "seconds": m.seconds}))
            .collect()
    };
    match req["action"].as_str().unwrap_or("status") {
        "status" => Ok(json!({
            "track": track,
            "default_name": default_name,
            "bpm": grid.map(|g| g.bpm),
            "settings": was,
            "stems": ds::stems_ready(&track),
            "loop_beats": loop_beats,
            "made": made(&ds::made_from(&track, 12)),
        })),
        "save" => {
            if req["track"].as_str().is_some_and(|expected| expected != track) {
                return Err("the deck's track changed: close and reopen Sample".into());
            }
            let settings = ds::settings_from(req, &was)?;
            let position = s
                .history
                .lock()
                .unwrap()
                .back()
                .map(|(_, b)| b.deck_pos[d] / b.stamp.rate.max(1) as f64)
                .filter(|x| x.is_finite())
                .ok_or("the deck's position isn't known yet")?;
            let cut = ds::region(settings.what, grid, looped, position)?;
            let name = if settings.name.is_empty() { default_name.clone() } else { settings.name.clone() };
            let m = ds::make(&track, &settings, &name, cut)?;
            SAMPLE_SETTINGS.lock().unwrap()[d] = settings;
            Ok(json!({
                "saved": made(std::slice::from_ref(&m))[0],
                "made": made(&ds::made_from(&track, 12)),
            }))
        }
        other => Err(format!("deck_sample has no action '{other}' (status, save)")),
    }
}

pub(super) fn deck_op(p: &Params, req: &Value) -> Result<Value, String> {
    use crate::deck::transport::{Cmd, Grid};
    let i = deck_index(p, req)?;
    let sh = &p.decks[i];
    let num = |k: &str| {
        req[k]
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| format!("'{k}' must be a number"))
    };
    let flag = |k: &str| {
        req[k]
            .as_bool()
            .ok_or_else(|| format!("'{k}' must be true or false"))
    };
    let cmd = match req["do"].as_str().unwrap_or("") {
        "load" => {
            let path = req["path"].as_str().ok_or("load needs a path")?;
            let rate = match p.last_rate.load(Ordering::Relaxed) {
                0 => 48_000,
                r => r,
            };
            sh.rate.store(rate, Ordering::Relaxed);
            Cmd::Load(crate::deck::pcm::Pcm::load(
                std::path::Path::new(path),
                rate,
            )?)
        }
        "eject" => {
            p.set_grid(i, None, 48_000);
            p.set_grid_downbeat(i, None);
            Cmd::Eject
        }
        "reverse" => Cmd::Reverse(flag("on")?),
        "scratch" => {
            let seconds = num("seconds")?;
            let velocity = num("velocity")?;
            if !seconds.is_finite()
                || seconds < 0.0
                || !velocity.is_finite()
                || !(-4.0..=4.0).contains(&velocity)
            {
                return Err("Invalid scratch position/velocity".into());
            }
            Cmd::Scratch { seconds, velocity }
        }
        "scratch_end" => Cmd::ScratchEnd,
        "play" => Cmd::Play,
        "pause" => Cmd::Pause,
        "toggle" => Cmd::Toggle,
        "cue" => Cmd::Cue,
        "cue_release" => Cmd::CueRelease,
        "gate_cue" => Cmd::GateCue(num("seconds")?),
        // Headphones only: {"seconds": 42.0} starts, no seconds stops.
        "audition" => Cmd::Audition(req["seconds"].as_f64().filter(|s| s.is_finite() && *s >= 0.0)),
        "seek" => Cmd::Seek(num("seconds")?),
        "tempo" => Cmd::Tempo(num("value")?),
        "bend" => Cmd::Bend(num("value")?),
        "keylock" => Cmd::Keylock(flag("on")?),
        "key" => Cmd::Key(num("semitones")? as i32),
        "grid" => {
            let grid = match (req["bpm"].as_f64(), req["offset"].as_f64()) {
                (Some(bpm), Some(offset)) if bpm > 20.0 && bpm < 400.0 => Some(Grid {
                    bpm,
                    offset,
                    downbeat: req["downbeat"].as_u64().filter(|d| *d < 4).map(|d| d as u8),
                }),
                _ => None,
            };
            // Beat FX lock to the same grid.
            let rate = match p.last_rate.load(Ordering::Relaxed) {
                0 => 48_000,
                r => r,
            };
            p.set_grid(i, grid.map(|g| (g.bpm, g.offset)), rate);
            p.set_grid_downbeat(i, grid.and_then(|g| g.downbeat));
            Cmd::Grid(grid)
        }
        "loop" => Cmd::AutoLoop(num("beats")?.clamp(1.0 / 32.0, 128.0)),
        "loop_in" => Cmd::LoopIn,
        "loop_out" => Cmd::LoopOut,
        "loop_exit" => Cmd::LoopExit,
        "reloop" => Cmd::Reloop,
        "loop_set" => {
            let from = num("from")?;
            let to = num("to")?;
            if from < 0.0 || to <= from || to > 86_400.0 {
                return Err("invalid loop bounds".into());
            }
            Cmd::LoopSet {
                from,
                to,
                play: flag("play")?,
            }
        }
        "loop_adjust" => Cmd::LoopAdjust {
            out: flag("out")?,
            seconds: num("seconds")?.clamp(-10.0, 10.0),
        },
        "censor" => Cmd::Censor(flag("on")?),
        "cue_recall" => Cmd::RecallCue(num("seconds")?),
        "loop_scale" => Cmd::LoopScale(num("factor")?.clamp(0.25, 4.0)),
        "jump" => Cmd::Jump(num("beats")?.clamp(-512.0, 512.0)),
        "slip" => Cmd::Slip(flag("on")?),
        "quantize" => Cmd::Quantize(flag("on")?),
        other => return Err(format!("unknown deck action '{other}'")),
    };
    sh.send(cmd);
    Ok(json!("ok"))
}

pub(super) fn deck_status(p: &Params, i: usize) -> Value {
    deck_status_with(p, i, false)
}

/// The deck's status; `take_fill`: this read starts a new `fill_min`
/// window. Only the deck status request takes it: the engine's own readers
/// (Auto Mix 4×/s, controller feedback 33×/s) emptied it before anyone saw
/// how close a deck came to an underrun.
pub(super) fn deck_status_with(p: &Params, i: usize, take_fill: bool) -> Value {
    let sh = &p.decks[i];
    let st = sh.status.lock().unwrap().clone();
    let rate = sh.rate.load(Ordering::Relaxed).max(1) as f64;
    let (pos, step) = sh.playhead();
    // Deck status is polled frequently by each open deck/Booth surface. Keep
    // this response to identity only; waveform/activity summaries belong in
    // the explicit stems-status request, not every transport snapshot.
    let stem_set = {
        let set = sh.stem_set.lock().unwrap().clone();
        set.map(|set| json!({"id": set.id, "track_id": set.track_id}))
    };
    let stem_error = sh.stem_error.lock().unwrap().clone();
    json!({
        "stems":sh.stems.snapshot(),
        "stem_set":stem_set,
        "stem_error":stem_error.as_deref(),
        "loaded": st.loaded,
        "path": st.path.as_ref(),
        "load": sh.loads.load(Ordering::Acquire),
        "title": st.title.as_ref(),
        "artist": st.artist.as_ref(),
        "codec": st.codec.as_ref(),
        "native_rate": st.native_rate,
        "bits": st.bits,
        "duration": st.duration,
        "decoded": st.decoded,
        "decode_done": st.decode_done,
        "decoded_audio": st.decoded_audio,
        "decode_error": st.decode_error.as_deref(),
        "position": pos / rate,
        "step": step,
        "playing": st.playing,
        "scheduled_start_sample": st.scheduled_start_sample,
        "end": st.end,
        "tempo": st.tempo,
        "bend": st.bend,
        "keylock": st.keylock,
        "key": st.key,
        "loop_in": st.loop_in,
        "loop_out": st.loop_out,
        "loop_on": st.loop_on,
        "slip": st.slip,
        "reverse": st.reverse,
        "censor": st.censor,
        "quantize": st.quantize,
        "cue_point": st.cue_point,
        "previewing": st.previewing,
        "auditioning": sh.auditioning.load(Ordering::Relaxed),
        "grid": st.grid.map(|g| json!({"bpm": g.bpm, "offset": g.offset, "downbeat": g.downbeat})),
        "underruns": sh.underruns.load(Ordering::Relaxed),
        "underruns_total": sh.underruns_total.load(Ordering::Relaxed),
        // Fewest frames rendered ahead since the last status (null: not
        // playing meanwhile): how close the deck came to an underrun.
        "fill_min": match if take_fill {
            sh.fill_min.swap(usize::MAX, Ordering::Relaxed)
        } else {
            sh.fill_min.load(Ordering::Relaxed)
        } {
            usize::MAX => None,
            n => Some(n),
        },
    })
}

pub(super) fn channel(p: &Params, v: &Value) -> Result<usize, String> {
    let name = v["channel"].as_str().ok_or("no channel")?;
    p.channel(name)
        .ok_or_else(|| format!("no channel '{name}'"))
}

pub(super) fn number(v: &Value, key: &str) -> Result<f32, String> {
    v[key]
        .as_f64()
        .map(|x| x as f32)
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("'{key}' must be a number"))
}

/// Which implementation each kernel uses, on what CPU, from which profile.
pub(super) fn backend_once() -> String {
    static B: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    B.get_or_init(crate::backend).clone()
}

/// The kernels can't change while the engine runs: worked out once.
pub(super) fn kernels_json() -> Value {
    static K: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    K.get_or_init(kernels_json_now).clone()
}

pub(super) fn kernels_json_now() -> Value {
    let t = patch_dsp::registry::table();
    let cpu = patch_dsp::cpu::this();
    let ops: serde_json::Map<String, Value> = patch_dsp::abi::Op::ALL
        .iter()
        .map(|op| (op.name().to_string(), json!(t.get(*op).name)))
        .collect();
    json!({
        "cpu": cpu.model,
        "arch": cpu.arch,
        "features": cpu.features.iter().map(|f| f.name()).collect::<Vec<_>>(),
        "selected": ops,
        "profile": patch_dsp::profile::load().is_some(),
        "tier_override": std::env::var("PATCH_DSP").ok(),
    })
}

pub(super) fn fx_json(p: &Params, c: usize) -> Value {
    let f = p.fx_of(c);
    let chain: Vec<_> = f.kinds().map(|k| k.name()).collect();
    json!({"kind": f.kind.name(), "chain": chain, "on": f.on, "beats": f.beats, "depth": f.depth})
}

pub(super) fn load_pan(p: &Params, c: usize) -> f32 {
    f32::from_bits(p.pan[c].load(Ordering::Relaxed))
}

pub(super) fn set_node_name(s: &Shared, name: &str) {
    let h = s.handle.load(Ordering::Acquire);
    if let (false, Ok(c)) = (h.is_null(), CString::new(name)) {
        // SAFETY: the handle is valid until patch_engine_run returns.
        unsafe { patch_engine_set_name(h, c.as_ptr()) };
    }
}

pub(super) fn op_is(req: &Value, op: &str) -> bool {
    req["op"].as_str() == Some(op)
}

pub(super) fn bridge_source_field(req: &Value) -> Result<Option<usize>, String> {
    match req.get("source") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) if s == "a" => Ok(Some(0)),
        Some(Value::String(s)) if s == "b" => Ok(Some(1)),
        Some(Value::String(s)) => Err(format!(
            "unknown stem bridge source '{s}' (expected 'a' or 'b')"
        )),
        Some(_) => Err("stem bridge source must be 'a' or 'b'".into()),
    }
}

pub(super) fn bridge_stem_field(req: &Value) -> Result<Option<usize>, String> {
    match req.get("stem") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => match s.as_str() {
            "vocals" => Ok(Some(0)),
            "drums" => Ok(Some(1)),
            "bass" => Ok(Some(2)),
            "other" => Ok(Some(3)),
            _ => Err(format!(
                "unknown stem bridge stem '{s}' (expected vocals, drums, bass, or other)"
            )),
        },
        Some(_) => Err("stem bridge stem must be a stem ID".into()),
    }
}

pub(super) fn handle_stem_bridge(p: &Params, req: &Value) -> Result<Value, String> {
    let source = bridge_source_field(req)?;
    let stem = bridge_stem_field(req)?;
    let on = match req.get("on") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(v)) => Some(*v),
        Some(_) => return Err("stem bridge 'on' must be a boolean".into()),
    };
    let fx = match req.get("fx") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(v)) => Some(*v),
        Some(_) => return Err("stem bridge 'fx' must be a boolean".into()),
    };
    let depth = match req.get("depth") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v
                .as_f64()
                .ok_or("stem bridge FX depth must be a number from 0 to 1")?
                as f32;
            if !n.is_finite() || !(0.0..=1.0).contains(&n) {
                return Err("stem bridge FX depth must be a number from 0 to 1".into());
            }
            Some(n)
        }
    };

    let will_be_on = on.unwrap_or_else(|| p.stem_bridge_on.load(Ordering::Acquire));
    if fx == Some(true) && !will_be_on {
        return Err("enable a stem send before enabling its FX".into());
    }
    if on == Some(true) {
        let source = source.ok_or("stem bridge activation requires source 'a' or 'b'")?;
        let stem = stem.ok_or("stem bridge activation requires a known stem ID")?;
        p.set_stem_bridge(source, stem, true);
    } else if on == Some(false) {
        p.stem_bridge_on.store(false, Ordering::Release);
    }
    if let Some(fx) = fx {
        p.set_stem_bridge_fx(fx);
    }
    if let Some(depth) = depth {
        p.set_stem_bridge_depth(depth)?;
    }

    let on = p.stem_bridge_on.load(Ordering::Acquire);
    let source = p.stem_bridge_source.load(Ordering::Acquire) as usize;
    let stem = p.stem_bridge_stem.load(Ordering::Acquire) as usize;
    let fx = p.stem_bridge_fx.load(Ordering::Acquire);
    let depth = f32::from_bits(p.stem_bridge_depth.load(Ordering::Acquire));
    let stem_name = ["vocals", "drums", "bass", "other"][stem.min(3)];
    Ok(
        json!({"on":on,"fx":fx,"depth":depth,"source":if on {if source==0{"a"}else{"b"}} else {""},"stem":if on {stem_name} else {""},"destination":if on {if source==0{"b"}else{"a"}} else {""}}),
    )
}

#[cfg(test)]
#[test]
fn deck_index_requires_an_exact_available_deck_letter() {
    let p = Params::new(&["bus", "deck-a", "deck-b", "deck-c", "deck-d"]);
    for (letter, expected) in [("a", 0), ("b", 1), ("c", 2), ("d", 3)] {
        assert_eq!(deck_index(&p, &json!({"deck":letter})).unwrap(), expected);
    }
    for invalid in ["banana", "deck-a", "e", ""] {
        assert!(deck_index(&p, &json!({"deck":invalid})).is_err());
    }
    let ab_only = Params::new(&["bus", "deck-a", "deck-b"]);
    assert!(deck_index(&ab_only, &json!({"deck":"c"})).is_err());
}

#[cfg(test)]
#[test]
fn eject_is_a_deck_op_and_needs_an_available_deck() {
    let p = Params::new(&["bus", "deck-a", "deck-b"]);
    assert!(deck_op(&p, &json!({"deck":"a","do":"eject"})).is_ok());
    assert!(deck_op(&p, &json!({"deck":"c","do":"eject"})).is_err());
}

#[cfg(test)]
#[test]
fn stem_bridge_rejects_unknown_and_missing_ids_without_arming_a_route() {
    let p = Params::new(&["bus", "deck-a", "deck-b"]);
    assert!(handle_stem_bridge(&p, &json!({"on":true,"source":"c","stem":"bass"})).is_err());
    assert!(handle_stem_bridge(&p, &json!({"on":true,"source":"a","stem":"synth"})).is_err());
    assert!(handle_stem_bridge(&p, &json!({"on":true,"stem":"bass"})).is_err());
    assert!(handle_stem_bridge(&p, &json!({"on":true,"source":"a"})).is_err());
    assert!(!p.stem_bridge_on.load(Ordering::Acquire));
}

#[cfg(test)]
#[test]
fn stem_bridge_reports_and_validates_independent_fx_depth() {
    let p = Params::new(&["bus", "deck-a", "deck-b"]);
    let active = handle_stem_bridge(&p, &json!({"on":true,"source":"a","stem":"bass"})).unwrap();
    assert_eq!(active["fx"], false);
    assert_eq!(active["depth"], 0.5);
    let with_fx = handle_stem_bridge(&p, &json!({"fx":true,"depth":0.82})).unwrap();
    assert_eq!(with_fx["fx"], true);
    assert!((with_fx["depth"].as_f64().unwrap() - 0.82).abs() < 1e-6);
    assert!(handle_stem_bridge(&p, &json!({"depth":1.1})).is_err());
    assert!((f32::from_bits(p.stem_bridge_depth.load(Ordering::Acquire)) - 0.82).abs() < 1e-6);
}

/// What a deck is doing, for auto-mix: (loaded, playing, path, seconds,
/// tempo, key shift).
pub(super) fn deck_now(p: &Params, d: usize) -> (bool, bool, String, f64, f64, i32) {
    // Read straight from the deck, as `deck_status` reports them (a number
    // that isn't finite reads 0, as its JSON null did). Not through
    // `deck_status`: the Auto Mix tick and the controllers call this many
    // times a second, and a status read also takes the deck's `fill_min`
    // (the closest call to an underrun) that diagnostics report.
    let Some(sh) = p.decks.get(d) else {
        return (false, false, String::new(), 0.0, 0.0, 0);
    };
    let (loaded, playing, path, tempo, key) = {
        let st = sh.status.lock().unwrap();
        (st.loaded, st.playing, st.path.to_string(), st.tempo, st.key)
    };
    let rate = sh.rate.load(Ordering::Relaxed).max(1) as f64;
    let finite = |x: f64| if x.is_finite() { x } else { 0.0 };
    (loaded, playing, path, finite(sh.playhead().0 / rate), finite(tempo), key)
}

