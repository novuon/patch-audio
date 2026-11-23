//! `Pattern<T>`: a function from a span of cycles to the events in it.
//!
//! The algebra is generic: `fast`, `every`, `euclid` work the same on drum
//! names, deck filter amounts or visual parameters. A pattern holds no
//! state; querying the same span with the same seed always gives the same
//! events, on any machine. Randomness is a hash of the seed, the event's
//! onset and a per-node salt.
//!
//! Semantics follow TidalCycles' where they are useful (cycles, `whole`
//! and `part`, `<…>` alternating per cycle, `sometimes` keyed on onsets);
//! the implementation is Patch's own.

use crate::time::{Span, Time};
use std::sync::Arc;

/// One event, or a fragment of one: `whole` is the event's full span
/// (`None` for a continuous signal), `part` the piece inside the query.
#[derive(Clone, Debug, PartialEq)]
pub struct Hap<T> {
    pub whole: Option<Span>,
    pub part: Span,
    pub value: T,
}

impl<T> Hap<T> {
    /// This fragment holds the event's start: the piece a scheduler sends.
    pub fn has_onset(&self) -> bool {
        self.whole.is_some_and(|w| w.begin == self.part.begin)
    }

    /// The event's start (the fragment's, for a signal).
    pub fn onset(&self) -> Time {
        self.whole.map_or(self.part.begin, |w| w.begin)
    }

    fn with_span(self, f: &impl Fn(Time) -> Time) -> Hap<T> {
        Hap {
            whole: self.whole.map(|w| w.with_time(f)),
            part: self.part.with_time(f),
            value: self.value,
        }
    }

    fn map<U>(self, f: impl FnOnce(T) -> U) -> Hap<U> {
        Hap {
            whole: self.whole,
            part: self.part,
            value: f(self.value),
        }
    }
}

/// What a pattern is asked: a span, and the seed of its randomness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Query {
    pub span: Span,
    pub seed: u64,
}

impl Query {
    fn with_span(self, span: Span) -> Query {
        Query {
            span,
            seed: self.seed,
        }
    }
}

pub trait Value: Clone + Send + Sync + 'static {}
impl<T: Clone + Send + Sync + 'static> Value for T {}

type QueryFn<T> = dyn Fn(Query) -> Vec<Hap<T>> + Send + Sync;

pub struct Pattern<T> {
    query: Arc<QueryFn<T>>,
}

impl<T> Clone for Pattern<T> {
    fn clone(&self) -> Self {
        Pattern {
            query: self.query.clone(),
        }
    }
}

