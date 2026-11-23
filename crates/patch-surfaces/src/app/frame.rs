//! The frame tick, its timer, and the scopes.

use super::*;

fn meter_index(order: &[&str], id: &str) -> Option<usize> {
    order.iter().position(|node| *node == id)
}

// Program is measured after summing and master gain; deck maxima cannot
// represent cancellation, FX tails or a muted master.
fn program_levels(m: &metering::Metering) -> [f32; 2] {
    std::array::from_fn(|i| {
        m.output.get(i).map_or(0.0, |b| {
            meter_pos(patch_ui::meter::MeterKind::System, b.level)
        })
    })
}

impl AudioApp {
    pub(super) fn tick(&self, t: f64) {
        self.plugins_tick();
        self.glide_player();
        self.stem_views.borrow_mut().retain(|v| v.alive());
        // The decks' stems as the engine moves them, every frame.
        let stems = self.provider.borrow().dj_stems();
        for view in self.stem_views.borrow().iter() {
            view.tick(t, self.is_live());
            if let Some((g, l)) = stems {
                view.frame(t, g, l);
            }
        }
        self.frame_no.set(self.frame_no.get().wrapping_add(1));
        self.mark("");
        // Let displays catch up after a slow UI frame instead of stretching
        // their release over several subsequent frames.
        let dt = (t - self.last_t.get()).clamp(0.0, 0.5) as f32;
        self.last_t.set(t);
        // Read the newest stamped block before calculating beat positions
        // and drawing waveforms. Those views then use this frame's playhead.
        let frame = self.provider.borrow_mut().meters(t);
        self.mark("read meters");
        // The DJ mixer as the engine has it, every frame (AI crossfades,
        // MIDI, the CLI), unless you're moving it here right now.
        if let Some((x, g)) = self.provider.borrow().dj_mixer()
            && self
                .mixer_touched
                .get()
                .is_none_or(|t| t.elapsed().as_millis() > 400)
        {
            self.xfader.set(x);
            self.deck_gain.set(g);
        }
        // The EQ knobs likewise (the AI DJ's sweeps and cuts), unless
        // you're turning one here right now.
        if let Some(k) = self.provider.borrow().dj_eq()
            && self
                .dj_changed
                .get()
                .is_none_or(|t| t.elapsed().as_millis() > 400)
        {
            self.eq.set(k);
        }
        self.show_eq();
        // Beat phase of both decks, every frame (extrapolated, no IPC).
        let ph = self.beat_phases();
        let bar_beat = |p: f32| {
            if p < 0.0 {
                SharedString::default()
            } else {
                format!(
                    "{}.{}",
                    (p / 4.0).floor() as i64 + 1,
                    (p.floor() as i64).rem_euclid(4) + 1
                )
                .into()
            }
        };
        let (ta, tb) = (bar_beat(ph[0]), bar_beat(ph[1]));
        for w in self.dj_windows_in_sight() {
            let d = w.global::<DjState>();
            d.set_phase_a(ph[0]);
            d.set_phase_b(ph[1]);
            d.set_phase_c(ph[2]); d.set_phase_d(ph[3]);
            d.set_beat_a(ta.clone());
            d.set_beat_b(tb.clone());
            d.set_beat_c(bar_beat(ph[2])); d.set_beat_d(bar_beat(ph[3]));
        }
        let labels: (SharedString, SharedString) = (
            frame.input_label.as_str().into(),
            frame.output_label.as_str().into(),
        );
        let in_sources: SharedString = if frame.input_sources.len() > 1 {
            frame.input_sources.join(" · ").into()
        } else {
            SharedString::new()
        };
        let has_spectrum = !frame.spectrum_db.is_empty();
        let measured_vu = frame.input.vu.is_some();
        let layouts: (SharedString, SharedString) = (
            frame.input.layout.describe().into(),
            frame.output.layout.describe().into(),
        );
        let measured = frame.loudness;
        let mut m = self.metering.borrow_mut();
        m.step(&frame, dt);
        self.mark("ballistics");

        // Strip meters, in place.
        // Clip lamps: brief hold after a sample at or above −0.1 dBFS.
        let mut clips = self.clip_until.borrow_mut();
        clips.resize(frame.strips_db.len() + 2, f64::NEG_INFINITY);
        for (i, db) in frame.strips_db.iter().enumerate() {
            if *db >= -0.1 {
                clips[i] = t + 0.5;
            }
        }
        let n = frame.strips_db.len();
        if frame.input.max_db() >= -0.1 {
            clips[n] = t + 0.5;
        }
        if frame.output.max_db() >= -0.1 {
            clips[n + 1] = t + 0.5;
        }
        let (in_clip, out_clip) = (clips[n] > t, clips[n + 1] > t);
        // Shared models redraw every window bound to them. Out of sight,
        // the Mixer's strips step at 4 Hz (the plugin rack reads their
        // levels) and the Meter's models wait until a Meter is in view.
        let strips_due = self.frame_no.get().is_multiple_of(15)
            || self.surface_in_sight(&["mixer", "inspector", "plugins"]);
        let meters_seen = self.surface_in_sight(&["meters"]);
        // MeterFrame follows provider.strips(), which also contains the
        // hidden Program Mix and per-deck channels. The visible Mixer omits
        // those rows, so never pair its row number with a meter array index.
        let meter_order: Vec<&str> = self
            .provider
            .borrow()
            .strips()
            .iter()
            .map(|s| s.node)
            .collect();
        for i in 0..if strips_due { self.strips.row_count() } else { 0 } {
            if let Some(mut row) = self.strips.row_data(i) {
                let Some(meter_i) = meter_index(&meter_order, row.id.as_str()) else {
                    continue;
                };
                let Some(b) = m.strips.get(meter_i).copied() else {
                    continue;
                };
                // Sides are measured when the provider has them. Otherwise
                // both meters repeat the combined peak (the louder side).
                let sides = m.strips_lr.get(meter_i).copied().unwrap_or([b, b]);
                let kind = strip_meter_kind(&row.id, row.master);
                let (level, peak) = (meter_pos(kind, b.level), meter_pos(kind, b.peak));
                let (level_l, peak_l) = (
                    meter_pos(kind, sides[0].level),
                    meter_pos(kind, sides[0].peak),
                );
                let (level_r, peak_r) = (
                    meter_pos(kind, sides[1].level),
                    meter_pos(kind, sides[1].peak),
                );
                // Measured VU, or none (never estimated from peaks).
                let vu = m
                    .strips_vu
                    .get(meter_i)
                    .copied()
                    .filter(|v| *v > -70.0)
                    .map(|v| meter_pos(kind, v + patch_meter::DEFAULT_REFERENCE_DBFS as f32))
                    .unwrap_or(-1.0);
                // Clip follows the louder side.
                let clip = clips.get(meter_i).is_some_and(|until| *until > t);
                if (row.level - level).abs() > 1e-4
                    || (row.peak - peak).abs() > 1e-4
                    || (row.level_l - level_l).abs() > 1e-4
                    || (row.level_r - level_r).abs() > 1e-4
                    || (row.peak_l - peak_l).abs() > 1e-4
                    || (row.peak_r - peak_r).abs() > 1e-4
                    || (row.vu - vu).abs() > 1e-4
                    || row.clip != clip
                {
                    row.level = level;
                    row.peak = peak;
                    row.level_l = level_l;
                    row.level_r = level_r;
                    row.peak_l = peak_l;
                    row.peak_r = peak_r;
                    row.vu = vu;
                    row.clip = clip;
                    self.strips.set_row_data(i, row);
                }
            }
        }
        drop(clips);
        // The console's buses: level falls 24 dB/s, the peak 8 dB/s.
        {
            let mut held = self.bus_meter.borrow_mut();
            for (i, now) in frame.buses_db.iter().enumerate().take(4) {
                let now = now[0].max(now[1]);
                let h = &mut held[i];
                h[0] = now.max(h[0] - 24.0 * dt);
                h[1] = h[0].max(h[1] - 8.0 * dt);
                if let Some(mut row) = self.buses.row_data(i).filter(|_| strips_due) {
                    let kind = patch_ui::meter::MeterKind::System;
                    let (level, peak) = (meter_pos(kind, h[0]), meter_pos(kind, h[1]));
                    if (row.level - level).abs() > 1e-4 || (row.peak - peak).abs() > 1e-4 {
                        row.level = level;
                        row.peak = peak;
                        self.buses.set_row_data(i, row);
                    }
                }
            }
        }
        let sel = self.selected.borrow().clone();
        let sel_vu = self
            .strips
            .iter()
            .find(|s| s.id.as_str() == sel)
            .map(|s| s.vu)
            .unwrap_or(-1.0);
        let selected_strip = self.strips.iter().find(|s| s.id.as_str() == sel);
        let selected_kind = selected_strip
            .as_ref()
            .map_or(patch_ui::meter::MeterKind::System, |s| {
                strip_meter_kind(&s.id, s.master)
            });
        let sel_level = selected_strip
            .as_ref()
            .and_then(|s| meter_index(&meter_order, s.id.as_str()))
            .and_then(|i| m.strips.get(i))
            .map(|b| meter_pos(selected_kind, b.level))
            .unwrap_or_else(|| {
                m.output
                    .iter()
                    .map(|b| meter_pos(patch_ui::meter::MeterKind::System, b.level))
                    .fold(0.0, f32::max)
            });

        self.mark("strips");
        // Spectrum, in place (replacing the model would rebuild every bar).
        for (i, v) in m.spectrum_pos().enumerate().filter(|_| meters_seen) {
            if i < self.spectrum.row_count() {
                if (self.spectrum.row_data(i).unwrap_or(0.0) - v).abs() > 1e-4 {
                    self.spectrum.set_row_data(i, v);
                }
            } else {
                self.spectrum.push(v);
            }
        }

        self.mark("spectrum");
        // Loudness history: one step every 250 ms, scrolled smoothly between.
        // The step comes first, then the scroll: in the frame a bar is added
        // the offset must already be back at 0, or the graph jumps a step
        // and back. Steps advance in exact 250 ms increments (constant speed).
        if t - self.last_hist.get() >= 0.25 {
            let last = self.last_hist.get();
            self.last_hist
                .set(if t - last > 1.0 { t } else { last + 0.25 });
            let mut h = self.hist_values.borrow_mut();
            let mom = measured.map(|l| l.momentary).unwrap_or(m.lufs_momentary());
            h.push_back(metering::lufs_pos(mom));
            while h.len() > HISTORY {
                h.pop_front();
            }
            // A flat history (silence) looks the same scrolled: it stays
            // still, and the Meter draws nothing new.
            let first = h.front().copied().unwrap_or(0.0);
            self.hist_flat
                .set(h.len() >= HISTORY && h.iter().all(|v| (v - first).abs() < 1e-4));
            let (area, line) = loudness_outline(h.iter().copied());
            self.broadcast_in_sight(|s| {
                s.set_lufs_area(area.as_str().into());
                s.set_lufs_line(line.as_str().into());
            });
            for (i, v) in h.iter().enumerate() {
                if i < self.history.row_count() {
                    if (self.history.row_data(i).unwrap_or(f32::NAN) - v).abs() > 1e-6 {
                        self.history.set_row_data(i, *v);
                    }
                } else {
                    self.history.push(*v);
                }
            }
        }
        let scroll = if self.hist_flat.get() {
            0.0
        } else {
            ((t - self.last_hist.get()) / 0.25).clamp(0.0, 1.0) as f32
        };
        self.broadcast_in_sight(|s| s.set_lufs_scroll(scroll));

        self.mark("history");
        // VU: measured (the engine's VU ballistics, or the mock's); never
        // estimated from peaks.
        // Input is always an absolute digital signal, even when its source is
        // a DJ deck. The performance scale is referenced at -18 dBFS and
        // clipped above +6 dB, which pins mastered deck audio near 0 dBFS.
        let input_kind = patch_ui::meter::MeterKind::System;
        let output_kind = patch_ui::meter::MeterKind::System;
        let vu_pos = |kind: patch_ui::meter::MeterKind, v: f32| {
            meter_pos(kind, v + patch_meter::DEFAULT_REFERENCE_DBFS as f32)
        };
        let vus = |bank: &[metering::Ballistic], lv: &Levels| -> Vec<f32> {
            bank.iter()
                .enumerate()
                .map(|(i, _)| {
                    lv.vu
                        .as_ref()
                        .and_then(|v| v.get(i).copied())
                        .unwrap_or(-120.0)
                })
                .collect()
        };
        let (in_vu, out_vu) = (vus(&m.input, &frame.input), vus(&m.output, &frame.output));
        let max = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        // Per-channel rows, updated in place (a new model would rebuild the bars).
        for (model, bank, lv, vu, kind) in [
            (
                &self.in_channels,
                &m.input,
                &frame.input,
                &in_vu,
                input_kind,
            ),
            (
                &self.out_channels,
                &m.output,
                &frame.output,
                &out_vu,
                output_kind,
            ),
        ] {
            let labels = lv.layout.labels();
            if !meters_seen {
                continue;
            }
            if model.row_count() != bank.len() {
                model.set_vec(Vec::new());
            }
            for (i, b) in bank.iter().enumerate() {
                let row = ChannelLevel {
                    label: labels.get(i).map(String::as_str).unwrap_or("").into(),
                    level: meter_pos(kind, b.level),
                    vu: vu_pos(kind, vu[i]),
                };
                match model.row_data(i) {
                    Some(old) if old == row => {}
                    Some(_) => model.set_row_data(i, row),
                    None => model.push(row),
                }
            }
        }
        let output_sample_peak = m.output.iter().map(|b| b.peak).fold(SILENCE_DB, f32::max);
        let output_peak = patch_ui::meter::PeakReading::sample(output_sample_peak, out_clip);
        let peaks = (
            m.input
                .iter()
                .map(|b| meter_pos(input_kind, b.peak))
                .fold(0.0, f32::max),
            meter_pos(output_kind, output_peak.sample_peak_dbfs),
        );
        // Deck transport metadata is polled more slowly to avoid IPC and
        // analysis work every frame. Push just the live meter fields at the
        // meter tick rate so DJ surfaces have the same cadence as Meter.
        self.mark("vu + levels");
        let deck_levels: [(f32, f32, f32, f32); 4] = std::array::from_fn(|i| {
            Self::deck_meter(
                self.deck_strips.get()[i],
                &m,
                self.deck_gain.get(),
                self.xfader.get(),
                i,
            )
        });
        // Numbers change at 5 Hz so they stay readable.
        let text = (t - self.last_text.get() >= 0.2).then(|| {
            self.last_text.set(t);
            let (mom, integ, tp) = match measured {
                Some(l) => (l.momentary, l.integrated, l.true_peak_max),
                None => (m.lufs_momentary(), m.lufs_integrated(), m.true_peak()),
            };
            (
                metering::lufs_text(mom),
                metering::lufs_text(integ),
                metering::db_text(tp),
                metering::db_text(
                    input_kind
                        .display_db(m.input.iter().map(|b| b.peak).fold(SILENCE_DB, f32::max)),
                ),
                metering::db_text(m.output.iter().map(|b| b.peak).fold(SILENCE_DB, f32::max)),
                metering::vu_text(max(&in_vu)),
                metering::vu_text(max(&out_vu)),
            )
        });
        self.mark("deck meters + text");
        let program = program_levels(&m);
        let input_levels: Vec<f32> = m.deck_inputs.iter().map(|b| meter_pos(patch_ui::meter::MeterKind::System, b[0].level.max(b[1].level))).collect();
        let input_clips: Vec<bool> = m.deck_inputs.iter().map(|b| b[0].peak.max(b[1].peak) >= 0.0).collect();
        let has_input = m.has_deck_inputs;
        drop(m);
        self.draw_law();
        self.mark("crossfader law");
        self.vis_levels
            .set([0, 1].map(|i| deck_levels.get(i).map_or(0.0, |lv| lv.0.max(lv.1))));
        let (xf, gains) = (self.xfader.get(), self.deck_gain.get());
        for w in self.dj_windows_in_sight() {
            let d = w.global::<DjState>();
            // A new model is always "changed" to Slint: replaced every frame,
            // these redrew the Crossfader at 60 fps with nothing playing.
            let trims = self.input_trims.get();
            if !model_is(&d.get_input_trims(), &trims) {
                d.set_input_trims(ModelRc::new(VecModel::from(trims.to_vec())));
            }
            if !model_is(&d.get_input_levels(), &input_levels) {
                d.set_input_levels(ModelRc::new(VecModel::from(input_levels.clone())));
            }
            if !model_is(&d.get_input_clips(), &input_clips) {
                d.set_input_clips(ModelRc::new(VecModel::from(input_clips.clone())));
            }
            d.set_input_meter_live(has_input);
            d.set_program_l(program[0]);
            d.set_program_r(program[1]);
            if (d.get_xfader() - xf).abs() > 1e-4 {
                d.set_xfader(xf);
            }
            if d.get_auto_gain() != self.auto_gain.get() {
                d.set_auto_gain(self.auto_gain.get());
            }
            if (d.get_gain_a() - gains[0]).abs() > 1e-4 || (d.get_gain_b() - gains[1]).abs() > 1e-4
            {
                d.set_gain_a(gains[0]);
                d.set_gain_b(gains[1]);
            }
            d.set_gain_c(gains[2]); d.set_gain_d(gains[3]);
            for (i, lv) in deck_levels.iter().enumerate() {
                let now = DeckLevels {
                    level_l: lv.0,
                    level_r: lv.1,
                    peak: lv.2,
                    vu: lv.3,
                };
                let was = match i { 0 => d.get_levels_a(), 1 => d.get_levels_b(), 2 => d.get_levels_c(), _ => d.get_levels_d() };
                let moved = (was.level_l - now.level_l).abs() > 1e-4
                    || (was.level_r - now.level_r).abs() > 1e-4
                    || (was.peak - now.peak).abs() > 1e-4
                    || (was.vu - now.vu).abs() > 1e-4;
                if moved {
                    match i { 0 => d.set_levels_a(now), 1 => d.set_levels_b(now), 2 => d.set_levels_c(now), _ => d.set_levels_d(now) }
                }
            }
        }
        self.mark("dj windows");
        self.poll_sequencer();
        self.poll_pattern();
        self.poll_tidal();
        let text_phase = &self.frame_loop.text_phase;
        if text.is_some() && text_phase.get().is_none() {
            text_phase.set(Some(0));
        }
        let mut text_mark = "text rate idle";
        if let Some(phase) = text_phase.get() {
            if phase == 0 {
                text_mark = "text rate: diag + perf";
                self.refresh_diag();
                self.refresh_perf();
                // Engine scalars (rate, cycle, DSP load) at the text rate.
                self.broadcast_in_sight(|s| self.push_engine(s));
            } else if phase == 1 {
                text_mark = "text rate: dj + player";
                self.poll_dj();
                self.poll_player();
                self.dj_workspaces.borrow_mut().retain(|v| v.alive());
                self.dj_polls.take("dj-presence");
                let dj_open = !self.dj_workspaces.borrow().is_empty()
                    || self
                        .windows
                        .borrow()
                        .iter()
                        .filter_map(|w| w.upgrade())
                        .any(|w| {
                            matches!(w.get_surface().as_str(), "performer" | "visualiser") || w.get_surface().starts_with("dj")
                                || w.get_surface().starts_with("deck-")
                        });
                let was_open = self.dj_presence_at.get().is_some();
                if dj_open
                    && self
                        .dj_presence_at
                        .get()
                        .is_none_or(|at| at.elapsed().as_secs() >= 1)
                {
                    self.dj_polls.request("dj-presence", serde_json::json!({"op":"module_presence", "module":"dj", "owner":std::process::id()}));
                    self.dj_presence_at.set(Some(Instant::now()));
                } else if !dj_open && was_open {
                    self.dj_presence_at.set(None);
                    self.dj_polls.request("dj-presence", serde_json::json!({"op":"module_presence", "module":"dj", "owner":std::process::id(), "open":false}));
                }
                for view in self.dj_workspaces.borrow().iter() {
                    view.tick(t, self.is_live());
                }
            } else {
                text_mark = "text rate: mics + recorder + broadcast";
                self.poll_mics();
                let recorder_open = self
                    .windows
                    .borrow()
                    .iter()
                    .filter_map(|w| w.upgrade())
                    .any(|w| {
                        w.get_surface() == "recorder"
                            || crate::dj_workspace::is_workspace(w.get_surface().as_str())
                    });
                if recorder_open || self.rec_busy.get() || !self.rec_snapshot.borrow().is_null() {
                    self.poll_recorder();
                } else {
                    self.poll_record_status();
                }
                let broadcast_open = self
                    .windows
                    .borrow()
                    .iter()
                    .filter_map(|w| w.upgrade())
                    .any(|w| matches!(w.get_surface().as_str(), "broadcast" | "performer"))
                    // A Visualiser alone may be a broadcast's picture: asked
                    // only while one runs.
                    || (self
                        .windows
                        .borrow()
                        .iter()
                        .filter_map(|w| w.upgrade())
                        .any(|w| w.get_surface() == "visualiser")
                        && patch_broadcast::daemon::socket_path().exists());
                // Also until a followed broadcast's outcome is in, with
                // every window closed: reopened, it says why it failed.
                if broadcast_open
                    || !self.bc.snapshot.borrow().is_null()
                    || self.bc.followed.get().is_some()
                {
                    self.poll_broadcast();
                }
                // The Network window reads its links once a second, only
                // while one is open.
                if self.windows.borrow().iter().filter_map(|w| w.upgrade()).any(|w| w.get_surface() == "network") {
                    self.poll_network();
                }
            }
            text_phase.set(if phase >= 2 { None } else { Some(phase + 1) });
        }
        self.mark(text_mark);
        let xruns = self.provider.borrow().engine().xruns;
        self.last_timing.replace(frame.timing.clone());
        let timing = frame.timing.as_ref().map(|t| match t.reference {
            MeterReference::Output => (
                format!(
                    "OUTPUT · {:.0} ms · {} · trim {} ms",
                    t.latency_ms,
                    latency_level(t.level),
                    crate::num::signed(t.trim_ms, 0)
                ),
                true,
            ),
            MeterReference::Engine => ("ENGINE · not delayed to the ear".to_string(), false),
        });
        self.broadcast_in_sight(|s| {
            if let Some((text, output)) = &timing {
                s.set_timing_text(text.as_str().into());
                s.set_timing_output(*output);
            }
            s.set_in_peak(peaks.0);
            s.set_out_peak(peaks.1);
            s.set_in_layout(layouts.0.clone());
            s.set_out_layout(layouts.1.clone());
            s.set_sel_level(sel_level);
            s.set_sel_vu(sel_vu);
            s.set_sel_meter_kind(
                if selected_kind == patch_ui::meter::MeterKind::Performance {
                    crate::ui::MeterKind::Performance
                } else {
                    crate::ui::MeterKind::System
                },
            );
            s.set_in_clip(in_clip);
            s.set_out_clip(output_peak.clipping);
            s.set_xruns_known(xruns.is_some());
            s.set_xruns(xruns.unwrap_or(0).min(i32::MAX as u32) as i32);
            s.set_in_label(labels.0.clone());
            s.set_in_sources(in_sources.clone());
            s.set_out_label(labels.1.clone());
            s.set_has_spectrum(has_spectrum);
            s.set_vu_measured(measured_vu);
            s.set_loudness_measured(measured.is_some());
            if let Some((lm, li, tp, ip, op, iv, ov)) = &text {
                if let Some(l) = measured {
                    s.set_lufs_s(metering::lufs_text(l.short_term).as_str().into());
                    s.set_lra(format!("{:.1}", l.range).as_str().into());
                }
                s.set_in_vu_text(iv.as_str().into());
                s.set_out_vu_text(ov.as_str().into());
                s.set_lufs_m(lm.as_str().into());
                s.set_lufs_i(li.as_str().into());
                s.set_true_peak(tp.as_str().into());
                s.set_in_peak_text(ip.as_str().into());
                s.set_out_peak_text(op.as_str().into());
            }
        });
        // Waveform image generation is more expensive than meter updates.
        // Every frame: a scrolling waveform needs the full frame rate (a
        // 30 fps cap on a 60 Hz tick drew every 2nd or 3rd frame: uneven,
        // it stuttered). Still decks cost nothing: unchanged ones aren't
        // drawn again.
        self.mark("timing broadcast");
        self.last_scope.set(t);
        self.render_scopes();
        self.mark("deck waveforms");
        let an: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.get_surface() == "analyzer" && self.in_sight(w))
            .collect();
        // In step with the meters: the same output delay (OUTPUT reference).
        if let Some(tm) = self.last_timing.borrow().as_ref() {
            self.analyzer.set_delay_ms(tm.latency_ms);
        }
        self.analyzer.tick(&an, t, self.is_live(), self.frozen);
        self.mark("analyzer");
        let mut vis: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| matches!(w.get_surface().as_str(), "visualiser" | "performer") && self.in_sight(w))
            .collect();
        // Preserve the Visualiser as the primary GPU stage; Broadcast DJ is
        // a lightweight subscriber to its already-composed choreography.
        vis.sort_by_key(|w| w.get_surface() != "visualiser");
        let music = if vis.is_empty() {
            Default::default()
        } else {
            self.visual_music()
        };
        self.visual.tick(&vis, t, &music, self.frozen);
        self.broadcast_views.borrow_mut().retain(|view| view.alive());
        if vis.iter().any(|w| w.get_surface() == "performer") {
            for view in self.broadcast_views.borrow().iter() { view.tick(t, self.frozen); }
        }
        self.mark("visualiser");
        // The Tidal Unit's backdrop: the Visualiser under the code.
        let tid: Vec<PatchWindow> = if self.tidal_backdrop() {
            self.windows
                .borrow()
                .iter()
                .filter_map(|w| w.upgrade())
                .filter(|w| w.get_surface() == "tidal" && self.in_sight(w))
                .collect()
        } else {
            vec![]
        };
        if !tid.is_empty() {
            let music = self.tidal_music();
            self.tidal_visual.tick(&tid, t, &music, self.frozen);
            self.mark("tidal backdrop");
        }
        let ai: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| {
                matches!(
                    w.get_surface().as_str(),
                    "dj" | "dj-mini" | "deck-a" | "deck-b"
                ) && self.in_sight(w)
            })
            .collect();
        if let Some(tm) = self.last_timing.borrow().as_ref() {
            self.aidj.set_delay_ms(tm.latency_ms);
        }
        self.aidj.tick(&ai, t, self.is_live(), self.frozen);
        self.mark("ai-dj");
        let eqw: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.get_surface() == "eq" && self.in_sight(w))
            .collect();
        if !eqw.is_empty() {
            self.master_eq.poll(t, self.is_live() && !self.frozen);
            let sp = self.metering.borrow().spectrum_db().to_vec();
            self.master_eq.tick(&eqw, t, &sp);
        }
        self.mark("eq");
        self.ai_tick();
        self.mark("ai tick");
        self.controller_tick();
        self.mark("controllers");
        self.sample_tick();
    }

    pub(super) fn ensure_timer(&self) {
        if self.frozen || self.timer.running() {
            return;
        }
        let me = self.me.clone();
        // 60 fps: meters move like hardware meters. Ballistics are time-based.
        // Each window draws on the display's frame callback, so the timer's
        // drift against the display costs nothing: measured per display
        // frame, windows miss <1 % (and those are mostly timing jitter at
        // the frame boundary). Updating from the frame callbacks themselves
        // measured worse (11 % missed; changes made while a window draws
        // don't schedule its next frame). A moving deck strip asks for its
        // own next frame instead (see `glide`).
        self.timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(1000 / 60),
            move || {
                if let Some(a) = me.upgrade() {
                    a.frame();
                }
            },
        );
    }

    /// One UI update: everything that moves, at this moment.
    pub(super) fn frame(&self) {
        if self.windows.borrow().is_empty() {
            return;
        }
        let fl = &self.frame_loop;
        let t0 = Instant::now();
        if loop_trace() {
            eprintln!(
                "L {:.3} tick",
                LOOP_T0.with(|t| t0.duration_since(*t)).as_secs_f64() * 1e3
            );
        }
        let second = fl.second.get().elapsed() >= Duration::from_secs(1);
        if second {
            fl.second.set(t0);
            self.refresh_external();
            self.refresh_cues();
            self.budget.second();
        }
        // Over budget, what moves runs on every 2nd or 3rd display frame,
        // in every window at once (input is handled as it comes).
        let n = self.frames.get().wrapping_add(1);
        self.frames.set(n);
        if !self.frozen && !self.budget.animate(n) {
            return;
        }
        self.tick(self.start.elapsed().as_secs_f64() + 10.0);
        // Worst UI update per second (diagnostics; PATCH_TRACE_FRAMES=1
        // prints it with the second's total: the rest of the UI thread's
        // time is Slint drawing).
        fl.worst.set(fl.worst.get().max(t0.elapsed()));
        fl.total.set(fl.total.get() + t0.elapsed());
        if second {
            self.frame_worst_ms.set(fl.worst.get().as_secs_f64() * 1e3);
            self.write_heap_trace();
            if std::env::var_os("PATCH_TRACE_FRAMES").is_some() {
                eprintln!(
                    "patch frame: worst {:.2} ms, updates {:.1} ms in the last second: {}",
                    fl.worst.get().as_secs_f64() * 1e3,
                    fl.total.get().as_secs_f64() * 1e3,
                    self.prof_report()
                );
            }
            fl.worst.set(Duration::ZERO);
            fl.total.set(Duration::ZERO);
        }
    }

    /// The detailed waveform of what deck `i` has loaded (cached; retried
    /// every second while its analysis is still running).
    pub(super) fn scope_detail(
        &self,
        i: usize,
        path: &str,
    ) -> Option<Rc<patch_analysis::detail::Detail>> {
        let mut cache = self.scope_detail.borrow_mut();
        let fresh = match &cache[i] {
            Some((p, d, at)) if p == path => {
                d.as_ref().is_some_and(|d| {
                    d.columns_per_second == patch_analysis::detail::COLUMNS_PER_SECOND
                }) || at.elapsed() < Duration::from_secs(1)
            }
            _ => false,
        };
        if !fresh {
            let d = if self.frozen {
                Some(dj::demo_detail(i))
            } else {
                patch_analysis::detail_of(std::path::Path::new(path)).or_else(|| {
                    let analysis = self.analysis(path)?;
                    patch_analysis::detail::overview(&analysis.waveform, analysis.duration)
                })
            };
            cache[i] = Some((path.to_string(), d.map(Rc::new), Instant::now()));
        }
        cache[i].as_ref().and_then(|(_, d, _)| d.clone())
    }

    /// Render the scrolling waveforms for the Deck windows that are open,
    /// at the engine's stamped playhead (as heard).
    pub(super) fn render_scopes(&self) {
        let wins: Vec<(PatchWindow, usize)> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| (w.get_surface().starts_with("deck-") || matches!(w.get_surface().as_str(), "dj-club" | "dj-mini")) && self.in_sight(w))
            .flat_map(|w| {
                if w.get_surface() == "dj-mini" {
                    vec![(w.clone_strong(), 0), (w, 1)]
                } else if w.get_surface() == "dj-club" {
                    let state = w.global::<crate::ui::BoothControls>();
                    [state.get_left_deck(), state.get_right_deck()].into_iter()
                        .map(|id| (w.clone_strong(), Self::deck_index(&id))).collect()
                } else {
                    let i = Self::deck_index(w.get_surface().trim_start_matches("deck-"));
                    vec![(w, i)]
                }
            })
            .collect();
        if wins.is_empty() {
            return;
        }
        let stamped = self.provider.borrow().deck_playheads();
        let loads = self.provider.borrow().deck_loads();
        let heads = self.heads.get();
        for (w, i) in wins {

            // The playhead comes stamped every frame, the deck's track with
            // its status (every 200 ms): when a deck changes track (a load,
            // a hand-back at a transition's end) the strip drew the old
            // track at the new one's position until the status caught up,
            // then jumped again. Hold it until both name the same load.
            if !self.frozen
                && let Some(&now) = loads.get(i)
            {
                let st = self.decks[i].snapshot();
                if st.native && st.load.is_some() {
                    if st.load != Some(now) {
                        self.decks[i].poll_soon();
                        continue;
                    }
                    if self.deck_sources.borrow()[i] != st.source {
                        // The deck's refresh asks for an analysis when it sees
                        // a new source; from here on it would not, and a track
                        // loaded behind the UI's back stayed "analysing" with
                        // no waveform.
                        if let Some(analyst) = &self.analyst {
                            analyst.now(&st.source);
                        }
                        self.deck_sources.borrow_mut()[i] = st.source.clone();
                    }
                }
            }
            let path = if self.frozen {
                dj::DeckStatus::demo(i).source
            } else {
                self.deck_sources.borrow()[i].clone()
            };
            let Some(detail) = self.scope_detail(i, &path) else {
                continue;
            };
            let at = match stamped.get(i).copied().flatten() {
                Some((secs, _)) => secs,
                None => {
                    let (t0, when, playing, speed) = heads[i];
                    if playing {
                        t0 + when.elapsed().as_secs_f64() * speed
                    } else {
                        t0
                    }
                }
            };
            // The grid only (copying the analysis each frame copied its waveform).
            let grid_of = |a: &patch_analysis::Analysis| {
                a.bpm
                    .filter(|_| a.bpm_confidence >= 0.5)
                    .map(|b| (b, a.beat_offset, a.downbeat))
            };
            let grid = match self.analyses.borrow().get(&path) {
                Some(a) => grid_of(a),
                None if self.frozen => grid_of(&dj::demo_analysis(i)),
                None => None,
            };
            let cues = self.cues.borrow().get(&path).copied().unwrap_or_default();
            let st = if self.frozen {
                dj::DeckStatus::demo(i)
            } else {
                dj::DeckStatus::default()
            };
            let (looping, loop_on) = if self.frozen {
                (st.looping, st.loop_on)
            } else {
                self.deck_loops.get()[i]
            };
            let t = w.global::<Theme>();
            let colors = crate::scope::Colors {
                bg: t.get_bg_deeper(),
                // Grey body and mids; the highs (the bright core) the accent
                // blended into the well: a deeper crimson on dark, a softer
                // one on light, never a bright stripe. The colour is free:
                // the strip is drawn as before.
                low: t.get_text_faint().mix(&t.get_bg_deeper(), 0.7),
                mid: t.get_text_dim().mix(&t.get_bg_deeper(), 0.85),
                high: t.get_accent().mix(&t.get_bg_deeper(), 0.6),
                beat: t.get_border_strong(),
                bar: t.get_text_dim(),
                accent: t.get_accent(),
                clip: t.get_error(),
            };
            // A strip of 16 s drawn around an anchor, shifted on the GPU as
            // the playhead moves (redrawing 8 s around it each frame cost a
            // tenth of a core); drawn again only when the playhead leaves
            // ±3.5 s of the anchor, or grid, cues, loop or theme change.
            let d = w.global::<DjState>();
            let anchor = self.scope_anchor.borrow().get(i).copied().flatten();
            let near = anchor.filter(|a| (at - a).abs() < 3.5);
            // A new anchor on the strip's own pixel grid (one detail column
            // per pixel): the redrawn strip's columns are those of the one
            // it replaces, where an anchor between them redrew every peak
            // a fraction of a column over, a twitch every 3.5 s.
            let px = SCOPE_STRIP_S / 2400.0;
            let anchor = near.unwrap_or((at / px).round() * px);
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (
                    &path,
                    anchor.to_bits(),
                    format!(
                        "{grid:?}{cues:?}{looping:?}{loop_on}{:?}",
                        self.scope_style.get()
                    ),
                    colors.bg.as_argb_encoded(),
                    colors.accent.as_argb_encoded(),
                    colors.high.as_argb_encoded(),
                )
                    .hash(&mut h);
                (h.finish() as i64 & 0x7fff_ffff) as i32
            };
            d.set_scope_style(self.scope_style.get().name().into());
            // A beat as a fraction of the visible half of the strip (the
            // pulse on the beat in the deck); 0 without a grid.
            let beat = grid.map_or(0.0, |(bpm, ..)| (60.0 / bpm / (SCOPE_STRIP_S / 2.0)) as f32);
            match i { 0 => d.set_scope_a_beat(beat), 1 => d.set_scope_b_beat(beat), 2 => d.set_scope_c_beat(beat), _ => d.set_scope_d_beat(beat) }
            let shift = ((anchor - at) / SCOPE_STRIP_S) as f32;
            // Over budget, the governor's every 2nd or 3rd frame holds.
            let every_frame = self.budget.level() == 0;
            GLIDE.with(|g| {
                let g = &mut g.borrow_mut()[i];
                g.anchor = Some(anchor);
                g.every_frame = every_frame;
            });
            match i { 0 => d.set_scope_a_shift(shift), 1 => d.set_scope_b_shift(shift), 2 => d.set_scope_c_shift(shift), _ => d.set_scope_d_shift(shift) }
            if (match i { 0 => d.get_scope_key(), 1 => d.get_scope_key_b(), 2 => d.get_scope_key_c(), _ => d.get_scope_key_d() }) == key {
                continue;
            }
            match i { 0 => d.set_scope_key(key), 1 => d.set_scope_key_b(key), 2 => d.set_scope_key_c(key), _ => d.set_scope_key_d(key) }
            {
                let mut a = self.scope_anchor.borrow_mut();
                a.resize(4, None);
                a[i] = Some(anchor);
            }
            let view = crate::scope::View {
                detail: &detail,
                at: anchor,
                span: SCOPE_STRIP_S,
                grid,
                cues,
                looping,
                loop_on,
                playhead: false,
                style: self.scope_style.get(),
            };
            let img = slint::Image::from_rgba8(crate::scope::render(2400, 112, &view, &colors));
            match i { 0 => d.set_scope_a(img), 1 => d.set_scope_b(img), 2 => d.set_scope_c(img), _ => d.set_scope_d(img) }
            match i { 0 => d.set_scope_a_shift(0.0), 1 => d.set_scope_b_shift(0.0), 2 => d.set_scope_c_shift(0.0), _ => d.set_scope_d_shift(0.0) }
        }
    }
}

