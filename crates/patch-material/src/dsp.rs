//! Rendering and measurement: pure Rust on interleaved stereo `f32`.
//!
//! A render takes a decoded source, cuts the resolved region, and applies the
//! recipe's transforms in order. Nothing here allocates on a real-time thread,
//! decodes, or touches an engine: it is offline work on a background worker.
//! Every transform is deterministic, so the same recipe renders bit-identically.

use crate::model::{FilterKind, Recipe, Transform};
use crate::traits::Stretch;

/// Frames in an interleaved stereo buffer.
pub fn frames(audio: &[f32]) -> usize {
    audio.len() / 2
}

/// The shortest fade an edge gets: shorter than this clicks.
pub const MIN_FADE_MS: f64 = 2.0;

fn clamped_fade_frames(rate: u32, ms: f64, len: usize) -> usize {
    let n = (ms.max(MIN_FADE_MS) * rate as f64 / 1000.0).round() as usize;
    n.min(len / 2).max(1)
}

/// The region's frames `[from, to)` as a new buffer (clamped to the source).
pub fn cut(audio: &[f32], from: usize, to: usize) -> Vec<f32> {
    let f = frames(audio);
    let a = from.min(f);
    let b = to.min(f).max(a);
    audio[a * 2..b * 2].to_vec()
}

/// A raised-cosine fade in at the buffer's start.
pub fn fade_in(audio: &mut [f32], rate: u32, ms: f64) {
    let n = clamped_fade_frames(rate, ms, frames(audio));
    let len = frames(audio);
    for i in 0..n {
        let w = raised_cosine(i as f64 / n as f64);
        for c in 0..2 {
            audio[i * 2 + c] *= w as f32;
        }
    }
    let _ = len;
}

/// A raised-cosine fade out at the buffer's end.
pub fn fade_out(audio: &mut [f32], rate: u32, ms: f64) {
    let n = clamped_fade_frames(rate, ms, frames(audio));
    let len = frames(audio);
    for i in 0..n {
        let w = raised_cosine(i as f64 / n as f64);
        let at = len - 1 - i;
        for c in 0..2 {
            audio[at * 2 + c] *= w as f32;
        }
    }
}

/// 0 → 0, 1 → 1, smooth (raised cosine / Hann-ish half).
fn raised_cosine(x: f64) -> f64 {
    0.5 - 0.5 * (std::f64::consts::PI * x.clamp(0.0, 1.0)).cos()
}

pub fn reverse(audio: &mut [f32]) {
    let f = frames(audio);
    for i in 0..f / 2 {
        for c in 0..2 {
            audio.swap(i * 2 + c, (f - 1 - i) * 2 + c);
        }
    }
}

pub fn gain_db(audio: &mut [f32], db: f64) {
    let g = 10f64.powf(db / 20.0) as f32;
    for s in audio.iter_mut() {
        *s *= g;
    }
}

