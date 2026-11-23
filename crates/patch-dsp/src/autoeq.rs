//! Headphone corrections from AutoEq (github.com/jaakkopasanen/AutoEq):
//! its index, its `ParametricEQ.txt` profiles, and a refit onto Patch's
//! eight bands.
//!
//! AutoEq's profiles have ten filters (a low shelf, eight bells, a high
//! shelf) and a preamp. Patch's EQ has eight bands, so a profile with more
//! filters is refitted: the AutoEq curve is the target, the eight most
//! important filters the start, and a coordinate search moves frequency,
//! gain and Q until the curve matches. The result says how close it got.
//! Pure functions: fetching and caching belong to the caller.

use crate::filter::{self, Band, ChannelTarget, FilterType};

/// Where the profiles live (raw files of AutoEq's `results` tree).
pub const RESULTS_URL: &str =
    "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/";

/// One headphone measurement in AutoEq's `INDEX.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Relative path inside `results/`, decoded ("oratory1990/over-ear/Sennheiser HD 650").
    pub path: String,
    /// Who measured it ("oratory1990", "crinacle on 711").
    pub source: String,
}

impl Entry {
    /// The profile's URL.
    pub fn url(&self) -> String {
        let file = format!(
            "{} ParametricEQ.txt",
            self.path.rsplit('/').next().unwrap_or("")
        );
        format!(
            "{RESULTS_URL}{}/{}",
            encode(&self.path, true),
            encode(&file, false)
        )
    }
}

/// Percent-encodes everything but unreserved characters (and `/` in paths).
fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `- [Sennheiser HD 650](./oratory1990/over-ear/Sennheiser%20HD%20650) by oratory1990`
pub fn parse_index(md: &str) -> Vec<Entry> {
    md.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("- [")?;
            let (name, rest) = rest.split_once("](./")?;
            // Names and paths may hold parentheses: the link ends at the last ") by ".
            let (path, source) = rest
                .rsplit_once(") by ")
                .or_else(|| Some((rest.trim_end().strip_suffix(')')?, "")))?;
            let path = decode(path);
            // Inside the results tree only.
            if name.trim().is_empty() || path.contains("..") || path.starts_with('/') {
                return None;
            }
            Some(Entry {
                name: name.trim().to_string(),
                path,
                source: source.trim().to_string(),
            })
        })
        .collect()
}

/// Lower case, no spaces or punctuation: "HD650" finds "HD 650".
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Measurements trusted most first (the order AutoEq's own docs recommend).
fn source_rank(source: &str) -> u8 {
    match source.split(" on ").next().unwrap_or("") {
        "oratory1990" => 0,
        "crinacle" => 1,
        "Rtings" => 2,
        _ => 3,
    }
}

/// Headphones whose name holds every word of the query, one entry per name
/// (the best source), names that start with it first, then shortest (the
/// base model before its variants).
pub fn search<'a>(entries: &'a [Entry], query: &str, limit: usize) -> Vec<&'a Entry> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(squash)
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let whole = squash(query);
    let mut best: Vec<&Entry> = Vec::new();
    for e in entries {
        let n = squash(&e.name);
        if !words.iter().all(|w| n.contains(w.as_str())) {
            continue;
        }
        match best.iter_mut().find(|b| b.name == e.name) {
            Some(b) if source_rank(&e.source) < source_rank(&b.source) => *b = e,
            Some(_) => {}
            None => best.push(e),
        }
    }
    best.sort_by_key(|e| {
        let n = squash(&e.name);
        (!n.starts_with(&whole), !n.contains(&whole), e.name.len())
    });
    best.truncate(limit);
    best
}

/// One AutoEq filter (or a Patch band: the same three numbers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    pub kind: FilterType,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
}

/// A parsed `ParametricEQ.txt`.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub preamp_db: f64,
    pub filters: Vec<Filter>,
}

