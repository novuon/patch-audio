//! Drawing decks: overviews, deck items, the fader law, deck meters.

use super::*;

impl AudioApp {
    /// Deck `i`'s overview images (unplayed, played), drawn again only for
    /// a new track or theme.
    pub(super) fn overview(
        &self,
        i: usize,
        source: &str,
        an: Option<&patch_analysis::Analysis>,
    ) -> (
        slint::Image,
        slint::Image,
        slint::Image,
        slint::Image,
        ModelRc<f32>,
    ) {
        let Some(a) = an.filter(|a| !a.waveform.is_empty()) else {
            return (
                slint::Image::default(),
                slint::Image::default(),
                slint::Image::default(),
                slint::Image::default(),
                ModelRc::default(),
            );
        };
        let th = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .next()
            .map(|w| {
                let t = w.global::<Theme>();
                (t.get_text_dim(), t.get_text_faint(), t.get_bg_deeper())
            });
        let (dim, faint, meter_bg) = th.unwrap_or((
            slint::Color::from_rgb_u8(0x8a, 0x8a, 0x8d),
            slint::Color::from_rgb_u8(0x5a, 0x5a, 0x5a),
            slint::Color::from_rgb_u8(0x20, 0x20, 0x20),
        ));
        let key = format!(
            "{source}|{}|{:08x}{:08x}{:08x}",
            a.waveform.len(),
            dim.as_argb_encoded(),
            faint.as_argb_encoded(),
            meter_bg.as_argb_encoded()
        );
        let mut cache = self.overviews.borrow_mut();
        if let Some((k, rest, played, arc, arc_compact, bins)) =
            cache.get(i).and_then(|c| c.as_ref())
            && *k == key
        {
            return (
                rest.clone(),
                played.clone(),
                arc.clone(),
                arc_compact.clone(),
                bins.clone(),
            );
        }
        let wave: Vec<[f32; 2]> = a
            .waveform
            .iter()
            .map(|w| [w[0] as f32 / 255.0, w[1] as f32 / 255.0])
            .collect();
        let rgba = |c: slint::Color, alpha: f32| {
            [
                c.red(),
                c.green(),
                c.blue(),
                (c.alpha() as f32 * alpha) as u8,
            ]
        };
        let rest = slint::Image::from_rgba8(crate::overview::render(&wave, rgba(faint, 0.6)));
        let played = slint::Image::from_rgba8(crate::overview::render(&wave, rgba(dim, 1.0)));
        // The AI timeline's strokes: a white mask the deck tints (the theme's
        // faint ink, the accent where played). Drawn in the deepest
        // background colour they all but vanished on a dark theme.
        let arc = slint::Image::from_rgba8(crate::overview::render_arc(&wave, [255; 4], false));
        let arc_compact =
            slint::Image::from_rgba8(crate::overview::render_arc(&wave, [255; 4], true));
        if cache.len() <= i {
            cache.resize(i + 1, None);
        }
        let bins: ModelRc<f32> = Rc::new(VecModel::from(crate::overview::arc_bins(&wave))).into();
        cache[i] = Some((
            key,
            rest.clone(),
            played.clone(),
            arc.clone(),
            arc_compact.clone(),
            bins.clone(),
        ));
        (rest, played, arc, arc_compact, bins)
    }