pub fn peak(audio: &[f32]) -> f32 {
    audio.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

pub fn rms(audio: &[f32]) -> f32 {
    if audio.is_empty() {
        return 0.0;
    }
    (audio.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / audio.len() as f64).sqrt()
        as f32
}

/// Scale so the buffer's peak is exactly `peak_db`.
pub fn normalize(audio: &mut [f32], peak_db: f64) {
    let p = peak(audio);
    if p <= 1e-9 {
        return;
    }
    let target = 10f64.powf(peak_db / 20.0) as f32;
    gain_db(audio, 20.0 * (target / p).log10() as f64);
}

/// Fold the tail into the head with an equal-power crossfade, so the buffer
/// loops without a step. The result is `crossfade` frames shorter.
pub fn loop_seam(audio: &mut Vec<f32>, rate: u32, crossfade_ms: f64) {
    let f = frames(audio);
    let c = ((crossfade_ms.max(0.0) * rate as f64 / 1000.0).round() as usize).min(f / 2);
    if c == 0 {
        return;
    }
    let out_frames = f - c;
    let mut out = vec![0.0f32; out_frames * 2];
    // [0, c): blend the tail's beginning into the head, equal-power.
    for i in 0..c {
        let t = i as f64 / c as f64;
        let (fi, fo) = (
            (std::f64::consts::FRAC_PI_2 * t).sin() as f32,
            (std::f64::consts::FRAC_PI_2 * t).cos() as f32,
        );
        for ch in 0..2 {
            let head = audio[i * 2 + ch];
            let tail = audio[(f - c + i) * 2 + ch];
            out[i * 2 + ch] = head * fi + tail * fo;
        }
    }
    // [c, out_frames): the untouched middle.
    out[c * 2..out_frames * 2].copy_from_slice(&audio[c * 2..out_frames * 2]);
    *audio = out;
}

/// A one-pole low-pass / high-pass. `y += a·(x − y)`.
pub fn filter(audio: &mut [f32], rate: u32, kind: FilterKind, hz: f64) {
    let fc = hz.clamp(10.0, rate as f64 / 2.0 - 1.0);
    let a = (1.0 - (-std::f64::consts::TAU * fc / rate as f64).exp()) as f32;
    for ch in 0..2 {
        let mut y = 0.0f32;
        let mut i = ch;
        while i < audio.len() {
            y += a * (audio[i] - y);
            audio[i] = match kind {
                FilterKind::LowPass => y,
                FilterKind::HighPass => audio[i] - y,
            };
            i += 2;
        }
    }
}

/// An ADSR envelope shaped across the whole buffer (times in seconds, `s` a
/// level 0…1).
pub fn envelope(audio: &mut [f32], rate: u32, a: f64, d: f64, s: f64, r: f64) {
    let f = frames(audio);
    if f == 0 {
        return;
    }
    let ar = (a.max(0.0) * rate as f64) as usize;
    let dr = (d.max(0.0) * rate as f64) as usize;
    let rr = (r.max(0.0) * rate as f64) as usize;
    let s_level = s.clamp(0.0, 1.0);
    for i in 0..f {
        let g = if i < ar && ar > 0 {
            raised_cosine(i as f64 / ar as f64)
        } else if i < ar + dr && dr > 0 {
            let t = (i - ar) as f64 / dr as f64;
            1.0 + (s_level - 1.0) * t
        } else if i >= f.saturating_sub(rr) && rr > 0 {
            let t = (f - 1 - i) as f64 / rr as f64;
            s_level * raised_cosine(t)
        } else {
            s_level
        };
        for c in 0..2 {
            audio[i * 2 + c] *= g as f32;
        }
    }
}

/// Resample by semitones: both speed and pitch change (`2^(n/12)`), linearly
/// interpolated.
pub fn repitch(audio: &[f32], semitones: f64) -> Vec<f32> {
    let ratio = 2f64.powf(semitones / 12.0);
    if (ratio - 1.0).abs() < 1e-9 || audio.len() < 4 {
        return audio.to_vec();
    }
    let f = frames(audio);
    let out_f = ((f as f64) / ratio).floor().max(1.0) as usize;
    let mut out = vec![0.0f32; out_f * 2];
    for i in 0..out_f {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let frac = (pos - i0 as f64) as f32;
        let i1 = (i0 + 1).min(f - 1);
        for c in 0..2 {
            let s0 = audio[i0 * 2 + c];
            let s1 = audio[i1 * 2 + c];
            out[i * 2 + c] = s0 + (s1 - s0) * frac;
        }
    }
    out
}

/// Hold a short slice, looping it to fill `beats` at `bpm` — a granular
/// freeze that never steps.
pub fn freeze(audio: &[f32], rate: u32, beats: f64, bpm: f64) -> Vec<f32> {
    let target = ((beats.max(0.0) * 60.0 / bpm.max(1.0)) * rate as f64) as usize;
    let f = frames(audio);
    if f == 0 || target == 0 {
        return Vec::new();
    }
    let grain = ((0.05 * rate as f64) as usize).min(f).max(1);
    let mut out = vec![0.0f32; target * 2];
    for i in 0..target {
        let src = i % grain;
        for c in 0..2 {
            out[i * 2 + c] = audio[src * 2 + c];
        }
    }
    // Crossfade the joins so the grain repeats do not click.
    let n = clamped_fade_frames(rate, MIN_FADE_MS, target);
    for j in 0..target {
        if j % grain < n {
            let w = raised_cosine((j % grain) as f64 / n as f64);
            for c in 0..2 {
                out[j * 2 + c] *= w as f32;
            }
        }
    }
    out
}

/// Repeat the first `division` beats `repeats` times, at `bpm`.
pub fn stutter(audio: &[f32], rate: u32, division: f64, repeats: u32, bpm: f64) -> Vec<f32> {
    let slice_f = ((division.max(0.0) * 60.0 / bpm.max(1.0)) * rate as f64) as usize;
    let f = frames(audio);
    let slice_f = slice_f.min(f);
    if slice_f == 0 || repeats == 0 {
        return audio.to_vec();
    }
    let mut out = Vec::with_capacity(slice_f * repeats as usize * 2);
    for _ in 0..repeats {
        out.extend_from_slice(&audio[..slice_f * 2]);
    }
    out
}

/// Onset offsets (seconds) from an energy-flux detector.
pub fn transients(audio: &[f32], rate: u32) -> Vec<f64> {
    let f = frames(audio);
    if f < 1024 {
        return Vec::new();
    }
    let win = 512usize;
    let hop = 256usize;
    let mono: Vec<f32> = (0..f)
        .map(|i| (audio[i * 2] + audio[i * 2 + 1]) * 0.5)
        .collect();
    let mut flux = Vec::new();
    let mut prev = 0.0f32;
    let mut start = 0;
    while start + win <= f {
        let energy: f32 = mono[start..start + win].iter().map(|s| s * s).sum::<f32>() / win as f32;
        flux.push((energy - prev).max(0.0));
        prev = energy;
        start += hop;
    }
    if flux.is_empty() {
        return Vec::new();
    }
    let mean = flux.iter().map(|x| *x as f64).sum::<f64>() / flux.len() as f64;
    let threshold = (mean * 1.5).max(1e-6);
    let mut out = Vec::new();
    for i in 1..flux.len() - 1 {
        let v = flux[i] as f64;
        if v > threshold && v >= flux[i - 1] as f64 && v >= flux[i + 1] as f64 {
            out.push((i * hop) as f64 / rate as f64);
        }
    }
    out
}

/// How well the buffer loops: 1 when the seam is a step no larger than the
/// signal itself, 0 when it jumps by the full peak.
pub fn loop_suitability(audio: &[f32]) -> f32 {
    let f = frames(audio);
    if f < 2 {
        return 0.0;
    }
    let step = (0..2)
        .map(|c| (audio[c] - audio[(f - 1) * 2 + c]).abs())
        .fold(0.0f32, f32::max);
    let p = peak(audio).max(1e-6);
    (1.0 - step / p).clamp(0.0, 1.0)
}

/// The nearest transient to `seconds` within `max_ms` (else `seconds`).
pub fn snap_seconds(audio: &[f32], rate: u32, seconds: f64, max_ms: f64) -> f64 {
    let tol = max_ms.max(0.0) / 1000.0;
    let mut best = seconds;
    let mut best_d = f64::INFINITY;
    for t in transients(audio, rate) {
        let d = (t - seconds).abs();
        if d <= tol && d < best_d {
            best_d = d;
            best = t;
        }
    }
    best
}

/// Render a recipe: decode the source, cut the resolved region, then apply the
/// transforms in order. `stretch` is required for `TimeStretch`/`PitchShift`;
/// without it those transforms are refused.
pub fn render(
    recipe: &Recipe,
    source: &dyn crate::traits::SourceAudio,
    stretch: Option<&dyn Stretch>,
) -> Result<Vec<f32>, String> {
    let rate = recipe.region.rate;
    let decoded = source.decode(&recipe.source, rate)?;
    if frames(&decoded) < recipe.region.end_frame as usize {
        return Err(format!(
            "source has {} frames, region needs {}",
            frames(&decoded),
            recipe.region.end_frame
        ));
    }
    let mut audio = cut(
        &decoded,
        recipe.region.start_frame as usize,
        recipe.region.end_frame as usize,
    );
    let bpm = recipe.region.source_bpm.unwrap_or(120.0);
    for t in &recipe.transforms {
        match t {
            Transform::Trim { seconds } => {
                let n = (*seconds * rate as f64).max(0.0) as usize;
                audio.truncate(n.min(frames(&audio)) * 2);
            }
            Transform::SnapTransient { max_ms } => {
                let snapped = snap_seconds(&audio, rate, 0.0, *max_ms);
                let drop = (snapped * rate as f64).round() as usize;
                if drop < frames(&audio) {
                    audio = audio[drop * 2..].to_vec();
                }
            }
            Transform::Fade { in_ms, out_ms } => {
                fade_in(&mut audio, rate, *in_ms);
                fade_out(&mut audio, rate, *out_ms);
            }
            Transform::Reverse => reverse(&mut audio),
            Transform::Gain { db } => gain_db(&mut audio, *db),
            Transform::Normalize { peak_db } => normalize(&mut audio, *peak_db),
            Transform::LoopSeam { crossfade_ms } => loop_seam(&mut audio, rate, *crossfade_ms),
            Transform::Filter { kind, hz } => filter(&mut audio, rate, *kind, *hz),
            Transform::Envelope { a, d, s, r } => envelope(&mut audio, rate, *a, *d, *s, *r),
            Transform::Repitch { semitones } => audio = repitch(&audio, *semitones),
            Transform::TimeStretch { to_bpm } => {
                let stretch = stretch.ok_or("time-stretch needs the engine's Rubber Band")?;
                if *to_bpm <= 0.0 {
                    return Err("time-stretch needs a positive BPM".into());
                }
                let ratio = *to_bpm / bpm;
                audio = stretch.time_stretch(&audio, rate, ratio)?;
            }
            Transform::PitchShift { semitones } => {
                let stretch = stretch.ok_or("pitch-shift needs the engine's Rubber Band")?;
                audio = stretch.pitch_shift(&audio, rate, *semitones)?;
            }
            Transform::Freeze { beats } => audio = freeze(&audio, rate, *beats, bpm),
            Transform::Stutter { division, repeats } => {
                audio = stutter(&audio, rate, *division, *repeats, bpm)
            }
            // Slices do not change the audio (the material keeps its order);
            // the sampler reads `n` as the number of chop points.
            Transform::Slices { n } => {
                if *n == 0 {
                    return Err("slices needs n ≥ 1".into());
                }
            }
        }
    }
    // Every edge is faded, so a cut never clicks.
    fade_in(&mut audio, rate, MIN_FADE_MS);
    fade_out(&mut audio, rate, MIN_FADE_MS);
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, seconds: f64, hz: f64) -> Vec<f32> {
        let n = (seconds * rate as f64) as usize;
        (0..n)
            .flat_map(|i| {
                let s = (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.5;
                [s, s]
            })
            .collect()
    }

    /// The largest step between consecutive samples (a click is a big step).
    fn max_step(a: &[f32]) -> f32 {
        a.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()))
    }

    #[test]
    fn a_cut_has_no_click_at_its_edges() {
        let rate = 48_000;
        let src = tone(rate, 1.0, 440.0);
        let mut cut = cut(&src, 100, 48_100);
        fade_in(&mut cut, rate, MIN_FADE_MS);
        fade_out(&mut cut, rate, MIN_FADE_MS);
        assert!(cut[0].abs() < 1e-6 && *cut.last().unwrap() < 1e-6);
        assert!(
            max_step(&cut) < 0.2,
            "edge is click-free: {}",
            max_step(&cut)
        );
    }

    #[test]
    fn loop_seam_makes_a_buffer_loop_smoothly() {
        let rate = 48_000;
        let mut src = tone(rate, 0.5, 330.0);
        loop_seam(&mut src, rate, 10.0);
        assert_eq!(
            frames(&src),
            (0.5 * rate as f64) as usize - (0.01 * rate as f64) as usize
        );
        assert!(loop_suitability(&src) > 0.9, "{}", loop_suitability(&src));
    }

    #[test]
    fn normalize_hits_the_peak() {
        let mut a = tone(48_000, 0.1, 440.0);
        normalize(&mut a, -6.0);
        let want = 10f32.powf(-6.0 / 20.0);
        assert!((peak(&a) - want).abs() < 1e-3, "{}", peak(&a));
    }

    #[test]
    fn repitch_by_an_octave_halves_the_length() {
        let a = tone(48_000, 0.2, 440.0);
        let up = repitch(&a, 12.0);
        assert!((frames(&up) as f64 - frames(&a) as f64 / 2.0).abs() <= 1.0);
    }

    #[test]
    fn transients_finds_onsets_on_a_gated_tone() {
        let rate = 48_000;
        let mut a = vec![0.0f32; 0];
        for _ in 0..4 {
            a.extend(tone(rate, 0.1, 440.0));
            a.extend(vec![0.0f32; rate as usize / 10 * 2]);
        }
        let t = transients(&a, rate);
        assert!(t.len() >= 3, "{t:?}");
    }

    #[test]
    fn stutter_repeats_the_slice() {
        let rate = 48_000;
        let a = tone(rate, 1.0, 440.0);
        let out = stutter(&a, rate, 1.0, 2, 60.0); // 1 beat at 60 BPM = 1 s
        assert_eq!(frames(&out), rate as usize * 2);
    }

    struct Source;
    impl crate::traits::SourceAudio for Source {
        fn decode(&self, _s: &crate::model::SourceRef, rate: u32) -> Result<Vec<f32>, String> {
            Ok(tone(rate, 1.0, 220.0))
        }
    }

    fn recipe() -> crate::model::Recipe {
        use crate::model::{Kind, Resolved, SourceRef, VERSION};
        crate::model::Recipe {
            version: VERSION,
            source: SourceRef {
                track_id: "t".into(),
                path: "/x".into(),
                fingerprint: "0".repeat(64),
                stem: None,
                stem_set: None,
            },
            region: Resolved {
                rate: 48_000,
                start_frame: 1000,
                end_frame: 48_000,
                first_beat: 0.02,
                beats: 4.0,
                bar: 0,
                phrase: Some(0),
                source_bpm: Some(120.0),
                source_key: None,
                confidence: 0.9,
                why: "x".into(),
            },
            kind: Kind::BarLoop,
            transforms: vec![
                Transform::Fade {
                    in_ms: 3.0,
                    out_ms: 3.0,
                },
                Transform::Gain { db: -3.0 },
            ],
            analysis_version: 7,
        }
    }

    #[test]
    fn the_same_recipe_renders_bit_identically() {
        let r = recipe();
        let a = render(&r, &Source, None).unwrap();
        let b = render(&r, &Source, None).unwrap();
        assert_eq!(a, b);
        assert!(frames(&a) >= 47_000);
        assert!(loop_suitability(&a) > 0.9);
    }

    #[test]
    fn a_stretch_transform_is_refused_without_the_engine() {
        let mut r = recipe();
        r.transforms.push(Transform::TimeStretch { to_bpm: 124.0 });
        let e = render(&r, &Source, None).unwrap_err();
        assert!(e.contains("Rubber Band"), "{e}");
    }
}
