//! Presentation of observed AI milestones and explicitly estimated checks.
use patch_core::aivis::{Mode, Phase, VisualState};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Tracker {
    pair: String,
    last: Option<patch_core::aivis::Transition>,
    seen: [[Option<f64>; 6]; 2],
    /// The outgoing deck's (position, beat) when this pair was first seen:
    /// its beat length is measured from here, so the grid's offset (the
    /// first downbeat is rarely at 0:00) cancels out.
    anchor: Option<(f64, f64)>,
    frozen_playhead: [Option<(f32, Instant)>; 2],
    previews: [Option<(String, Vec<crate::ui::MissionEvent>)>; 2],
    /// Each marker's estimated time as last drawn: it moves only when a new
    /// estimate differs by more than `STEADY_S`. Recomputed from the
    /// playhead and the beat (read a moment apart) every update, the letters
    /// shifted a fraction of a pixel each time and shimmered.
    planned: [[Option<f64>; 6]; 2],
    /// When the transition was last in a status: one missing from a single
    /// reply (a replan, a slow poll) keeps the markers instead of blanking
    /// them for a frame.
    last_at: Option<Instant>,
}

/// A marker moves only when its estimate changes by more than this.
const STEADY_S: f64 = 0.25;
/// A transition missing from the status for less than this is still drawn.
const HOLD: Duration = Duration::from_millis(1500);

