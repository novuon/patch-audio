//! Musical key, in musical and Camelot notation.
//!
//! A chromagram from spectral *peaks* (8192-point Hann at 22.05 kHz,
//! 2.7 Hz bins; C2–C7), each frame normalised so loud passages don't
//! dominate, summed over the track and correlated (Pearson) with the
//! Krumhansl–Kessler major and minor key profiles in all 12 transpositions.

use crate::{harmony::HarmonicProfile, stft};
use serde::{Deserialize, Serialize};

pub const NAMES: [&str; 12] = [
    "C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B",
];

const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    /// Pitch class of the tonic, 0 = C.
    pub tonic: u8,
    pub minor: bool,
}

impl Key {
    /// "A minor", "E♭ major".
    pub fn name(self) -> String {
        format!(
            "{} {}",
            NAMES[self.tonic as usize % 12],
            if self.minor { "minor" } else { "major" }
        )
    }

    /// Short: "Am", "E♭".
    pub fn short(self) -> String {
        format!(
            "{}{}",
            NAMES[self.tonic as usize % 12],
            if self.minor { "m" } else { "" }
        )
    }

    /// Camelot wheel number (1–12); major keys are B, minor keys A.
    pub fn camelot_number(self) -> u8 {
        let major_tonic = if self.minor {
            (self.tonic + 3) % 12
        } else {
            self.tonic % 12
        };
        ((major_tonic as u32 * 7 % 12 + 7) % 12 + 1) as u8
    }

    /// "8A", "5B".
    pub fn camelot(self) -> String {
        format!(
            "{}{}",
            self.camelot_number(),
            if self.minor { "A" } else { "B" }
        )
    }

    /// Transposed by `semitones`.
    pub fn shifted(self, semitones: i32) -> Key {
        Key {
            tonic: (self.tonic as i32 + semitones).rem_euclid(12) as u8,
            minor: self.minor,
        }
    }

    /// Harmonic compatibility on the Camelot wheel: same key, ±1 on the
    /// same ring, or the relative major/minor.
    pub fn compatible(self, other: Key) -> bool {
        let (a, b) = (self.camelot_number() as i32, other.camelot_number() as i32);
        let step = (a - b).rem_euclid(12);
        if self.minor == other.minor {
            step == 0 || step == 1 || step == 11
        } else {
            step == 0
        }
    }
}

fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let (ma, mb) = (a.iter().sum::<f64>() / 12.0, b.iter().sum::<f64>() / 12.0);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// The chromagram (12 bins, C first) of a mono signal.
pub fn chroma(mono: &[f32], rate: u32) -> [f64; 12] {
    const WIN: usize = 8192;
    let bin_hz = rate as f64 / WIN as f64;
    let (lo, hi) = ((65.4 / bin_hz) as usize, (2093.0 / bin_hz) as usize);
    // Pitch class of each bin (A4 = 440 Hz).
    let pc: Vec<usize> = (0..=WIN / 2)
        .map(|k| {
            let f = (k as f64 * bin_hz).max(1.0);
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            (midi.round() as i64).rem_euclid(12) as usize
        })
        .collect();
    let mut total = [0.0f64; 12];
    stft(mono, WIN, WIN / 2, |_, mag| {
        let mut frame = [0.0f64; 12];
        for k in lo.max(1)..hi.min(mag.len() - 1) {
            // Spectral peaks only: the partials of notes, not noise.
            if mag[k] > mag[k - 1] && mag[k] >= mag[k + 1] {
                frame[pc[k]] += mag[k].sqrt();
            }
        }
        let sum: f64 = frame.iter().sum();
        if sum > 1e-9 {
            for i in 0..12 {
                total[i] += frame[i] / sum;
            }
        }
    });
    total
}

