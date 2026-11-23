//! What the DJ surfaces know of the music: visual music, beat phases, analyses, sync.

use super::*;

impl AudioApp {
    /// What the Visualiser follows: the deck heard most (playing, the
    /// louder of two), its title, tempo, beat and level.
    pub(super) fn visual_music(&self) -> patch_visual::director::Music {
        let lv = self.vis_levels.get();
        let ph = self.beat_phases();
        let st: Vec<dj::DeckStatus> = (0..2)
            .map(|i| {
                if self.is_live() {
                    self.decks[i].snapshot()
                } else if self.frozen {
                    dj::DeckStatus::demo(i)
                } else {
                    dj::DeckStatus::default()
                }
            })
            .collect();
        let on = |i: usize| st[i].loaded && !st[i].paused;
        let lead = match (on(0), on(1)) {
            (true, false) => 0,
            (false, true) => 1,
            (true, true) => usize::from(lv[1] > lv[0]),
            _ => return Default::default(),
        };
        let s = &st[lead];
        // The tempo as played (the track's own times the deck's speed).
        let speed = self.heads.get()[lead].3;
        let bpm = self
            .analyses
            .borrow()
            .get(&s.source)
            .and_then(|a| a.bpm)
            .unwrap_or(0.0)
            * if speed > 0.0 { speed } else { 1.0 };
        patch_visual::director::Music {
            title: patch_visual::banner::clean_title(&s.title),
            artist: crate::visual::artist_of(&s.source),
            playing: true,
            level: lv[lead],
            beat: ph[lead] as f64,
            bpm,
            transition: on(0) && on(1),
            handover: None,
            // dBFS per band (25 Hz … 20 kHz) as 0…1: −60 dB silent, −12 full.
            bands: self
                .metering
                .borrow()
                .spectrum_db()
                .iter()
                .map(|db| ((db + 60.0) / 48.0).clamp(0.0, 1.0))
                .collect(),
            // Not in a stretch the track's grid found without a kick.
            kicking: self
                .analyses
                .borrow()
                .get(&s.source)
                .and_then(|a| {
                    a.grid.as_ref().map(|g| {
                        !g.beatless
                            .iter()
                            .any(|r| (r[0] as f64..r[1] as f64).contains(&s.time))
                    })
                })
                .unwrap_or(true),
            // A song rather than a club track: a voice over no steady kick.
            calm: self
                .analyses
                .borrow()
                .get(&s.source)
                .map(|a| {
                    patch_visual::pack::laid_back(
                        0.0,
                        a.vocals.as_ref().map(|v| v.presence),
                        a.grid.as_ref().map(|g| (g.coverage, g.jitter_ms)),
                    )
                })
                .unwrap_or(0.0),
        }
    }

    /// Beat position of each deck now (beats since the grid's first beat),
    /// extrapolated from the last playhead: cheap enough for every frame.
    pub(super) fn beat_phases(&self) -> [f32; 4] {
        let heads = self.heads.get();
        let stamped = self.provider.borrow().deck_playheads();
        let mut out = [-1.0; 4];
        for i in 0..4 {
            // The playhead now and how fast it moves (seconds per second;
            // the engine's step is 0 when a deck stands).
            let (t, moving) = match stamped.get(i).copied().flatten() {
                // Native deck: the engine's own position as heard now.
                Some((secs, step)) => (secs, step),
                None => match heads[i] {
                    (t0, at, true, speed) => (t0 + at.elapsed().as_secs_f64() * speed, speed),
                    (t0, ..) => (t0, 0.0),
                },
            };
            GLIDE.with(|g| {
                let g = &mut g.borrow_mut()[i];
                g.head = Some((t, moving, Instant::now()));
                g.grid = None;
            });
            let st_src = if self.frozen {
                dj::DeckStatus::demo(i).source
            } else {
                self.deck_sources.borrow()[i].clone()
            };
            // The grid the engine plays the deck to (manual edits and the
            // precise pass included), from the background status poll; the
            // raw analysis only without one.
            let engine = (!self.frozen)
                .then(|| self.decks[i].snapshot())
                .filter(|st| st.native && st.source == st_src)
                .and_then(|st| st.grid)
                .map(|(bpm, offset, _)| (Some(bpm), 1.0, offset));
            let analysis = engine
                .or_else(|| {
                    self.analyses
                        .borrow()
                        .get(&st_src)
                        .map(|a| (a.bpm, a.bpm_confidence, a.beat_offset))
                })
                .or_else(|| {
                    self.frozen.then(|| {
                        let a = dj::demo_analysis(i);
                        (a.bpm, a.bpm_confidence, a.beat_offset)
                    })
                });
            let Some((Some(bpm), confidence, beat_offset)) = analysis else {
                continue;
            };
            if confidence < 0.5 || confidence.is_nan() {
                continue;
            }
            GLIDE.with(|g| g.borrow_mut()[i].grid = Some((bpm, beat_offset)));
            out[i] = ((t - beat_offset) / (60.0 / bpm)) as f32;
        }
        out
    }

