//! Beat sync: tempo maps, beat positions, and the syncer thread that keeps a following deck on the master's beat.

use super::*;

/// Full source beat position. Keep its integer part until after half/double
/// time conversion: odd master beats are half-beats to a half-time follower.
#[cfg(test)]
pub(super) fn beat_position(pos_frames: f64, rate: f64, bpm: f64, offset: f64) -> f64 {
    (pos_frames / rate - offset) * bpm / 60.0
}

/// Tempo fader setting and beat ratio for the closest half/normal/double
/// time match. A match outside the transport's range cannot stay locked.
pub(super) fn sync_tempo(master_bpm: f64, master_tempo: f64, follower_bpm: f64) -> Option<(f64, f64)> {
    let target = master_bpm * (1.0 + master_tempo);
    if !target.is_finite() || target <= 0.0 || !follower_bpm.is_finite() || follower_bpm <= 0.0 {
        return None;
    }
    let tempo = [0.5, 1.0, 2.0]
        .iter()
        .map(|k| target * k / follower_bpm - 1.0)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))?;
    (tempo.is_finite() && tempo.abs() <= 0.5)
        .then_some((tempo, follower_bpm * (1.0 + tempo) / target))
}

pub(super) fn tempo_map_bpm_at(map: &[SyncTempoSegment], fallback_bpm: f64, seconds: f64) -> f64 {
    let index = map.partition_point(|segment| segment.end_s <= seconds);
    map.get(index)
        .filter(|segment| seconds >= segment.start_s && seconds < segment.end_s)
        .map(|segment| {
            if !segment.start_s.is_finite()
                || !segment.end_s.is_finite()
                || segment.end_s <= segment.start_s
            {
                return f64::NAN;
            }
            let phase =
                ((seconds - segment.start_s) / (segment.end_s - segment.start_s)).clamp(0.0, 1.0);
            segment.bpm_start + (segment.bpm_end - segment.bpm_start) * phase
        })
        .filter(|bpm| bpm.is_finite() && (40.0..=300.0).contains(bpm))
        .unwrap_or(fallback_bpm)
}

/// Integrate the piecewise-linear reviewed tempo to get musical beat phase.
/// Gaps use the scalar grid BPM. This runs in `syncer`, never in audio RT.
pub(super) fn beat_position_on_map(
    seconds: f64,
    grid: crate::deck::transport::Grid,
    map: &[SyncTempoSegment],
) -> f64 {
    if !seconds.is_finite() || !grid.offset.is_finite() || grid.bpm <= 0.0 {
        return f64::NAN;
    }
    let (start, end, direction) = if seconds >= grid.offset {
        (grid.offset, seconds, 1.0)
    } else {
        (seconds, grid.offset, -1.0)
    };
    let mut cursor = start;
    let mut beats = 0.0;
    let first_segment = map.partition_point(|segment| segment.end_s <= cursor);
    for segment in &map[first_segment..] {
        if segment.start_s >= end {
            break;
        }
        if segment.end_s <= cursor || segment.start_s >= end {
            continue;
        }
        let overlap_start = cursor.max(segment.start_s);
        let overlap_end = end.min(segment.end_s);
        if overlap_start > cursor {
            beats += (overlap_start - cursor) * grid.bpm / 60.0;
        }
        if overlap_end > overlap_start {
            let segment_bpm = |at: f64| {
                let phase =
                    ((at - segment.start_s) / (segment.end_s - segment.start_s)).clamp(0.0, 1.0);
                segment.bpm_start + (segment.bpm_end - segment.bpm_start) * phase
            };
            let first = segment_bpm(overlap_start);
            let last = segment_bpm(overlap_end);
            beats += (overlap_end - overlap_start) * (first + last) / 120.0;
            cursor = overlap_end;
        }
    }
    if end > cursor {
        beats += (end - cursor) * grid.bpm / 60.0;
    }
    direction * beats
}