/// 0 ≤ x < 1 from a seed, a time and a salt (splitmix64).
pub fn rand_at(seed: u64, t: Time, salt: u64) -> f64 {
    let mut z = seed
        ^ (t.num() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (t.den() as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
        ^ salt.wrapping_mul(0x1656_67b1_9e37_79f9);
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Euclid's even spread of `k` onsets over `n` steps (Bjorklund's
/// algorithm, as in Toussaint's "The Euclidean Algorithm Generates
/// Traditional Musical Rhythms"): (3, 8) → x . . x . . x . and
/// (5, 8) → x . x x . x x . ; then rotated *later* by `r` steps (Strudel's
/// direction; TidalCycles rotates the other way, see the Pattern findings).
pub fn bjorklund(k: u32, n: u32, r: i32) -> Vec<bool> {
    let n = n.max(1) as usize;
    let k = (k as usize).min(n);
    let mut v = if k == 0 {
        vec![false; n]
    } else {
        // Pair the onsets' groups with the rests' until one rest group (or
        // none) is left over.
        let mut a: Vec<Vec<bool>> = vec![vec![true]; k];
        let mut b: Vec<Vec<bool>> = vec![vec![false]; n - k];
        while b.len() > 1 {
            let m = a.len().min(b.len());
            let rest: Vec<Vec<bool>> = if a.len() > m {
                a[m..].to_vec()
            } else {
                b[m..].to_vec()
            };
            a = (0..m)
                .map(|i| [a[i].clone(), b[i].clone()].concat())
                .collect();
            b = rest;
        }
        a.into_iter().chain(b).flatten().collect()
    };
    let r = (r as i64).rem_euclid(n as i64) as usize;
    v.rotate_right(r);
    v
}

impl<T: Value> Pattern<T> {
    pub fn new(f: impl Fn(Query) -> Vec<Hap<T>> + Send + Sync + 'static) -> Pattern<T> {
        Pattern { query: Arc::new(f) }
    }

    pub fn query(&self, q: Query) -> Vec<Hap<T>> {
        (self.query)(q)
    }

    pub fn query_span(&self, span: Span, seed: u64) -> Vec<Hap<T>> {
        self.query(Query { span, seed })
    }

    /// The events that start in `span`, in time order: what plays.
    pub fn onsets(&self, span: Span, seed: u64) -> Vec<Hap<T>> {
        let mut v: Vec<Hap<T>> = self
            .query_span(span, seed)
            .into_iter()
            .filter(|h| h.has_onset())
            .collect();
        v.sort_by_key(|a| a.part.begin);
        v
    }

    // ── Constructors ─────────────────────────────────────────────────────

    pub fn silence() -> Pattern<T> {
        Pattern::new(|_| Vec::new())
    }

    /// One event per cycle, filling it.
    pub fn pure(v: T) -> Pattern<T> {
        Pattern::new(move |q| {
            q.span
                .split_cycles()
                .into_iter()
                .map(|part| {
                    let c = part.begin.sam();
                    Hap {
                        whole: Some(Span::new(c, c + Time::ONE)),
                        part,
                        value: v.clone(),
                    }
                })
                .collect()
        })
    }

    /// A continuous signal: its value at the start of each queried span.
    pub fn signal(f: impl Fn(Time, u64) -> T + Send + Sync + 'static) -> Pattern<T> {
        Pattern::new(move |q| {
            vec![Hap {
                whole: None,
                part: q.span,
                value: f(q.span.begin, q.seed),
            }]
        })
    }

    /// One pattern per cycle in turn, each counting its own cycles
    /// (`<a b c>`): a pattern inside comes round every n cycles.
    pub fn slowcat(pats: Vec<Pattern<T>>) -> Pattern<T> {
        if pats.is_empty() {
            return Pattern::silence();
        }
        let n = pats.len() as i64;
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let cyc = span.begin.cycle();
                let i = cyc.rem_euclid(n) as usize;
                let shift = Time::whole(cyc - cyc.div_euclid(n));
                let inner = span.with_time(|t| t - shift);
                out.extend(
                    pats[i]
                        .query(q.with_span(inner))
                        .into_iter()
                        .map(|h| h.with_span(&|t| t + shift)),
                );
            }
            out
        })
    }

    /// The patterns squeezed one after another into each cycle.
    pub fn fastcat(pats: Vec<Pattern<T>>) -> Pattern<T> {
        Pattern::timecat(pats.into_iter().map(|p| (Time::ONE, p)).collect())
    }

    /// Weighted: each pattern takes its share of the cycle (`a@3 b`).
    pub fn timecat(parts: Vec<(Time, Pattern<T>)>) -> Pattern<T> {
        let total = parts.iter().fold(Time::ZERO, |a, (w, _)| a + *w);
        if total <= Time::ZERO {
            return Pattern::silence();
        }
        let mut acc = Time::ZERO;
        let mut layers = Vec::new();
        for (w, p) in parts {
            if w > Time::ZERO {
                layers.push(p.compress(acc / total, (acc + w) / total));
            }
            acc = acc + w;
        }
        Pattern::stack(layers)
    }

    /// All at once.
    pub fn stack(pats: Vec<Pattern<T>>) -> Pattern<T> {
        Pattern::new(move |q| pats.iter().flat_map(|p| p.query(q)).collect())
    }

    /// One of them each cycle, chosen at random (`a | b`).
    pub fn choose_cycle(pats: Vec<Pattern<T>>, salt: u64) -> Pattern<T> {
        if pats.is_empty() {
            return Pattern::silence();
        }
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let r = rand_at(q.seed, span.begin.sam(), salt);
                let i = ((r * pats.len() as f64) as usize).min(pats.len() - 1);
                out.extend(pats[i].query(q.with_span(span)));
            }
            out
        })
    }

    // ── Time ─────────────────────────────────────────────────────────────

    /// The pattern's cycle squeezed into `[b, e)` of every cycle.
    pub fn compress(&self, b: Time, e: Time) -> Pattern<T> {
        if b >= e || b < Time::ZERO || e > Time::ONE {
            return Pattern::silence();
        }
        let p = self.clone();
        let w = e - b;
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let c = span.begin.sam();
                let Some(inside) = span.intersect(Span::new(c + b, c + e)) else {
                    continue;
                };
                let inner = inside.with_time(|t| c + (t - c - b) / w);
                out.extend(
                    p.query(q.with_span(inner))
                        .into_iter()
                        .map(|h| h.with_span(&|t| c + b + (t - c) * w)),
                );
            }
            out
        })
    }

    /// Faster by `by` (a negative speed plays backwards; zero is silence).
    pub fn fast(&self, by: Time) -> Pattern<T> {
        if by.is_zero() {
            return Pattern::silence();
        }
        if by < Time::ZERO {
            return self.rev().fast(-by);
        }
        if by == Time::ONE {
            return self.clone();
        }
        let p = self.clone();
        Pattern::new(move |q| {
            p.query(q.with_span(q.span.with_time(|t| t * by)))
                .into_iter()
                .map(|h| h.with_span(&|t| t / by))
                .collect()
        })
    }

    pub fn slow(&self, by: Time) -> Pattern<T> {
        if by.is_zero() {
            return Pattern::silence();
        }
        self.fast(Time::ONE / by)
    }

    /// Shifted earlier by `t` cycles (`0.25 <~`).
    pub fn early(&self, t: Time) -> Pattern<T> {
        if t.is_zero() {
            return self.clone();
        }
        let p = self.clone();
        Pattern::new(move |q| {
            p.query(q.with_span(q.span.with_time(|x| x + t)))
                .into_iter()
                .map(|h| h.with_span(&|x| x - t))
                .collect()
        })
    }

    pub fn late(&self, t: Time) -> Pattern<T> {
        self.early(-t)
    }

    /// Each cycle backwards.
    pub fn rev(&self) -> Pattern<T> {
        let p = self.clone();
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let c = span.begin.sam();
                let mirror = |t: Time| c + c + Time::ONE - t;
                let reflected = Span::new(mirror(span.end), mirror(span.begin));
                for h in p.query(q.with_span(reflected)) {
                    out.push(Hap {
                        whole: h.whole.map(|w| Span::new(mirror(w.end), mirror(w.begin))),
                        part: Span::new(mirror(h.part.end), mirror(h.part.begin)),
                        value: h.value,
                    });
                }
            }
            out
        })
    }

    /// Each event repeated `n` times inside its own span.
    pub fn ply(&self, n: u32) -> Pattern<T> {
        if n <= 1 {
            return self.clone();
        }
        let p = self.clone();
        let n = n as i64;
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for h in p.query(q) {
                let Some(w) = h.whole else {
                    out.push(h);
                    continue;
                };
                let step = w.duration() / Time::whole(n);
                for i in 0..n {
                    let sub = Span::new(
                        w.begin + step * Time::whole(i),
                        w.begin + step * Time::whole(i + 1),
                    );
                    if let Some(part) = sub.intersect(h.part) {
                        out.push(Hap {
                            whole: Some(sub),
                            part,
                            value: h.value.clone(),
                        });
                    }
                }
            }
            out
        })
    }

    /// Sampled `n` times a cycle: a signal becomes events.
    pub fn segment(&self, n: u32) -> Pattern<T> {
        let grid = Pattern::fastcat((0..n.max(1)).map(|_| Pattern::pure(true)).collect());
        grid.structure(self)
    }

    /// `pat`'s values at the onsets of `self`'s true events, with their
    /// timing: the rhythm of one, the content of the other.
    pub fn structure<U: Value>(&self, pat: &Pattern<U>) -> Pattern<U>
    where
        T: Into<bool>,
    {
        let rhythm = self.clone();
        let pat = pat.clone();
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for b in rhythm.query(q) {
                let Some(w) = b.whole else { continue };
                if !b.value.clone().into() {
                    continue;
                }
                for h in pat.query(q.with_span(w)) {
                    let at_onset = match h.whole {
                        Some(hw) => hw.begin <= w.begin && w.begin < hw.end,
                        None => h.part.begin == w.begin,
                    };
                    if at_onset {
                        out.push(Hap {
                            whole: Some(w),
                            part: b.part,
                            value: h.value,
                        });
                    }
                }
            }
            out
        })
    }

    /// `k` onsets spread over `n` steps, rotated by `r` (`bd(3,8)`).
    pub fn euclid(&self, k: u32, n: u32, r: i32) -> Pattern<T> {
        let steps = bjorklund(k, n, r);
        let rhythm = Pattern::fastcat(steps.into_iter().map(Pattern::pure).collect());
        rhythm.structure(self)
    }

    /// Each cycle starts `1/n` further in, coming round every `n` cycles.
    pub fn iter(&self, n: u32) -> Pattern<T> {
        if n <= 1 {
            return self.clone();
        }
        let p = self.clone();
        let n = n as i64;
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let shift = Time::new(span.begin.cycle().rem_euclid(n), n);
                out.extend(p.early(shift).query(q.with_span(span)));
            }
            out
        })
    }

    // ── Conditional ──────────────────────────────────────────────────────

    /// `f` applied on the cycles `pick` chooses.
    pub fn when_cycle(
        &self,
        pick: impl Fn(i64) -> bool + Send + Sync + 'static,
        f: impl FnOnce(Pattern<T>) -> Pattern<T>,
    ) -> Pattern<T> {
        let plain = self.clone();
        let changed = f(self.clone());
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let p = if pick(span.begin.cycle()) {
                    &changed
                } else {
                    &plain
                };
                out.extend(p.query(q.with_span(span)));
            }
            out
        })
    }

    /// `f` every `n`th cycle, counting from `offset`.
    pub fn every(
        &self,
        n: u32,
        offset: i64,
        f: impl FnOnce(Pattern<T>) -> Pattern<T>,
    ) -> Pattern<T> {
        if n == 0 {
            return self.clone();
        }
        let n = n as i64;
        self.when_cycle(move |c| (c - offset).rem_euclid(n) == 0, f)
    }

    /// `f` applied inside `[b, e)` of each cycle (by onset), the rest as is.
    pub fn within(&self, b: Time, e: Time, f: impl FnOnce(Pattern<T>) -> Pattern<T>) -> Pattern<T> {
        let inside = move |t: Time| {
            let p = t.cycle_pos();
            b <= p && p < e
        };
        Pattern::stack(vec![
            f(self.clone()).filter_haps(move |h| inside(h.onset())),
            self.filter_haps(move |h| !inside(h.onset())),
        ])
    }

    /// `f` on one `n`th of the cycle, a different one each cycle.
    pub fn chunk(&self, n: u32, f: impl Fn(Pattern<T>) -> Pattern<T>) -> Pattern<T> {
        if n == 0 {
            return self.clone();
        }
        let n = n as i64;
        let parts: Vec<Pattern<T>> = (0..n)
            .map(|i| self.within(Time::new(i, n), Time::new(i + 1, n), &f))
            .collect();
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let i = span.begin.cycle().rem_euclid(n) as usize;
                out.extend(parts[i].query(q.with_span(span)));
            }
            out
        })
    }

    /// The pattern with `f` of it on top.
    pub fn superimpose(&self, f: impl FnOnce(Pattern<T>) -> Pattern<T>) -> Pattern<T> {
        Pattern::stack(vec![self.clone(), f(self.clone())])
    }

    /// The pattern with `f` of a copy `t` cycles later on top.
    pub fn off(&self, t: Time, f: impl FnOnce(Pattern<T>) -> Pattern<T>) -> Pattern<T> {
        Pattern::stack(vec![self.clone(), f(self.late(t))])
    }

    // ── Chance (deterministic: keyed on each event's onset) ──────────────

    /// Each event dropped with probability `p`.
    pub fn degrade_by(&self, p: f64, salt: u64) -> Pattern<T> {
        self.filter_seeded(move |seed, h| rand_at(seed, h.onset(), salt) >= p)
    }

    /// Exactly the events `degrade_by` drops.
    pub fn undegrade_by(&self, p: f64, salt: u64) -> Pattern<T> {
        self.filter_seeded(move |seed, h| rand_at(seed, h.onset(), salt) < p)
    }

    /// `f` on each event with probability `p`.
    pub fn sometimes_by(
        &self,
        p: f64,
        salt: u64,
        f: impl FnOnce(Pattern<T>) -> Pattern<T>,
    ) -> Pattern<T> {
        Pattern::stack(vec![
            self.degrade_by(p, salt),
            f(self.undegrade_by(p, salt)),
        ])
    }

    /// `f` on whole cycles with probability `p`.
    pub fn somecycles_by(
        &self,
        p: f64,
        salt: u64,
        f: impl FnOnce(Pattern<T>) -> Pattern<T>,
    ) -> Pattern<T> {
        let plain = self.clone();
        let changed = f(self.clone());
        Pattern::new(move |q| {
            let mut out = Vec::new();
            for span in q.span.split_cycles() {
                let hit = rand_at(q.seed, span.begin.sam(), salt) < p;
                out.extend(if hit { &changed } else { &plain }.query(q.with_span(span)));
            }
            out
        })
    }

    // ── Values ───────────────────────────────────────────────────────────

    pub fn fmap<U: Value>(&self, f: impl Fn(T) -> U + Send + Sync + 'static) -> Pattern<U> {
        let p = self.clone();
        let f = Arc::new(f);
        Pattern::new(move |q| {
            p.query(q)
                .into_iter()
                .map(|h| {
                    let f = f.clone();
                    h.map(|v| f(v))
                })
                .collect()
        })
    }

    pub fn filter_haps(
        &self,
        keep: impl Fn(&Hap<T>) -> bool + Send + Sync + 'static,
    ) -> Pattern<T> {
        let p = self.clone();
        Pattern::new(move |q| p.query(q).into_iter().filter(|h| keep(h)).collect())
    }

    fn filter_seeded(
        &self,
        keep: impl Fn(u64, &Hap<T>) -> bool + Send + Sync + 'static,
    ) -> Pattern<T> {
        let p = self.clone();
        Pattern::new(move |q| p.query(q).into_iter().filter(|h| keep(q.seed, h)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: i64, d: i64) -> Time {
        Time::new(n, d)
    }

    fn onsets<T: Value>(p: &Pattern<T>, cycles: i64) -> Vec<(Time, T)> {
        p.onsets(Span::cycles(0, cycles), 0)
            .into_iter()
            .map(|h| (h.part.begin, h.value))
            .collect()
    }

    #[test]
    fn sequences_subdivide_and_alternate() {
        let p = Pattern::fastcat(vec![
            Pattern::pure("a"),
            Pattern::pure("b"),
            Pattern::pure("c"),
        ]);
        assert_eq!(
            onsets(&p, 1),
            vec![(t(0, 1), "a"), (t(1, 3), "b"), (t(2, 3), "c")]
        );
        let alt = Pattern::slowcat(vec![Pattern::pure("a"), Pattern::pure("b")]);
        assert_eq!(
            onsets(&alt, 3),
            vec![(t(0, 1), "a"), (t(1, 1), "b"), (t(2, 1), "a")]
        );
        // <a <b c>>: the inner one counts its own turns.
        let nested = Pattern::slowcat(vec![
            Pattern::pure("a"),
            Pattern::slowcat(vec![Pattern::pure("b"), Pattern::pure("c")]),
        ]);
        let vals: Vec<_> = onsets(&nested, 4).into_iter().map(|x| x.1).collect();
        assert_eq!(vals, ["a", "b", "a", "c"]);
    }

    #[test]
    fn fast_slow_and_rotation() {
        let p = Pattern::fastcat(vec![Pattern::pure(1), Pattern::pure(2)]);
        assert_eq!(onsets(&p.fast(Time::whole(2)), 1).len(), 4);
        let slow = p.slow(Time::whole(2));
        assert_eq!(onsets(&slow, 2), vec![(t(0, 1), 1), (t(1, 1), 2)]);
        assert_eq!(
            onsets(&p.early(t(1, 2)), 1),
            vec![(t(0, 1), 2), (t(1, 2), 1)]
        );
        assert_eq!(
            onsets(&p.late(t(1, 4)), 1),
            vec![(t(1, 4), 1), (t(3, 4), 2)]
        );
    }

    #[test]
    fn rev_mirrors_each_cycle() {
        let p = Pattern::fastcat(vec![Pattern::pure(1), Pattern::pure(2), Pattern::pure(3)]);
        let vals: Vec<_> = onsets(&p.rev(), 2).into_iter().map(|x| x.1).collect();
        assert_eq!(vals, [3, 2, 1, 3, 2, 1]);
    }

    #[test]
    fn euclid_spreads_onsets() {
        assert_eq!(
            bjorklund(3, 8, 0),
            [true, false, false, true, false, false, true, false]
        );
        assert_eq!(
            bjorklund(5, 8, 0),
            [true, false, true, true, false, true, true, false]
        );
        assert_eq!(bjorklund(3, 8, 2).iter().position(|b| *b), Some(0));
        assert!(bjorklund(3, 8, 2)[2]);
        assert_eq!(bjorklund(7, 16, 0).iter().filter(|b| **b).count(), 7);
        assert_eq!(bjorklund(0, 4, 0), [false; 4]);
        assert_eq!(bjorklund(4, 4, 0), [true; 4]);
        let p = Pattern::pure("bd").euclid(3, 8, 0);
        let at: Vec<_> = onsets(&p, 1).into_iter().map(|x| x.0).collect();
        assert_eq!(at, [t(0, 1), t(3, 8), t(6, 8)]);
    }

    #[test]
    fn every_applies_on_its_cycles() {
        let p = Pattern::fastcat(vec![Pattern::pure(1), Pattern::pure(2)]);
        let e = p.every(3, 0, |x| x.fast(Time::whole(2)));
        let per_cycle: Vec<usize> = (0..6)
            .map(|c| e.onsets(Span::cycles(c, c + 1), 0).len())
            .collect();
        assert_eq!(per_cycle, [4, 2, 2, 4, 2, 2]);
    }

    #[test]
    fn chance_is_deterministic_and_complementary() {
        let p = Pattern::pure(1).fast(Time::whole(16));
        let a = onsets(&p.degrade_by(0.5, 7), 8);
        let b = onsets(&p.degrade_by(0.5, 7), 8);
        assert_eq!(a, b, "same seed, same events");
        let kept = a.len();
        let dropped = onsets(&p.undegrade_by(0.5, 7), 8).len();
        assert_eq!(kept + dropped, 128);
        assert!((40..88).contains(&kept), "about half: {kept}");
        let other_seed = p.degrade_by(0.5, 7).onsets(Span::cycles(0, 8), 99).len();
        assert_ne!(other_seed, 0);
        // sometimes: every event is either itself or transformed, once.
        let s = p.sometimes_by(0.5, 3, |x| x.fmap(|v| v + 10));
        let vals = onsets(&s, 8);
        assert_eq!(vals.len(), 128);
        assert!(vals.iter().any(|x| x.1 == 11) && vals.iter().any(|x| x.1 == 1));
    }

    #[test]
    fn split_queries_dont_duplicate_or_lose_events() {
        let p = Pattern::fastcat(vec![
            Pattern::pure(1),
            Pattern::fastcat(vec![Pattern::pure(2), Pattern::pure(3)]).fast(t(3, 2)),
        ])
        .slow(t(3, 2))
        .euclid(5, 8, 1);
        let whole = onsets(&p, 6);
        let mut pieces = Vec::new();
        let mut b = Time::ZERO;
        let step = t(7, 100);
        while b < Time::whole(6) {
            let e = (b + step).min(Time::whole(6));
            pieces.extend(
                p.onsets(Span::new(b, e), 0)
                    .into_iter()
                    .map(|h| (h.part.begin, h.value)),
            );
            b = e;
        }
        assert_eq!(whole, pieces);
    }

    #[test]
    fn ply_segment_iter_chunk_off() {
        let p = Pattern::fastcat(vec![Pattern::pure(1), Pattern::pure(2)]);
        assert_eq!(onsets(&p.ply(3), 1).len(), 6);
        let saw = Pattern::signal(|t, _| t.cycle_pos().to_f64());
        let seg = onsets(&saw.segment(4), 1);
        assert_eq!(
            seg.iter().map(|x| x.1).collect::<Vec<_>>(),
            [0.0, 0.25, 0.5, 0.75]
        );
        let four = Pattern::fastcat((1..=4).map(Pattern::pure).collect());
        let it: Vec<_> = onsets(&four.iter(4), 2).into_iter().map(|x| x.1).collect();
        assert_eq!(it, [1, 2, 3, 4, 2, 3, 4, 1]);
        let ch: Vec<_> = onsets(&four.chunk(4, |x| x.fmap(|v| v * 10)), 2)
            .into_iter()
            .map(|x| x.1)
            .collect();
        assert_eq!(ch, [10, 2, 3, 4, 1, 20, 3, 4]);
        assert_eq!(onsets(&Pattern::pure(1).off(t(1, 4), |x| x), 1).len(), 2);
    }
}