/// The model holds exactly `v` (so setting `v` again would change nothing).
fn model_is<T: Clone + PartialEq + 'static>(m: &ModelRc<T>, v: &[T]) -> bool {
    use slint::Model;
    m.row_count() == v.len() && v.iter().enumerate().all(|(i, x)| m.row_data(i).as_ref() == Some(x))
}

#[cfg(test)]
mod tests {
    use super::meter_index;

    #[test]
    fn program_meters_follow_master_output_even_with_loud_decks() {
        use crate::metering::{Ballistic, Metering};
        let mut m = Metering::default();
        m.strips_lr.push([Ballistic::default(); 2]);
        for b in &mut m.strips_lr[0] {
            b.level = -1.0;
        }
        m.output.resize(2, Ballistic::default());
        assert_eq!(super::program_levels(&m), [0.0, 0.0], "muted program");
        m.output[0].level = -12.0;
        m.output[1].level = -30.0;
        let lr = super::program_levels(&m);
        assert!(lr[0] > lr[1] && lr[1] > 0.0, "independent stereo program");
        m.output.clear();
        assert_eq!(super::program_levels(&m), [0.0, 0.0], "no provider frame");
    }

    #[test]
    fn visible_mixer_rows_follow_provider_meter_ids() {
        let provider_order = [
            "app:chromium",
            "system-sounds",
            "engine.dj-mixer",
            "engine.mic-1",
            "master",
            "output",
            "engine.deck-a",
            "engine.deck-b",
        ];

        assert_eq!(meter_index(&provider_order, "app:chromium"), Some(0));
        assert_eq!(meter_index(&provider_order, "system-sounds"), Some(1));
        assert_eq!(meter_index(&provider_order, "engine.mic-1"), Some(3));
        assert_eq!(meter_index(&provider_order, "output"), Some(5));
        assert_eq!(meter_index(&provider_order, "master"), Some(4));
        assert_eq!(meter_index(&provider_order, "engine.deck-a"), Some(6));
        assert_eq!(meter_index(&provider_order, "missing"), None);
    }
}
