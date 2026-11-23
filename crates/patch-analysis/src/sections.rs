//! The track map: a track's parts (intro, main, breakdown, build, drop,
//! outro) on 4-bar phrase lines, each scored for beat-mixing, as a DJ reads
//! a waveform: mix on the clear, steady stretches; a beat comes in only where
//! its kick is clear.
//!
//! Derived from what every analysis already holds (the kick grid and its
//! beatless stretches, coarse section boundaries, energy per second, vocals,
//! verified regions), so it costs no extra decoding and older analyses get
//! a map too. Labels are a reading of energy and kick, not of the music's
//! form: "main" is any kicked stretch that is neither intro, drop nor outro.

use crate::Analysis;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Intro,
    Main,
    Breakdown,
    Build,
    Drop,
    Outro,
}

impl Kind {
    /// The word the decks print (small caps in the UI).
    pub fn word(self) -> &'static str {
        match self {
            Kind::Intro => "intro",
            Kind::Main => "main",
            Kind::Breakdown => "break",
            Kind::Build => "build",
            Kind::Drop => "drop",
            Kind::Outro => "outro",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Part {
    pub start_s: f64,
    pub end_s: f64,
    /// Whole bars (4/4), counted on the grid.
    pub bars: u32,
    pub kind: Kind,
    /// Share of the part's beats with the kick playing, 0…1.
    pub kick: f32,
    /// Loudness against the track's body (its middle 60 %), 1 = as loud.
    pub energy: f32,
    /// Share of the part that is sung, 0…1 (0 when vocals aren't measured).
    pub vocals: f32,
    /// How well a beat can be mixed over this part, 0…1: a steady kick for
    /// long enough, a grid that holds, little voice. 0 without a kick.
    pub mix: f32,
    /// Rank among the track's parts by `mix` (1 = best), for the best few
    /// that are worth it; 0 otherwise.
    pub rank: u8,
}

/// How many parts get a rank.
pub const RANKED: usize = 3;
/// A part needs at least this `mix` to be ranked.
const RANK_FLOOR: f32 = 0.35;
/// A beat counts as kicked when less than half of it falls in a beatless
/// stretch.
const KICKED: f32 = 0.5;

/// The track's parts in order, covering it from its first downbeat's phrase
/// to its end. Empty without a tempo.
pub fn map(analysis: &Analysis) -> Vec<Part> {
    let mut a = analysis.clone();
    a.use_grid();
    let (Some(bpm), true) = (a.bpm, a.duration > 0.0) else {
        return vec![];
    };
    if !(40.0..=300.0).contains(&bpm) {
        return vec![];
    }
    let beat = 60.0 / bpm;
    let bar = 4.0 * beat;
    let downbeat = a.beat_offset + a.downbeat.unwrap_or(0) as f64 * beat;
    let phrase = 4.0 * bar;
    let beatless: Vec<(f64, f64)> = a
        .grid
        .as_ref()
        .map(|g| g.beatless.iter().map(|[s, e]| (*s as f64, *e as f64)).collect())
        .unwrap_or_default();
    // Phrases needn't start on the first downbeat: take the bar of the four
    // on which most kick entries and exits land (within half a bar).
    let first = (0..4)
        .map(|k| downbeat + k as f64 * bar)
        .max_by_key(|origin| {
            beatless
                .iter()
                .flat_map(|(s, e)| [*s, *e])
                .filter(|t| {
                    let off = (t - origin).rem_euclid(phrase);
                    off.min(phrase - off) <= bar * 0.5
                })
                .count()
        })
        .unwrap_or(downbeat);
    // Phrase lines every 4 bars from there; a cut lands on the nearest one
    // within a bar, else on its own bar line.
    let snap = |t: f64| -> f64 {
        let p = first + ((t - first) / phrase).round() * phrase;
        if (p - t).abs() <= bar {
            p
        } else {
            first + ((t - first) / bar).round() * bar
        }
    };
    let mut cuts: Vec<f64> = vec![0.0, a.duration];
    for (s, e) in &beatless {
        cuts.push(snap(*s));
        cuts.push(snap(*e));
    }
    // Older analyses have no section evidence: read it from the energy.
    let found;
    let structure = match &a.structure {
        Some(st) => st,
        None => {
            found = crate::structure::analyze(&a.energy, &[], &[], a.duration);
            &found
        }
    };
    cuts.extend(structure.boundaries.iter().map(|b| snap(b.time_s)));
    cuts.retain(|t| t.is_finite() && *t >= 0.0 && *t <= a.duration);
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|x, y| (*x - *y).abs() < bar * 0.5);

    let kick_share = |s: f64, e: f64| -> f32 {
        let len = (e - s).max(1e-6);
        let out: f64 = beatless
            .iter()
            .map(|(bs, be)| (be.min(e) - bs.max(s)).max(0.0))
            .sum();
        (1.0 - out / len).clamp(0.0, 1.0) as f32
    };
    let mut parts: Vec<Part> = cuts
        .windows(2)
        .filter(|w| w[1] - w[0] > 1e-3)
        .map(|w| Part {
            start_s: w[0],
            end_s: w[1],
            bars: ((w[1] - w[0]) / bar).round().max(0.0) as u32,
            kind: Kind::Main,
            kick: kick_share(w[0], w[1]),
            energy: 0.0,
            vocals: 0.0,
            mix: 0.0,
            rank: 0,
        })
        .collect();
    // Parts under 4 bars join their neighbour of the same beat (kicked or
    // not), else the previous one: no 1-bar slivers on the map.
    let mut i = 0;
    while i < parts.len() {
        if parts[i].bars >= 4 || parts.len() == 1 {
            i += 1;
            continue;
        }
        let kicked = parts[i].kick >= KICKED;
        let into = if i + 1 < parts.len() && (i == 0 || (parts[i + 1].kick >= KICKED) == kicked) {
            i + 1
        } else {
            i - 1
        };
        let (lo, hi) = (i.min(into), i.max(into));
        let merged_end = parts[hi].end_s;
        parts[lo].end_s = merged_end;
        parts.remove(hi);
        let p = &mut parts[lo];
        p.bars = ((p.end_s - p.start_s) / bar).round() as u32;
        p.kick = kick_share(p.start_s, p.end_s);
        i = lo;
    }
    // Neighbours alike in beat and loudness are one part.
    let body = body_energy(&a.energy);
    for p in parts.iter_mut() {
        p.energy = mean_energy(&a.energy, p.start_s, p.end_s) / body;
    }
    let mut j = 1;
    while j < parts.len() {
        let (x, y) = (&parts[j - 1], &parts[j]);
        let same_beat = (x.kick >= KICKED) == (y.kick >= KICKED);
        let close = (x.energy - y.energy).abs() < 0.12;
        if same_beat && close {
            let end = y.end_s;
            parts.remove(j);
            let p = &mut parts[j - 1];
            p.end_s = end;
            p.bars = ((p.end_s - p.start_s) / bar).round() as u32;
            p.kick = kick_share(p.start_s, p.end_s);
            p.energy = mean_energy(&a.energy, p.start_s, p.end_s) / body;
        } else {
            j += 1;
        }
    }
    label(&mut parts, &a.energy);
    // Neighbours of one kind are one part (a breakdown read in two halves).
    let mut j = 1;
    while j < parts.len() {
        if parts[j].kind == parts[j - 1].kind {
            let end = parts.remove(j).end_s;
            let p = &mut parts[j - 1];
            p.end_s = end;
            p.bars = ((p.end_s - p.start_s) / bar).round() as u32;
            p.kick = kick_share(p.start_s, p.end_s);
            p.energy = mean_energy(&a.energy, p.start_s, p.end_s) / body;
        } else {
            j += 1;
        }
    }
    for p in parts.iter_mut() {
        p.vocals = a
            .vocals
            .as_ref()
            .map(|v| {
                let (s, e) = (p.start_s.max(0.0) as usize, p.end_s.max(0.0) as usize);
                let n = e.saturating_sub(s).max(1);
                (s..e).filter(|t| v.sung_at(*t as f64)).count() as f32 / n as f32
            })
            .unwrap_or(0.0);
        p.mix = mix_score(&a, p);
    }
    rank(&mut parts);
    parts
}

fn label(parts: &mut [Part], energy: &[u8]) {
    let n = parts.len();
    let kicked = |p: &Part| p.kick >= KICKED;
    let first_kick = parts.iter().position(kicked);
    let last_kick = parts.iter().rposition(kicked);
    // The track's loud stretch: the first kicked part at body level.
    let first_full = parts
        .iter()
        .position(|p| kicked(p) && p.energy >= 0.85)
        .or(first_kick);
    for i in 0..n {
        let p = &parts[i];
        parts[i].kind = if !kicked(p) {
            match (first_kick, last_kick) {
                (Some(f), _) if i < f => Kind::Intro,
                (_, Some(l)) if i > l => Kind::Outro,
                (None, _) if i == 0 => Kind::Intro,
                _ => {
                    // The end of a breakdown that climbs into a drop.
                    let next_kicked = parts.get(i + 1).is_some_and(kicked);
                    let (s, e) = (p.start_s, p.end_s);
                    let mid = (s + e) / 2.0;
                    let within = mean_energy(energy, mid, e) / mean_energy(energy, s, mid).max(1.0);
                    // Or louder than the breakdown part before it.
                    let stepped = if i > 0 && !kicked(&parts[i - 1]) {
                        p.energy / parts[i - 1].energy.max(0.01)
                    } else {
                        0.0
                    };
                    if next_kicked && within.max(stepped) >= 1.35 && i > 0 && !kicked(&parts[i - 1]) {
                        Kind::Build
                    } else {
                        Kind::Breakdown
                    }
                }
            }
        } else if first_full.is_some_and(|f| i < f) {
            Kind::Intro
        } else if i > 0 && !kicked(&parts[i - 1]) && first_kick != Some(i) {
            Kind::Drop
        } else if last_kick == Some(i) && i > 0 && p.energy < 0.85 {
            // The last kicked part, quieter: only beatless parts follow.
            Kind::Outro
        } else {
            Kind::Main
        };
    }
    // A kicked part after the last loud one, quieter, is the outro's start.
    if let Some(last_loud) = parts.iter().rposition(|p| kicked(p) && p.energy >= 0.85) {
        for p in parts.iter_mut().skip(last_loud + 1) {
            if kicked(p) && p.energy < 0.85 {
                p.kind = Kind::Outro;
            }
        }
    }
}

/// How well a beat mixes over this part. A DJ wants a steady kick for at
/// least 8 bars, a grid that holds there, and no voice to clash with; an
/// intro or outro is where mixing is expected, a drop is where it hurts.
fn mix_score(a: &Analysis, p: &Part) -> f32 {
    if p.kick < KICKED {
        return 0.0;
    }
    let length = (p.bars as f32 / 16.0).min(1.0);
    let steady = a.grid.as_ref().map_or(0.6, |g| {
        // ±1 ms scatter is tight, 6 ms loose; drift over 20 ms wanders.
        let jitter = (1.0 - (g.jitter_ms - 1.0).max(0.0) / 5.0).clamp(0.2, 1.0);
        let drift = (1.0 - (g.drift_ms - 6.0).max(0.0) / 20.0).clamp(0.3, 1.0);
        jitter * drift
    });
    // Measured locally when a deep region covers the part.
    let local = a
        .deep_regions
        .iter()
        .filter(|r| r.start_s <= p.start_s + 1.0 && r.end_s >= p.end_s - 1.0)
        .find_map(|r| r.rhythm_check.as_ref())
        .and_then(|c| c.offset_jitter_ms)
        .map_or(1.0, |j| (1.0 - (j - 4.0).max(0.0) / 14.0).clamp(0.2, 1.0));
    let voice = 1.0 - 0.6 * p.vocals;
    let role = match p.kind {
        Kind::Intro | Kind::Outro => 1.0,
        Kind::Main => 0.8,
        Kind::Drop => 0.55,
        Kind::Breakdown | Kind::Build => 0.0,
    };
    (p.kick * length * steady * local * voice * role).clamp(0.0, 1.0)
}

fn rank(parts: &mut [Part]) {
    let mut order: Vec<usize> = (0..parts.len())
        .filter(|&i| parts[i].mix >= RANK_FLOOR)
        .collect();
    order.sort_by(|&x, &y| parts[y].mix.total_cmp(&parts[x].mix));
    for (r, &i) in order.iter().take(RANKED).enumerate() {
        parts[i].rank = r as u8 + 1;
    }
}

fn body_energy(e: &[u8]) -> f32 {
    if e.len() < 10 {
        return e.iter().map(|v| *v as f32).sum::<f32>().max(1.0) / e.len().max(1) as f32;
    }
    let (lo, hi) = (e.len() / 5, e.len() * 4 / 5);
    (e[lo..hi].iter().map(|v| *v as f32).sum::<f32>() / (hi - lo) as f32).max(1.0)
}

fn mean_energy(e: &[u8], s: f64, t: f64) -> f32 {
    let lo = (s.max(0.0) as usize).min(e.len());
    let hi = (t.max(s).ceil() as usize).min(e.len());
    if hi <= lo {
        return 0.0;
    }
    e[lo..hi].iter().map(|v| *v as f32).sum::<f32>() / (hi - lo) as f32
}

/// The moments a DJ previews from, by the keys that jump there in the
/// Library: 1 the start (the first downbeat's phrase), 2 the first drop,
/// 3 the main (longest) break, 4 the second drop, 5 the outro. None when the
/// track has no such part.
pub const CUE_WORDS: [&str; 5] = ["start", "drop 1", "break", "drop 2", "outro"];

pub fn cues(parts: &[Part]) -> [Option<f64>; 5] {
    let start = parts.first().map(|p| p.start_s.max(0.0));
    let mut drops = parts.iter().filter(|p| p.kind == Kind::Drop);
    let drop1 = drops.next().map(|p| p.start_s);
    let drop2 = drops.next().map(|p| p.start_s);
    // The longest break; the first of equal ones.
    let brk = parts
        .iter()
        .filter(|p| p.kind == Kind::Breakdown)
        .fold(None::<&Part>, |best, p| match best {
            Some(b) if b.bars >= p.bars => Some(b),
            _ => Some(p),
        })
        .map(|p| p.start_s);
    let outro = parts.iter().rev().find(|p| p.kind == Kind::Outro).map(|p| p.start_s);
    [start, drop1, brk, drop2, outro]
}

#[cfg(test)]
mod tests {
    fn part_of(kind: Kind, start_s: f64, bars: u32) -> Part {
        Part { start_s, end_s: start_s + bars as f64 * 2.0, bars, kind, kick: 1.0, energy: 1.0, vocals: 0.0, mix: 0.0, rank: 0 }
    }

