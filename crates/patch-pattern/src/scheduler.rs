//! Patch's pattern scheduler: the clock, the lookahead, the hot swap.
//!
//! Runs on a control thread of the Patch service, never on the audio
//! thread. Each [`Scheduler::prepare`] asks the engine for the events of the
//! next window (`cursor` … now + lookahead), stamps each with its graph
//! sample through the [`TempoMap`], and hands them out in time order. A new
//! definition is evaluated at once (an error leaves the running one in
//! place) and takes over at a musical boundary the caller chooses.
//!
//! Instrumented throughout ([`Metrics`]): evaluation and query cost, late
//! events, swap latency. Those numbers decide what a native engine must beat.

use crate::backend::{Backend, Definition, Loaded, PatternError};
use crate::event::Event;
use crate::tempo::{TempoMap, snap, snap_up};
use crate::time::{Span, Time};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// When a new definition takes over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantize {
    /// At the first time not yet prepared (within the lookahead).
    Now,
    /// The next beat.
    Beat,
    /// The next cycle (a bar, by default).
    #[default]
    Cycle,
    /// The next multiple of n cycles (4: a phrase).
    Cycles(u32),
}

impl Quantize {
    /// The first boundary at or after `t`.
    pub fn boundary(self, t: Time, cycle_beats: Time) -> Time {
        match self {
            Quantize::Now => snap_up(t),
            Quantize::Beat => {
                let beats = t * cycle_beats;
                beats.ceil() / cycle_beats
            }
            Quantize::Cycle => t.ceil(),
            Quantize::Cycles(n) => {
                let n = Time::whole(n.max(1) as i64);
                (t / n).ceil() * n
            }
        }
    }
}

/// An event with its graph sample.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Scheduled {
    pub sample: i64,
    pub end: i64,
    pub event: Event,
    /// Prepared after its time (the lookahead was too short).
    pub late: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub evals: u64,
    pub eval_errors: u64,
    pub last_eval_us: u64,
    pub max_eval_us: u64,
    pub queries: u64,
    pub query_errors: u64,
    pub last_query_us: u64,
    pub max_query_us: u64,
    pub events: u64,
    pub late_events: u64,
    pub max_late_samples: i64,
    pub swaps: u64,
    /// From the definition arriving to it sounding, last swap (ms).
    pub last_swap_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub playing: bool,
    pub cycle: f64,
    pub bpm: f64,
    pub cycle_beats: Time,
    pub active: Option<u32>,
    /// Pending definition's slot and the cycle it takes over at.
    pub pending: Option<(u32, Time)>,
    pub prepared_to: Option<Time>,
    pub lookahead_ms: f64,
}

pub struct Scheduler {
    engine: Box<dyn Backend>,
    pub tempo: TempoMap,
    active: Option<u32>,
    pending: Option<Pending>,
    next_slot: u32,
    cursor: Option<Time>,
    playing: bool,
    pub lookahead: i64,
    pub metrics: Metrics,
}

struct Pending {
    slot: u32,
    at: Time,
    cycle_beats: Time,
    asked: i64,
}

impl Scheduler {
    pub fn new(engine: Box<dyn Backend>, rate: u32, bpm: f64) -> Scheduler {
        Scheduler {
            engine,
            tempo: TempoMap::new(rate, Time::whole(4), bpm, Time::ZERO, 0),
            active: None,
            pending: None,
            next_slot: 1,
            cursor: None,
            playing: false,
            // 100 ms: control commands and the RT queue both have time.
            lookahead: rate as i64 / 10,
            metrics: Metrics::default(),
        }
    }

