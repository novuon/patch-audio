//! The Player's waveform: a file's loudness over its whole length, to see
//! where the talk, the music and the silences are, and to click on.
//!
//! A file can be hours long (a podcast, a recorded set), so it is never
//! decoded into memory: ffmpeg streams it as 4 kHz mono, at the lowest
//! priority, and each sample only raises the peak and the energy of one of
//! `BINS` columns. The picture fills in from the left while it reads, and
//! the result (3.2 kB) is cached by the file's path, size and time.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Columns in a waveform, whatever the file's length.
pub const BINS: usize = 1600;
/// What ffmpeg is asked for: enough for an overview's peaks.
const RATE: usize = 4000;
/// Cached waveforms kept (the oldest go first).
const KEPT: usize = 400;

#[derive(Clone, Default)]
pub struct Wave {
    /// Each column's peak, 0 to 255 (a square-root scale: quiet talk shows).
    pub peak: Vec<u8>,
    /// Each column's RMS, on the same scale.
    pub rms: Vec<u8>,
    /// Columns read so far.
    pub filled: usize,
    pub done: bool,
}

/// A waveform being read, shared with the thread that reads it.
pub struct Reading {
    pub path: String,
    pub wave: Arc<Mutex<Wave>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Reading {
    /// Another file was loaded: its reader stops.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A level (0 to 1) on the waveform's scale.
fn scale(level: f64) -> u8 {
    (level.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
}

/// Folds samples into columns as they arrive.
struct Fold {
    total: usize,
    seen: usize,
    bin: usize,
    peak: i32,
    energy: f64,
    count: usize,
}

impl Fold {
    fn new(seconds: f64) -> Self {
        Self { total: ((seconds * RATE as f64) as usize).max(BINS), seen: 0, bin: 0, peak: 0, energy: 0.0, count: 0 }
    }

    /// Add samples; finished columns go to `out` as (peak, rms).
    fn add(&mut self, samples: impl Iterator<Item = i16>, out: &mut Vec<(u8, u8)>) {
        for s in samples {
            let bin = (self.seen * BINS / self.total).min(BINS - 1);
            if bin != self.bin {
                out.push(self.column());
                // A file longer than its header said: the rest joins the last column.
                self.bin = bin;
            }
            let v = (s as i32).abs();
            self.peak = self.peak.max(v);
            self.energy += (v as f64) * (v as f64);
            self.count += 1;
            self.seen += 1;
        }
    }

    fn column(&mut self) -> (u8, u8) {
        let rms = if self.count > 0 { (self.energy / self.count as f64).sqrt() } else { 0.0 };
        let column = (scale(self.peak as f64 / 32768.0), scale(rms / 32768.0));
        (self.peak, self.energy, self.count) = (0, 0.0, 0);
        column
    }
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("omarchy-patch/player-waves"))
}

/// The cache file for this file as it is now (its path, size and time).
fn cache_file(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    // FNV-1a: a name, not a secret.
    let mut hash = 0xcbf29ce484222325u64;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash = (hash ^ *b as u64).wrapping_mul(0x100000001b3);
        }
    };
    feed(path.to_string_lossy().as_bytes());
    feed(&meta.len().to_le_bytes());
    feed(&modified.to_le_bytes());
    Some(cache_dir()?.join(format!("{hash:016x}.pw")))
}

fn read_cache(file: &Path) -> Option<Wave> {
    let bytes = std::fs::read(file).ok()?;
    (bytes.len() == BINS * 2).then(|| Wave {
        peak: bytes[..BINS].to_vec(),
        rms: bytes[BINS..].to_vec(),
        filled: BINS,
        done: true,
    })
}

fn write_cache(file: &Path, wave: &Wave) {
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut bytes = wave.peak.clone();
    bytes.extend_from_slice(&wave.rms);
    let tmp = file.with_extension("new");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
    // Keep the folder small: beyond KEPT, the oldest quarter goes.
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() > KEPT {
        files.sort();
        for (_, old) in files.iter().take(files.len() - KEPT * 3 / 4) {
            let _ = std::fs::remove_file(old);
        }
    }
}