pub(super) fn sync_tempo_map(analysis: &patch_analysis::Analysis) -> Vec<SyncTempoSegment> {
    let mut map = if let Some(human) = analysis.human_grid.as_ref() {
        if human.segments.len() > 4096 {
            return Vec::new();
        }
        let mut map = Vec::with_capacity(human.segments.len());
        for segment in &human.segments {
            let end_s = segment.end_s.unwrap_or(analysis.duration);
            if !segment.start_s.is_finite()
                || !end_s.is_finite()
                || end_s <= segment.start_s
                || !segment.bpm_start.is_finite()
                || !segment.bpm_end.is_finite()
                || !(40.0..=300.0).contains(&segment.bpm_start)
                || !(40.0..=300.0).contains(&segment.bpm_end)
            {
                return Vec::new();
            }
            map.push(SyncTempoSegment {
                start_s: segment.start_s,
                end_s,
                bpm_start: segment.bpm_start,
                bpm_end: segment.bpm_end,
            });
        }
        map
    } else {
        let Some(evidence) = analysis.rhythm.as_ref().filter(|evidence| {
            evidence.analyzer_version == patch_analysis::tempo::RHYTHM_EVIDENCE_VERSION
        }) else {
            return Vec::new();
        };
        if evidence.regions.len() > 4096 {
            return Vec::new();
        }
        evidence
            .regions
            .iter()
            .filter_map(|region| {
                // Only variable-tempo tracks carry regional tempos at all.
                let bpm = f64::from(region.local_bpm?);
                (region.start_s.is_finite()
                    && region.end_s.is_finite()
                    && region.end_s > region.start_s
                    && (40.0..=300.0).contains(&bpm))
                .then_some(SyncTempoSegment {
                    start_s: region.start_s,
                    end_s: region.end_s,
                    bpm_start: bpm,
                    bpm_end: bpm,
                })
            })
            .collect()
    };
    map.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    for i in 1..map.len() {
        if map[i].start_s < map[i - 1].start_s {
            return Vec::new();
        }
        let overlap = map[i - 1].end_s - map[i].start_s;
        if overlap > 0.001 {
            return Vec::new();
        }
        // Adjacent regions derive their boundaries through separate floating
        // point expressions. Remove sub-millisecond numerical overlap while
        // keeping genuinely contradictory maps out of sync.
        if overlap > 0.0 {
            map[i - 1].end_s = map[i].start_s;
        }
    }
    map
}

#[cfg(test)]
#[test]
fn half_time_sync_keeps_the_master_beat_number() {
    let rate = 48_000.0;
    let master = beat_position(24_000.0, rate, 120.0, 0.0);
    assert_eq!(master, 1.0);
    assert_eq!((master * 0.5).rem_euclid(1.0), 0.5);
    assert_eq!((master * 2.0).rem_euclid(1.0), 0.0);
}

#[cfg(test)]
#[test]
fn sync_tempo_selects_a_reachable_beat_ratio() {
    assert_eq!(sync_tempo(120.0, 0.0, 60.0), Some((0.0, 0.5)));
    assert_eq!(sync_tempo(120.0, 0.0, 240.0), Some((0.0, 2.0)));
    assert_eq!(sync_tempo(120.0, 0.0, 20.0), None);
}

#[cfg(test)]
#[test]
fn human_tempo_map_sync_uses_integrated_phase_and_local_tempo() {
    let grid = crate::deck::transport::Grid {
        bpm: 120.0,
        offset: 0.0,
        downbeat: Some(0),
    };
    let map = [
        SyncTempoSegment {
            start_s: 0.0,
            end_s: 60.0,
            bpm_start: 120.0,
            bpm_end: 130.0,
        },
        SyncTempoSegment {
            start_s: 60.0,
            end_s: 120.0,
            bpm_start: 110.0,
            bpm_end: 110.0,
        },
    ];
    assert_eq!(tempo_map_bpm_at(&map, grid.bpm, 30.0), 125.0);
    assert_eq!(tempo_map_bpm_at(&map, grid.bpm, 60.0), 110.0);
    assert!((beat_position_on_map(30.0, grid, &map) - 61.25).abs() < 1e-9);
    assert!((beat_position_on_map(60.0, grid, &map) - 125.0).abs() < 1e-9);
    assert!((beat_position_on_map(65.0, grid, &map) - (125.0 + 5.0 * 110.0 / 60.0)).abs() < 1e-9);
    assert_eq!(beat_position_on_map(65.0, grid, &[]), 130.0);
}

