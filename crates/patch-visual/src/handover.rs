//! A transition's choreography. While the AI DJ mixes, the stage keeps
//! playing whole scenes, alternating the two tracks' own designs (each
//! in its own palette and beat feel) in a pattern of its own:
//!
//! * **ping-pong**: one, then the other, in equal passages;
//! * **call and answer**: the outgoing calls, the incoming answers twice
//!   as quickly, then the turns swap;
//! * **converge**: the outgoing's passages shorten as the incoming's grow;
//! * **mashup**: between the plain turns, one track's backdrop wears the
//!   other's layer.
//!
//! Every pattern starts on the outgoing track and ends on the incoming
//! one, whose last passage (from its lift scenes) runs into the drop.
//! Passages are long enough to watch: four bars at least on a mix of eight
//! bars or more, eight on a mix of sixteen; no quick one-bar turns. The plan is whole bars, worked out from both titles and the
//! transition's length: the same mix, the same show.

/// Whose design a passage plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Out,
    In,
    /// The outgoing track's backdrop under the incoming's layer.
    OutWithIn,
    /// The incoming track's backdrop under the outgoing's layer.
    InWithOut,
}

impl Side {
    /// Whose backdrop and palette lead.
    pub fn incoming(self) -> bool {
        matches!(self, Side::In | Side::InWithOut)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    PingPong,
    CallAnswer,
    Converge,
    Mashup,
}

impl Pattern {
    pub const ALL: [Pattern; 4] = [
        Pattern::PingPong,
        Pattern::CallAnswer,
        Pattern::Converge,
        Pattern::Mashup,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Pattern::PingPong => "ping-pong",
            Pattern::CallAnswer => "call and answer",
            Pattern::Converge => "converge",
            Pattern::Mashup => "mashup",
        }
    }
}

/// One passage, in whole bars from the transition's start. `turn` counts
/// the earlier passages led by the same track: each shows its next scene.
/// `finale`: the closing quick turns, from the tracks' lift scenes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Passage {
    pub start: u32,
    pub bars: u32,
    pub side: Side,
    pub turn: usize,
    pub finale: bool,
}

pub fn pattern(seed: u64) -> Pattern {
    Pattern::ALL[((seed ^ (seed >> 29)) % 4) as usize]
}

/// The choreography for a transition of `beats`, seeded by both titles.
pub fn plan(beats: f64, seed: u64, slow: bool) -> Vec<Passage> {
    let bars = ((beats / 4.0).round() as u32).max(1);
    let pattern = pattern(seed);
    let _ = slow;
    let body = bars;
    let unit = match bars {
        0..8 => (bars / 2).max(1),
        8..16 => 4,
        16..64 => 8,
        _ => 16,
    };
    // The short turns never go under four bars (a short mix has none).
    let half = if bars < 8 { unit } else { (unit / 2).max(4) };
    let mut sides: Vec<(Side, u32)> = vec![];
    let mut at = 0;
    let mut n = 0usize;
    while at < body {
        let early = (at as f32) < body as f32 * 0.5;
        let (side, len) = match pattern {
            Pattern::PingPong => (if n.is_multiple_of(2) { Side::Out } else { Side::In }, unit),
            // Out, In, In; past halfway, In, Out, Out.
            Pattern::CallAnswer => match (n % 3, early) {
                (0, true) => (Side::Out, unit),
                (_, true) => (Side::In, half),
                (0, false) => (Side::In, unit),
                (_, false) => (Side::Out, half),
            },
            Pattern::Converge => match (n % 2, early) {
                (0, true) => (Side::Out, unit),
                (0, false) => (Side::Out, half),
                (_, true) => (Side::In, half),
                (_, false) => (Side::In, unit),
            },
            Pattern::Mashup => (
                [Side::Out, Side::OutWithIn, Side::In, Side::InWithOut][n % 4],
                unit,
            ),
        };
        // The first passage leaves room for the other track.
        let room = if at == 0 {
            (body / 2).max(1)
        } else {
            body - at
        };
        let len = len.min(room).max(1);
        sides.push((side, len));
        at += len;
        n += 1;
    }
    // Start on the outgoing track, end the body on the incoming one.
    if let Some(first) = sides.first_mut() {
        first.0 = Side::Out;
    }
    if sides.len() > 1
        && let Some(last) = sides.last_mut()
        && !last.0.incoming()
    {
        last.0 = Side::In;
    }
    // The same track twice in a row is one longer passage, not a cut.
    sides.dedup_by(|b, a| {
        if a.0 == b.0 {
            a.1 += b.1;
            true
        } else {
            false
        }
    });
    // The last passage, the incoming track's, runs into the drop from its
    // lift scenes.
    let body_passages = if bars >= 8 { sides.len() - 1 } else { sides.len() };
    let (mut at, mut outs, mut ins) = (0, 0, 0);
    sides
        .into_iter()
        .enumerate()
        .map(|(i, (side, bars))| {
            let turn = if side.incoming() { &mut ins } else { &mut outs };
            let p = Passage {
                start: at,
                bars,
                side,
                turn: *turn,
                finale: i >= body_passages,
            };
            *turn += 1;
            at += bars;
            p
        })
        .collect()
}

/// The passage playing `beats_in` beats into the transition.
pub fn step(plan: &[Passage], beats_in: f64) -> usize {
    plan.iter()
        .rposition(|s| s.start as f64 * 4.0 <= beats_in + 1e-6)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_cover_the_transition_and_alternate_both_tracks() {
        let mut patterns = std::collections::HashSet::new();
        for bars in [1u32, 2, 4, 8, 12, 16, 24, 32, 48, 64] {
            for seed in 0..32u64 {
                for slow in [false, true] {
                    let p = plan(bars as f64 * 4.0, seed, slow);
                    patterns.insert(pattern(seed));
                    let mut at = 0;
                    for s in &p {
                        assert_eq!(s.start, at, "{bars} bars: {p:?}");
                        assert!(s.bars > 0);
                        at += s.bars;
                    }
                    assert_eq!(at, bars, "{p:?}");
                    assert_eq!(p[0].side, Side::Out);
                    if bars >= 2 {
                        assert!(p.last().unwrap().side.incoming(), "{p:?}");
                    }
                    if bars >= 64 {
                        // On a long mix both tracks keep coming back.
                        let ins = p.iter().filter(|s| s.side.incoming()).count();
                        let outs = p.len() - ins;
                        assert!(ins >= 2 && outs >= 2, "{p:?}");
                    }
                    if bars >= 8 {
                        assert!(p.last().unwrap().finale);
                        // Long enough to watch: no quick turns.
                        assert!(p.iter().all(|s| s.bars >= 4), "{p:?}");
                    }
                    if bars >= 16 {
                        assert!(p.iter().map(|s| s.bars).sum::<u32>() / p.len() as u32 >= 6, "{p:?}");
                    }
                }
            }
        }
        assert_eq!(patterns.len(), 4);
        let p = plan(32.0 * 4.0, 3, false);
        assert_eq!(step(&p, 0.0), 0);
        assert_eq!(step(&p, 32.0 * 4.0 - 1.0), p.len() - 1);
    }

    #[test]
    fn turns_count_each_tracks_own_scenes() {
        for seed in 0..8 {
            let p = plan(32.0 * 4.0, seed, false);
            for side in [false, true] {
                let turns: Vec<usize> = p
                    .iter()
                    .filter(|s| s.side.incoming() == side)
                    .map(|s| s.turn)
                    .collect();
                assert_eq!(turns, (0..turns.len()).collect::<Vec<_>>());
            }
        }
    }
}