    /// The analysis of a track (memory, then the disk cache).
    pub(super) fn analysis(&self, path: &str) -> Option<patch_analysis::Analysis> {
        if path.is_empty() {
            return None;
        }
        // The waveform is filled once, when an entry is cached: this runs
        // for every deck refresh, and a track without a saved overview
        // opened the library on each.
        if let Some(a) = self.analyses.borrow().get(path) {
            let mut a = a.clone();
            patch_engine::track_analysis::apply_saved_manual_grid(path, &mut a);
            return Some(a);
        }
        let mut raw = patch_engine::track_analysis::stored_analysis(path)?;
        if raw.waveform.is_empty() {
            raw.waveform = patch_library::open_quick()
                .ok()
                .and_then(|db| db.waveform_bins(path))
                .unwrap_or_default();
        }
        self.analyses
            .borrow_mut()
            .insert(path.to_string(), raw.clone());
        let mut a = raw;
        patch_engine::track_analysis::apply_saved_manual_grid(path, &mut a);
        Some(a)
    }

    /// The library row's figures of a track's analysis, without copying it.
    pub(super) fn analysis_brief(&self, path: &str) -> Option<Brief> {
        if let Some(a) = self.analyses.borrow().get(path) {
            return Some(Brief::of(a));
        }
        self.library_briefs.borrow().get(path).cloned()
    }

    /// What a deck has loaded and its analysis (demo data in screenshots).
    pub(super) fn deck_analysis(
        &self,
        i: usize,
        st: &dj::DeckStatus,
    ) -> Option<patch_analysis::Analysis> {
        if self.frozen {
            return Some(dj::demo_analysis(i));
        }
        // A loaded track's grid is measured again behind the UI's back (a
        // better method, the AI's precise pass): read it back every 2 s.
        // (Kept from startup, the old grid went to the engine over the one
        // the AI had just set: heard off beat.)
        thread_local! {
            static CHECKED: std::cell::RefCell<HashMap<String, Instant>> = Default::default();
        }
        let due = CHECKED.with(|c| {
            let mut c = c.borrow_mut();
            let due = c
                .get(&st.source)
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            if due {
                c.insert(st.source.clone(), Instant::now());
            }
            due
        });
        if due
            && let Some(fresh) = patch_engine::track_analysis::stored_analysis(&st.source)
            && self
                .analyses
                .borrow()
                .get(&st.source)
                .is_some_and(|a| a.grid != fresh.grid)
        {
            let mut fresh = fresh;
            let mut analyses = self.analyses.borrow_mut();
            if fresh.waveform.is_empty()
                && let Some(old) = analyses.get_mut(&st.source)
            {
                // A new grid, the same audio: keep the overview drawn.
                fresh.waveform = std::mem::take(&mut old.waveform);
            }
            analyses.insert(st.source.clone(), fresh);
        }
        let mut a = self.analysis(&st.source)?;
        // The measured kick grid; until the librarian has one, the older
        // kick refinement (and ask for the grid meanwhile).
        if a.use_grid() {
            return Some(a);
        }
        let _ = patch_analysis::grid_soon(std::path::Path::new(&st.source), false);
        if st.native && let Some(bpm) = a.bpm.filter(|_| a.bpm_confidence >= 0.5)
            // In the background the first time: the UI never decodes.
            && let Some(Some(fit)) = patch_analysis::kick_grid_soon(std::path::Path::new(&st.source), bpm)
        {
            let offset = fit.offset;
            let beat = 60.0 / bpm;
            let shift = (offset - a.beat_offset + beat / 2.0).rem_euclid(beat) - beat / 2.0;
            if shift.abs() > beat * 0.1 {
                a.downbeat = None;
                a.downbeat_confidence = 0.0;
            }
            a.beat_offset = offset;
            a.bpm = Some(fit.bpm);
            self.analyses
                .borrow_mut()
                .insert(st.source.clone(), a.clone());
        }
        Some(a)
    }

