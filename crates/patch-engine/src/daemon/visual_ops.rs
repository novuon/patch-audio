//! Visual and pattern requests: the visual intent and scene cues, the sequencer, patterns.

use super::*;

pub(super) fn op_pattern(s: &Shared, req: &Value) -> Result<Value, String> {
        let r = s.pattern.request(s, req)?;
        if req["action"] == "play" {
            wake(s);
        }
        Ok(r)
}

pub(super) fn op_tidal(s: &Shared, req: &Value) -> Result<Value, String> {
        let r = s.tidal.request(&TidalHost(s), req)?;
        if matches!(req["action"].as_str(), Some("play" | "eval")) {
            wake(s);
        }
        Ok(r)
}

pub(super) fn op_visual_intent(s: &Shared, req: &Value) -> Result<Value, String> {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let result = s.visual_intent.request(req, ms)?;
        if req["action"] == "clear" {
            let mut cue = s.visual_scene.lock().map_err(|_| "visual cue state unavailable")?;
            *cue = serde_json::json!({"sequence":cue["sequence"].as_u64().unwrap_or(0).wrapping_add(1),"clear":true});
        }
        Ok(result)
}

pub(super) fn op_visual_scene_cue(s: &Shared, req: &Value) -> Result<Value, String> {
        let action = req["action"].as_str().unwrap_or("get");
        let mut cue = s.visual_scene.lock().map_err(|_| "visual cue state unavailable")?;
        match action {
            "get" => Ok(cue.clone()),
            "call" => {
                let part = req["part"].as_str().filter(|p| matches!(*p, "text" | "groove" | "lift" | "calm")).ok_or("invalid choreography part")?;
                let index = req["index"].as_u64().filter(|i| *i < 64).ok_or("invalid scene index")?;
                let sequence = cue["sequence"].as_u64().unwrap_or(0).wrapping_add(1);
                *cue = serde_json::json!({"sequence":sequence,"part":part,"index":index,"requested_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d|d.as_millis() as u64)});
                Ok(cue.clone())
            }
            "clear" => { *cue = serde_json::json!({"sequence":cue["sequence"].as_u64().unwrap_or(0).wrapping_add(1),"clear":true}); Ok(cue.clone()) }
            _ => Err("invalid visual scene action".into()),
        }
}

pub(super) fn op_sequencer(s: &Shared, req: &Value) -> Result<Value, String> {
        let result = s.params.sequencer.request(req)?;
        if s.params.sequencer.playing.load(Ordering::Relaxed) { wake(s); }
        Ok(result)
}

pub(super) fn op_sequencer_sync(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let on = req["on"].as_bool().unwrap_or(false);
        let source = match req["source"].as_str().unwrap_or("a") {
            "b" => 1u32,
            _ => 0u32,
        };
        s.params.sequencer.set_sync(on, source);
        if on {
            let deck_idx = source as usize;
            let (grid, tempo) = {
                let st = p.decks[deck_idx].status.lock().unwrap();
                (st.grid, st.tempo)
            };
            if let Some(g) = grid {
                s.params.sequencer.set_bpm((g.bpm * (1.0 + tempo)).round() as u32);
            }
        }
        Ok(json!(s.params.sequencer.sync_on()))
}
