//! Decoded tracks in memory.
//!
//! A track is decoded once, at load, by ffmpeg with an explicit high-quality
//! sample-rate conversion (soxr, 28-bit precision) to the graph rate, into
//! interleaved stereo f32. Decoding is progressive: the deck can start
//! playing the first seconds while the rest is still arriving. The source's
//! native format (codec, rate, bit depth, channels) is kept as metadata:
//! Patch never pretends a conversion improved it.

use std::cell::UnsafeCell;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Longest track a deck will allocate for. The analysis decode uses the same
/// three-hour ceiling; a longer declared duration is rejected before the
/// sample buffer exists.
pub const MAX_TRACK_SECONDS: f64 = 3.0 * 3600.0;
/// A metadata probe that runs longer than this is killed. ffprobe writes its
/// JSON at the end, so a stall is a wall-clock deadline, not an idle timeout.
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
/// Same stdout ceiling the library probe uses.
const MAX_PROBE_STDOUT: usize = 2 * 1024 * 1024;
/// A deck decoder that produces nothing for this long is killed. The loader
/// thread is not the audio callback, but a stuck ffmpeg must not sit on the
/// engine forever.
const DECODE_STALL: Duration = Duration::from_secs(60);

/// What the file is, before any conversion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceInfo {
    pub codec: String,
    pub rate: u32,
    /// Bits per sample of the source (0 when the codec has none, e.g. MP3).
    pub bits: u32,
    pub channels: u32,
    pub duration: f64,
    pub title: String,
    pub artist: String,
}

/// Read the source format with ffprobe.
pub fn probe(path: &Path) -> Result<SourceInfo, String> {
    probe_within(path, PROBE_DEADLINE)
}

