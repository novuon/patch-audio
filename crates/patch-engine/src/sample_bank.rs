//! The Tidal instrument's samples: files from the sample folders
//! ([`patch_pattern::samples`]) decoded off the audio thread and handed to
//! it in a fixed table of slots.
//!
//! Code that is evaluated has its samples loaded first ([`Bank::warm`]),
//! so its first hits sound. A sound that is asked for while it isn't
//! loaded is loaded in the background and skips that one hit. Memory is
//! capped: past it the longest-unused samples (not played for a while)
//! are let go.
//!
//! Real-time safe: buffers are moved into the audio thread through a
//! mailbox ([`crate::sampler::Mailbox`]) and replaced ones come back
//! through another, to be freed here. Every push happens under the bank's
//! lock, so each mailbox has one producer at a time.

use crate::sampler::Mailbox;
use patch_pattern::instrument::SampleInfo;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Slots in the instrument's table.
pub const SLOTS: usize = 4096;
/// Sample memory before unused samples are let go (bytes of f32).
pub const CAP_BYTES: usize = 1 << 30;
/// The longest file loaded (longer ones are cut here).
pub const MAX_SECONDS: u64 = 300;
/// A sample unused this long may be let go to make room.
const IDLE: Duration = Duration::from_secs(20);

/// One decoded file: interleaved frames at its own rate.
pub struct SampleBuf {
    pub data: Box<[f32]>,
    pub channels: usize,
    pub frames: usize,
    pub rate: u32,
}

impl SampleBuf {
    fn bytes(&self) -> usize {
        self.data.len() * 4
    }
}

/// What the audio thread is asked to do with a slot.
pub enum Load {
    Put(u32, SampleBuf),
    Clear(u32),
}

type Stamp = (Option<SystemTime>, u64);

fn stamp(p: &Path) -> Stamp {
    std::fs::metadata(p).map_or((None, 0), |m| (m.modified().ok(), m.len()))
}

struct Entry {
    slot: u32,
    frames: u64,
    rate: u32,
    bytes: usize,
    used: Instant,
    stamp: Stamp,
}

#[derive(Default)]
struct Inner {
    loaded: HashMap<PathBuf, Entry>,
    free: Vec<u32>,
    bytes: usize,
    pending: HashSet<PathBuf>,
    failed: HashMap<PathBuf, String>,
    loader: Option<mpsc::Sender<PathBuf>>,
}

type Done = Arc<Mutex<Vec<(PathBuf, Stamp, Result<SampleBuf, String>)>>>;

pub struct Bank {
    /// Control → audio thread.
    pub(crate) loads: Mailbox<Load>,
    /// Audio → control thread: buffers to free.
    pub(crate) back: Mailbox<SampleBuf>,
    inner: Mutex<Inner>,
    done: Done,
}

impl Default for Bank {
    fn default() -> Bank {
        Bank {
            loads: Mailbox::new(1024),
            back: Mailbox::new(SLOTS),
            inner: Mutex::new(Inner {
                free: (0..SLOTS as u32).rev().collect(),
                ..Inner::default()
            }),
            done: Arc::default(),
        }
    }
}

/// How the bank is doing, for the status.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BankStatus {
    pub loaded: usize,
    pub bytes: usize,
    pub loading: usize,
    pub failed: usize,
    pub last_error: Option<String>,
}

fn name_of(p: &Path) -> String {
    let folder = p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()).unwrap_or("?");
    let file = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    format!("{folder}/{file}")
}

impl Bank {
    /// The sample at `path` as the instrument plays it; loaded in the
    /// background (an error now) if it isn't yet.
    pub fn lookup(&self, path: &Path) -> Result<SampleInfo, String> {
        let mut b = self.inner.lock().unwrap();
        self.settle(&mut b);
        if let Some(e) = b.loaded.get_mut(path) {
            e.used = Instant::now();
            return Ok(SampleInfo { slot: e.slot, frames: e.frames, rate: e.rate });
        }
        if let Some(why) = b.failed.get(path) {
            return Err(why.clone());
        }
        if b.pending.insert(path.to_path_buf()) {
            self.queue(&mut b, path.to_path_buf());
        }
        Err(format!("{} is still loading", name_of(path)))
    }

