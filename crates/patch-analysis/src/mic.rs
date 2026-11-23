//! Deterministic, offline voice-sample measurements. These are signal
//! indicators, not a diagnosis of voice quality or guaranteed speech detection.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calibration {
    pub version: u32,
    pub sample_rate: u32,
    pub duration_s: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    /// 10th percentile of 20 ms window RMS. Valid as a noise estimate only
    /// when the take contains a quiet section.
    pub quiet_window_dbfs: f64,
    pub window_range_db: f64,
    pub headroom_db: f64,
    pub clipped_samples: u64,
    pub active_fraction: f64,
    /// Energy fractions below 150 Hz, 150–4500 Hz, and above 4500 Hz.
    pub band_energy: [f64; 3],
    pub low_frequency_burst_fraction: f64,
    pub high_frequency_activity_fraction: f64,
    pub warnings: Vec<String>,
    /// Suggestions require explicit user application. Never hardware gain.
    pub recommended_hpf_hz: f32,
    pub recommended_gate_db: Option<f32>,
    pub recommended_deess: f32,
    pub recommended_compression: f32,
    pub suggested_software_trim_db: f32,
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-6).log10()
}

pub fn analyze(samples: &[f32], rate: u32) -> Result<Calibration, String> {
    if !(8000..=192000).contains(&rate) {
        return Err("unsupported calibration sample rate".into());
    }
    if samples.len() < rate as usize || samples.len() > rate as usize * 60 {
        return Err("record a sample between 1 and 60 seconds, including room silence".into());
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err("sample contains non-finite audio".into());
    }
    let width = (rate as usize / 50).max(1);
    let mut windows = Vec::with_capacity(samples.len().div_ceil(width));
    let mut energy = [0.0_f64; 3];
    let mut low = 0.0;
    let mut body = 0.0;
    let mut peak = 0.0_f64;
    let mut clipped = 0;
    let mut bursts = 0;
    let mut sib = 0;
    let mut records = Vec::with_capacity(windows.capacity());
    let k_low = 1.0 - (-std::f64::consts::TAU * 150.0 / rate as f64).exp();
    let k_body = 1.0 - (-std::f64::consts::TAU * 4500.0 / rate as f64).exp();
    for window in samples.chunks(width) {
        let mut sum = 0.0;
        let mut band = [0.0; 3];
        for &s in window {
            let x = s as f64;
            peak = peak.max(x.abs());
            clipped += u64::from(x.abs() >= 1.0);
            sum += x * x;
            low += k_low * (x - low);
            body += k_body * (x - body);
            for (e, v) in band.iter_mut().zip([low, body - low, x - body]) {
                *e += v * v;
            }
        }
        let rms = (sum / window.len() as f64).sqrt();
        windows.push(db(rms));
        records.push((db(rms), band));
        for i in 0..3 {
            energy[i] += band[i];
        }
    }
    windows.sort_by(f64::total_cmp);
    let percentile = |p: f64| windows[((windows.len() - 1) as f64 * p).round() as usize];
    let floor = percentile(0.1);
    let at = (floor + 10.0).max(-55.0);
    let mut active = 0;
    for (rms, bands) in &records {
        if *rms > at {
            active += 1;
            let total = bands.iter().sum::<f64>().max(1e-20);
            bursts += usize::from(bands[0] / total > 0.5);
            sib += usize::from(bands[2] / total > 0.35);
        }
    }
    let total = energy.iter().sum::<f64>();
    let sum = samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
    let range = percentile(0.95) - percentile(0.1);
    let quiet_valid = range > 12.0 && active > 0;
    let mut warnings = vec!["Include quiet room audio and normal speech; band activity is a heuristic, not a speech classifier.".into()];
    if !quiet_valid {
        warnings
            .push("No clear quiet section: noise floor and gate recommendation withheld.".into());
    }
    if active == 0 {
        warnings
            .push("No clear voice activity: tone and dynamics recommendations withheld.".into());
    }
    if clipped > 0 {
        warnings.push("Capture clipped. Software cannot restore clipped samples; lower device gain explicitly and record again.".into());
    }
    let burst_fraction = bursts as f64 / active.max(1) as f64;
    let sib_fraction = sib as f64 / active.max(1) as f64;
    Ok(Calibration {
        version: 1,
        sample_rate: rate,
        duration_s: samples.len() as f64 / rate as f64,
        peak_dbfs: db(peak),
        rms_dbfs: db((sum / samples.len() as f64).sqrt()),
        quiet_window_dbfs: floor,
        window_range_db: range,
        headroom_db: -db(peak),
        clipped_samples: clipped,
        active_fraction: active as f64 / records.len() as f64,
        band_energy: energy.map(|v| if total > 0.0 { v / total } else { 0.0 }),
        low_frequency_burst_fraction: burst_fraction,
        high_frequency_activity_fraction: sib_fraction,
        warnings,
        recommended_hpf_hz: if burst_fraction > 0.15 { 100.0 } else { 60.0 },
        recommended_gate_db: quiet_valid.then_some((floor as f32 + 6.0).clamp(-80.0, -20.0)),
        recommended_deess: if active > 0 && sib_fraction > 0.15 {
            0.4
        } else {
            0.0
        },
        recommended_compression: if active > 0 && range > 20.0 { 0.3 } else { 0.0 },
        suggested_software_trim_db: if active == 0 || clipped > 0 {
            0.0
        } else {
            (-12.0 - db(peak)).clamp(-12.0, 12.0) as f32
        },
    })
}

/// Bounded local-file decoding, shared by Mic's UI and CLI. Decoding is
/// offline; a >60 second take is rejected by analysis rather than truncated.
pub fn decode_short(path: &std::path::Path) -> Result<Vec<f32>, String> {
    if !path.is_file() {
        return Err("choose a local voice recording".into());
    }
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-t", "61", "-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "-",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr)));
    }
    if out.stdout.len() % 4 != 0 {
        return Err("incomplete float samples".into());
    }
    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect())
}

/// Optional control-plane analysis providers return advice only. Normal audio
/// processing has no inference dependency or provider calls.
pub trait RecommendationProvider {
    fn recommend(&self, measurements: &Calibration) -> Result<Vec<String>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_does_not_recommend_boost_or_gate() {
        let c = analyze(&vec![0.0; 48000], 48000).unwrap();
        assert_eq!(c.suggested_software_trim_db, 0.0);
        assert_eq!(c.recommended_gate_db, None);
        assert_eq!(c.active_fraction, 0.0);
        assert!(c.rms_dbfs.is_finite());
    }
    #[test]
    fn measured_levels_and_clipping() {
        let x = vec![1.0; 48000];
        let c = analyze(&x, 48000).unwrap();
        assert_eq!(c.peak_dbfs, 0.0);
        assert_eq!(c.rms_dbfs, 0.0);
        assert_eq!(c.clipped_samples, 48000);
        assert_eq!(c.suggested_software_trim_db, 0.0);
        assert!(analyze(&[f32::NAN; 8000], 8000).is_err());
    }
    #[test]
    fn quiet_section_produces_a_bounded_gate_suggestion() {
        let x: Vec<f32> = (0..96000)
            .map(|i| {
                let a = if i < 24000 { 0.001 } else { 0.15 };
                a * (std::f32::consts::TAU * 200.0 * i as f32 / 48000.0).sin()
            })
            .collect();
        let c = analyze(&x, 48000).unwrap();
        assert!((c.peak_dbfs + 16.478).abs() < 0.01);
        assert!(c.recommended_gate_db.unwrap() < -50.0);
        assert!(c.active_fraction > 0.7);
    }
}