/// `Preamp: -6.1 dB` and `Filter 1: ON LSC Fc 105 Hz Gain 6.4 dB Q 0.70`
/// lines; filters that are OFF or of another kind are skipped.
pub fn parse_profile(text: &str) -> Result<Profile, String> {
    let mut preamp_db = 0.0;
    let mut filters = Vec::new();
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if let ["Preamp:", v, ..] = w.as_slice() {
            preamp_db = v.parse().map_err(|_| format!("bad preamp line '{line}'"))?;
            continue;
        }
        let Some(i) = w.iter().position(|x| *x == "ON") else {
            continue;
        };
        let kind = match w.get(i + 1).copied() {
            Some("PK" | "PEQ") => FilterType::Bell,
            Some("LSC" | "LS" | "LSQ") => FilterType::LowShelf,
            Some("HSC" | "HS" | "HSQ") => FilterType::HighShelf,
            _ => continue,
        };
        let after = |key: &str| {
            w.iter()
                .position(|x| *x == key)
                .and_then(|k| w.get(k + 1))
                .and_then(|v| v.parse::<f64>().ok())
        };
        let (Some(freq), Some(gain_db)) = (after("Fc"), after("Gain")) else {
            continue;
        };
        let q = after("Q").unwrap_or(if kind == FilterType::Bell { 1.41 } else { 0.71 });
        if freq.is_finite() && gain_db.is_finite() && q.is_finite() && freq > 0.0 && q > 0.0 {
            filters.push(Filter {
                kind,
                freq,
                gain_db,
                q,
            });
        }
    }
    if filters.is_empty() {
        return Err("no filters in the profile".into());
    }
    Ok(Profile { preamp_db, filters })
}

/// Patch's band ranges (`patch_core::eq`).
const FREQ: (f64, f64) = (20.0, 20_000.0);
const GAIN: f64 = 18.0;
const QS: (f64, f64) = (0.1, 18.0);
const RATE: f64 = 48_000.0;

fn clamp(f: Filter) -> Filter {
    Filter {
        freq: f.freq.clamp(FREQ.0, FREQ.1),
        gain_db: f.gain_db.clamp(-GAIN, GAIN),
        q: f.q.clamp(QS.0, QS.1),
        ..f
    }
}

/// Response (dB) of a filter at the grid's points, Patch's own design
/// (shelves at the standard slope, as the engine runs them).
fn response(f: &Filter, grid: &[(f64, f64)], out: &mut [f64]) {
    let band = Band {
        enabled: true,
        kind: f.kind,
        frequency_hz: f.freq.min(RATE * 0.45),
        gain_db: f.gain_db,
        q: f.q,
        target: ChannelTarget::All,
    };
    let Ok(c) = filter::design(band, RATE) else {
        out.fill(0.0);
        return;
    };
    for (o, &(cos1, cos2)) in out.iter_mut().zip(grid) {
        // |H|² from the cosines alone (sin² = 1 − cos²).
        let (s1, s2) = (
            (1.0 - cos1 * cos1).max(0.0).sqrt(),
            (1.0 - cos2 * cos2).max(0.0).sqrt(),
        );
        let num = (c.b0 + c.b1 * cos1 + c.b2 * cos2, c.b1 * s1 + c.b2 * s2);
        let den = (1.0 + c.a1 * cos1 + c.a2 * cos2, c.a1 * s1 + c.a2 * s2);
        let m2 = (num.0 * num.0 + num.1 * num.1) / (den.0 * den.0 + den.1 * den.1).max(1e-300);
        *o = 10.0 * m2.max(1e-24).log10();
    }
}

/// The frequencies a fit is judged at: log-spaced, 20 Hz – 20 kHz.
fn grid(n: usize) -> (Vec<f64>, Vec<(f64, f64)>) {
    let hz: Vec<f64> = (0..n)
        .map(|i| 20.0 * 1000f64.powf(i as f64 / (n - 1) as f64))
        .collect();
    let cs = hz
        .iter()
        .map(|&f| {
            let w = std::f64::consts::TAU * f / RATE;
            (w.cos(), (2.0 * w).cos())
        })
        .collect();
    (hz, cs)
}

/// The summed response of filters at the grid.
fn curve(filters: &[Filter], grid: &[(f64, f64)]) -> Vec<f64> {
    let mut sum = vec![0.0; grid.len()];
    let mut one = vec![0.0; grid.len()];
    for f in filters {
        response(f, grid, &mut one);
        for (s, o) in sum.iter_mut().zip(&one) {
            *s += o;
        }
    }
    sum
}

/// A profile on Patch's bands, and how far it is from AutoEq's curve.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub bands: Vec<Filter>,
    /// The cut that keeps the fitted curve's highest point at 0 dB.
    pub preamp_db: f64,
    /// RMS and largest difference from AutoEq's curve, 20 Hz – 10 kHz (dB).
    pub rms_db: f64,
    pub max_db: f64,
}

/// Errors above 10 kHz count a third: AutoEq trusts its treble less, and
/// the ear's resonances there move with every seating of the headphone.
fn weight(hz: f64) -> f64 {
    if hz > 10_000.0 { 0.33 } else { 1.0 }
}

