//! Diagnostics: what is in sight, frame marks, heap traces, performance and diagnostics surfaces.

use super::*;

impl AudioApp {
    /// Worth updating this frame: the window presented a frame lately, or
    /// (out of sight, on a hidden workspace) a few times a second, which
    /// keeps a redraw pending so it draws the moment it comes into view.
    /// Whether a window can be seen now, so its animations and images are
    /// worth drawing. With Hyprland: its window is on a visible workspace
    /// (from Hyprland's events). Else a guess from frames: presented in the
    /// last 250 ms, and every 15th frame (a hidden window never presents,
    /// so it would never start again).
    pub(super) fn in_sight(&self, w: &PatchWindow) -> bool {
        use slint::ComponentHandle as _;
        // Headless shots are never on screen, and always looked at.
        if patch_host::shot::active() {
            return true;
        }
        if let Some(seen) = patch_host::hypr::visible_titles()
            && let Ok(titles) = seen.read()
        {
            return titles.contains(w.global::<Host>().get_window_title().as_str());
        }
        let Some(at) = self
            .presented
            .borrow()
            .get(&(w.window() as *const _ as usize))
            .map(|c| c.get())
        else {
            return true;
        };
        at.elapsed() < Duration::from_millis(250) || self.frame_no.get().is_multiple_of(15)
    }

    /// PATCH_TRACE_FRAMES: time since the previous mark goes to `label`.
    pub(super) fn mark(&self, label: &'static str) {
        if !self.prof_on {
            return;
        }
        if self.heap_trace.is_some() {
            let h = patch_host::heap::in_use_mb();
            let d = h - self.heap_at.replace(h);
            let l = if label.is_empty() {
                "between updates (drawing, input)"
            } else {
                label
            };
            let mut by = self.heap_by.borrow_mut();
            match by.iter_mut().find(|(x, _)| *x == l) {
                Some(e) => e.1 += d,
                None => by.push((l, d)),
            }
        }
        let now = Instant::now();
        if let (Some(at), false) = (self.prof_at.replace(Some(now)), label.is_empty()) {
            let mut p = self.prof.borrow_mut();
            match p.iter_mut().find(|(l, _)| *l == label) {
                Some(e) => e.1 += now - at,
                None => p.push((label, now - at)),
            }
        }
    }

    /// Every 10 s: the heap, and each step's net growth since (≥ 0.2 MB).
    pub(super) fn write_heap_trace(&self) {
        let Some(path) = &self.heap_trace else { return };
        if self
            .heap_written
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        {
            return;
        }
        self.heap_written.set(Some(Instant::now()));
        let mut by = std::mem::take(&mut *self.heap_by.borrow_mut());
        by.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        let steps = by
            .iter()
            .filter(|(_, d)| d.abs() >= 0.2)
            .map(|(l, d)| format!("{l} {d:+.1}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let (used, free) = patch_host::heap::stats().unwrap_or_default();
        let line = format!(
            "{:>6.0} s  heap {used} MB (free {free})  {steps}\n",
            self.start.elapsed().as_secs_f64()
        );
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// The second's biggest update costs, and a fresh start.
    pub(super) fn prof_report(&self) -> String {
        let mut p = std::mem::take(&mut *self.prof.borrow_mut());
        p.sort_by_key(|x| std::cmp::Reverse(x.1));
        p.iter()
            .take(8)
            .map(|(l, d)| format!("{l} {:.1}", d.as_secs_f64() * 1e3))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The Performance window: measure only while one is open.
    pub(super) fn refresh_perf(&self) {
        let wins: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.get_surface() == "perf" && self.in_sight(w))
            .collect();
        self.perf.set_open(!wins.is_empty());
        let others: Vec<String> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .map(|w| w.get_surface().to_string())
            .filter(|s| s != "perf" && s != "controller")
            .collect();
        if !wins.is_empty() && self.perf.update(self.frame_worst_ms.get(), others) {
            for w in &wins {
                self.perf.apply(&w.global::<PerfState>());
            }
        }
    }

    pub(super) fn bind_perf(&self, w: &PatchWindow) {
        let s = w.global::<PerfState>();
        s.set_tiles(self.perf.tiles.clone().into());
        s.set_procs(self.perf.procs.clone().into());
        s.set_nodes(self.perf.nodes.clone().into());
        self.perf.apply(&s);
        let me = self.me.clone();
        s.on_record_baseline(move || {
            if let Some(a) = me.upgrade() {
                a.perf.record_baseline();
            }
        });
        let me = self.me.clone();
        s.on_clear_baseline(move || {
            if let Some(a) = me.upgrade() {
                a.perf.clear_baseline();
            }
        });
        let me = self.me.clone();
        s.on_copy_report(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(text) = a.perf.report() else { return };
            let ok = std::process::Command::new("wl-copy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut c| {
                    use std::io::Write;
                    c.stdin
                        .take()
                        .map(|mut i| i.write_all(text.as_bytes()))
                        .transpose()?;
                    Ok(())
                })
                .is_ok();
            *a.toast.borrow_mut() = if ok {
                "performance report copied".into()
            } else {
                "wl-copy is not installed".into()
            };
            a.push_toast();
        });
    }

    /// What an empty Diagnostics window says instead of its rows.
    fn diag_note(&self, note: &str) {
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            if w.get_surface() == "diagnostics" {
                let s = w.global::<DiagState>();
                if s.get_note() != note {
                    s.set_note(note.into());
                }
            }
        }
    }