    /// Load `paths` now (those changed on disk again), for up to `budget`;
    /// the rest in the background. Called before evaluated code plays.
    pub fn warm(&self, paths: &[PathBuf], budget: Duration) {
        let start = Instant::now();
        for p in paths {
            let st = stamp(p);
            {
                let mut b = self.inner.lock().unwrap();
                self.settle(&mut b);
                b.failed.remove(p);
                if b.loaded.get(p).is_some_and(|e| e.stamp == st) || b.pending.contains(p) {
                    continue;
                }
                if start.elapsed() >= budget {
                    b.pending.insert(p.clone());
                    self.queue(&mut b, p.clone());
                    continue;
                }
            }
            // Decoded without the lock: lookups go on meanwhile.
            let r = decode(p);
            let mut b = self.inner.lock().unwrap();
            self.install(&mut b, p.clone(), st, r);
        }
    }

    pub fn status(&self) -> BankStatus {
        let mut b = self.inner.lock().unwrap();
        self.settle(&mut b);
        BankStatus {
            loaded: b.loaded.len(),
            bytes: b.bytes,
            loading: b.pending.len(),
            failed: b.failed.len(),
            last_error: b.failed.values().next().cloned(),
        }
    }

    fn queue(&self, b: &mut Inner, path: PathBuf) {
        if b.loader.is_none() {
            let (tx, rx) = mpsc::channel::<PathBuf>();
            let done = self.done.clone();
            let spawned = std::thread::Builder::new()
                .name("patch-tidal-samples".into())
                .spawn(move || {
                    for p in rx {
                        let st = stamp(&p);
                        let r = decode(&p);
                        done.lock().unwrap().push((p, st, r));
                    }
                });
            if spawned.is_ok() {
                b.loader = Some(tx);
            }
        }
        let sent = b.loader.as_ref().is_some_and(|tx| tx.send(path.clone()).is_ok());
        if !sent {
            b.loader = None;
            b.pending.remove(&path);
            b.failed.insert(path, "the sample loader isn't running".into());
        }
    }

    /// Background loads installed, given-back buffers freed.
    fn settle(&self, b: &mut Inner) {
        while self.back.pop().is_some() {}
        let done: Vec<_> = std::mem::take(&mut *self.done.lock().unwrap());
        for (p, st, r) in done {
            self.install(b, p, st, r);
        }
    }

    fn install(&self, b: &mut Inner, path: PathBuf, st: Stamp, r: Result<SampleBuf, String>) {
        b.pending.remove(&path);
        let buf = match r {
            Ok(buf) => buf,
            Err(e) => {
                b.failed.insert(path, e);
                return;
            }
        };
        let bytes = buf.bytes();
        // A file loaded before and changed since takes its old slot.
        let slot = match b.loaded.remove(&path) {
            Some(old) => {
                b.bytes -= old.bytes;
                Some(old.slot)
            }
            None => None,
        };
        let slot = match slot {
            Some(s) => s,
            None => match self.room(b, bytes) {
                Some(s) => s,
                None => {
                    b.failed.insert(
                        path,
                        format!("sample memory is full ({} MB in use)", b.bytes >> 20),
                    );
                    return;
                }
            },
        };
        let (frames, rate) = (buf.frames as u64, buf.rate);
        match self.loads.push(Load::Put(slot, buf)) {
            Ok(()) => {
                b.bytes += bytes;
                b.loaded.insert(path, Entry { slot, frames, rate, bytes, used: Instant::now(), stamp: st });
            }
            Err(_) => {
                b.free.push(slot);
                b.failed.insert(path, "the instrument is busy; try again".into());
            }
        }
    }