fn probe_within(path: &Path, deadline: Duration) -> Result<SourceInfo, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-select_streams", "a:0", "-show_entries"])
        .arg("stream=codec_name,sample_rate,bits_per_raw_sample,bits_per_sample,channels:format=duration:format_tags=title,artist")
        .args(["-of", "json"])
        .arg(path);
    let out = bounded_output(&mut cmd, MAX_PROBE_STDOUT, deadline)?;
    if !out.status.success() {
        return Err("ffprobe could not read the file".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let s = &v["streams"][0];
    let num = |x: &serde_json::Value| {
        x.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or(x.as_f64())
            .unwrap_or(0.0)
    };
    let bits = num(&s["bits_per_raw_sample"]).max(num(&s["bits_per_sample"])) as u32;
    Ok(SourceInfo {
        codec: s["codec_name"].as_str().unwrap_or("").to_string(),
        rate: num(&s["sample_rate"]) as u32,
        bits,
        channels: s["channels"].as_u64().unwrap_or(0) as u32,
        duration: num(&v["format"]["duration"]),
        title: tag(&v["format"]["tags"], "title"),
        artist: tag(&v["format"]["tags"], "artist"),
    })
}

/// A tag by name in any case: FLAC and Vorbis files carry `TITLE` and
/// `ARTIST`, MP3 `title`. Read in lower case only, a FLAC deck had no
/// title, and the Visualiser showed the file name, track number and all.
fn tag(tags: &serde_json::Value, key: &str) -> String {
    tags.as_object()
        .and_then(|m| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Frames to allocate for a probed duration, including the existing one
/// percent margin plus one second. Rejects a missing, non-finite, or
/// over-long duration before that multiplication can request a huge buffer.
fn capacity_frames(duration: f64, rate: u32) -> Result<usize, String> {
    if !duration.is_finite() || duration <= 0.0 {
        return Err("the file has no audio duration".into());
    }
    if duration > MAX_TRACK_SECONDS {
        return Err(format!(
            "the file is longer than {MAX_TRACK_SECONDS:.0} seconds"
        ));
    }
    let frames = (duration * f64::from(rate) * 1.01) as usize + rate as usize;
    if frames.checked_mul(2).is_none() {
        return Err(format!(
            "the file is longer than {MAX_TRACK_SECONDS:.0} seconds"
        ));
    }
    Ok(frames)
}

/// `Command` stdout with a byte cap and a deadline. On either limit the child
/// is killed. Stderr is discarded; callers pass arguments as argv.
fn bounded_output(
    cmd: &mut Command,
    max_stdout: usize,
    deadline: Duration,
) -> Result<std::process::Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("ffprobe: no output")?;
    let fd = stdout.as_raw_fd();
    // SAFETY: the descriptor belongs to this child, and is still open.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        let _ = child.kill();
        let _ = child.wait();
        return Err("ffprobe: could not watch the probe".into());
    }
    let started = std::time::Instant::now();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let read = loop {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break Err("ffprobe timed out".to_string());
        }
        let wait_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` points at one live descriptor.
        let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Err(format!("ffprobe: {err}"));
        }
        if rc == 0 {
            break Err("ffprobe timed out".to_string());
        }
        match stdout.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if out.len().saturating_add(n) > max_stdout {
                    break Err(format!("ffprobe output exceeds {max_stdout} bytes"));
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(e) => break Err(format!("ffprobe: {e}")),
        }
    };
    drop(stdout);
    if read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| format!("ffprobe: {e}"))?;
    read?;
    Ok(std::process::Output {
        status,
        stdout: out,
        stderr: Vec::new(),
    })
}

/// Path and tags as shared strings, built when the track is loaded. The deck
/// worker clones the `Arc`s into status and does not format them.
#[derive(Clone)]
pub(crate) struct TrackLabels {
    pub path: Arc<str>,
    pub title: Arc<str>,
    pub artist: Arc<str>,
    pub codec: Arc<str>,
}

pub(crate) fn track_labels(path: &Path, info: &SourceInfo) -> TrackLabels {
    TrackLabels {
        path: Arc::from(path.to_string_lossy().as_ref()),
        title: Arc::from(info.title.as_str()),
        artist: Arc::from(info.artist.as_str()),
        codec: Arc::from(info.codec.as_str()),
    }
}

/// A buffer of zeros, at once: the allocator's zeroed pages (calloc), not
/// written one by one (a five-minute track's 60 MB took ~80 ms of the load
/// request, a stall every window waiting on it showed).
fn zeroed<T: Copy + Default>(n: usize) -> Box<[UnsafeCell<T>]> {
    let v: Box<[T]> = vec![T::default(); n].into_boxed_slice();
    // SAFETY: UnsafeCell<T> is repr(transparent) over T: same layout.
    unsafe { Box::from_raw(Box::into_raw(v) as *mut [UnsafeCell<T>]) }
}

/// How the decoded samples are kept.
///
/// * `I16`: 16-bit with 3 dB headroom and TPDF dither: half the memory,
///   its noise (−93 dBFS) far below hearing in a mix and under a lossy
///   codec's own. Every track by default. A 7-minute track at 48 kHz:
///   81 MB → 40 MB.
/// * `F32`: 32-bit float, for lossless sources (FLAC, WAV, AIFF) with the
///   High-quality decks setting: their full (24-bit) resolution stays.
///
/// The `I16` scale is full scale in steps: [`I16_SCALE`] for tracks; for
/// stems (stored as 16-bit, 6 dB under) a power of two that keeps their
/// stored samples exactly ([`Pcm::load_stem`]).
enum Store {
    F32(Box<[UnsafeCell<f32>]>),
    I16(Box<[UnsafeCell<i16>]>, f32),
}

/// 16-bit full scale, 3 dB under the float's: peaks to +3 dBFS fit.
const I16_SCALE: f32 = 32767.0 * 0.707_945_8;

impl Store {
    /// The samples' memory: start and length in bytes.
    fn span(&self) -> (*const libc::c_void, usize) {
        match self {
            Store::F32(d) => (d.as_ptr().cast(), d.len() * 4),
            Store::I16(d, _) => (d.as_ptr().cast(), d.len() * 2),
        }
    }

    fn len(&self) -> usize {
        match self {
            Store::F32(d) => d.len(),
            Store::I16(d, _) => d.len(),
        }
    }

    /// SAFETY: `i` must be a published sample.
    #[inline]
    unsafe fn at(&self, i: usize) -> f32 {
        // SAFETY: the caller's guarantee.
        unsafe {
            match self {
                Store::F32(d) => *d[i].get(),
                Store::I16(d, scale) => *d[i].get() as f32 / scale,
            }
        }
    }
}

/// Interleaved stereo at the graph rate, filled progressively by one
/// loader thread and read by the deck worker.
pub struct Pcm {
    pub path: PathBuf,
    pub info: SourceInfo,
    pub rate: u32,
    /// Display strings for status. Built with the track, shared by `Arc`.
    pub(crate) labels: TrackLabels,
    data: Store,
    /// Frames decoded so far (release-published by the loader).
    ready: AtomicUsize,
    /// Decoding finished (`ready` is final).
    pub done: AtomicBool,
    /// A replaced/ejected deck asked the loader worker to stop decoding.
    cancelled: AtomicBool,
    /// The samples are locked in RAM ([`Pcm::keep_resident`]).
    locked: AtomicBool,
    pub error: std::sync::Mutex<Option<Arc<str>>>,
}

/// Bytes of decoded audio locked in RAM, across every track and stem.
static LOCKED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// How much decoded audio may be locked: 3 GiB, or an eighth of the RAM on
/// a smaller machine, so pinned tracks never crowd out the rest.
fn lock_budget() -> usize {
    // SAFETY: plain sysconf queries.
    let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
    let ram = (pages.max(0) as usize).saturating_mul(size.max(0) as usize);
    (3usize << 30).min(ram / 8)
}

impl Drop for Pcm {
    fn drop(&mut self) {
        if *self.locked.get_mut() {
            let (ptr, len) = self.data.span();
            // SAFETY: the range this Pcm locked; its memory is still alive.
            unsafe { libc::munlock(ptr, len) };
            LOCKED_BYTES.fetch_sub(len, Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for Pcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pcm")
            .field("path", &self.path)
            .field("frames", &self.frames())
            .finish()
    }
}

// SAFETY: the loader writes only frames ≥ `ready` and publishes them with a
// release store; readers only read frames < `ready` (acquire). No frame is
// ever written after it is published.
unsafe impl Sync for Pcm {}
unsafe impl Send for Pcm {}

impl Pcm {
    /// Decode a cache artifact completely in the caller's background thread.
    /// A whole file at `rate`, every sample × `gain` (a stored stem's level).
    /// Not locked in RAM: offline rehearsals and Derived Material decode
    /// this way too. A deck locks what it is given ([`Shared::send`]).
    ///
    /// [`Shared::send`]: super::worker::Shared::send
    pub fn load_full(path: &Path, rate: u32, gain: f32) -> Result<Arc<Pcm>, String> {
        let output = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(path)
            .args(["-f", "f32le", "-ac", "2", "-ar"])
            .arg(rate.to_string())
            .args(["-af", "aresample=resampler=soxr:precision=28", "pipe:1"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "Stem decode: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let samples: Vec<f32> = output
            .stdout
            .chunks_exact(4)
            .map(|s| f32::from_le_bytes(s.try_into().unwrap()) * gain)
            .collect();
        if samples.is_empty() || samples.iter().any(|s| !s.is_finite()) {
            return Err("Invalid stem samples".into());
        }
        let mut pcm = Self::from_samples(rate, samples);
        let pcm_mut = Arc::get_mut(&mut pcm).unwrap();
        pcm_mut.path = path.to_path_buf();
        pcm_mut.labels = track_labels(&pcm_mut.path, &pcm_mut.info);
        Ok(pcm)
    }
    /// A prepared stem for a deck: [`load_full`], then kept as 16-bit
    /// (half the memory: four stems of a 7-minute track 645 → 323 MB).
    /// Stems are stored as 16-bit 6 dB under full scale, so a power-of-two
    /// scale keeps every stored sample exactly; a louder or resampled stem
    /// gets the largest such scale its peak fits (−90 dB steps or finer).
    ///
    /// [`load_full`]: Pcm::load_full
    pub fn load_stem(path: &Path, rate: u32, gain: f32) -> Result<Arc<Pcm>, String> {
        let full = Self::load_full(path, rate, gain)?;
        Ok(Self::compacted(&full))
    }

    /// A finished float track as 16-bit with an exact power-of-two scale.
    fn compacted(full: &Arc<Pcm>) -> Arc<Pcm> {
        let Some(samples) = full.published() else {
            return full.clone();
        };
        let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let mut scale = 32768.0f32;
        while scale > 1.0 && peak * scale > 32767.0 {
            scale *= 0.5;
        }
        let data = Store::I16(
            samples
                .iter()
                .map(|x| UnsafeCell::new((x * scale).round().clamp(-32768.0, 32767.0) as i16))
                .collect(),
            scale,
        );
        Arc::new(Pcm {
            path: full.path.clone(),
            info: full.info.clone(),
            rate: full.rate,
            labels: full.labels.clone(),
            data,
            ready: AtomicUsize::new(full.frames()),
            done: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            error: std::sync::Mutex::new(None),
        })
    }

    pub fn frames(&self) -> usize {
        self.ready.load(Ordering::Acquire)
    }

    /// Lock the decoded samples in RAM, best effort: a deck renders ahead
    /// from them, and a page the kernel had swapped out would stall that read
    /// on the disk. Needs the realtime group's memlock limit (without it the
    /// lock fails and nothing changes); bounded by [`lock_budget`].
    pub fn keep_resident(&self) {
        let (ptr, len) = self.data.span();
        if len == 0 || self.locked.load(Ordering::Acquire) {
            return;
        }
        let before = LOCKED_BYTES.fetch_add(len, Ordering::Relaxed);
        // SAFETY: the range is this Pcm's own live allocation.
        if before + len > lock_budget() || unsafe { libc::mlock(ptr, len) } != 0 {
            LOCKED_BYTES.fetch_sub(len, Ordering::Relaxed);
            return;
        }
        self.locked.store(true, Ordering::Release);
    }

    /// Whether the samples are locked in RAM.
    pub fn resident(&self) -> bool {
        self.locked.load(Ordering::Acquire)
    }

    /// Bytes of decoded audio locked in RAM, all tracks and stems together.
    pub fn locked_bytes() -> usize {
        LOCKED_BYTES.load(Ordering::Relaxed)
    }

    pub fn capacity_frames(&self) -> usize {
        self.data.len() / 2
    }

    /// Bytes the decoded track takes (the whole capacity).
    pub fn bytes(&self) -> usize {
        match &self.data {
            Store::F32(d) => d.len() * 4,
            Store::I16(d, _) => d.len() * 2,
        }
    }

    /// Frames the track will have when decoded (estimate until done).
    pub fn length(&self) -> usize {
        if self.done.load(Ordering::Acquire) {
            self.frames()
        } else {
            self.capacity_frames().max(self.frames())
        }
    }

    /// The decoded frames so far, interleaved stereo, as one f32 slice
    /// (one atomic read, not one per sample): the float store only; a
    /// 16-bit one is read through `read` into the caller's buffer.
    pub fn published(&self) -> Option<&[f32]> {
        let n = self.frames() * 2;
        match &self.data {
            // SAFETY: frames below `ready` are published and never written
            // again; UnsafeCell<f32> has the layout of f32.
            Store::F32(d) => {
                Some(unsafe { std::slice::from_raw_parts(d.as_ptr() as *const f32, n) })
            }
            Store::I16(..) => None,
        }
    }

    /// Sample of frame `i`, channel `c`; 0 outside what's decoded.
    #[inline]
    pub fn get(&self, i: usize, c: usize) -> f32 {
        if i < self.frames() {
            // SAFETY: published frame, never written again.
            unsafe { self.data.at(i * 2 + c) }
        } else {
            0.0
        }
    }

    /// Copy frames [from, from + n) (interleaved) into `out`; zeros past
    /// the decoded end. Returns frames that were real audio.
    pub fn read(&self, from: usize, out: &mut [f32]) -> usize {
        let n = out.len() / 2;
        let have = self.frames().saturating_sub(from).min(n);
        match &self.data {
            Store::F32(d) => {
                for (o, v) in out[..have * 2]
                    .iter_mut()
                    .zip(&d[from * 2..(from + have) * 2])
                {
                    // SAFETY: as in `get`.
                    *o = unsafe { *v.get() };
                }
            }
            Store::I16(d, scale) => {
                for (o, v) in out[..have * 2]
                    .iter_mut()
                    .zip(&d[from * 2..(from + have) * 2])
                {
                    // SAFETY: as in `get`.
                    *o = unsafe { *v.get() } as f32 / scale;
                }
            }
        }
        out[have * 2..].fill(0.0);
        have
    }

    /// Kept as 16-bit (a lossy source).
    pub fn compact(&self) -> bool {
        matches!(self.data, Store::I16(..))
    }

    /// Request cancellation without waiting; the loader owns and reaps ffmpeg.
    pub fn cancel_decode(&self) {
        if !self.done.load(Ordering::Acquire) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    /// A track from samples already in memory (tests, generators).
    pub fn from_samples(rate: u32, interleaved: Vec<f32>) -> Arc<Pcm> {
        let frames = interleaved.len() / 2;
        let data = Store::F32(interleaved.into_iter().map(UnsafeCell::new).collect());
        let info = SourceInfo {
            rate,
            channels: 2,
            duration: frames as f64 / rate as f64,
            ..Default::default()
        };
        let labels = track_labels(Path::new(""), &info);
        Arc::new(Pcm {
            path: PathBuf::new(),
            info,
            rate,
            labels,
            data,
            ready: AtomicUsize::new(frames),
            done: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            error: std::sync::Mutex::new(None),
        })
    }

    /// A 16-bit track from float samples (tests of the compact store).
    #[cfg(test)]
    pub fn from_samples_i16(rate: u32, interleaved: Vec<f32>) -> Arc<Pcm> {
        let f = Pcm::from_samples(rate, vec![]);
        let frames = interleaved.len() / 2;
        let data = Store::I16(
            interleaved
                .iter()
                .map(|x| UnsafeCell::new((x * I16_SCALE).round().clamp(-32768.0, 32767.0) as i16))
                .collect(),
            I16_SCALE,
        );
        // Pcm has a Drop, so its fields are built anew, not moved out of `f`.
        Arc::new(Pcm {
            path: PathBuf::new(),
            info: SourceInfo {
                duration: frames as f64 / rate as f64,
                ..f.info.clone()
            },
            rate,
            labels: f.labels.clone(),
            data,
            ready: AtomicUsize::new(frames),
            done: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            error: std::sync::Mutex::new(None),
        })
    }

    /// Start decoding `path` at `rate`. Returns at once; frames arrive in
    /// the background.
    pub fn load(path: &Path, rate: u32) -> Result<Arc<Pcm>, String> {
        let info = probe(path)?;
        Self::load_decoded(path, rate, info)
    }

    fn load_decoded(path: &Path, rate: u32, info: SourceInfo) -> Result<Arc<Pcm>, String> {
        // Capacity from the probed duration, with a small margin. A duration
        // past [`MAX_TRACK_SECONDS`] is refused here, before the allocation.
        let frames = capacity_frames(info.duration, rate)?;
        // Lossy sources report no bit depth: 16-bit always. Lossless ones
        // too, unless the setup asks for full resolution.
        let full = info.bits > 0 && patch_core::setup::load().hq_decks;
        let data = if !full {
            Store::I16(zeroed(frames * 2), I16_SCALE)
        } else {
            Store::F32(zeroed(frames * 2))
        };
        let labels = track_labels(path, &info);
        let pcm = Arc::new(Pcm {
            path: path.to_path_buf(),
            info,
            rate,
            labels,
            data,
            ready: AtomicUsize::new(0),
            done: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            error: std::sync::Mutex::new(None),
        });
        // Spawned plainly (posix_spawn): a pre_exec hook forces a full fork
        // of the engine, whose copy-on-write protection of every page (the
        // decks' tracks: hundreds of MB) made the audio thread fault and miss
        // its cycle on every load. Idle scheduling is set on the child after.
        let mut cmd = Command::new("ffmpeg");
        let mut child = cmd
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(path)
            .args([
                "-vn",
                "-ac",
                "2",
                "-af",
                &format!("aresample=resampler=soxr:precision=28:osr={rate}"),
                "-f",
                "f32le",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        // The decoder must never compete with the audio: idle scheduling
        // (spare CPU only), lowest priority, one thread.
        // SAFETY: plain syscalls on our own child's pid.
        unsafe {
            let pid = child.id() as libc::pid_t;
            let p = libc::sched_param { sched_priority: 0 };
            libc::sched_setscheduler(pid, libc::SCHED_IDLE, &p);
            libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, 19);
        }
        let mut out = child.stdout.take().ok_or("ffmpeg: no output")?;
        let p = pcm.clone();
        std::thread::Builder::new()
            .name("patch-deck-load".into())
            .spawn(move || {
                let mut child = child;
                // This thread copies what the decoder makes: low priority too.
                // SAFETY: plain syscalls on the calling thread.
                unsafe {
                    let p = libc::sched_param { sched_priority: 0 };
                    libc::sched_setscheduler(0, libc::SCHED_IDLE, &p);
                }
                let fd = out.as_raw_fd();
                // Nonblocking reads let this worker observe cancellation even
                // when ffmpeg is idle and has no bytes ready to write.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    *p.error.lock().unwrap() =
                        Some(Arc::from("could not prepare deck decoder cancellation"));
                    p.done.store(true, Ordering::Release);
                    return;
                }
                let mut buf = vec![0u8; 1 << 16];
                let mut carry: Vec<u8> = Vec::with_capacity(8);
                let mut joined = Vec::with_capacity(buf.len() + 8);
                let mut frame = 0usize;
                let cap = p.capacity_frames();
                let mut idle_since = std::time::Instant::now();
                let mut stalled = false;
                // TPDF dither for the 16-bit store: two uniform draws, ±1 LSB.
                let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ (p.path.as_os_str().len() as u64);
                let mut uniform = move || {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 40) as f32 / (1u64 << 24) as f32
                };
                let mut to16 = move |x: f32| {
                    (x * I16_SCALE + uniform() - uniform())
                        .round()
                        .clamp(-32768.0, 32767.0) as i16
                };
                loop {
                    if p.cancelled.load(Ordering::Acquire) {
                        let _ = child.kill();
                        break;
                    }
                    let n = match out.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            idle_since = std::time::Instant::now();
                            n
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if idle_since.elapsed() >= DECODE_STALL {
                                stalled = true;
                                let _ = child.kill();
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let mut bytes: &[u8] = &buf[..n];
                    if !carry.is_empty() {
                        joined.clear();
                        joined.extend_from_slice(&carry);
                        joined.extend_from_slice(bytes);
                        bytes = &joined;
                        carry.clear();
                    }
                    let whole = bytes.len() / 8 * 8;
                    for f in bytes[..whole].as_chunks::<8>().0 {
                        if frame >= cap {
                            break;
                        }
                        let l = f32::from_le_bytes([f[0], f[1], f[2], f[3]]);
                        let r = f32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                        // SAFETY: frame ≥ ready: not yet visible to readers.
                        unsafe {
                            match &p.data {
                                Store::F32(d) => {
                                    *d[frame * 2].get() = l;
                                    *d[frame * 2 + 1].get() = r;
                                }
                                Store::I16(d, _) => {
                                    *d[frame * 2].get() = to16(l);
                                    *d[frame * 2 + 1].get() = to16(r);
                                }
                            }
                        }
                        frame += 1;
                    }
                    carry.extend_from_slice(&bytes[whole..]);
                    p.ready.store(frame, Ordering::Release);
                    if frame >= cap {
                        // The pipe is still open. Waiting here deadlocks if
                        // ffmpeg is blocked on a full stdout; kill it first.
                        let _ = child.kill();
                        break;
                    }
                }
                drop(out);
                let _ = child.wait();
                p.done.store(true, Ordering::Release);
                if stalled {
                    *p.error.lock().unwrap() = Some(Arc::from("decoder stalled"));
                } else if frame == 0 {
                    *p.error.lock().unwrap() = Some(Arc::from("nothing decoded"));
                } else if !p.cancelled.load(Ordering::Acquire) {
                    p.keep_resident();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(pcm)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tags_are_read_in_any_case() {
        let flac = serde_json::json!({"TITLE": "Sunrise  (Deetron Soleil AM Remix)", "ARTIST": "Dave Spoon"});
        assert_eq!(super::tag(&flac, "title"), "Sunrise  (Deetron Soleil AM Remix)");
        assert_eq!(super::tag(&flac, "artist"), "Dave Spoon");
        assert_eq!(super::tag(&serde_json::json!({"title": "Afterimage"}), "title"), "Afterimage");
        assert_eq!(super::tag(&serde_json::Value::Null, "title"), "");
    }

    use super::*;

    #[test]
    fn a_stem_kept_as_16_bit_is_exact_and_half_the_size() {
        // A stored stem: 16-bit samples, 6 dB under, played back × 2.
        let x: Vec<f32> = (0..4800)
            .map(|i| ((i as f32 * 0.37).sin() * 32767.0).round() / 32768.0 * 2.0)
            .collect();
        let full = Pcm::from_samples(48_000, x.clone());
        let stem = Pcm::compacted(&full);
        assert!(stem.compact());
        assert_eq!(stem.bytes() * 2, full.bytes());
        assert_eq!(stem.frames(), full.frames());
        let mut out = vec![0.0; x.len()];
        stem.read(0, &mut out);
        assert_eq!(out, x, "every stored sample comes back exactly");
        assert_eq!(stem.get(7, 1), x[15]);
        // A louder one is scaled to fit, never clipped.
        let loud: Vec<f32> = x.iter().map(|v| v * 1.9).collect();
        let stem = Pcm::compacted(&Pcm::from_samples(48_000, loud.clone()));
        stem.read(0, &mut out);
        let err = out.iter().zip(&loud).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err <= 1.0 / 4096.0, "{err}");
    }

    #[test]
    fn resident_tracks_are_locked_once_and_unlocked_on_drop() {
        // A 7-minute stem in f32: 161 MB.
        let pcm = Pcm::from_samples(48_000, vec![0.25; 7 * 60 * 48_000 * 2]);
        let t = std::time::Instant::now();
        pcm.keep_resident();
        pcm.keep_resident();
        let locked = pcm.locked.load(Ordering::Acquire);
        eprintln!("locked {locked} in {:?}", t.elapsed());
        if locked {
            assert!(Pcm::locked_bytes() >= pcm.bytes());
        }
        let t = std::time::Instant::now();
        drop(pcm);
        eprintln!("dropped in {:?}", t.elapsed());
    }

    #[test]
    fn capacity_matches_the_old_margin_and_rejects_an_overlong_duration() {
        let rate = 48_000u32;
        let duration = 125.5;
        let frames = capacity_frames(duration, rate).unwrap();
        assert_eq!(
            frames,
            (duration * f64::from(rate) * 1.01) as usize + rate as usize
        );
        assert!(capacity_frames(0.0, rate).is_err());
        assert!(capacity_frames(-1.0, rate).is_err());
        assert!(capacity_frames(f64::NAN, rate).is_err());
        assert!(capacity_frames(f64::INFINITY, rate).is_err());
        assert!(capacity_frames(MAX_TRACK_SECONDS, rate).is_ok());
        let err = capacity_frames(MAX_TRACK_SECONDS + 0.001, rate).unwrap_err();
        assert!(err.contains("10800"), "{err}");
        // The refusal happens before a buffer exists: a nonexistent path is
        // never handed to ffmpeg.
        let err = Pcm::load_decoded(
            Path::new("/no/such/patch-track"),
            rate,
            SourceInfo {
                duration: MAX_TRACK_SECONDS + 10.0,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("10800"), "{err}");
        assert!(!err.contains("ffmpeg"), "{err}");
    }

    #[test]
    fn a_stuck_probe_is_killed_and_stdout_is_capped() {
        let dir = std::env::temp_dir().join(format!(
            "patch-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("hang");
        let c_path =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(fifo.as_os_str()))
                .unwrap();
        // SAFETY: `c_path` is a NUL-terminated path in a directory we created.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        let err = probe_within(&fifo, Duration::from_millis(400)).unwrap_err();
        let elapsed = started.elapsed();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            elapsed < Duration::from_secs(2),
            "probe ran for {elapsed:?}: {err}"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "probe returned before its deadline ({elapsed:?}): {err}"
        );

        let blob = dir.join("blob");
        std::fs::write(&blob, vec![b'x'; 256 * 1024]).unwrap();
        let mut cmd = Command::new("cat");
        cmd.arg(&blob);
        let err = bounded_output(&mut cmd, 64 * 1024, Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");

        let wav = dir.join("tone.wav");
        write_pcm_wav(&wav, 48_000, 12_000);
        let info = probe(&wav).unwrap();
        assert!((info.duration - 0.25).abs() < 0.02, "{}", info.duration);
        assert_eq!(info.rate, 48_000);
        assert_eq!(info.channels, 2);
        assert!(!info.codec.is_empty(), "{info:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn write_pcm_wav(path: &Path, rate: u32, frames: u32) {
        let data_bytes = frames * 4;
        let mut data = Vec::with_capacity(44 + data_bytes as usize);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(rate * 4).to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..frames {
            let sample = ((i as f32 * 0.05).sin() * 8000.0) as i16;
            data.extend_from_slice(&sample.to_le_bytes());
            data.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }
}