/// Fits a profile onto at most `max` bands (Patch: 8). A profile that
/// already fits is taken as it is (within Patch's ranges).
pub fn fit(profile: &Profile, max: usize) -> Fit {
    let (hz, g) = grid(160);
    let target = curve(&profile.filters, &g);
    let mut bands: Vec<Filter> = profile.filters.iter().copied().map(clamp).collect();
    if bands.len() > max {
        // Start from the filters that move the curve most (gain × width).
        let area =
            |f: &Filter| f.gain_db.abs() / if f.kind == FilterType::Bell { f.q } else { 0.7 };
        bands.sort_by(|a, b| area(b).total_cmp(&area(a)));
        bands.truncate(max);
        refine(&mut bands, &target, &hz, &g);
    }
    bands.sort_by(|a, b| a.freq.total_cmp(&b.freq));
    // Judged on a finer grid than the search's, so nothing hides between
    // its points.
    let (hz, g) = grid(400);
    let (got, target) = (curve(&bands, &g), curve(&profile.filters, &g));
    let (mut sq, mut n, mut max_db) = (0.0, 0.0, 0.0f64);
    for ((s, t), &f) in got.iter().zip(&target).zip(&hz) {
        if f <= 10_000.0 {
            sq += (s - t) * (s - t);
            n += 1.0;
            max_db = max_db.max((s - t).abs());
        }
    }
    let peak = got.iter().copied().fold(f64::MIN, f64::max);
    Fit {
        bands,
        preamp_db: -peak.max(0.0),
        rms_db: (sq / n).sqrt(),
        max_db,
    }
}