    #[test]
    fn cues_are_start_drops_longest_break_and_outro() {
        let parts = [
            part_of(Kind::Intro, 0.5, 16),
            part_of(Kind::Build, 32.5, 8),
            part_of(Kind::Drop, 48.5, 16),
            part_of(Kind::Breakdown, 80.5, 8),
            part_of(Kind::Main, 96.5, 8),
            part_of(Kind::Breakdown, 112.5, 16),
            part_of(Kind::Drop, 144.5, 16),
            part_of(Kind::Outro, 176.5, 16),
        ];
        assert_eq!(cues(&parts), [Some(0.5), Some(48.5), Some(112.5), Some(144.5), Some(176.5)]);
        // Missing parts are None, not a guess.
        assert_eq!(cues(&parts[..3]), [Some(0.5), Some(48.5), None, None, None]);
        assert_eq!(cues(&[]), [None; 5]);
    }

    use super::*;
    use crate::grid::GridQuality;

    /// A 128 BPM track (1.875 s bars) laid out in bars: (bars, energy,
    /// kicked).
    fn track(layout: &[(u32, u8, bool)]) -> Analysis {
        let bar = 1.875;
        let mut energy = vec![];
        let mut beatless = vec![];
        let mut t = 0.0;
        for &(bars, e, kicked) in layout {
            let len = bars as f64 * bar;
            if !kicked {
                beatless.push([t as f32, (t + len) as f32]);
            }
            t += len;
            while (energy.len() as f64) < t {
                energy.push(e);
            }
        }
        let mut a = crate::analyse(&[]);
        a.duration = t;
        a.bpm = Some(128.0);
        a.beat_offset = 0.0;
        a.downbeat = Some(0);
        a.energy = energy;
        a.structure = None;
        a.vocals = None;
        a.grid = Some(GridQuality {
            method: crate::grid::METHOD,
            precise: false,
            bpm: 128.0,
            offset: 0.0,
            downbeat: Some(0),
            downbeat_confidence: 0.9,
            jitter_ms: 1.0,
            drift_ms: 3.0,
            coverage: 0.8,
            beatless,
            mixability: 0.9,
            phase_confidence: 0.9,
            click_offset_ms: None,
        });
        a
    }