/// The best-fitting key and its raw profile margin over the runner-up.
/// This score is not a calibrated probability.
///
/// Energy chroma above the kick (C3–C9) against Sha'ath's profiles
/// (Pearson) gives the tonic; major or minor is then decided from the
/// degrees that tell them apart (minor − major third, sixth, seventh), a
/// logistic fit to 612 professionally tagged tracks of a DJ library. Right
/// for 56.7 % of them, two-fold cross-validated (DJ-compatible ≈ 68 %), where
/// the earlier peak chroma × Krumhansl–Kessler was 39.4 % (55.2 %).
/// `patch-bench --key-eval` repeats the comparison.
pub fn detect(mono: &[f32], rate: u32) -> Option<(Key, f32)> {
    let c = chroma_energy(mono, rate, 130.8, 8372.0, 0.0, false);
    let sum: f64 = c.iter().sum();
    if sum <= 0.0 {
        return None;
    }
    let scored = best_key(&c, &profiles::SHAATH, false);
    let (best, next) = (scored[0].0, scored[1].0);
    let margin = if best > 0.0 {
        ((best - next) / best * 4.0).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let tonic = scored[0].1.tonic;
    let at = |i: usize| c[(i + tonic as usize) % 12] / sum * 12.0;
    let z = -0.6392 + 11.6574 * (at(3) - at(4)) + 3.1251 * (at(8) - at(9)) + 4.3208 * (at(10) - at(11));
    Some((Key { tonic, minor: z > 0.0 }, margin))
}

/// Compute global key and local chroma from one STFT pass. The local profile
/// is descriptive evidence; it does not claim chord labels.
pub fn detect_with_harmony(mono: &[f32], rate: u32) -> (Option<(Key, f32)>, HarmonicProfile) {
    const WIN: usize = 8192;
    let rate = rate.max(1);
    let bin_hz = rate as f64 / WIN as f64;
    let (lo, hi) = ((65.4 / bin_hz) as usize, (2093.0 / bin_hz) as usize);
    let pc: Vec<usize> = (0..=WIN / 2)
        .map(|k| {
            let f = (k as f64 * bin_hz).max(1.0);
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            (midi.round() as i64).rem_euclid(12) as usize
        })
        .collect();
    let hop = WIN / 2;
    let duration = mono.len() as f64 / f64::from(rate);
    let mut regions: Vec<([f64; 12], usize, usize)> = Vec::new();
    stft(mono, WIN, hop, |index, mag| {
        let start_s = index as f64 * hop as f64 / f64::from(rate);
        let region_index = (start_s / crate::harmony::WINDOW_SECONDS).floor() as usize;
        if regions.len() <= region_index {
            regions.resize(region_index + 1, ([0.0; 12], 0, 0));
        }
        let (local, active, count) = &mut regions[region_index];
        *count += 1;
        let mut frame = [0.0f64; 12];
        for k in lo.max(1)..hi.min(mag.len() - 1) {
            if mag[k] > mag[k - 1] && mag[k] >= mag[k + 1] {
                frame[pc[k]] += mag[k].sqrt();
            }
        }
        let sum = frame.iter().sum::<f64>();
        if sum > 1e-9 {
            *active += 1;
            for bin in 0..12 {
                let value = frame[bin] / sum;
                local[bin] += value;
            }
        }
    });
    let profile = HarmonicProfile::from_regions(duration, regions);
    // The global key from its own (energy) chroma; the regions keep the
    // peak chroma above as local evidence.
    (detect(mono, rate), profile)
}

/// Key profiles, for evaluation and choice.
pub mod profiles {
    /// Krumhansl & Kessler (1982), probe-tone ratings.
    pub const KK: ([f64; 12], [f64; 12]) = (super::MAJOR, super::MINOR);
    /// Temperley (2001, Kostka–Payne corpus).
    pub const TEMPERLEY: ([f64; 12], [f64; 12]) = (
        [5.0, 2.0, 3.5, 2.0, 4.5, 4.0, 2.0, 4.5, 2.0, 3.5, 1.5, 4.0],
        [5.0, 2.0, 3.5, 4.5, 2.0, 4.0, 2.0, 4.5, 3.5, 2.0, 1.5, 4.0],
    );
    /// Sha'ath (libKeyFinder), tuned on popular and electronic music.
    pub const SHAATH: ([f64; 12], [f64; 12]) = (
        [
            7.2390, 3.5035, 3.5845, 2.8451, 5.8190, 4.5587, 2.4478, 6.9947, 3.3911, 4.5561, 4.0739,
            4.4593,
        ],
        [
            7.0026, 3.1436, 4.3590, 5.4042, 3.6723, 4.0897, 3.9079, 6.1996, 3.6301, 2.8736, 5.3504,
            3.8362,
        ],
    );
}

/// Energy chroma on a log-frequency axis: each spectral bin's energy is
/// shared between its two nearest semitones (triangular), from `lo_hz` to
/// `hi_hz`; frames dominated by noise (drum hits: high spectral flatness)
/// are skipped. `tuning` shifts the semitone grid (fraction of a semitone).
pub fn chroma_energy(
    mono: &[f32],
    rate: u32,
    lo_hz: f64,
    hi_hz: f64,
    tuning: f64,
    skip_noisy: bool,
) -> [f64; 12] {
    const WIN: usize = 8192;
    let bin_hz = rate as f64 / WIN as f64;
    let (lo, hi) = (
        (lo_hz / bin_hz) as usize,
        ((hi_hz / bin_hz) as usize).min(WIN / 2),
    );
    let pos: Vec<f64> = (0..=WIN / 2)
        .map(|k| 69.0 + 12.0 * ((k as f64 * bin_hz).max(1.0) / 440.0).log2() - tuning)
        .collect();
    let mut total = [0.0f64; 12];
    stft(mono, WIN, WIN / 2, |_, mag| {
        if skip_noisy {
            // Spectral flatness over the band: geometric / arithmetic mean.
            let band = &mag[lo.max(1)..hi.min(mag.len())];
            let n = band.len().max(1) as f64;
            let am = band.iter().sum::<f64>() / n;
            let gm = (band.iter().map(|v| (v + 1e-12).ln()).sum::<f64>() / n).exp();
            if am <= 0.0 || gm / am > 0.35 {
                return;
            }
        }
        for k in lo.max(1)..hi.min(mag.len()) {
            let e = mag[k] * mag[k];
            let m = pos[k];
            let (f, frac) = (m.floor(), m - m.floor());
            total[(f as i64).rem_euclid(12) as usize] += e * (1.0 - frac);
            total[((f as i64) + 1).rem_euclid(12) as usize] += e * frac;
        }
    });
    // Compress: a few loud notes shouldn't outweigh the harmony.
    for v in &mut total {
        *v = v.sqrt();
    }
    total
}

/// Tuning (fraction of a semitone, −0.5 … 0.5) from where spectral peaks
/// sit relative to equal temperament at A4 = 440 Hz.
pub fn tuning(mono: &[f32], rate: u32) -> f64 {
    const WIN: usize = 8192;
    let bin_hz = rate as f64 / WIN as f64;
    let (lo, hi) = ((100.0 / bin_hz) as usize, (2000.0 / bin_hz) as usize);
    let (mut s, mut c) = (0.0f64, 0.0f64);
    stft(mono, WIN, WIN * 2, |_, mag| {
        for k in lo.max(2)..hi.min(mag.len() - 1) {
            if mag[k] > mag[k - 1] && mag[k] >= mag[k + 1] && mag[k] > 1e-3 {
                // Parabolic peak position, then its deviation (circular mean).
                let d = 0.5 * (mag[k - 1] - mag[k + 1]) / (mag[k - 1] - 2.0 * mag[k] + mag[k + 1]);
                let f = (k as f64 + d) * bin_hz;
                let dev = 12.0 * (f / 440.0).log2();
                let ang = std::f64::consts::TAU * dev;
                s += mag[k] * ang.sin();
                c += mag[k] * ang.cos();
            }
        }
    });
    s.atan2(c) / std::f64::consts::TAU
}

/// Best key for a chroma against a profile pair, by Pearson or cosine.
pub fn best_key(c: &[f64; 12], prof: &([f64; 12], [f64; 12]), cosine: bool) -> Vec<(f64, Key)> {
    let sim = |a: &[f64; 12], b: &[f64; 12]| {
        if cosine {
            let (mut n, mut da, mut db) = (0.0, 0.0, 0.0);
            for i in 0..12 {
                n += a[i] * b[i];
                da += a[i] * a[i];
                db += b[i] * b[i];
            }
            if da > 0.0 && db > 0.0 {
                n / (da * db).sqrt()
            } else {
                0.0
            }
        } else {
            pearson(a, b)
        }
    };
    let mut scored = vec![];
    for tonic in 0..12u8 {
        for (minor, profile) in [(false, &prof.0), (true, &prof.1)] {
            let mut rotated = [0.0; 12];
            for i in 0..12 {
                rotated[(i + tonic as usize) % 12] = profile[i];
            }
            scored.push((sim(c, &rotated), Key { tonic, minor }));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
}

/// Musical name → key: "Gb Minor", "F#m", "A♭ major", "Ebm", "C".
pub fn from_name(s: &str) -> Option<Key> {
    let t = s.trim().replace('♯', "#").replace('♭', "b");
    let lower = t.to_lowercase();
    let minor = lower.ends_with("minor")
        || lower.ends_with("min")
        || (lower.ends_with('m') && !lower.ends_with("maj"));
    let root: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '#')
        .collect();
    let mut chars = root.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let acc: i32 = match chars.next() {
        Some('#') => 1,
        // Right after the letter, "b" is always a flat ("Ebm", "Bb major").
        Some('b') => -1,
        _ => 0,
    };
    Some(Key {
        tonic: (base + acc).rem_euclid(12) as u8,
        minor,
    })
}

/// Camelot ("8A") → key.
pub fn from_camelot(s: &str) -> Option<Key> {
    let (n, l) = s.trim().split_at(s.trim().len().checked_sub(1)?);
    let n: u8 = n.parse().ok()?;
    let minor = match l {
        "A" | "a" => true,
        "B" | "b" => false,
        _ => return None,
    };
    (0..12u8)
        .map(|t| Key { tonic: t, minor })
        .find(|k| k.camelot_number() == n)
}

#[cfg(test)]
mod eval {
    use super::*;

    /// Key accuracy against labelled tracks: EVAL_TSV = "path<TAB>camelot"
    /// per line. `cargo test --release -p patch-analysis key_eval --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn key_eval() {
        let Ok(tsv) = std::env::var("EVAL_TSV") else {
            return;
        };
        let rows: Vec<(String, Key)> = std::fs::read_to_string(tsv)
            .unwrap()
            .lines()
            .filter_map(|l| {
                let (p, k) = l.split_once('\t')?;
                Some((p.to_string(), from_camelot(k)?))
            })
            .collect();
        let half = crate::RATE / 2;
        let variants: Vec<(&str, Box<dyn Fn(&[f32]) -> [f64; 12] + Sync>)> = vec![
            (
                "peaks 65-2093 (current)",
                Box::new(move |m: &[f32]| chroma(m, half)),
            ),
            (
                "energy 50-2000",
                Box::new(move |m: &[f32]| chroma_energy(m, half, 50.0, 2000.0, 0.0, false)),
            ),
            (
                "energy 50-2000 noisy-skip",
                Box::new(move |m: &[f32]| chroma_energy(m, half, 50.0, 2000.0, 0.0, true)),
            ),
            (
                "energy 50-5000 noisy-skip",
                Box::new(move |m: &[f32]| chroma_energy(m, half, 50.0, 5000.0, 0.0, true)),
            ),
            (
                "energy 50-2000 noisy-skip tuned",
                Box::new(move |m: &[f32]| {
                    chroma_energy(m, half, 50.0, 2000.0, tuning(m, half), true)
                }),
            ),
            (
                "energy 100-2000 noisy-skip tuned",
                Box::new(move |m: &[f32]| {
                    chroma_energy(m, half, 100.0, 2000.0, tuning(m, half), true)
                }),
            ),
        ];
        let profs = [
            ("KK", profiles::KK),
            ("Temperley", profiles::TEMPERLEY),
            ("Shaath", profiles::SHAATH),
        ];
        // chromas[track][variant]
        let chromas: Vec<Vec<[f64; 12]>> = std::thread::scope(|sc| {
            let hs: Vec<_> = rows
                .chunks(rows.len().div_ceil(10).max(1))
                .map(|chunk| {
                    let variants = &variants;
                    sc.spawn(move || {
                        chunk
                            .iter()
                            .map(|(p, _)| {
                                let st = crate::decode(std::path::Path::new(p)).unwrap();
                                let mono: Vec<f32> = st
                                    .chunks_exact(4)
                                    .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
                                    .collect();
                                variants.iter().map(|(_, f)| f(&mono)).collect::<Vec<_>>()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        for (vi, (vname, _)) in variants.iter().enumerate() {
            for (pname, prof) in &profs {
                for cosine in [false, true] {
                    let (mut exact, mut near) = (0, 0);
                    for (t, (_, truth)) in rows.iter().enumerate() {
                        let k = best_key(&chromas[t][vi], prof, cosine)[0].1;
                        if k == *truth {
                            exact += 1
                        } else if k.compatible(*truth) {
                            near += 1
                        }
                    }
                    println!(
                        "{vname:36} {pname:9} {:7}  exact {exact:3}  compatible {near:3}  of {}",
                        if cosine { "cosine" } else { "pearson" },
                        rows.len()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(midi: f64, secs: f64, rate: u32) -> Vec<f32> {
        let f = 440.0 * 2f64.powf((midi - 69.0) / 12.0);
        (0..(secs * rate as f64) as usize)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (1..=5)
                    .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64)
                    .sum::<f64>() as f32
                    * 0.1
            })
            .collect()
    }

    fn chord(notes: &[f64], secs: f64, rate: u32) -> Vec<f32> {
        let parts: Vec<Vec<f32>> = notes.iter().map(|m| note(*m, secs, rate)).collect();
        (0..parts[0].len())
            .map(|i| parts.iter().map(|p| p[i]).sum())
            .collect()
    }

    fn progression(chords: &[&[f64]], rate: u32) -> Vec<f32> {
        let mut x = vec![];
        for _ in 0..3 {
            for c in chords {
                x.extend(chord(c, 1.5, rate));
            }
        }
        x
    }

    #[test]
    fn c_major_cadence() {
        // C – F – G – C
        let x = progression(
            &[
                &[48.0, 64.0, 67.0, 72.0],
                &[53.0, 65.0, 69.0, 72.0],
                &[55.0, 62.0, 67.0, 71.0],
                &[48.0, 64.0, 67.0, 72.0],
            ],
            22_050,
        );
        let (k, _) = detect(&x, 22_050).unwrap();
        assert_eq!(
            k,
            Key {
                tonic: 0,
                minor: false
            },
            "{}",
            k.name()
        );
    }

    #[test]
    fn local_harmonic_profile_retains_a_change_hidden_by_global_key() {
        let mut audio = note(60.0, 8.0, 22_050);
        audio.extend(note(61.0, 8.0, 22_050));
        let (global_key, profile) = detect_with_harmony(&audio, 22_050);
        assert!(global_key.is_some());
        assert_eq!(profile.frames.len(), 2);
        assert!(profile.frames[0].chroma[0] > profile.frames[0].chroma[1]);
        assert!(profile.frames[1].chroma[1] > profile.frames[1].chroma[0]);
        let first = profile.average(0.0, 8.0).unwrap();
        let second = profile.average(8.0, 16.0).unwrap();
        assert!(first.1 > 0.9 && second.1 > 0.9);
        assert!(first.0[0] > second.0[0]);
    }

    #[test]
    fn a_minor_cadence() {
        // Am – Dm – E – Am
        let x = progression(
            &[
                &[45.0, 60.0, 64.0, 69.0],
                &[50.0, 62.0, 65.0, 69.0],
                &[52.0, 59.0, 64.0, 68.0],
                &[45.0, 60.0, 64.0, 69.0],
            ],
            22_050,
        );
        let (k, _) = detect(&x, 22_050).unwrap();
        assert_eq!(
            k,
            Key {
                tonic: 9,
                minor: true
            },
            "{}",
            k.name()
        );
    }

    #[test]
    fn names_and_camelot_parse() {
        assert_eq!(
            from_name("Gb Minor"),
            Some(Key {
                tonic: 6,
                minor: true
            })
        );
        assert_eq!(
            from_name("F#m"),
            Some(Key {
                tonic: 6,
                minor: true
            })
        );
        assert_eq!(
            from_name("Ebm"),
            Some(Key {
                tonic: 3,
                minor: true
            })
        );
        assert_eq!(
            from_name("A♭ major"),
            Some(Key {
                tonic: 8,
                minor: false
            })
        );
        assert_eq!(
            from_name("C"),
            Some(Key {
                tonic: 0,
                minor: false
            })
        );
        assert_eq!(
            from_name("Bb"),
            Some(Key {
                tonic: 10,
                minor: false
            })
        );
        assert_eq!(from_camelot("11A").map(|k| k.camelot()), Some("11A".into()));
        assert_eq!(
            from_camelot("11A"),
            from_name("Gb Minor"),
            "Beatport's two tags agree"
        );
    }

    #[test]
    fn camelot_wheel() {
        let k = |tonic, minor| Key { tonic, minor };
        assert_eq!(k(0, false).camelot(), "8B");
        assert_eq!(k(9, true).camelot(), "8A");
        assert_eq!(k(7, false).camelot(), "9B");
        assert_eq!(k(11, false).camelot(), "1B");
        assert_eq!(k(8, true).camelot(), "1A"); // G♯m
        assert_eq!(k(5, false).camelot(), "7B");
        assert_eq!(k(0, true).camelot(), "5A");
        assert!(k(0, false).compatible(k(7, false)), "8B–9B");
        assert!(k(0, false).compatible(k(9, true)), "relative minor");
        assert!(!k(0, false).compatible(k(2, false)), "8B–10B");
        assert_eq!(k(9, true).shifted(3), k(0, true));
    }
}
