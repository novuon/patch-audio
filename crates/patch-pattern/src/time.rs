//! Musical time, exactly: a rational number of cycles.
//!
//! A pattern is queried over spans of cycles; thirds, fifths and sevenths
//! of a cycle are common, so floating point would drift (0.1 + 0.2 ≠ 0.3,
//! and a triplet's third step lands a hair off its grid). Every time here
//! is `num / den`, reduced, `den > 0`. Products go through i128 and are
//! reduced at once, so ordinary musical arithmetic stays small.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Div, Mul, Neg, Sub};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Time {
    num: i64,
    den: i64,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Time {
    pub const ZERO: Time = Time { num: 0, den: 1 };
    pub const ONE: Time = Time { num: 1, den: 1 };

    /// `num / den`, reduced. Panics on a zero denominator (a bug, not input:
    /// parsers reject zero before building a time).
    pub fn new(num: i64, den: i64) -> Time {
        Time::reduce(num as i128, den as i128)
    }

    pub const fn whole(n: i64) -> Time {
        Time { num: n, den: 1 }
    }

    fn reduce(num: i128, den: i128) -> Time {
        assert!(den != 0, "time with a zero denominator");
        let g = gcd(num, den).max(1);
        let (mut n, mut d) = (num / g, den / g);
        if d < 0 {
            n = -n;
            d = -d;
        }
        // Musical times stay tiny; saturating keeps a runaway pattern
        // (fast 1e9 of fast 1e9) from panicking the scheduler.
        let clamp = |v: i128| v.clamp(i64::MIN as i128 + 1, i64::MAX as i128) as i64;
        Time {
            num: clamp(n),
            den: clamp(d).max(1),
        }
    }

    pub fn num(self) -> i64 {
        self.num
    }

    pub fn den(self) -> i64 {
        self.den
    }

    /// The cycle this time is in (floor).
    pub fn sam(self) -> Time {
        Time::whole(self.num.div_euclid(self.den))
    }

    /// The start of the next cycle.
    pub fn next_sam(self) -> Time {
        self.sam() + Time::ONE
    }

    /// Smallest whole number of cycles ≥ self.
    pub fn ceil(self) -> Time {
        let s = self.sam();
        if s == self { s } else { s + Time::ONE }
    }

    /// Position inside its cycle, 0 ≤ x < 1.
    pub fn cycle_pos(self) -> Time {
        self - self.sam()
    }

    pub fn cycle(self) -> i64 {
        self.num.div_euclid(self.den)
    }

    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn is_zero(self) -> bool {
        self.num == 0
    }

    pub fn min(self, o: Time) -> Time {
        if self <= o { self } else { o }
    }

    pub fn max(self, o: Time) -> Time {
        if self >= o { self } else { o }
    }

    /// The nearest fraction with a denominator ≤ `max_den` (continued
    /// fractions): for the occasional f64 that must become a time, such as
    /// a tempo ratio. Parsers read decimals exactly instead ([`Time::parse`]).
    pub fn approx(x: f64, max_den: i64) -> Time {
        if !x.is_finite() {
            return Time::ZERO;
        }
        let neg = x < 0.0;
        let mut v = x.abs();
        let (mut h0, mut h1, mut k0, mut k1) = (0i128, 1i128, 1i128, 0i128);
        for _ in 0..40 {
            let a = v.floor();
            let ai = a as i128;
            let (h2, k2) = (ai * h1 + h0, ai * k1 + k0);
            if k2 > max_den as i128 {
                break;
            }
            (h0, h1, k0, k1) = (h1, h2, k1, k2);
            let f = v - a;
            if f < 1e-12 {
                break;
            }
            v = 1.0 / f;
        }
        let t = Time::reduce(h1, k1.max(1));
        if neg { -t } else { t }
    }

    /// "3", "-2", "0.25", "1/3", "1.5/2": exact (a decimal is its own
    /// fraction, 0.1 is 1/10).
    pub fn parse(s: &str) -> Option<Time> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once('/') {
            let (a, b) = (Time::parse(a)?, Time::parse(b)?);
            return (!b.is_zero()).then(|| a / b);
        }
        let (neg, body) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        if body.is_empty() {
            return None;
        }
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if int.is_empty() && frac.is_empty() || int.len() + frac.len() > 17 {
            return None;
        }
        let digits: i64 = format!("{int}{frac}").parse().ok()?;
        let t = Time::new(digits, 10i64.pow(frac.len() as u32));
        Some(if neg { -t } else { t })
    }
}

impl Default for Time {
    fn default() -> Time {
        Time::ZERO
    }
}

impl From<i64> for Time {
    fn from(n: i64) -> Time {
        Time::whole(n)
    }
}

impl Ord for Time {
    fn cmp(&self, o: &Time) -> Ordering {
        (self.num as i128 * o.den as i128).cmp(&(o.num as i128 * self.den as i128))
    }
}

impl PartialOrd for Time {
    fn partial_cmp(&self, o: &Time) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, o: Time) -> Time {
        if self.den == o.den {
            return Time::reduce(self.num as i128 + o.num as i128, self.den as i128);
        }
        Time::reduce(
            self.num as i128 * o.den as i128 + o.num as i128 * self.den as i128,
            self.den as i128 * o.den as i128,
        )
    }
}

impl Sub for Time {
    type Output = Time;
    fn sub(self, o: Time) -> Time {
        self + -o
    }
}

impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time {
            num: -self.num,
            den: self.den,
        }
    }
}

impl Mul for Time {
    type Output = Time;
    fn mul(self, o: Time) -> Time {
        Time::reduce(
            self.num as i128 * o.num as i128,
            self.den as i128 * o.den as i128,
        )
    }
}

impl Div for Time {
    type Output = Time;
    fn div(self, o: Time) -> Time {
        Time::reduce(
            self.num as i128 * o.den as i128,
            self.den as i128 * o.num as i128,
        )
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// In JSON a time is its exact text ("1/3"); a plain number is accepted too.
impl Serialize for Time {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Time, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Int(i64),
            Float(f64),
        }
        match Raw::deserialize(d)? {
            Raw::Text(s) => {
                Time::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a time: {s}")))
            }
            Raw::Int(n) => Ok(Time::whole(n)),
            Raw::Float(x) => Ok(Time::parse(&x.to_string()).unwrap_or(Time::approx(x, 1 << 20))),
        }
    }
}

/// A half-open span of cycles, `begin ≤ t < end`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub begin: Time,
    pub end: Time,
}

impl Span {
    pub fn new(begin: Time, end: Time) -> Span {
        Span { begin, end }
    }

    pub fn cycles(a: i64, b: i64) -> Span {
        Span::new(Time::whole(a), Time::whole(b))
    }

    pub fn duration(self) -> Time {
        self.end - self.begin
    }

    /// The span cut at each cycle boundary: `[0.5, 2.25)` → `[0.5, 1)`,
    /// `[1, 2)`, `[2, 2.25)`. A zero-width span stays one piece.
    pub fn split_cycles(self) -> Vec<Span> {
        let mut out = Vec::new();
        if self.begin == self.end {
            out.push(self);
            return out;
        }
        let mut b = self.begin;
        while b < self.end {
            let e = b.next_sam().min(self.end);
            out.push(Span::new(b, e));
            b = e;
        }
        out
    }

    /// Where two spans overlap. Touching spans don't, except that a
    /// zero-width span at `t` is inside a span that contains `t`.
    pub fn intersect(self, o: Span) -> Option<Span> {
        let b = self.begin.max(o.begin);
        let e = self.end.min(o.end);
        if b < e {
            return Some(Span::new(b, e));
        }
        if b == e {
            let zero_inside =
                |z: Span, s: Span| z.begin == z.end && s.begin <= z.begin && z.begin < s.end;
            if zero_inside(self, o) || zero_inside(o, self) {
                return Some(Span::new(b, e));
            }
        }
        None
    }

    pub fn with_time(self, f: impl Fn(Time) -> Time) -> Span {
        Span::new(f(self.begin), f(self.end))
    }

    /// The span, its times relative to the cycle `begin` is in.
    pub fn cycle_relative(self) -> (Time, Span) {
        let sam = self.begin.sam();
        (sam, Span::new(self.begin - sam, self.end - sam))
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[{} → {})", self.begin, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_is_exact() {
        let third = Time::new(1, 3);
        assert_eq!(third + third + third, Time::ONE);
        assert_eq!(
            Time::parse("0.1").unwrap() + Time::parse("0.2").unwrap(),
            Time::parse("0.3").unwrap()
        );
        assert_eq!(Time::new(2, -4), Time::new(-1, 2));
        assert_eq!(Time::new(7, 3).sam(), Time::whole(2));
        assert_eq!(Time::new(-1, 3).sam(), Time::whole(-1));
        assert_eq!(Time::new(7, 3).cycle_pos(), Time::new(1, 3));
        assert_eq!(Time::new(7, 3).ceil(), Time::whole(3));
        assert!(Time::new(1, 3) < Time::new(1, 2));
    }

    #[test]
    fn a_million_triplets_never_drift() {
        let step = Time::new(1, 3);
        let mut t = Time::ZERO;
        for _ in 0..3_000_000 {
            t = t + step;
        }
        assert_eq!(t, Time::whole(1_000_000));
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(Time::parse("1/3"), Some(Time::new(1, 3)));
        assert_eq!(Time::parse("-0.25"), Some(Time::new(-1, 4)));
        assert_eq!(Time::parse("1.5/2"), Some(Time::new(3, 4)));
        assert_eq!(Time::parse("x"), None);
        assert_eq!(Time::parse("1/0"), None);
        assert_eq!(Time::new(3, 4).to_string(), "3/4");
        let j = serde_json::to_string(&Time::new(1, 3)).unwrap();
        assert_eq!(j, "\"1/3\"");
        assert_eq!(serde_json::from_str::<Time>("2").unwrap(), Time::whole(2));
        assert_eq!(
            serde_json::from_str::<Time>("0.5").unwrap(),
            Time::new(1, 2)
        );
        assert_eq!(Time::approx(0.333333333, 1000), Time::new(1, 3));
    }

    #[test]
    fn spans_split_at_cycles() {
        let s = Span::new(Time::new(1, 2), Time::new(9, 4));
        assert_eq!(
            s.split_cycles(),
            vec![
                Span::new(Time::new(1, 2), Time::ONE),
                Span::cycles(1, 2),
                Span::new(Time::whole(2), Time::new(9, 4)),
            ]
        );
        assert!(Span::cycles(0, 1).intersect(Span::cycles(1, 2)).is_none());
        let z = Span::new(Time::ONE, Time::ONE);
        assert_eq!(Span::cycles(1, 2).intersect(z), Some(z));
    }
}