    fn kinds(parts: &[Part]) -> Vec<(Kind, u32)> {
        parts.iter().map(|p| (p.kind, p.bars)).collect()
    }

    #[test]
    fn a_club_track_reads_intro_main_break_drop_outro() {
        let a = track(&[
            (16, 120, true),  // DJ intro: kick, little else
            (32, 200, true),  // main
            (16, 70, false),  // breakdown
            (32, 210, true),  // drop
            (16, 110, true),  // outro: kick, thinning out
        ]);
        let parts = map(&a);
        assert_eq!(
            kinds(&parts),
            vec![
                (Kind::Intro, 16),
                (Kind::Main, 32),
                (Kind::Breakdown, 16),
                (Kind::Drop, 32),
                (Kind::Outro, 16),
            ]
        );
    }

    #[test]
    fn the_intro_and_outro_rank_above_the_drop_and_a_breakdown_never_ranks() {
        let a = track(&[
            (16, 120, true),
            (32, 200, true),
            (16, 70, false),
            (32, 210, true),
            (16, 110, true),
        ]);
        let parts = map(&a);
        let by = |k: Kind| parts.iter().find(|p| p.kind == k).unwrap();
        assert_eq!(by(Kind::Breakdown).mix, 0.0);
        assert_eq!(by(Kind::Breakdown).rank, 0);
        assert!(by(Kind::Intro).rank == 1 || by(Kind::Outro).rank == 1);
        assert!(by(Kind::Intro).mix > by(Kind::Drop).mix);
    }