    /// Deck `i`'s track map: whether to show it, its lane image, its parts
    /// and one bar's seconds. Made again only for a new track, theme or
    /// switch.
    pub(super) fn track_map(
        &self,
        i: usize,
        source: &str,
        an: Option<&patch_analysis::Analysis>,
    ) -> (bool, slint::Image, ModelRc<TrackPart>, f32) {
        let (on, spots) = super::dj_poll::map_prefs();
        let none = || (false, slint::Image::default(), ModelRc::default(), 0.0);
        let Some(a) = an.filter(|a| on && a.duration > 0.0) else {
            return none();
        };
        let th = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .next()
            .map(|w| {
                let t = w.global::<Theme>();
                (t.get_text_dim(), t.get_text_faint(), t.get_accent())
            });
        let (dim, faint, accent) = th.unwrap_or((
            slint::Color::from_rgb_u8(0x8a, 0x8a, 0x8d),
            slint::Color::from_rgb_u8(0x5a, 0x5a, 0x5a),
            slint::Color::from_rgb_u8(0xe0, 0x4a, 0x3a),
        ));
        let key = format!(
            "{source}|{}|{}|{spots}|{:08x}{:08x}{:08x}",
            a.generation(),
            a.duration,
            dim.as_argb_encoded(),
            faint.as_argb_encoded(),
            accent.as_argb_encoded()
        );
        let mut cache = self.track_maps.borrow_mut();
        if let Some((k, lane, parts, bar)) = cache.get(i).and_then(|c| c.as_ref())
            && *k == key
        {
            return (parts.row_count() > 0, lane.clone(), parts.clone(), *bar);
        }
        let map = patch_analysis::sections::map(a);
        let rank = |p: &patch_analysis::sections::Part| if spots { p.rank } else { 0 };
        let span = |t: f64| (t / a.duration).clamp(0.0, 1.0) as f32;
        let rgba = |c: slint::Color, alpha: f32| {
            [c.red(), c.green(), c.blue(), (c.alpha() as f32 * alpha) as u8]
        };
        let lane = slint::Image::from_rgba8(crate::overview::render_lane(
            &map
                .iter()
                .map(|p| (span(p.start_s), span(p.end_s), p.kick >= 0.5, rank(p) > 0))
                .collect::<Vec<_>>(),
            rgba(dim, 0.9),
            rgba(faint, 0.9),
            rgba(accent, 1.0),
        ));
        let parts: ModelRc<TrackPart> = Rc::new(VecModel::from(
            map.iter()
                .map(|p| {
                    let word = p.kind.word().to_uppercase();
                    TrackPart {
                        from: span(p.start_s),
                        to: span(p.end_s),
                        // The rank's box takes about two characters.
                        chars: word.chars().count() as i32 + if rank(p) > 0 { 2 } else { 0 },
                        word: word.into(),
                        rank: rank(p) as i32,
                    }
                })
                .collect::<Vec<_>>(),
        ))
        .into();
        let mut b = a.clone();
        b.use_grid();
        let bar = b.bpm.map_or(0.0, |bpm| (240.0 / bpm) as f32);
        if cache.len() <= i {
            cache.resize(i + 1, None);
        }
        cache[i] = Some((key, lane.clone(), parts.clone(), bar));
        (!map.is_empty(), lane, parts, bar)
    }