#[cfg(test)]
#[test]
fn sync_tempo_map_rejects_malformed_or_overlapping_segments() {
    let mut analysis = patch_analysis::analyse(&vec![0.0; patch_analysis::RATE as usize * 2]);
    analysis.human_grid = Some(patch_analysis::HumanGridOverride {
        bpm: 120.0,
        offset: 0.0,
        beats_per_bar: Some(4),
        downbeat_phase: Some(0),
        segments: vec![
            patch_analysis::HumanTempoSegment {
                start_s: 0.0,
                end_s: Some(1.5),
                bpm_start: 120.0,
                bpm_end: 121.0,
            },
            patch_analysis::HumanTempoSegment {
                start_s: 1.0,
                end_s: Some(2.0),
                bpm_start: 122.0,
                bpm_end: 122.0,
            },
        ],
    });
    assert!(sync_tempo_map(&analysis).is_empty());
    analysis.human_grid.as_mut().unwrap().segments[1].start_s = 1.5;
    let map = sync_tempo_map(&analysis);
    assert_eq!(map.len(), 2);
    assert_eq!(map[1].start_s, 1.5);

    analysis.human_grid = None;
    analysis.rhythm = Some(patch_analysis::tempo::RhythmEvidence {
        selection_strength: None,
        analyzer_version: patch_analysis::tempo::RHYTHM_EVIDENCE_VERSION,
        beats: vec![],
        regions: vec![
            patch_analysis::tempo::RhythmRegion {
                start_s: 0.0,
                end_s: 0.5,
                beat_count: 4,
                observed_onset_coverage: 0.75,
                mean_onset_support: 2.0,
                onset_jitter_ms: 5.0,
                phase_spread_ms: 10.0,
                local_bpm: Some(124.0),
                tempo_fit_residual_ms: None,
            },
            patch_analysis::tempo::RhythmRegion {
                start_s: 0.499_999_9,
                end_s: 1.0,
                beat_count: 4,
                observed_onset_coverage: 0.75,
                mean_onset_support: 2.0,
                onset_jitter_ms: 5.0,
                phase_spread_ms: 10.0,
                local_bpm: Some(126.0),
                tempo_fit_residual_ms: None,
            },
        ],
        tempo_hypotheses: vec![],
        meter_hypotheses: vec![],
    });
    let map = sync_tempo_map(&analysis);
    assert_eq!(map.len(), 2);
    assert_eq!(map[0].bpm_start, 124.0);
    assert_eq!(map[0].end_s, map[1].start_s);
}