    pub fn engine(&self) -> &dyn Backend {
        self.engine.as_ref()
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Evaluate `def`; on success it takes over at `q`'s next boundary
    /// (immediately when nothing plays). On error nothing changes.
    pub fn load(
        &mut self,
        def: &Definition,
        q: Quantize,
        now: i64,
    ) -> Result<(Loaded, Time), Vec<PatternError>> {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.wrapping_add(1).max(1);
        let t0 = Instant::now();
        let loaded = match self.engine.load(slot, def) {
            Ok(l) => l,
            Err(e) => {
                self.metrics.eval_errors += 1;
                return Err(e);
            }
        };
        let us = t0.elapsed().as_micros() as u64;
        self.metrics.evals += 1;
        self.metrics.last_eval_us = us;
        self.metrics.max_eval_us = self.metrics.max_eval_us.max(us);
        if let Some(p) = self.pending.take() {
            self.engine.unload(p.slot);
        }
        let from = self.cursor.unwrap_or_else(|| self.tempo.cycle_at(now));
        let cycle_beats = def.cycle_beats();
        let at = if self.active.is_none() || !self.playing {
            from
        } else {
            q.boundary(from, self.tempo.cycle_beats)
        };
        self.pending = Some(Pending {
            slot,
            at,
            cycle_beats,
            asked: now,
        });
        if !self.playing {
            self.promote(now);
        }
        Ok((loaded, at))
    }

    fn promote(&mut self, swap_sample: i64) {
        let Some(p) = self.pending.take() else { return };
        if let Some(old) = self.active.replace(p.slot) {
            self.engine.unload(old);
        }
        if p.cycle_beats != self.tempo.cycle_beats {
            // Same tempo in beats, a different cycle length: re-anchor so the
            // beat (not the cycle) keeps its sample.
            let bpm = self.tempo.bpm_at(p.at);
            let sample = self.tempo.sample_at(p.at);
            self.tempo.cycle_beats = p.cycle_beats;
            self.tempo.align(sample, p.at, bpm);
        }
        self.metrics.swaps += 1;
        self.metrics.last_swap_ms =
            (swap_sample - p.asked).max(0) as f64 * 1000.0 / self.tempo.rate as f64;
    }

    /// Start at cycle `at` on graph sample `now`.
    pub fn play(&mut self, now: i64, at: Time, bpm: f64) {
        self.tempo.align(now, at, bpm);
        self.cursor = Some(snap(at));
        self.playing = true;
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.cursor = None;
        self.promote(0);
    }

    pub fn seek(&mut self, now: i64, at: Time) {
        let bpm = self.tempo.bpm_at(self.tempo.cycle_at(now));
        self.tempo.align(now, at, bpm);
        if self.playing {
            self.cursor = Some(snap(at));
        }
    }

    /// A new tempo from the next prepared time on.
    pub fn set_bpm(&mut self, now: i64, bpm: f64) {
        let at = self.cursor.unwrap_or_else(|| self.tempo.cycle_at(now));
        self.tempo.set_bpm(at, bpm);
    }

    /// Follow an external clock (a deck's grid): `cycle` is at `sample`.
    /// Re-anchors only when the map has drifted by more than `tolerance`
    /// samples or the tempo changed, so prepared events stay put.
    pub fn follow(&mut self, sample: i64, cycle: f64, bpm: f64, tolerance: i64) -> bool {
        let predicted = self.tempo.cycle_at_f64(sample as f64);
        let spc = self.tempo.rate as f64 * 60.0 * self.tempo.cycle_beats.to_f64() / bpm.max(1.0);
        let err = ((cycle - predicted) * spc).abs();
        let tempo_moved = (self.tempo.bpm_at(self.tempo.cycle_at(sample)) - bpm).abs() > 0.0005;
        if err as i64 <= tolerance && !tempo_moved {
            return false;
        }
        // A jump (the deck was cued, loaded or seeked): play from where the
        // deck is now. Keeping what was prepared would leave a clock behind
        // it silent until it caught up (minutes, on 11-22), or one ahead
        // of it hand out every cycle in between at once.
        if (cycle - predicted).abs() > 1.0 {
            let at = Time::approx(cycle, 1 << 20);
            self.tempo.align(sample, at, bpm);
            self.cursor = None;
            return true;
        }
        // Anchor at the first unprepared time, at the position the external
        // clock gives it, so nothing already handed out moves.
        let at = self.cursor.unwrap_or_else(|| Time::approx(cycle, 1 << 20));
        let at_f = at.to_f64();
        let at_sample = sample + ((at_f - cycle) * spc).round() as i64;
        self.tempo.align(at_sample, at, bpm);
        true
    }

    /// The events due before `now + lookahead` not handed out yet.
    pub fn prepare(&mut self, now: i64) -> Vec<Scheduled> {
        if !self.playing {
            return Vec::new();
        }
        let horizon = self.tempo.cycle_at(now + self.lookahead);
        let start = self.cursor.unwrap_or_else(|| self.tempo.cycle_at(now));
        if horizon <= start {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut b = start;
        while b < horizon {
            // Up to the swap point with the old definition, then the new.
            if let Some(p) = &self.pending
                && p.at <= b
            {
                let swap = self.tempo.sample_at(p.at);
                self.promote(swap);
                continue;
            }
            let e = match &self.pending {
                Some(p) if p.at < horizon => p.at,
                _ => horizon,
            };
            if let Some(slot) = self.active {
                self.query(slot, Span::new(b, e), now, &mut out);
            }
            b = e;
        }
        self.cursor = Some(horizon);
        out
    }

    fn query(&mut self, slot: u32, span: Span, now: i64, out: &mut Vec<Scheduled>) {
        let t0 = Instant::now();
        let r = self.engine.query(slot, span);
        let us = t0.elapsed().as_micros() as u64;
        self.metrics.queries += 1;
        self.metrics.last_query_us = us;
        self.metrics.max_query_us = self.metrics.max_query_us.max(us);
        match r {
            Ok(events) => {
                for event in events {
                    // An engine may return fragments outside the span;
                    // only onsets inside it are this window's.
                    if event.onset() < span.begin || event.onset() >= span.end {
                        continue;
                    }
                    let sample = self.tempo.sample_at(event.onset());
                    let end = self.tempo.sample_at(event.whole.end);
                    let late = sample < now;
                    if late {
                        self.metrics.late_events += 1;
                        self.metrics.max_late_samples =
                            self.metrics.max_late_samples.max(now - sample);
                    }
                    self.metrics.events += 1;
                    out.push(Scheduled {
                        sample,
                        end,
                        event,
                        late,
                    });
                }
            }
            Err(_) => self.metrics.query_errors += 1,
        }
    }

    /// The running definition's events in `span`, without scheduling them
    /// (the surface's inspector, AI comparison).
    pub fn preview(&mut self, span: Span) -> Result<Vec<Event>, PatternError> {
        let slot = self
            .active
            .ok_or_else(|| PatternError::new("nothing loaded"))?;
        self.engine.query(slot, span)
    }

    /// The engine was replaced or restarted: what it held is gone.
    pub fn forget_slots(&mut self) {
        self.active = None;
        self.pending = None;
    }

    pub fn status(&self, now: i64) -> Status {
        let c = self.tempo.cycle_at_f64(now as f64);
        Status {
            playing: self.playing,
            cycle: c,
            bpm: self.tempo.bpm_at(Time::approx(c, 1 << 16)),
            cycle_beats: self.tempo.cycle_beats,
            active: self.active,
            pending: self.pending.as_ref().map(|p| (p.slot, p.at)),
            prepared_to: self.cursor,
            lookahead_ms: self.lookahead as f64 * 1000.0 / self.tempo.rate as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Reference;

    fn src(code: &str) -> Definition {
        Definition::Source {
            language: "patch".into(),
            code: code.into(),
            cycle_beats: Time::whole(4),
        }
    }

    fn sched() -> Scheduler {
        // 48 kHz, 120 BPM: a 4-beat cycle is 96 000 samples.
        Scheduler::new(Box::new(Reference::default()), 48_000, 120.0)
    }

    /// Run `blocks` audio blocks of 256 frames, preparing every 10 ms.
    fn run(s: &mut Scheduler, from: i64, blocks: i64) -> Vec<Scheduled> {
        let mut out = s.prepare(from);
        let mut now = from;
        for _ in 0..blocks {
            now += 256;
            if now % 480 < 256 {
                out.extend(s.prepare(now));
            }
        }
        out
    }

    #[test]
    fn events_land_on_their_exact_samples() {
        let mut s = sched();
        s.load(&src("s(\"bd*4 [hh hh] sd\")"), Quantize::Now, 0)
            .unwrap();
        s.play(0, Time::ZERO, 120.0);
        let ev = run(&mut s, 0, 4 * 96_000 / 256);
        let first: Vec<i64> = ev.iter().take(7).map(|e| e.sample).collect();
        // bd at 0, 1/12, 1/6, 1/4; hh at 1/3, 1/2; sd at 2/3 of 96 000.
        assert_eq!(first, [0, 8_000, 16_000, 24_000, 32_000, 48_000, 64_000]);
        assert!(ev.iter().all(|e| !e.late));
        assert_eq!(
            ev.iter()
                .filter(|e| e.sample >= 96_000 * 3 && e.sample < 96_000 * 4)
                .count(),
            7
        );
        let mut sorted = ev.clone();
        sorted.sort_by_key(|e| e.sample);
        assert_eq!(sorted, ev, "handed out in time order");
    }

    #[test]
    fn a_bad_definition_leaves_the_running_one() {
        let mut s = sched();
        s.load(&src("s(\"bd*4\")"), Quantize::Now, 0).unwrap();
        s.play(0, Time::ZERO, 120.0);
        run(&mut s, 0, 100);
        assert!(
            s.load(&src("s(\"bd [\")"), Quantize::Cycle, 25_600)
                .is_err()
        );
        let ev = run(&mut s, 25_600, 2 * 96_000 / 256);
        assert!(ev.iter().all(|e| e.event.value.to_string() == "bd"));
        assert!(!ev.is_empty());
        assert_eq!(s.metrics.eval_errors, 1);
    }

    #[test]
    fn swaps_happen_on_the_boundary() {
        let mut s = sched();
        s.load(&src("s(\"bd*4\")"), Quantize::Now, 0).unwrap();
        s.play(0, Time::ZERO, 120.0);
        let mut ev = run(&mut s, 0, 100);
        let (_, at) = s
            .load(&src("s(\"sd*4\")"), Quantize::Cycle, 25_600)
            .unwrap();
        assert_eq!(at, Time::ONE);
        ev.extend(run(&mut s, 25_600, 2 * 96_000 / 256));
        let switch = ev
            .iter()
            .position(|e| e.event.value.to_string() == "sd")
            .unwrap();
        assert_eq!(ev[switch].sample, 96_000, "exactly on the bar");
        assert!(
            ev[..switch]
                .iter()
                .all(|e| e.event.value.to_string() == "bd")
        );
        // A phrase boundary.
        let (_, at) = s
            .load(&src("s(\"cp\")"), Quantize::Cycles(4), 200_000)
            .unwrap();
        assert_eq!(at, Time::whole(4));
        // Beat.
        let (_, at) = s.load(&src("s(\"cp\")"), Quantize::Beat, 200_000).unwrap();
        assert!(at > Time::whole(2) && (at * Time::whole(4)).den() == 1);
    }

    #[test]
    fn tempo_changes_and_following_a_deck() {
        let mut s = sched();
        s.load(&src("s(\"bd\")"), Quantize::Now, 0).unwrap();
        s.play(0, Time::ZERO, 120.0);
        let mut ev = run(&mut s, 0, 400);
        s.set_bpm(102_400, 60.0);
        ev.extend(run(&mut s, 102_400, 2_000));
        let at: Vec<i64> = ev.iter().map(|e| e.sample).collect();
        assert_eq!(&at[..2], [0, 96_000], "{at:?}");
        // From where it changed, a cycle is twice as long.
        assert_eq!(at[3] - at[2], 192_000, "{at:?}");
        assert!(at[2] - at[1] > 96_000 && at[2] - at[1] < 192_000);
        // A deck says cycle 10 is at sample 1 000 000 (it was nudged).
        assert!(s.follow(1_000_000, 10.0, 60.0, 48));
        assert!(!s.follow(1_000_000, 10.0, 60.0, 48), "in tolerance now");
    }

    /// A deck far behind (or ahead of) the clock: Tidal plays on from where
    /// the deck is, at once, never silent until it catches up.
    #[test]
    fn following_a_deck_that_jumped() {
        for deck in [-5.0, 400.0] {
            let mut s = sched();
            s.load(&src("s(\"bd*4\")"), Quantize::Now, 0).unwrap();
            s.play(0, Time::ZERO, 120.0);
            // A hundred cycles in.
            let now = 100 * 96_000;
            run(&mut s, now - 2_560, 10);
            assert!(s.follow(now, deck, 120.0, 48));
            assert!((s.status(now).cycle - deck).abs() < 0.01, "{deck}: {}", s.status(now).cycle);
            let ev = run(&mut s, now, 400);
            assert!(!ev.is_empty(), "{deck}: silent");
            assert!(ev.len() < 16, "{deck}: a burst of {}", ev.len());
            assert!(ev.iter().all(|e| e.sample >= now - 48 && e.sample < now + 120_000), "{deck}");
        }
    }

    #[test]
    fn determinism_across_runs_and_window_sizes() {
        let code = "s(\"hh*16?\").sometimes(fast(2))\ns(\"bd(5,8)\").every(3, rev)";
        let mut a = sched();
        a.load(&src(code), Quantize::Now, 0).unwrap();
        a.play(0, Time::ZERO, 120.0);
        let ea = run(&mut a, 0, 3_000);
        let mut b = sched();
        b.lookahead = 48_000 / 3;
        b.load(&src(code), Quantize::Now, 0).unwrap();
        b.play(0, Time::ZERO, 120.0);
        let eb = run(&mut b, 0, 3_000);
        let n = ea.len().min(eb.len()) - 50;
        assert_eq!(ea[..n], eb[..n]);
    }
}