    /// A free slot with room for `bytes`, letting unused samples go.
    fn room(&self, b: &mut Inner, bytes: usize) -> Option<u32> {
        while b.free.is_empty() || b.bytes + bytes > CAP_BYTES {
            let now = Instant::now();
            let victim = b
                .loaded
                .iter()
                .filter(|(_, e)| now.duration_since(e.used) >= IDLE)
                .min_by_key(|(_, e)| e.used)
                .map(|(p, _)| p.clone())?;
            let e = b.loaded.remove(&victim)?;
            if self.loads.push(Load::Clear(e.slot)).is_err() {
                b.loaded.insert(victim, e);
                return None;
            }
            b.bytes -= e.bytes;
            b.free.push(e.slot);
        }
        b.free.pop()
    }
}

/// A piece of an audio file (`from` … `to` seconds; all of it without
/// them) saved as a new sample of `name` in Patch's own folder: the next
/// file of that sound, 24-bit WAV at the file's rate, written whole before
/// it appears (so a running pattern never reads half a file).
pub fn save(name: &str, source: &Path, from: Option<f64>, to: Option<f64>) -> Result<PathBuf, String> {
    let dir = lock_sample_folder(name)?;
    save_part(&dir.0, source, from, to, 1.0, None).map(|s| s.path)
}

/// A sample just saved: its file and the `n` that plays it (`name:n`).
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub n: usize,
}

/// Keep every slice of a capture together, including across CLI processes.
pub(crate) fn lock_sample_folder(name: &str) -> Result<(PathBuf, std::fs::File), String> {
    if !patch_pattern::samples::valid_name(name) {
        return Err(format!("'{name}' can't name a sound: letters, digits, _ - . only"));
    }
    let dir = patch_pattern::samples::own_root().join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".capture.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    Ok((dir, lock))
}

/// Save a cut while holding the destination folder's lock.
pub(crate) fn save_part(
    dir: &Path,
    source: &Path,
    from: Option<f64>,
    to: Option<f64>,
    gain: f32,
    label: Option<&str>,
) -> Result<Saved, String> {
    let start = from.unwrap_or(0.0).max(0.0);
    let limit = MAX_SECONDS as f64;
    let duration = to.map_or(limit, |end| (end - start).min(limit));
    if !duration.is_finite() || duration <= 0.0 {
        return Err("the end comes before the start".into());
    }
    // After the highest number there (files deleted in between leave
    // gaps; a new one never takes an old one's name).
    let next = audio_files(dir)
        .iter()
        .filter_map(|p| p.file_name()?.to_str()?.split('-').next()?.parse::<usize>().ok())
        .max()
        .map_or(0, |m| m + 1);
    let stem: String = label
        .map(str::to_string)
        .or_else(|| source.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .unwrap_or_else(|| "sample".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(40)
        .collect();
    let out = dir.join(format!("{next:03}-{stem}.wav"));
    let tmp = dir.join(format!(".{next:03}-{stem}.part.wav"));
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-y"]);
    if let Some(a) = from {
        cmd.args(["-ss", &format!("{:.6}", a.max(0.0))]);
    }
    cmd.arg("-i").arg(source).args(["-vn", "-ac", "2"]);
    if (gain - 1.0).abs() > 1e-4 {
        cmd.args(["-af", &format!("volume={gain:.6}")]);
    }
    cmd.args(["-c:a", "pcm_s24le", "-t", &format!("{duration:.6}")]).arg(&tmp);
    let st = cmd
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !st.success() || !tmp.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("couldn't cut {} (ffmpeg failed)", source.display()));
    }
    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    let n = audio_files(dir).iter().position(|p| *p == out).unwrap_or(0);
    Ok(Saved { path: out, n })
}

/// A folder's sounds in the order `n` counts them.
fn audio_files(dir: &Path) -> Vec<PathBuf> {
    let ix = patch_pattern::samples::Index::scan(&[dir.parent().map(Path::to_path_buf).unwrap_or_default()]);
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ix.get(name).map(|f| f.files.clone()).unwrap_or_default()
}

/// A file as frames: WAV read here (most sample packs), anything else
/// through ffmpeg at 48 kHz.
pub fn decode(path: &Path) -> Result<SampleBuf, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", name_of(path)))?;
    match wav(&bytes) {
        Some(Ok(b)) => Ok(b),
        Some(Err(e)) => Err(format!("{}: {e}", name_of(path))),
        None => ffmpeg(path),
    }
}