impl Tracker {
    pub fn update(
        &mut self,
        s: &VisualState,
        plan: &serde_json::Value,
    ) -> (bool, [Vec<crate::ui::MissionEvent>; 2]) {
        let active = s.mode == Mode::Act
            && s.sources.iter().any(|s| s.playing)
            && !matches!(
                s.phase,
                Phase::Recovering | Phase::Unavailable | Phase::Idle
            );
        let mut out = [vec![], vec![]];
        if !active {
            self.pair.clear();
            self.last = None;
            self.seen = [[None; 6]; 2];
            self.anchor = None;
            self.frozen_playhead = [None; 2];
            self.planned = [[None; 6]; 2];
            self.last_at = None;
            return (false, out);
        }
        let monitoring = s.transition.is_none() && s.phase == Phase::Monitoring;
        if let Some(tr) = &s.transition {
            self.last = Some(tr.clone());
            self.last_at = Some(Instant::now());
        }
        let held = self.last_at.is_some_and(|t| t.elapsed() < HOLD);
        let Some(tr) = s
            .transition
            .as_ref()
            .or(if monitoring || held { self.last.as_ref() } else { None })
        else {
            for (i, id) in ["deck-a", "deck-b"].iter().enumerate() {
                if let Some(src) = s
                    .sources
                    .iter()
                    .find(|src| src.id == *id && !src.title.is_empty())
                {
                    out[i] = cached_preview(&mut self.previews[i], src, 16.0);
                }
            }
            return (true, out);
        };
        let from = s.sources.iter().find(|s| s.id == tr.from);
        let pair = format!("{}:{}:{}", tr.from, tr.to, tr.start_beat);
        if pair != self.pair {
            self.pair = pair;
            self.seen = [[None; 6]; 2];
            self.anchor = None;
            self.frozen_playhead = [None; 2];
            self.planned = [[None; 6]; 2];
        }
        let matching = plan["from"].as_str() == tr.from.strip_prefix("deck-")
            && plan["start_beat"]
                .as_f64()
                .is_some_and(|b| (b - tr.start_beat).abs() < 0.01);
        let checked = matching && plan["alignment_checked"].as_bool() == Some(true);
        // The 8 marker is the eight-bar check (the review 16 bars out runs
        // silently, without a marker).
        let reviewed = matching && plan["eight_checked"].as_bool() == Some(true);
        let attempting =
            matching && plan["eight_attempted"].as_bool() == Some(true) && !reviewed;
        // Keep its status tied directly to the engine's confirmed result.
        let done = [
            s.selected().is_some(),
            tr.ready || tr.started,
            reviewed,
            checked,
            tr.started,
            monitoring || (tr.started && tr.done == tr.total),
        ];
        let labels = ["", "prepare", "8-bar", "", "mix", "complete"];
        for (i, events) in out.iter_mut().enumerate() {
            let id = if i == 0 { "deck-a" } else { "deck-b" };
            let Some(src) = s
                .sources
                .iter()
                .find(|s| s.id == id && (s.id == tr.from || s.id == tr.to))
            else {
                continue;
            };
            if id == tr.to {
                *events = cached_preview(&mut self.previews[i], src, tr.bars as f64);
                if (tr.ready || tr.started)
                    && let Some(prepare) = events.first_mut()
                {
                    prepare.status = 1;
                }
                if (monitoring || (tr.started && tr.done == tr.total))
                    && let Some(complete) = events.last_mut()
                {
                    complete.status = 1;
                }
                continue;
            }
            let duration = (src.position + src.remaining).max(1.0);
            for k in 0..6 {
                if done[k] && self.seen[i][k].is_none() {
                    self.seen[i][k] = Some(src.position);
                }
                if k == 5 && done[k] && self.frozen_playhead[i].is_none() {
                    self.frozen_playhead[i] = Some((
                        (src.position / duration).clamp(0.0, 1.0) as f32,
                        Instant::now() + Duration::from_secs(4),
                    ));
                }
                // Recomputed every update from where the playhead is now. It
                // was computed once, early in the track, from position ÷ beat,
                // which counts the time before the first downbeat as beats:
                // 5 s in with the first downbeat at 1.3 s that made a 0.46 s
                // beat 0.62 s, put the mix past the track's end and drew
                // 8 · M · C squashed together at the right edge.
                let planned = if id == tr.from {
                    from.and_then(|f| {
                        let beat = f.beat?;
                        let anchor = *self.anchor.get_or_insert((f.position, beat));
                        let spb = seconds_per_beat(anchor, (f.position, beat), f.bpm)?;
                        let offset = match k {
                            2 => -32.0,
                            3 => -8.0,
                            4 => 0.0,
                            5 => tr.bars as f64 * 4.0,
                            _ => return None,
                        };
                        Some((f.position + (tr.start_beat + offset - beat) * spb).max(0.0))
                    })
                } else {
                    None
                };
                let planned = planned.map(|t| match self.planned[i][k] {
                    Some(was) if (t - was).abs() <= STEADY_S => was,
                    _ => {
                        self.planned[i][k] = Some(t);
                        t
                    }
                });
                let time = planned.or(self.seen[i][k]);
                let stamp = time
                    .map(|t| {
                        format!(
                            "{}{:02}:{:02}",
                            if planned.is_some() { "~" } else { "" },
                            t as u64 / 60,
                            t as u64 % 60
                        )
                    })
                    .unwrap_or_else(|| "pending".into());
                if k == 0 || k == 3 {
                    continue;
                } // The alignment check runs silently, without a display stage.
                let freeze_progress = if k == 5 {
                    self.frozen_playhead[i]
                        .filter(|(_, until)| Instant::now() < *until)
                        .map(|(p, _)| p)
                        .unwrap_or(-1.0)
                } else {
                    -1.0
                };
                events.push(crate::ui::MissionEvent {
                    preview: false,
                    label: labels[k].into(),
                    stamp: stamp.into(),
                    progress: time
                        .map(|t| (t / duration).clamp(0.0, 1.0) as f32)
                        .unwrap_or(-1.0),
                    freeze_progress,
                    status: if self.seen[i][k].is_some() || (k == 2 && reviewed) {
                        1
                    } else if k == 2 && attempting {
                        2
                    } else {
                        0
                    },
                });
            }
        }
        (true, out)
    }
}

/// Seconds of track per beat: measured between the first sighting and now
/// once 16 beats have passed (exact whatever the grid's offset and the
/// tempo change), else the tempo as played. A measurement more than 8 % off
/// the tempo (a seek or a loop in between) is not used.
fn seconds_per_beat(anchor: (f64, f64), now: (f64, f64), bpm: Option<f64>) -> Option<f64> {
    let tempo = bpm.filter(|b| *b > 0.0).map(|b| 60.0 / b);
    let beats = now.1 - anchor.1;
    let seconds = now.0 - anchor.0;
    if beats >= 16.0 && seconds > 0.0 {
        let measured = seconds / beats;
        if tempo.is_none_or(|t| (measured / t - 1.0).abs() <= 0.08) {
            return Some(measured);
        }
    }
    tempo
}

