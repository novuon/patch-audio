//! The engine's own life: handing over to a successor, fading, quitting.

use super::*;

pub(super) fn op_retire(s: &Shared, _req: &Value) -> Result<Value, String> {
        // The successor reads the session from disk: this one's last
        // move must be there first.
        if let Err(e) = crate::session::flush(Duration::from_secs(2)) {
            eprintln!("engine: session not saved before handover: {e}");
        }
        s.links.lock().unwrap().hand_over_stream_lanes();
        s.retired.store(true, Ordering::Release);
        set_node_name(s, RETIRING);
        std::fs::rename(socket_path(), retiring_socket_path()).map_err(|e| e.to_string())?;
        Ok(json!("ok"))
}

pub(super) fn op_unretire(s: &Shared, _req: &Value) -> Result<Value, String> {
        if UnixStream::connect(socket_path()).is_ok() {
            return Err("another engine owns the socket".into());
        }
        std::fs::rename(retiring_socket_path(), socket_path()).map_err(|e| e.to_string())?;
        set_node_name(s, crate::links::ENGINE);
        s.retired.store(false, Ordering::Release);
        crate::events::kick();
        Ok(json!("ok"))
}

pub(super) fn op_fade(s: &Shared, req: &Value) -> Result<Value, String> {
        let to = db_to_linear(number(req, "db")?.clamp(-120.0, 12.0));
        let ms = req["ms"].as_f64().unwrap_or(150.0).clamp(0.0, 5000.0);
        let params = s.params.clone();
        std::thread::spawn(move || {
            let from = params.master();
            let steps = (ms / 5.0).ceil().max(1.0) as usize;
            let t0 = std::time::Instant::now();
            for i in 1..=steps {
                params.set_master(from + (to - from) * i as f32 / steps as f32);
                let next =
                    Duration::from_micros((ms * 1000.0 * i as f64 / steps as f64) as u64);
                if let Some(wait) = next.checked_sub(t0.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        });
        Ok(json!("ok"))
}

pub(super) fn op_quit(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        if let Err(e) = crate::session::flush(Duration::from_secs(2)) {
            eprintln!("engine: session not saved before quitting: {e}");
        }
        // After a handover the successor plays the bus: no loopback.
        if !req["handover"].as_bool().unwrap_or(false) {
            // The monitor carries the volume again (restore_loopback),
            // then the output stage stops applying it.
            s.links.lock().unwrap().restore_loopback();
            p.system_post.store(false, Ordering::Relaxed);
        }
        let h = s.handle.load(Ordering::Acquire);
        if !h.is_null() {
            // SAFETY: the handle stays valid until patch_engine_run returns,
            // which only happens after this quit.
            unsafe { patch_engine_quit(h) };
        }
        Ok(json!("ok"))
}