    /// Refresh the Diagnostics window (only while one is open).
    pub(super) fn refresh_diag(&self) {
        // Only for a Diagnostics window you can see: four engine round
        // trips at the text rate were spent on a hidden one.
        let open = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "diagnostics" && self.in_sight(&w));
        if !open {
            return;
        }
        if !self.is_live() {
            self.diag_note("The Patch service is not running · start it with: omarchy-patch engine start");
            return;
        }
        self.diag_polls
            .request_with("diag", serde_json::Value::Null, |_| {
                let [status, clock] = crate::diag::requests();
                let status = patch_engine::client::call(&status)?;
                let clock = patch_engine::client::call(&clock)?;
                let a = patch_engine::client::call(
                    &serde_json::json!({"op": "deck_status", "deck": "a"}),
                )?;
                let b = patch_engine::client::call(
                    &serde_json::json!({"op": "deck_status", "deck": "b"}),
                )?;
                Ok(serde_json::json!({"status": status, "clock": clock, "a": a, "b": b}))
            });
        if let Some(reply) = self.diag_polls.take("diag") {
            match reply.result {
                Ok(v) => *self.diag_snapshot.borrow_mut() = v,
                Err(e) => self.diag_note(&format!("Couldn't read the Patch service: {e}")),
            }
        }
        if self.diag_snapshot.borrow().is_null() {
            return;
        }
        let snapshot = self.diag_snapshot.borrow();
        let (status, clock) = (&snapshot["status"], &snapshot["clock"]);
        let decks = vec![
            ("a".to_string(), snapshot["a"].clone()),
            ("b".to_string(), snapshot["b"].clone()),
        ];
        let timing = self.last_timing.borrow().clone();
        let level = timing
            .as_ref()
            .map(|t| latency_level(t.level))
            .unwrap_or("—");
        let reference = match timing.as_ref().map(|t| t.reference) {
            Some(MeterReference::Output) => "OUTPUT (what you hear)",
            Some(MeterReference::Engine) => "ENGINE (not delayed)",
            None => "—",
        };
        let rows = crate::diag::collect(&crate::diag::Inputs {
            status,
            clock,
            decks: &decks,
            latency_ms: timing.as_ref().map(|t| t.latency_ms),
            latency_level: level,
            trim_ms: timing.as_ref().map(|t| t.trim_ms).unwrap_or(0.0),
            reference,
            analysis_queue: self
                .analyst
                .as_ref()
                .map(|a| a.pending.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(0),
            engine_cpu: status["pid"]
                .as_u64()
                .and_then(|pid| self.cpu_engine.percent(pid as u32)),
            ui_cpu: self.cpu_ui.percent(std::process::id()),
            ui_frame_ms: self.frame_worst_ms.get(),
        });
        let rows: Vec<DiagRow> = rows
            .into_iter()
            .map(|r| DiagRow {
                key: r.key.into(),
                value: r.value.into(),
                warn: r.warn,
            })
            .collect();
        // In place: same length → update rows (no rebuild flicker).
        if self.diag_rows.row_count() == rows.len() {
            for (i, r) in rows.into_iter().enumerate() {
                if self.diag_rows.row_data(i).as_ref() != Some(&r) {
                    self.diag_rows.set_row_data(i, r);
                }
            }
        } else {
            self.diag_rows.set_vec(rows);
        }
    }
}