    #[test]
    fn a_beatless_intro_is_not_a_mixing_spot() {
        let a = track(&[(16, 60, false), (48, 200, true), (16, 70, false), (32, 210, true)]);
        let parts = map(&a);
        assert_eq!(parts[0].kind, Kind::Intro);
        assert_eq!(parts[0].mix, 0.0);
        assert_eq!(parts[0].rank, 0);
    }

    #[test]
    fn a_breakdown_that_climbs_into_the_drop_ends_in_a_build() {
        let a = track(&[
            (32, 200, true),
            (16, 60, false),
            (8, 140, false),
            (32, 210, true),
        ]);
        let parts = map(&a);
        assert_eq!(
            kinds(&parts).iter().map(|k| k.0).collect::<Vec<_>>(),
            vec![Kind::Main, Kind::Breakdown, Kind::Build, Kind::Drop]
        );
    }

    #[test]
    fn parts_lie_on_phrase_lines_and_cover_the_track() {
        let a = track(&[(16, 120, true), (32, 200, true), (16, 70, false), (32, 210, true)]);
        let parts = map(&a);
        assert_eq!(parts.first().unwrap().start_s, 0.0);
        assert!((parts.last().unwrap().end_s - a.duration).abs() < 1e-9);
        for w in parts.windows(2) {
            assert_eq!(w[0].end_s, w[1].start_s);
            assert_eq!(w[0].bars % 4, 0, "{:?}", kinds(&parts));
        }
    }

    #[test]
    fn no_tempo_no_map() {
        let mut a = track(&[(16, 120, true)]);
        a.bpm = None;
        a.grid = None;
        assert!(map(&a).is_empty());
    }
}