/// Keeps synced decks locked: tempo follows the master's fader, beat phase
/// is measured from both decks' positions in the *same* engine cycle and
/// corrected — once hard (a beat jump) when engaging, then gradually (a
/// bend of at most ±0.4 %). A manual nudge suspends phase correction.
pub(super) fn syncer(s: Arc<Shared>) {
    use crate::deck::transport::Cmd;
    let p = &*s.params;
    let mut last_seq = 0;
    loop {
        // Every graph block while a deck is synced; else 10 times a second
        // (it was 200 times, for nothing).
        let synced = s.sync.lock().unwrap().iter().any(|x| x.on);
        let resting = !s.always.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(if synced {
            5
        } else if resting {
            250
        } else {
            100
        }));
        let Some((seq, block)) = s.history.lock().unwrap().back().copied() else {
            continue;
        };
        if seq == last_seq {
            continue;
        }
        last_seq = seq;
        let rate = block.stamp.rate.max(1) as f64;
        let mut all = s.sync.lock().unwrap();
        let tempo_maps = s.tempo_maps.lock().unwrap();
        for f in 0..p.decks.len() {
            let st = all[f];
            if !st.on {
                continue;
            }
            let fs = p.decks[f].status.lock().unwrap().clone();
            // Master: chosen, else the other playing deck.
            let m = st.master.or_else(|| {
                (0..p.decks.len()).find(|&d| d != f && p.decks[d].status.lock().unwrap().playing)
            });
            let Some(m) = m.filter(|&m| m != f) else {
                continue;
            };
            let ms = p.decks[m].status.lock().unwrap().clone();
            let (Some(gf), Some(gm)) = (fs.grid, ms.grid) else {
                continue;
            };
            let (pf, pm) = (block.deck_pos[f], block.deck_pos[m]);
            if pf.is_nan() || pm.is_nan() || !fs.playing || !ms.playing {
                continue;
            }
            let bpm_f = tempo_map_bpm_at(&tempo_maps[f], gf.bpm, pf / rate);
            let bpm_m = tempo_map_bpm_at(&tempo_maps[m], gm.bpm, pm / rate);
            // Safety net for a mutual lock (set before the invariant existed,
            // or by hand on both decks): only the quieter deck follows.
            let gains = s.session.lock().unwrap().mixer.gains_db();
            let audible = |d: usize| gains.get(d).copied().unwrap_or(-120.0);
            if all[m].on && all[m].master.is_none_or(|x| x == f) && audible(f) > audible(m) {
                if fs.bend.abs() > 1e-5 {
                    p.decks[f].send(Cmd::Bend(0.0));
                }
                all[f].error_ms = 0.0;
                continue;
            }
            // The deck you hear never jumps: above −24 dB it corrects by
            // bending only (stronger when far off); jumps are for a deck
            // that is still silent (engaging before the fader opens).
            let silent = audible(f) <= -24.0;
            // Tempo: the master's fader tempo (not its corrections), half or
            // double time when closer. Outside ±50%, no valid lock exists.
            let Some((tempo, ratio)) = sync_tempo(bpm_m, ms.tempo, bpm_f) else {
                all[f].error_ms = 0.0;
                if fs.bend.abs() > 1e-5 {
                    p.decks[f].send(Cmd::Bend(0.0));
                }
                continue;
            };
            let abrupt_tempo_change = (tempo - fs.tempo).abs() > 0.001;
            if all[f]
                .last_commanded_tempo
                .is_none_or(|previous| (tempo - previous).abs() >= 0.00005)
            {
                p.decks[f].send(Cmd::Tempo(tempo));
                all[f].last_commanded_tempo = Some(tempo);
                if abrupt_tempo_change {
                    all[f].tempo_at = Some(std::time::Instant::now());
                }
            }
            // Not yet playing at the new tempo: measure, but don't jump.
            let settling = all[f]
                .tempo_at
                .is_some_and(|t| t.elapsed() < Duration::from_millis(150));
            // Beats per master beat: 1, or 2/0.5 in half/double time.
            let phm = (beat_position_on_map(pm / rate, gm, &tempo_maps[m]) * ratio).rem_euclid(1.0);
            let phf = beat_position_on_map(pf / rate, gf, &tempo_maps[f]).rem_euclid(1.0);
            let mut err = phm - phf;
            if err >= 0.5 {
                err -= 1.0;
            } else if err < -0.5 {
                err += 1.0;
            }
            // Show and control phase error in output time, after the
            // follower's tempo fader, rather than in source-track time.
            let err_s = err * 60.0 / (bpm_f * (1.0 + tempo));
            all[f].error_ms = err_s * 1e3;
            if st.phase_hold {
                continue;
            }
            if st.engage && settling && silent {
                continue;
            }
            let mut engage = st.engage;
            if engage && !silent {
                // Already audible when engaged: no jump, bend in instead.
                all[f].engage = false;
                engage = false;
            }
            if engage {
                all[f].engage = false;
                // Bars too when both downbeats are known at the same beat
                // ratio: the downbeat lands on the downbeat.
                let bar_err = match (gf.downbeat, gm.downbeat) {
                    (Some(df), Some(dm)) if (ratio - 1.0).abs() < 1e-6 => {
                        let beats = |pos: f64,
                                     g: crate::deck::transport::Grid,
                                     d: u8,
                                     map: &[SyncTempoSegment]| {
                            beat_position_on_map(pos / rate, g, map) - d as f64
                        };
                        let mut e = (beats(pm, gm, dm, &tempo_maps[m]) / 4.0).rem_euclid(1.0)
                            - (beats(pf, gf, df, &tempo_maps[f]) / 4.0).rem_euclid(1.0);
                        if e >= 0.5 {
                            e -= 1.0;
                        } else if e < -0.5 {
                            e += 1.0;
                        }
                        Some(e * 4.0)
                    }
                    _ => None,
                };
                let mut jump = bar_err.unwrap_or(err);
                // Never before the start of the track: the same phase one
                // bar (or beat) later instead.
                let unit = if bar_err.is_some() { 4.0 } else { 1.0 };
                while pf / rate + jump * 60.0 / gf.bpm < 0.0 {
                    jump += unit;
                }
                if (jump * 60.0 / gf.bpm).abs() > 0.005 {
                    all[f].last_jump = Some(std::time::Instant::now());
                    p.decks[f].send(Cmd::Jump(jump));
                    p.decks[f].send(Cmd::Bend(0.0));
                    all[f].bars = bar_err.is_some();
                    continue;
                }
                all[f].bars = bar_err.is_some();
            }
            // Far off (a jump that landed badly, a big drift): correct hard
            // again, at most every half second.
            if silent
                && !settling
                && err_s.abs() > 0.02
                && st
                    .last_jump
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(500))
            {
                let mut j = err;
                while pf / rate + j * 60.0 / gf.bpm < 0.0 {
                    j += 1.0;
                }
                p.decks[f].send(Cmd::Jump(j));
                p.decks[f].send(Cmd::Bend(0.0));
                all[f].last_jump = Some(std::time::Instant::now());
                continue;
            }
            // Gradual: proportional bend, dead band 0.2 ms, at most ±0.4 %
            // (±1.5 % while an audible deck is more than 20 ms off, so it
            // pulls in within a beat or two instead of seconds).
            let limit = if !silent && err_s.abs() > 0.02 {
                0.015
            } else {
                0.004
            };
            let bend = if err_s.abs() < 0.0002 {
                0.0
            } else {
                (err_s * 2.0).clamp(-limit, limit)
            };
            if (bend - fs.bend).abs() > 1e-5 {
                p.decks[f].send(Cmd::Bend(bend));
            }
        }
        // Sequencer sync: the steps follow the deck's position on the RT
        // thread; this keeps the shown BPM at the deck's played tempo.
        if s.params.sequencer.sync_on() {
            let src = s.params.sequencer.sync_source() as usize;
            if src < p.decks.len() {
                let (gs, tempo) = {
                    let st = p.decks[src].status.lock().unwrap();
                    (st.grid, st.tempo)
                };
                if let Some(g) = gs {
                    let target = (g.bpm * (1.0 + tempo)).round() as u32;
                    let current = s.params.sequencer.bpm();
                    if target != current {
                        s.params.sequencer.set_bpm(target);
                    }
                }
            }
        }
    }
}

thread_local! {
    /// The graph sample the mixer changes being made now are due at (the
    /// planner sets it around a timed step): they are queued for the RT
    /// thread, which applies them on that sample.
    pub(super) static DUE_AT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// Samples before `DUE_AT` the change glides over (a ramp's step: the
    /// whole step, so a sweep sounds as a sweep, not as a staircase).
    pub(super) static DUE_GLIDE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(super) fn due_at() -> Option<u64> {
    DUE_AT.with(|d| d.get())
}

pub(super) fn due_glide() -> u32 {
    DUE_GLIDE.with(|d| d.get())
}