    pub(super) fn deck_item(&self, i: usize) -> DeckItem {
        let deck = &self.decks[i];
        let st = if self.is_live() {
            deck.snapshot()
        } else if self.frozen {
            dj::DeckStatus::demo(i)
        } else {
            dj::DeckStatus::default()
        };
        // Fader × crossfader (the trim is shown on its own).
        let gain_db = patch_engine::session::Mixer {
            faders: [self.deck_gain.get()[0], self.deck_gain.get()[1]],
            extra_faders: [self.deck_gain.get()[2], self.deck_gain.get()[3]],
            extra_trims_db: [0.0; 2],
            xfader: self.xfader.get(),
            auto_gain: false,
            group_db: 0.0,
            group_mute: false,
            trims_db: [0.0; 2],
            ..patch_engine::session::Mixer::default()
        }
        .gains4_db()[i];
        // Its level: the same values the meter tick writes (one source, so
        // the two writers never disagree and the bars never jump).
        let live = st.loaded && !st.seekable;
        let progress = if live {
            0.0
        } else {
            st.duration.map(|d| (st.time / d) as f32).unwrap_or(0.0)
        };
        // The engine owns live native-deck pitch state (including sync and
        // set-start resets). Mirror it before building the model.
        if st.native && st.loaded {
            let mut pitch = self.pitch.borrow_mut();
            pitch[i].tempo = st.tempo;
            pitch[i].bend = st.bend;
            pitch[i].key_shift = st.key_shift;
            pitch[i].keylock = st.keylock;
            if let Some(range) = dj::RANGES.iter().copied().find(|r| *r >= st.tempo.abs()) {
                pitch[i].range = pitch[i].range.max(range);
            }
        }
        let p = self.pitch.borrow()[i];
        // The deck owns display assets regardless of who loaded the track.
        // AI loads and engine handovers bypass the Library's manual-load
        // callback, so request analysis when the observed source changes too.
        if st.loaded && !self.frozen && self.deck_sources.borrow()[i] != st.source
            && let Some(analyst) = &self.analyst
        {
            analyst.now(&st.source);
        }
        let an = if st.loaded {
            self.deck_analysis(i, &st)
        } else {
            None
        };
        {
            let mut h = self.heads.get();
            h[i] = (st.time, Instant::now(), st.loaded && !st.paused, p.speed());
            self.heads.set(h);
            self.deck_sources.borrow_mut()[i] = st.source.clone();
            let mut l = self.deck_loops.get();
            l[i] = (st.looping, st.loop_on);
            self.deck_loops.set(l);
        }
        // Native decks get the beat grid (quantize, loops, jumps, sync).
        if st.native
            && !self.frozen
            && let Some(a) = an.as_ref().filter(|a| a.bpm_confidence >= 0.5)
        {
            let key = format!(
                "{}|{:?}|{:?}|{:?}|{:?}",
                st.source, st.load, a.bpm, a.beat_offset, a.downbeat
            );
            if self.grid_sent.borrow()[i] != key || st.grid.is_none() {
                // The downbeat only when the analysis is reasonably sure.
                let down = a.downbeat.filter(|_| a.downbeat_confidence >= 0.3);
                self.decks[i].set_grid(a.bpm, a.beat_offset, down);
                self.grid_sent.borrow_mut()[i] = key;
            }
        }
        let cues = self
            .cues
            .borrow()
            .get(&st.source)
            .copied()
            .unwrap_or_default();
        let dur = st.duration.unwrap_or(0.0);
        // The AI's parked incoming position is its first cue/start point.
        // Preserve it when playback starts; do not add a separate zero marker.
        let mission_cue = if let Some(first_cue) = cues[0] {
            // An explicit first cue is the mission start marker. A cached AI
            // parked position must not displace it to the track's start.
            first_cue
        } else {
            let mut starts = self.mission_cues.borrow_mut();
            if st.native && st.paused && st.sync_on {
                starts.insert(st.source.clone(), st.time);
            }
            *starts.entry(st.source.clone()).or_insert(st.cue_point)
        };
        let other_index = self.sync_reference(i);
        let other = &self.decks[other_index];
        let steady = |a: &patch_analysis::Analysis| a.bpm.is_some() && a.bpm_confidence >= 0.5;
        let can_sync = st.loaded && an.as_ref().is_some_and(steady) && {
            let so = if self.frozen {
                dj::DeckStatus::demo(other_index)
            } else {
                other.snapshot()
            };
            so.loaded && self.deck_analysis(other_index, &so).is_some_and(|a| steady(&a))
        };
        let semis = p.semitones();
        let key = an
            .as_ref()
            .and_then(|a| a.key)
            .map(|k| k.shifted(semis.round() as i32));
        let bpm = an.as_ref().and_then(|a| a.bpm);
        let phrases: Vec<f32> = match (bpm, an.as_ref()) {
            (Some(b), Some(a)) if a.duration > 0.0 => {
                let step = 32.0 * 60.0 / b; // 8 bars of 4/4
                let n = ((a.duration - a.beat_offset) / step) as usize;
                if n <= 96 {
                    (0..=n)
                        .map(|k| ((a.beat_offset + k as f64 * step) / a.duration) as f32)
                        .collect()
                } else {
                    vec![]
                }
            }
            _ => vec![],
        };
        let trim = self.trims.get()[i];
        let k = self.eq.get()[i];
        let loop_beats = match (st.looping, st.grid.map(|g| 60.0 / g.0)) {
            (Some((a, b)), Some(per)) => (b - a) / per,
            _ => 0.0,
        };
        let waves = self.overview(i, &st.source, an.as_ref());
        let map = self.track_map(i, &st.source, an.as_ref());
        let lines = track_lines(&st.title, &st.artist, &st.source);
        DeckItem {
            native: st.native,
            // Source format, plus loading progress and any underruns.
            format: {
                let mut f = st.format.clone();
                if st.native && st.decoded < 1.0 {
                    f.push_str(&format!(" · decoding {:.0} %", st.decoded * 100.0));
                }
                if st.underruns > 0 {
                    f.push_str(&format!(" · {} underruns", st.underruns));
                }
                f.into()
            },
            loop_on: st.loop_on,
            loop_beats: loop_beats as f32,
            loop_text: if st.loop_on {
                format!("loop {}", beats_text(loop_beats))
            } else {
                String::new()
            }
            .into(),
            loop_from: st
                .looping
                .filter(|_| dur > 0.0)
                .map(|(a, _)| (a / dur) as f32)
                .unwrap_or(-1.0),
            loop_to: st
                .looping
                .filter(|_| dur > 0.0)
                .map(|(_, b)| (b / dur) as f32)
                .unwrap_or(-1.0),
            slip: st.slip,
            previewing: st.previewing,
            quantize: st.quantize,
            synced: st.sync_on,
            sync_text: if st.sync_hold {
                "SYNC · held (nudged)".to_string()
            } else if st.sync_on && st.paused {
                "SYNC · ready when playing".to_string()
            } else if st.sync_on {
                format!("SYNC · {} ms", crate::num::signed(st.sync_error_ms, 2))
            } else {
                String::new()
            }
            .into(),
            cues: ModelRc::new(VecModel::from(
                cues.iter()
                    .map(|c| {
                        c.filter(|_| dur > 0.0)
                            .map(|t| (t / dur) as f32)
                            .unwrap_or(-1.0)
                    })
                    .collect::<Vec<_>>(),
            )),
            cue_texts: ModelRc::new(VecModel::from(
                cues.iter()
                    .map(|c| SharedString::from(c.map(dj::clock).unwrap_or_default()))
                    .collect::<Vec<_>>(),
            )),
            eq_high: k[0],
            eq_mid: k[1],
            eq_low: k[2],
            filter: k[3],
            eq_high_text: eq_text(k[0]).into(),
            eq_mid_text: eq_text(k[1]).into(),
            eq_low_text: eq_text(k[2]).into(),
            filter_text: filter_text(k[3]).into(),
            bpm: bpm
                .map(|b| format!("{:.2}", b * p.speed()))
                .unwrap_or_else(|| "—".into())
                .into(),
            bpm_orig: bpm
                .map(|b| format!("orig {b:.2}"))
                .unwrap_or_default()
                .into(),
            bpm_sure: an.as_ref().is_some_and(|a| a.bpm_confidence >= 0.5),
            tempo: (p.tempo / p.range) as f32,
            tempo_text: if p.bend != 0.0 {
                format!("{} % nudge", crate::num::signed((p.speed() - 1.0) * 100.0, 2))
            } else {
                format!("{} %", crate::num::signed(p.tempo * 100.0, 2))
            }
            .into(),
            range_text: format!("±{:.0} %", p.range * 100.0).into(),
            keylock: p.keylock,
            key: key
                .map(|k| k.camelot())
                .unwrap_or_else(|| "—".into())
                .into(),
            key_name: key.map(|k| k.name()).unwrap_or_default().into(),
            key_sure: an.as_ref().is_some_and(|a| a.key_confidence >= 0.2),
            shift_text: if semis.abs() < 0.05 {
                String::new()
            } else if (semis - semis.round()).abs() < 0.05 {
                format!("{:+.0} st", semis)
            } else {
                format!("{} st", crate::num::signed(semis, 1))
            }
            .into(),
            analysing: st.loaded && !live && an.is_none() && !st.source.contains("://"),
            gain_trim: if self.auto_gain.get() && an.is_some() && self.is_live() {
                format!("auto {} dB", crate::num::signed(trim, 1))
            } else {
                String::new()
            }
            .into(),
            has_wave: an.as_ref().is_some_and(|a| !a.waveform.is_empty()),
            cue_progress: if dur > 0.0 {
                (mission_cue / dur).clamp(0.0, 1.0) as f32
            } else {
                0.0
            },
            mission_bins: waves.4,
            mission_wave: waves.2,
            mission_wave_compact: waves.3,
            wave_rest: waves.0,
            wave_played: waves.1,
            phrases: ModelRc::new(VecModel::from(phrases)),
            has_map: map.0,
            map_lane: map.1,
            parts: map.2,
            bar_s: map.3,
            can_sync,
            id: deck.id.into(),
            name: deck.name.into(),
            loaded: st.loaded,
            title: lines.0.into(),
            artist: lines.1.into(),
            source: st.source.as_str().into(),
            favourite: if self.frozen { i == 0 } else { self.lib_favourites.borrow().contains(&st.source) },
            can_favourite: st.loaded && (self.frozen || !st.source.contains("://")),
            time: dj::clock(st.time).into(),
            remaining: st
                .duration
                .map(|d| dj::clock(d - st.time))
                .unwrap_or_default()
                .into(),
            length: st.duration.map(dj::clock).unwrap_or_default().into(),
            time_s: st.time as f32,
            duration_s: st.duration.unwrap_or(0.0) as f32,
            progress,
            live,
            playing: st.loaded && !st.paused,
            gain_db: db_text(gain_db.max(-60.0)).into(),
        }
    }