/// Coordinate search: each parameter in turn tries a step either way and
/// keeps what lowers the cost; the steps halve when nothing improves. Each
/// band's response is kept, so a trial computes only the band it moves.
fn refine(bands: &mut [Filter], target: &[f64], hz: &[f64], g: &[(f64, f64)]) {
    let w: Vec<f64> = hz.iter().map(|&f| weight(f)).collect();
    let mut resp: Vec<Vec<f64>> = bands
        .iter()
        .map(|b| {
            let mut r = vec![0.0; g.len()];
            response(b, g, &mut r);
            r
        })
        .collect();
    let mut sum = vec![0.0; g.len()];
    for r in &resp {
        for (s, o) in sum.iter_mut().zip(r) {
            *s += o;
        }
    }
    // The cost with band `i`'s response swapped for `new`.
    let cost = |sum: &[f64], old: &[f64], new: &[f64]| -> f64 {
        let mut c = 0.0;
        for k in 0..sum.len() {
            let e = sum[k] - old[k] + new[k] - target[k];
            c += w[k] * e * e;
        }
        c
    };
    let mut best = cost(&sum, &resp[0], &resp[0]);
    let mut trial = vec![0.0; g.len()];
    // Octaves, dB, and a factor on Q.
    let (mut df, mut dg, mut dq) = (0.25, 1.0, 0.25);
    for _ in 0..12 {
        loop {
            let mut improved = false;
            for i in 0..bands.len() {
                for p in 0..3 {
                    if p == 2 && bands[i].kind != FilterType::Bell {
                        continue; // shelves run at the standard slope
                    }
                    for sign in [1.0, -1.0] {
                        let mut b = bands[i];
                        match p {
                            0 => b.freq *= 2f64.powf(sign * df),
                            1 => b.gain_db += sign * dg,
                            _ => b.q *= 2f64.powf(sign * dq),
                        }
                        let b = clamp(b);
                        if b == bands[i] {
                            continue;
                        }
                        response(&b, g, &mut trial);
                        let c = cost(&sum, &resp[i], &trial);
                        if c < best - 1e-9 {
                            best = c;
                            improved = true;
                            bands[i] = b;
                            for k in 0..sum.len() {
                                sum[k] += trial[k] - resp[i][k];
                            }
                            std::mem::swap(&mut resp[i], &mut trial);
                        }
                    }
                }
            }
            if !improved {
                break;
            }
        }
        df /= 2.0;
        dg /= 2.0;
        dq /= 2.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD650: &str = "Preamp: -6.1 dB
Filter 1: ON LSC Fc 105 Hz Gain 6.4 dB Q 0.70
Filter 2: ON PK Fc 8800 Hz Gain 5.1 dB Q 1.42
Filter 3: ON PK Fc 118 Hz Gain -3.1 dB Q 0.50
Filter 4: ON PK Fc 37 Hz Gain 0.7 dB Q 3.96
Filter 5: ON PK Fc 3169 Hz Gain -1.7 dB Q 3.89
Filter 6: ON HSC Fc 10000 Hz Gain -2.1 dB Q 0.70
Filter 7: ON PK Fc 1227 Hz Gain -1.2 dB Q 2.53
Filter 8: ON PK Fc 2055 Hz Gain 1.2 dB Q 3.23
Filter 9: ON PK Fc 587 Hz Gain 0.4 dB Q 1.19
Filter 10: ON PK Fc 5332 Hz Gain -1.1 dB Q 5.75
";

    #[test]
    fn reads_a_profile() {
        let p = parse_profile(HD650).unwrap();
        assert_eq!(p.preamp_db, -6.1);
        assert_eq!(p.filters.len(), 10);
        assert_eq!(p.filters[0].kind, FilterType::LowShelf);
        assert_eq!(p.filters[5].kind, FilterType::HighShelf);
        assert_eq!(
            p.filters[9],
            Filter {
                kind: FilterType::Bell,
                freq: 5332.0,
                gain_db: -1.1,
                q: 5.75
            }
        );
        assert!(parse_profile("Preamp: -1 dB\n").is_err());
    }

    #[test]
    fn ten_filters_fit_on_eight_bands() {
        let f = fit(&parse_profile(HD650).unwrap(), 8);
        assert_eq!(f.bands.len(), 8);
        assert!(
            f.rms_db < 0.3 && f.max_db < 1.0,
            "rms {:.2} max {:.2}",
            f.rms_db,
            f.max_db
        );
        assert!(
            f.preamp_db < -4.0,
            "the bass shelf needs a cut: {}",
            f.preamp_db
        );
        assert!(f.bands.windows(2).all(|w| w[0].freq <= w[1].freq));
    }

    #[test]
    fn a_profile_that_fits_is_kept() {
        let p = parse_profile("Filter 1: ON PK Fc 1000 Hz Gain 3 dB Q 1\n").unwrap();
        let f = fit(&p, 8);
        assert_eq!(f.bands, p.filters);
        assert!(f.rms_db < 1e-9);
    }

    /// `AUTOEQ_DIR=<folder of ParametricEQ.txt files> cargo test --release
    /// -p patch-dsp autoeq_corpus -- --ignored --nocapture`: the fit on
    /// real profiles.
    #[test]
    #[ignore]
    fn autoeq_corpus() {
        let dir = std::env::var("AUTOEQ_DIR").expect("AUTOEQ_DIR");
        let (mut rms, mut worst, mut n) = (Vec::new(), 0.0f64, 0);
        let t = std::time::Instant::now();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let Ok(p) = parse_profile(&std::fs::read_to_string(e.path()).unwrap()) else {
                continue;
            };
            let f = fit(&p, 8);
            println!(
                "{:5.2} {:5.2} {:3} {:+5.1} {:+5.1} {}",
                f.rms_db,
                f.max_db,
                p.filters.len(),
                p.preamp_db,
                f.preamp_db,
                e.file_name().to_string_lossy()
            );
            rms.push(f.rms_db);
            worst = worst.max(f.max_db);
            n += 1;
        }
        rms.sort_by(f64::total_cmp);
        println!(
            "{n} profiles, {:.0} ms each; rms median {:.2} p90 {:.2} max {:.2}; worst point {:.2} dB",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64,
            rms[n / 2],
            rms[n * 9 / 10],
            rms[n - 1],
            worst
        );
    }

    #[test]
    fn index_and_search() {
        let md = "# Index\n\
- [Sennheiser HD 650](./crinacle/GRAS%2043AG-7%20over-ear/Sennheiser%20HD%20650) by crinacle on GRAS 43AG-7\n\
- [Sennheiser HD 650](./oratory1990/over-ear/Sennheiser%20HD%20650) by oratory1990\n\
- [Sennheiser HD 6XX](./oratory1990/over-ear/Sennheiser%20HD%206XX) by oratory1990\n\
- [Sennheiser HD 650 (2020)](./crinacle/711/Sennheiser%20HD%20650%20(2020)) by crinacle on 711\n\
- [Drop + Sennheiser HD 650 Custom](./Rtings/over-ear/x) by Rtings on HMS II.3\n\
- [Evil](./../../etc) by nobody\n";
        let all = parse_index(md);
        assert_eq!(all.len(), 5);
        assert_eq!(all[3].path, "crinacle/711/Sennheiser HD 650 (2020)");
        assert_eq!(all[3].source, "crinacle on 711");
        let hits = search(&all, "hd650", 5);
        assert_eq!(hits[0].source, "oratory1990");
        assert_eq!(hits[0].path, "oratory1990/over-ear/Sennheiser HD 650");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].url(),
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/oratory1990/over-ear/Sennheiser%20HD%20650/Sennheiser%20HD%20650%20ParametricEQ.txt"
        );
        assert!(search(&all, "  ", 5).is_empty());
    }
}