    /// Match `id`'s tempo to the other deck (half/double time when closer)
    /// and, if the other deck plays, line up the beats.
    pub(super) fn sync_deck(&self, id: &str) -> Result<String, String> {
        let i = Self::deck_index(id);
        let o = self.sync_reference(i);
        let (st_i, st_o) = (self.decks[i].status(), self.decks[o].status());
        // Native decks: the engine locks tempo and beat phase and keeps them.
        if st_i.native && st_o.native {
            for (d, st) in [(i, &st_i), (o, &st_o)] {
                let a = self.deck_analysis(d, st);
                if !a
                    .as_ref()
                    .is_some_and(|a| a.bpm.is_some() && a.bpm_confidence >= 0.5)
                {
                    return Err(format!(
                        "{} has no steady beat to sync to",
                        self.decks[d].name
                    ));
                }
            }
            let on = !st_i.sync_on || st_i.sync_hold;
            self.decks[i].sync(on, Some(self.decks[o].id))?;
            return Ok(if on {
                format!(
                    "{} locked to {}: tempo and beat phase",
                    self.decks[i].name, self.decks[o].name
                )
            } else {
                format!("{} sync off", self.decks[i].name)
            });
        }
        let (Some(ai), Some(ao)) = (self.deck_analysis(i, &st_i), self.deck_analysis(o, &st_o))
        else {
            return Err("Sync needs both decks loaded and analysed".into());
        };
        let (Some(bi), Some(bo)) = (ai.bpm, ao.bpm) else {
            return Err("Sync needs a steady beat on both decks".into());
        };
        // A doubtful tempo would make sync jump somewhere wrong: refuse.
        for (d, a) in [(i, &ai), (o, &ao)] {
            if a.bpm_confidence < 0.5 {
                return Err(format!(
                    "{} has no sufficiently supported steady beat for sync",
                    self.decks[d].name
                ));
            }
        }
        let po = self.pitch.borrow()[o];
        let target = bo * (1.0 + po.tempo);
        let tempo = [0.5, 1.0, 2.0]
            .iter()
            .map(|m| target * m / bi - 1.0)
            .min_by(|a, b| a.abs().total_cmp(&b.abs()))
            .unwrap_or(0.0);
        if tempo.abs() > dj::RANGES[dj::RANGES.len() - 1] {
            return Err(format!(
                "{:.1} BPM is out of reach of {:.1} BPM",
                bi, target
            ));
        }
        self.with_pitch(id, |p| {
            while tempo.abs() > p.range {
                p.range = p.next_range();
            }
            p.tempo = tempo;
        });
        // Beat phase: only when the other deck plays (else tempo only).
        let mut phased = false;
        if !st_o.paused && st_i.seekable {
            let (per_i, per_o) = (60.0 / bi, 60.0 / bo);
            let ph = |t: f64, off: f64, per: f64| ((t - off) / per).rem_euclid(1.0);
            let st_o = self.decks[o].status();
            let st_i = self.decks[i].status();
            let mut d = ph(st_o.time, ao.beat_offset, per_o) - ph(st_i.time, ai.beat_offset, per_i);
            if d >= 0.5 {
                d -= 1.0;
            } else if d < -0.5 {
                d += 1.0;
            }
            self.decks[i].seek_by(d * per_i);
            phased = true;
        }
        Ok(format!(
            "{} synced to {:.2} BPM ({} %){}",
            self.decks[i].name,
            target,
            crate::num::signed(tempo * 100.0, 2),
            if phased { " · beats aligned" } else { "" }
        ))
    }
}