/// Start reading a file's waveform (at once from the cache when it is
/// there). `seconds` is its length as the player reports it.
pub fn read(path: &str, seconds: f64) -> Reading {
    let wave = Arc::new(Mutex::new(Wave { peak: vec![0; BINS], rms: vec![0; BINS], filled: 0, done: false }));
    let cancel = Arc::new(AtomicBool::new(false));
    let reading = Reading { path: path.to_owned(), wave: wave.clone(), cancel: cancel.clone() };
    let source = PathBuf::from(path);
    let cache = cache_file(&source);
    if let Some(cached) = cache.as_deref().and_then(read_cache) {
        *wave.lock().unwrap() = cached;
        return reading;
    }
    let spawned = std::thread::Builder::new().name("patch-player-wave".into()).spawn(move || {
        let finish = |ok: bool| {
            let mut w = wave.lock().unwrap();
            w.done = true;
            if ok {
                w.filled = BINS;
                if let Some(cache) = &cache {
                    write_cache(cache, &w);
                }
            }
        };
        // Lowest priority: it reads beside live audio.
        let child = Command::new("nice")
            .args(["-n", "19", "ffmpeg", "-v", "error", "-nostdin", "-i"])
            .arg(&source)
            .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return finish(false) };
        let Some(mut pipe) = child.stdout.take() else { return finish(false) };
        let mut fold = Fold::new(seconds);
        let mut buffer = vec![0u8; 32 * 1024];
        let mut held = 0;
        let mut columns = Vec::new();
        let mut any = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            let read = match pipe.read(&mut buffer[held..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n + held,
            };
            any = true;
            let whole = read & !1;
            fold.add(buffer[..whole].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])), &mut columns);
            held = read - whole;
            if held == 1 {
                buffer[0] = buffer[whole];
            }
            if !columns.is_empty() {
                let mut w = wave.lock().unwrap();
                for (peak, rms) in columns.drain(..) {
                    let at = w.filled.min(BINS - 1);
                    w.peak[at] = peak;
                    w.rms[at] = rms;
                    w.filled = (w.filled + 1).min(BINS);
                }
            }
        }
        let ok = child.wait().is_ok_and(|s| s.success()) && any;
        if ok {
            let (peak, rms) = fold.column();
            let mut w = wave.lock().unwrap();
            let at = w.filled.min(BINS - 1);
            w.peak[at] = w.peak[at].max(peak);
            w.rms[at] = w.rms[at].max(rms);
        }
        finish(ok);
    });
    if spawned.is_err() {
        reading.wave.lock().unwrap().done = true;
    }
    reading
}

/// Draw a waveform as a mask (white, the shape in the alpha): the window
/// tints it, once for what is still to come and once for what has played.
/// Mirrored about the middle; the peaks faint, the body solid.
pub fn mask(wave: &Wave, width: u32, height: u32) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let (width, height) = (width.clamp(1, 4096), height.clamp(1, 1024));
    let mut image = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    let pixels = image.make_mut_slice();
    pixels.fill(slint::Rgba8Pixel { r: 255, g: 255, b: 255, a: 0 });
    let (w, h) = (width as usize, height as usize);
    let middle = h as f64 / 2.0;
    for x in 0..w {
        // Every column this pixel covers: the loudest shows.
        let from = x * BINS / w;
        let to = (((x + 1) * BINS).div_ceil(w)).clamp(from + 1, BINS);
        if from >= wave.filled {
            break;
        }
        let peak = wave.peak[from..to].iter().copied().max().unwrap_or(0) as f64 / 255.0;
        let rms = wave.rms[from..to].iter().copied().max().unwrap_or(0) as f64 / 255.0;
        // Silence still draws its centre line: the file is there.
        let peak_half = (peak * (middle - 1.0)).max(0.5);
        let rms_half = rms * (middle - 1.0);
        for y in 0..h {
            let off = ((y as f64 + 0.5) - middle).abs();
            let alpha = if off <= rms_half.max(0.5) {
                255
            } else if off <= peak_half {
                105
            } else {
                continue;
            };
            pixels[y * w + x].a = alpha;
        }
    }
    image
}

