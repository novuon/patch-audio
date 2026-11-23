//! Detailed waveform for scrolling displays: 150 columns per second, each
//! the peak of three bands (low < 200 Hz, mid, high > 2 kHz) and of the
//! full signal, 0–255 linear. Built once at analysis, stored as a compact
//! binary sidecar ("PWD1"), so displays never rescan or decode audio.

use std::io::{Read, Write};
use std::path::Path;

pub const COLUMNS_PER_SECOND: u32 = 150;

#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    pub columns_per_second: u32,
    /// [low, mid, high, peak] per column.
    pub columns: Vec<[u8; 4]>,
}

/// Temporary one-column-per-second waveform while detailed bands are rebuilt.
/// It preserves the source-time axis so the scrolling display does not vanish.
pub fn overview(bins: &[[u8; 2]], duration: f64) -> Option<Detail> {
    if bins.is_empty() || !duration.is_finite() || duration <= 0.0 || duration > 86_400.0 {
        return None;
    }
    let count = duration.ceil() as usize;
    Some(Detail {
        columns_per_second: 1,
        columns: (0..count)
            .map(|i| {
                let peak = bins[i * bins.len() / count][0];
                [peak; 4]
            })
            .collect(),
    })
}

/// One-pole filter coefficient for a corner frequency.
fn pole(f: f64, rate: f64) -> f64 {
    (-2.0 * std::f64::consts::PI * f / rate).exp()
}

/// Build from interleaved stereo at [`crate::RATE`].
pub fn build(stereo: &[f32]) -> Detail {
    let rate = crate::RATE as f64;
    let per = (crate::RATE / COLUMNS_PER_SECOND) as usize;
    let (al, ah) = (pole(200.0, rate), pole(2000.0, rate));
    let (mut l1, mut l2, mut h1, mut h2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut cols = Vec::with_capacity(stereo.len() / 2 / per + 1);
    let mut acc = [0.0f32; 4];
    let mut n = 0usize;
    for f in stereo.chunks_exact(2) {
        let x = ((f[0] + f[1]) * 0.5) as f64;
        // 2nd-order low-pass at 200 Hz; low-pass at 2 kHz gives the rest.
        l1 = (1.0 - al) * x + al * l1;
        l2 = (1.0 - al) * l1 + al * l2;
        h1 = (1.0 - ah) * x + ah * h1;
        h2 = (1.0 - ah) * h1 + ah * h2;
        let low = l2;
        let high = x - h2;
        let mid = h2 - l2;
        acc[0] = acc[0].max(low.abs() as f32);
        acc[1] = acc[1].max(mid.abs() as f32);
        acc[2] = acc[2].max(high.abs() as f32);
        acc[3] = acc[3].max(f[0].abs().max(f[1].abs()));
        n += 1;
        if n == per {
            cols.push(acc.map(|v| (v.min(1.0) * 255.0).round() as u8));
            acc = [0.0; 4];
            n = 0;
        }
    }
    if n > 0 {
        cols.push(acc.map(|v| (v.min(1.0) * 255.0).round() as u8));
    }
    Detail {
        columns_per_second: COLUMNS_PER_SECOND,
        columns: cols,
    }
}

pub fn write(path: &Path, d: &Detail) -> std::io::Result<()> {
    crate::cache_store::write_blob(path, |f| {
        f.write_all(b"PWD1")?;
        f.write_all(&d.columns_per_second.to_le_bytes())?;
        f.write_all(&(d.columns.len() as u32).to_le_bytes())?;
        let flat: Vec<u8> = d.columns.iter().flatten().copied().collect();
        f.write_all(&flat)
    })
}

pub fn read(path: &Path) -> Option<Detail> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[..4] != b"PWD1" {
        return None;
    }
    let cps = u32::from_le_bytes(head[4..8].try_into().ok()?);
    let n = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
    let mut flat = vec![0u8; n * 4];
    f.read_exact(&mut flat).ok()?;
    Some(Detail {
        columns_per_second: cps,
        columns: flat
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_preserves_source_time_axis() {
        let detail = overview(&[[10, 5], [100, 50]], 4.0).unwrap();
        assert_eq!(detail.columns_per_second, 1);
        assert_eq!(detail.columns, vec![[10; 4], [10; 4], [100; 4], [100; 4]]);
        assert!(overview(&[], 4.0).is_none());
        assert!(overview(&[[10, 5]], f64::INFINITY).is_none());
    }

    #[test]
    fn bands_separate_and_round_trip() {
        let rate = crate::RATE as f64;
        // 1 s of 60 Hz, then 1 s of 8 kHz.
        let mut x = vec![];
        for (f, secs) in [(60.0, 1.0), (8000.0, 1.0)] {
            for i in 0..(rate * secs) as usize {
                let v = (std::f64::consts::TAU * f * i as f64 / rate).sin() as f32 * 0.5;
                x.extend_from_slice(&[v, v]);
            }
        }
        let d = build(&x);
        assert_eq!(d.columns.len(), 300);
        let bass = d.columns[75];
        let treble = d.columns[225];
        assert!(bass[0] > 100 && bass[2] < 20, "bass column {bass:?}");
        assert!(
            treble[2] > 100 && treble[0] < 20,
            "treble column {treble:?}"
        );
        let p = std::env::temp_dir().join(format!("pwd1-{}.wfd", std::process::id()));
        write(&p, &d).unwrap();
        assert_eq!(read(&p).unwrap(), d);
        let _ = std::fs::remove_file(p);
    }
}