    /// Crossfader windows: the law drawn live (position and each deck's
    /// level in the mix), redrawn only when what it shows changed.
    pub(super) fn draw_law(&self) {
        let wins: Vec<PatchWindow> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.get_surface().starts_with("dj-mixer") && self.in_sight(w))
            .collect();
        if wins.is_empty() {
            return;
        }
        let m = self.metering.borrow();
        // Each deck's shape: lit by its level, and pulsing on its beats (a
        // flash on the beat, gone before the next).
        let phases = self.beat_phases();
        let level = |i: usize| {
            let (l, r, _, vu) = Self::deck_meter(
                self.deck_strips.get()[i],
                &m,
                self.deck_gain.get(),
                self.xfader.get(),
                i,
            );
            let lv = if vu >= 0.0 { vu } else { l.max(r) };
            let pulse = phases
                .get(i)
                .copied()
                .filter(|p| *p >= 0.0)
                .map_or(0.0, |p| (-(p - p.floor()) * 5.0).exp());
            (lv * (0.45 + 0.75 * pulse)).min(1.0)
        };
        let live = crate::law::Live {
            pos: self.xfader.get(),
            level: [level(0), level(1)],
        };
        drop(m);
        // NEXT MIX: beats to the blend, counted on from the deck it mixes
        // out of (the engine's figure at the last poll plus the beats that
        // deck played since); then the bar it's in.
        let (mix_beats, mix_bar) = match self.mix_anchor.get() {
            Some((beats_in, from, then, bars)) if phases[from] >= 0.0 => {
                let now = beats_in + (phases[from] - then) as f64;
                if now < 0.0 {
                    ((-now).ceil() as i32, String::new())
                } else {
                    (
                        0,
                        format!(
                            "bar {} / {bars:.0}",
                            ((now / 4.0).floor() as i64 + 1).min(bars.max(1.0) as i64)
                        ),
                    )
                }
            }
            _ => (-1, String::new()),
        };
        for w in wins {
            let d = w.global::<DjState>();
            d.set_mix_beats(mix_beats);
            if d.get_mix_bar().as_str() != mix_bar {
                d.set_mix_bar(mix_bar.clone().into());
            }
            let th = w.global::<Theme>();
            let scale = w.window().scale_factor();
            let (lw, lh) = (
                (d.get_law_w() * scale).round().max(2.0) as u32,
                (d.get_law_h() * scale).round().max(2.0) as u32,
            );
            let rgb = |c: slint::Color| [c.red(), c.green(), c.blue()];
            let colors = crate::law::Colors {
                a: rgb(th.get_text()),
                b: rgb(th.get_accent()),
                grid: rgb(th.get_border_strong()),
                marker: rgb(th.get_text_bright()),
            };
            // Quantised so it redraws when something visible changed: the
            // position at once, the levels' glow in 24 steps and at most
            // 20 times a second (it was redrawn every frame while playing).
            let glow = if self
                .law_drawn
                .get()
                .is_some_and(|t| t.elapsed() < Duration::from_millis(50))
            {
                self.law_glow.get()
            } else {
                ((live.level[0] * 24.0) as u32) << 8 | (live.level[1] * 24.0) as u32
            };
            let tint = [
                th.get_text(),
                th.get_accent(),
                th.get_border_strong(),
                th.get_text_bright(),
            ]
            .map(|c| c.as_argb_encoded());
            let key = (
                lw.min(4096),
                lh.min(2048),
                ((live.pos * 400.0) as u32) << 16 | glow,
                tint,
            );
            // Per window (its own state): a reopened window draws again.
            let k = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut h);
                (h.finish() & 0x7fff_ffff) as i32
            };
            if d.get_law_key() == k {
                continue;
            }
            d.set_law_key(k);
            self.law_drawn.set(Some(Instant::now()));
            self.law_glow.set(glow);
            let img = crate::law::render(key.0, key.1, th.get_s5() * scale, scale, &colors, live);
            d.set_law(slint::Image::from_rgba8(img));
        }
    }

    /// A deck's channel meter: (L, R, peak hold, VU) as 0…1 on the absolute
    /// dBFS scale (−60 … 0 dBFS), after the channel fader and crossfader
    /// (a faded-out deck reads empty). VU is −1 when not measured.
    pub(super) fn deck_meter<const N: usize>(
        index: Option<usize>,
        m: &metering::Metering,
        faders: [f32; N],
        xfader: f32,
        i: usize,
    ) -> (f32, f32, f32, f32) {
        let Some(index) = index else {
            return (0.0, 0.0, 0.0, -1.0);
        };
        // Engine levels already include the fader (exact, per cycle); an
        // older engine's don't, so apply the mirrored fader and crossfader.
        let gain = if m.post_fader {
            0.0
        } else {
            patch_engine::session::Mixer {
                faders: [faders[0], faders[1]],
                extra_faders: [*faders.get(2).unwrap_or(&0.0), *faders.get(3).unwrap_or(&0.0)],
                extra_trims_db: [0.0; 2],
                xfader,
                auto_gain: false,
                group_db: 0.0,
                group_mute: false,
                trims_db: [0.0; 2],
                ..patch_engine::session::Mixer::default()
            }
            .gains4_db()[i]
        };
        let pos = |db: f32| {
            if gain <= -120.0 || db <= SILENCE_DB + 1.0 {
                0.0
            } else {
                meter_pos(patch_ui::meter::MeterKind::System, db + gain)
            }
        };
        let (l, r) = m
            .strips_lr
            .get(index)
            .map(|s| (s[0].level, s[1].level))
            .unwrap_or((SILENCE_DB, SILENCE_DB));
        let peak = m
            .strips_lr
            .get(index)
            .map(|s| s[0].peak.max(s[1].peak))
            .unwrap_or(SILENCE_DB);
        let vu = m.strips_vu.get(index).copied().filter(|v| *v > -70.0);
        (
            pos(l),
            pos(r),
            pos(peak),
            vu.map(|v| pos(v + patch_meter::DEFAULT_REFERENCE_DBFS as f32))
                .unwrap_or(-1.0),
        )
    }
}