fn cached_preview(
    cache: &mut Option<(String, Vec<crate::ui::MissionEvent>)>,
    src: &patch_core::aivis::Source,
    entry_bars: f64,
) -> Vec<crate::ui::MissionEvent> {
    let key = format!(
        "{}:{}:{}",
        src.title,
        src.artist,
        (src.position + src.remaining).round()
    );
    if cache
        .as_ref()
        .is_none_or(|(k, v)| *k != key || v.is_empty())
    {
        *cache = Some((key, preview(src, entry_bars)));
    }
    cache.as_ref().unwrap().1.clone()
}

// Before this track leads, the engine has not selected its successor yet.
// Give it a clearly provisional lifecycle, replaced by the real plan when selected.
fn preview(src: &patch_core::aivis::Source, entry_bars: f64) -> Vec<crate::ui::MissionEvent> {
    let duration = src.position + src.remaining;
    let Some(bpm) = src.bpm.filter(|b| *b > 0.0) else {
        return vec![];
    };
    if duration <= 0.0 {
        return vec![];
    }
    let spb = if src.position > 0.0 && src.beat.is_some_and(|b| b > 8.0) {
        src.position / src.beat.unwrap()
    } else {
        60.0 / bpm
    };
    let bar = 4.0 * spb;
    let complete = (duration / (bar * 8.0)).floor() * bar * 8.0;
    let start = (complete - 16.0 * bar).max(0.0);
    let decision = (src.position + entry_bars * bar).min((start - 8.0 * bar).max(0.0));
    let times = [
        decision,
        (decision + 8.0).min((start - 8.0 * bar).max(0.0)),
        (start - 8.0 * bar).max(0.0),
        (start - 2.0 * bar).max(0.0),
        start,
        complete,
    ];
    ["", "prepare", "8-bar", "", "mix", "complete"]
        .iter()
        .zip(times)
        .filter(|(label, _)| !label.is_empty())
        .map(|(label, t)| crate::ui::MissionEvent {
            preview: true,
            label: (*label).into(),
            stamp: format!("~{:02}:{:02}", t as u64 / 60, t as u64 % 60).into(),
            progress: (t / duration).clamp(0.0, 1.0) as f32,
            freeze_progress: -1.0,
            status: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> VisualState {
        crate::aidj::fixture("transition_active").unwrap()
    }
    #[test]
    fn mission_is_exclusive_to_autonomous_ai_playback() {
        let mut t = Tracker::default();
        let mut s = state();
        s.mode = Mode::Act;
        assert!(t.update(&s, &serde_json::Value::Null).0);
        for mode in [Mode::Observe, Mode::Suggest, Mode::Prepare] {
            s.mode = mode;
            let (available, events) = t.update(&s, &serde_json::Value::Null);
            assert!(!available && events.iter().all(Vec::is_empty));
        }
        s.mode = Mode::Act;
        s.phase = Phase::Recovering;
        assert!(!t.update(&s, &serde_json::Value::Null).0);
        s.phase = Phase::Listening;
        for source in &mut s.sources {
            source.playing = false;
        }
        assert!(!t.update(&s, &serde_json::Value::Null).0);
    }
    /// The playhead and the beat are read a moment apart: a wobble between
    /// them must not move the markers (their letters shimmered), while a
    /// real change of plan still does.
    #[test]
    fn markers_hold_still_through_reading_jitter() {
        let mut t = Tracker::default();
        let mut s = state();
        s.mode = Mode::Act;
        let from = s.transition.as_ref().unwrap().from.clone();
        let (_, before) = t.update(&s, &serde_json::Value::Null);
        let i = if from == "deck-a" { 0 } else { 1 };
        let src = s.sources.iter_mut().find(|x| x.id == from).unwrap();
        // The playhead read 0.1 s later than the beat (the track's length
        // stays what it is).
        src.position += 0.1;
        src.remaining -= 0.1;
        let (_, after) = t.update(&s, &serde_json::Value::Null);
        for (a, b) in before[i].iter().zip(&after[i]) {
            assert_eq!(a.progress, b.progress, "{} moved", a.label);
        }
        s.transition.as_mut().unwrap().start_beat += 16.0;
        let (_, moved) = t.update(&s, &serde_json::Value::Null);
        assert!(moved[i].iter().zip(&after[i]).any(|(a, b)| a.progress != b.progress));
    }

    /// A transition missing from one status reply keeps its markers.
    #[test]
    fn a_missing_reply_does_not_blank_the_timeline() {
        let mut t = Tracker::default();
        let mut s = state();
        s.mode = Mode::Act;
        let (_, with) = t.update(&s, &serde_json::Value::Null);
        s.transition = None;
        let (_, without) = t.update(&s, &serde_json::Value::Null);
        assert_eq!(with[0].len(), without[0].len());
        assert_eq!(with[1].len(), without[1].len());
    }

    #[test]
    fn checks_require_confirmation_and_survive_handover() {
        let mut t = Tracker::default();
        let mut s = state();
        s.mode = Mode::Act;
        s.sources[1].position = 9.76;
        s.sources[1].remaining = 290.24;
        let tr = s.transition.as_ref().unwrap();
        let plan = serde_json::json!({"from":tr.from.strip_prefix("deck-"), "start_beat":tr.start_beat, "eight_checked":true,"alignment_checked":true});
        let (_, first) = t.update(&s, &serde_json::Value::Null);
        assert_eq!(first[1].len(), 4);
        assert!(first[1].iter().all(|e| e.preview && e.progress >= 0.0));
        assert_eq!(first[1][0].status, i32::from(tr.ready || tr.started));
        assert!(first[1][1..].iter().all(|e| e.status == 0));
        assert_eq!(first[0][1].status, 0);
        assert!(t.seen[0][3].is_none());
        let (_, checked) = t.update(&s, &plan);
        assert_eq!(checked[0][1].status, 1);
        assert_eq!(checked[1][2].progress, first[1][2].progress);
        assert!(t.seen[0][3].is_some());
        assert_eq!(checked[0][1].progress, first[0][1].progress);
        assert!(checked[0][1].progress < checked[0][2].progress);
        assert!(checked[0][2].progress < checked[0][3].progress);
        assert_eq!(
            checked[0]
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            ["prepare", "8-bar", "mix", "complete"]
        );
        s.transition = None;
        s.phase = Phase::Monitoring;
        let (_, completed) = t.update(&s, &serde_json::Value::Null);
        assert_eq!(completed[0][3].status, 1);
        assert_eq!(completed[0][1].status, 1);
    }
    /// 2026-10-14: a set's first track (6:00, 130 BPM, first downbeat at
    /// 1.3 s) planned its mix at beat 611 (4:43), but the timeline drew
    /// 8 · M · C squashed at the track's end: the beat length came from
    /// position ÷ beat 5 s in (0.62 s for 0.46 s) and was never updated.
    #[test]
    fn the_mix_lands_where_the_plan_says_from_the_first_seconds() {
        let mut t = Tracker::default();
        let mut s = state();
        s.mode = Mode::Act;
        let tr = s.transition.as_mut().unwrap();
        tr.start_beat = 611.0;
        tr.bars = 24.0;
        let from = tr.from.clone();
        let i = usize::from(from == "deck-b");
        let spb = 60.0 / 130.0;
        let mix = 1.3 + 611.0 * spb;
        let at = |s: &mut VisualState, t: &mut Tracker, position: f64| {
            let src = s.sources.iter_mut().find(|x| x.id == from).unwrap();
            src.position = position;
            src.remaining = 360.0 - position;
            src.bpm = Some(130.0);
            src.beat = Some((position - 1.3) / spb);
            let (_, events) = t.update(s, &serde_json::Value::Null);
            let m = events[i].iter().find(|e| e.label == "mix").unwrap().progress;
            let c = events[i].iter().find(|e| e.label == "complete").unwrap().progress;
            (m, c)
        };
        for position in [5.0, 30.0, 80.0, 200.0] {
            let (m, c) = at(&mut s, &mut t, position);
            assert!((m as f64 - mix / 360.0).abs() < 0.005, "mix at {} s, {position} s in", m * 360.0);
            // The 24-bar blend is drawn: C sits 44 s after M.
            assert!((c - m) as f64 * 360.0 > 40.0);
        }
    }
}