fn ffmpeg(path: &Path) -> Result<SampleBuf, String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-t", &MAX_SECONDS.to_string(), "-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: ffmpeg: {e}", name_of(path)))?;
    if !out.status.success() || out.stdout.len() < 8 {
        return Err(format!("{}: can't decode it", name_of(path)));
    }
    let data: Box<[f32]> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .map(|x| if x.is_finite() { x } else { 0.0 })
        .collect();
    Ok(SampleBuf { frames: data.len() / 2, data, channels: 2, rate: 48_000 })
}

/// A RIFF WAVE file's audio (PCM 8–32 bit, float 32/64, extensible):
/// `None` when it isn't one this reads (ffmpeg then tries).
fn wav(b: &[u8]) -> Option<Result<SampleBuf, String>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut i = 12;
    let mut fmt: Option<(u16, usize, u32, usize, usize)> = None;
    let mut data: Option<&[u8]> = None;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8;
        let end = body.saturating_add(len).min(b.len());
        if id == b"fmt " && len >= 16 && end >= body + 16 {
            let mut format = u16_at(body);
            let channels = u16_at(body + 2) as usize;
            let rate = u32_at(body + 4);
            let align = u16_at(body + 12) as usize;
            let bits = u16_at(body + 14) as usize;
            if format == 0xFFFE && end >= body + 26 {
                format = u16_at(body + 24);
            }
            fmt = Some((format, channels, rate, bits, align));
        } else if id == b"data" {
            data = Some(&b[body..end]);
        }
        i = body.saturating_add(len + (len & 1));
    }
    let (format, channels, rate, bits, align) = fmt?;
    let data = data?;
    if channels == 0 || !(1000..=768_000).contains(&rate) {
        return Some(Err("a WAV file with no channels or an odd rate".into()));
    }
    let width = bits.div_ceil(8);
    if align < width * channels || width == 0 {
        return None;
    }
    let read: fn(&[u8]) -> f32 = match (format, width) {
        (1, 1) => |s| (s[0] as f32 - 128.0) / 128.0,
        (1, 2) => |s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0,
        (1, 3) => |s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
        (1, 4) => |s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        (3, 4) => |s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        (3, 8) => |s| f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
        _ => return None,
    };
    let keep = channels.min(2);
    let frames = (data.len() / align).min((MAX_SECONDS * rate as u64) as usize);
    let mut out = Vec::with_capacity(frames * keep);
    for f in 0..frames {
        let at = f * align;
        for c in 0..keep {
            let x = read(&data[at + c * width..]);
            out.push(if x.is_finite() { x.clamp(-4.0, 4.0) } else { 0.0 });
        }
    }
    if frames == 0 {
        return Some(Err("an empty WAV file".into()));
    }
    Some(Ok(SampleBuf { data: out.into_boxed_slice(), channels: keep, frames, rate }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A WAV file of `frames` frames: 16-bit, a ramp per channel.
    pub(crate) fn wav_bytes(channels: u16, rate: u32, bits: u16, frames: usize) -> Vec<u8> {
        let width = bits as usize / 8;
        let align = width * channels as usize;
        let mut d = vec![];
        for f in 0..frames {
            for c in 0..channels as usize {
                let x = ((f as f32 / frames as f32) * 2.0 - 1.0) * if c == 0 { 0.5 } else { -0.5 };
                match bits {
                    16 => d.extend(((x * 32_767.0) as i16).to_le_bytes()),
                    24 => d.extend(&((x * 8_388_607.0) as i32).to_le_bytes()[..3]),
                    32 => d.extend(x.to_le_bytes()),
                    _ => d.push(((x * 127.0) as i32 + 128) as u8),
                }
            }
        }
        let mut b = vec![];
        b.extend(b"RIFF");
        b.extend(((36 + d.len() + 10) as u32).to_le_bytes());
        b.extend(b"WAVE");
        // An unknown chunk first, odd-sized (padded), as some editors write.
        b.extend(b"junk");
        b.extend(1u32.to_le_bytes());
        b.extend([0, 0]);
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend((if bits == 32 { 3u16 } else { 1u16 }).to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * align as u32).to_le_bytes());
        b.extend((align as u16).to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend(b"data");
        b.extend((d.len() as u32).to_le_bytes());
        b.extend(d);
        b
    }

    #[test]
    fn wav_formats_read() {
        for (ch, bits) in [(1, 16), (2, 16), (2, 24), (1, 32), (2, 8)] {
            let b = wav(&wav_bytes(ch, 44_100, bits, 1000)).unwrap().unwrap();
            assert_eq!((b.channels, b.frames, b.rate), (ch as usize, 1000, 44_100), "{ch} {bits}");
            let tol = if bits == 8 { 0.02 } else { 0.001 };
            assert!((b.data[0] + 0.5).abs() < tol, "{ch} {bits}: {}", b.data[0]);
            let last = b.data[(b.frames - 1) * b.channels];
            assert!((last - 0.499).abs() < tol, "{ch} {bits}: {last}");
            if ch == 2 {
                assert!((b.data[1] - 0.5).abs() < tol);
            }
        }
        assert!(wav(b"not a wav file at all").is_none());
    }

    fn folder(files: &[(&str, usize)]) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("patch-bank-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, frames) in files {
            std::fs::write(dir.join(f), wav_bytes(1, 48_000, 16, *frames)).unwrap();
        }
        dir
    }

    #[test]
    fn saved_parts_keep_their_requested_length_and_slots() {
        if std::process::Command::new("ffmpeg")
            .arg("-version").output().is_err()
        {
            return;
        }
        let root = folder(&[("source.wav", 48_000)]);
        let dir = root.join("kit");
        std::fs::create_dir(&dir).unwrap();
        for i in 0..4 {
            let start = f64::from(i) * 0.125;
            let saved = save_part(&dir, &root.join("source.wav"), Some(start), Some(start + 0.125), 1.0, None).unwrap();
            assert_eq!(saved.n, i as usize);
            assert_eq!(decode(&saved.path).unwrap().frames, 6000);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn warm_loads_lookups_find_and_changes_reload() {
        let dir = folder(&[("a.wav", 4800), ("b.wav", 9600)]);
        let bank = Bank::default();
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        bank.warm(&[a.clone(), b.clone()], Duration::from_secs(5));
        let ia = bank.lookup(&a).unwrap();
        let ib = bank.lookup(&b).unwrap();
        assert_eq!((ia.frames, ia.rate), (4800, 48_000));
        assert_eq!(ib.frames, 9600);
        assert_ne!(ia.slot, ib.slot);
        assert_eq!(bank.status().loaded, 2);
        // Two puts wait for the audio thread.
        let mut puts = 0;
        while let Some(l) = bank.loads.pop() {
            puts += usize::from(matches!(l, Load::Put(..)));
        }
        assert_eq!(puts, 2);
        // The file changes: warmed again, it reloads into its slot.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&a, wav_bytes(1, 48_000, 16, 2400)).unwrap();
        bank.warm(std::slice::from_ref(&a), Duration::from_secs(5));
        let again = bank.lookup(&a).unwrap();
        assert_eq!((again.slot, again.frames), (ia.slot, 2400));
        // Not warmed: loaded in the background, missing until then.
        std::fs::write(dir.join("c.wav"), wav_bytes(2, 44_100, 24, 441)).unwrap();
        let c = dir.join("c.wav");
        assert!(bank.lookup(&c).unwrap_err().contains("still loading"));
        let t = Instant::now();
        while bank.lookup(&c).is_err() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(bank.lookup(&c).unwrap().rate, 44_100);
        // An unreadable file says so.
        std::fs::write(dir.join("d.wav"), b"RIFF\x04\0\0\0WAVEfmt ").unwrap();
        bank.warm(&[dir.join("d.wav")], Duration::from_secs(5));
        assert!(bank.lookup(&dir.join("d.wav")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