/// Title and artist for a deck. The file path stays out of both: the deck
/// draws `source` only when Show track path is on.
pub(super) fn track_lines(title: &str, artist: &str, source: &str) -> (String, String) {
    let source = source.trim();
    let tagged_artist = artist.trim();
    let artist = if tagged_artist.is_empty() {
        crate::visual::artist_of(source)
    } else {
        tagged_artist.to_string()
    };
    let tagged = title.trim();
    // A title that is the path itself is the path, not a name.
    let tagged = if tagged == source { "" } else { tagged };
    let title = if !tagged.is_empty() {
        tagged.to_string()
    } else {
        let stem = std::path::Path::new(source)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let from_name = stem
            .split_once(" - ")
            .map(|(_, rest)| rest.trim().to_string())
            .filter(|rest| !rest.is_empty() && !artist.is_empty());
        let name = from_name.unwrap_or(stem);
        if name.trim().is_empty() {
            "Untitled".into()
        } else {
            name
        }
    };
    (title, artist)
}

#[cfg(test)]
mod tests {
    use super::track_lines;

    #[test]
    fn a_deck_shows_title_and_artist_not_the_path() {
        let path = "/home/user/Music/Kasper Lind/Midnight Transit.flac";
        let (title, artist) = track_lines("Midnight Transit", "Kasper Lind", path);
        assert_eq!(title, "Midnight Transit");
        assert_eq!(artist, "Kasper Lind");
        assert!(!title.contains(path) && !artist.contains(path));

        // A title that is the path itself is not a name.
        let (title, artist) = track_lines(path, "", path);
        assert_eq!(title, "Midnight Transit");
        assert!(artist.is_empty());
        assert!(!title.contains('/'));

        let bare = "/home/user/Music/82. Shermanology - Insane (Original Mix).mp3";
        let (title, artist) = track_lines("", "", bare);
        assert_eq!(title, "Insane (Original Mix)");
        assert_eq!(artist, "Shermanology");
        assert!(!title.contains("/home") && !artist.contains("/home"));
    }
}