/// A waveform for the headless shots: talk, with a music break.
pub fn demo() -> Wave {
    let mut wave = Wave { peak: vec![0; BINS], rms: vec![0; BINS], filled: BINS, done: true };
    let mut seed = 0x2545f4914f6cdd1du64;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f64 / (1u64 << 24) as f64
    };
    for i in 0..BINS {
        let t = i as f64 / BINS as f64;
        let music = (0.0..0.04).contains(&t) || (0.47..0.53).contains(&t) || t > 0.965;
        let phrase = ((t * 190.0).sin() * 0.5 + 0.5) * ((t * 37.0).sin() * 0.3 + 0.7);
        let pause = (t * 61.0).sin() > 0.93;
        let level = if music { 0.55 + noise() * 0.12 } else if pause { 0.02 } else { 0.12 + phrase * 0.3 + noise() * 0.1 };
        wave.rms[i] = scale(level * level);
        wave.peak[i] = scale((level * (1.5 + noise() * 0.5)).powi(2).min(1.0));
    }
    wave
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_fold_into_columns_by_their_place_in_the_file() {
        // Two seconds: the first silent, the second at half scale.
        let mut fold = Fold::new(2.0);
        let mut columns = Vec::new();
        fold.add(std::iter::repeat_n(0i16, RATE), &mut columns);
        fold.add(std::iter::repeat_n(16384i16, RATE), &mut columns);
        columns.push(fold.column());
        assert_eq!(columns.len(), BINS);
        assert!(columns[..BINS / 2].iter().all(|c| *c == (0, 0)));
        let loud = scale(0.5);
        assert!(columns[BINS / 2 + 1..].iter().all(|c| *c == (loud, loud)));
        // More samples than the length promised: no column past the last.
        let mut fold = Fold::new(1.0);
        let mut columns = Vec::new();
        fold.add(std::iter::repeat_n(1000i16, RATE * 3), &mut columns);
        assert_eq!(columns.len(), BINS - 1);
    }

    #[test]
    fn quiet_talk_is_visible_and_full_scale_fills_the_height() {
        assert_eq!(scale(0.0), 0);
        assert_eq!(scale(1.0), 255);
        assert!(scale(0.05) > 50, "-26 dB talk is a fifth of the height, not a hairline");
    }

    #[test]
    fn the_mask_is_mirrored_and_stops_where_the_reading_has_got_to() {
        let mut wave = Wave { peak: vec![255; BINS], rms: vec![128; BINS], filled: BINS / 2, done: false };
        // The first pixel's eight columns are silent.
        wave.peak[..8].fill(0);
        wave.rms[..8].fill(0);
        let image = mask(&wave, 200, 40);
        let px = image.as_slice();
        let alpha = |x: usize, y: usize| px[y * 200 + x].a;
        // Read so far: the left half only.
        assert_eq!(alpha(50, 20), 255);
        assert_eq!(alpha(150, 20), 0);
        // Body solid, peaks faint, mirrored top and bottom.
        assert_eq!(alpha(50, 12), 255);
        assert_eq!(alpha(50, 2), 105);
        assert_eq!(alpha(50, 2), alpha(50, 37));
        assert_eq!(alpha(50, 12), alpha(50, 27));
        // Silence keeps a centre line.
        assert_eq!(alpha(0, 20), 255);
        assert_eq!(alpha(0, 10), 0);
    }

    #[test]
    fn a_waveform_is_read_from_a_real_file_and_then_from_the_cache() {
        if Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("patch-wave-{}-{}", std::process::id(), patch_engine::now_ns()));
        std::fs::create_dir_all(&dir).unwrap();
        // One second of silence, then one of a full-scale square wave.
        let file = dir.join("tone.wav");
        let mut data = Vec::new();
        for i in 0..96000u32 {
            let s: i16 = if i < 48000 { 0 } else if (i / 24) % 2 == 0 { 30000 } else { -30000 };
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&96000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(&file, wav).unwrap();
        let reading = read(&file.to_string_lossy(), 2.0);
        let since = std::time::Instant::now();
        while !reading.wave.lock().unwrap().done && since.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let wave = reading.wave.lock().unwrap().clone();
        assert!(wave.done && wave.filled == BINS);
        assert!(wave.peak[BINS / 4] < 8, "the silent half");
        assert!(wave.peak[BINS * 3 / 4] > 200, "the loud half: {}", wave.peak[BINS * 3 / 4]);
        // The cache answers at once, without a reader.
        if let Some(cache) = cache_file(&file) {
            assert!(cache.exists());
            let again = read(&file.to_string_lossy(), 2.0);
            assert!(again.wave.lock().unwrap().done);
            assert_eq!(again.wave.lock().unwrap().peak, wave.peak);
            let _ = std::fs::remove_file(cache);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
