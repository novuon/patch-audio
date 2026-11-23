//! The recorder process.
//!
//! Started detached (`setsid`) by Patch, so a take keeps recording when
//! Patch's UI restarts or crashes. One reader thread does nothing but drain
//! the capture pipe, so capture never waits on the disk; the writer thread
//! converts, writes, meters loudness and syncs the file every second.
//! Controlled over `$XDG_RUNTIME_DIR/omarchy-patch/recorder.sock`:
//! `status` → one JSON line, `stop` → finish the file and exit.

use crate::wav::{BextLoudness, SampleFormat, WavWriter};
use crate::{ConvStats, Dither, convert_s24};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub fn socket_path() -> PathBuf {
    patch_core::ipc::runtime_dir().join("recorder.sock")
}

/// What to record: a PipeWire source (e.g. `patch.monitor`) or a stream.
#[derive(Debug, Clone)]
pub enum Source {
    Program,
    Device(String),
    Stream(u32),
}

impl Source {
    fn parec_arg(&self) -> String {
        match self {
            Source::Program => String::new(),
            Source::Device(d) => format!("--device={d}"),
            Source::Stream(i) => format!("--monitor-stream={i}"),
        }
    }

    pub fn encode(&self) -> String {
        match self {
            Source::Program => "program:mix".into(),
            Source::Device(d) => format!("device:{d}"),
            Source::Stream(i) => format!("stream:{i}"),
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        let (k, v) = s.split_once(':')?;
        match k {
            "program" if v == "mix" => Some(Source::Program),
            "device" if !v.is_empty() => Some(Source::Device(v.to_string())),
            "stream" => v.parse().ok().map(Source::Stream),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Take {
    pub source: Source,
    pub label: String,
    pub format: SampleFormat,
    pub rate: u32,
    pub path: PathBuf,
}

#[derive(Default)]
struct Shared {
    frames: AtomicU64,
    clips: AtomicU64,
    dropouts: AtomicU64,
    stop: AtomicBool,
    paused: AtomicBool,
    paused_frames: AtomicU64,
    peak: Mutex<f32>,
    level: Mutex<f32>,
    loudness: Mutex<Option<BextLoudness>>,
    error: Mutex<Option<String>>,
}

struct CaptureChild(std::process::Child);
impl Drop for CaptureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct SocketCleanup(PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run a take until `stop`. This is the body of `omarchy-patch record-daemon`.
pub fn run(take: Take) -> std::io::Result<()> {
    if matches!(take.source, Source::Program) {
        return crate::native::run(take);
    }
    let sock = socket_path();
    std::fs::create_dir_all(sock.parent().unwrap())?;
    if UnixStream::connect(&sock).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "recorder already running",
        ));
    }
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sock.with_extension("lock"))?;
    lock.try_lock()
        .map_err(|e| std::io::Error::other(format!("recorder already running: {e}")))?;
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    listener.set_nonblocking(true)?;
    patch_core::ipc::private_socket(&sock);
    let _cleanup = SocketCleanup(sock.clone());
    // The bar's icon shows a take running with every window closed.
    let _beat = patch_core::ipc::heartbeat("recording");
    if let Some(dir) = take.path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut wav = WavWriter::create(
        &take.path,
        take.format,
        2,
        take.rate,
        &format!("{} · {}", take.label, take.format.label()),
    )?;
    wav.sync()?;
    let mut child = CaptureChild(
        Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--channels=2",
                "--latency-msec=10",
                "--client-name=Patch recorder",
                "--stream-name=Patch recorder",
            ])
            .arg(format!("--rate={}", take.rate))
            .arg(take.source.parec_arg())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut pipe = child.0.stdout.take().expect("piped");
    let mut capture_stderr = child.0.stderr.take().expect("piped");
    // Drain stderr continuously so a failing capture source cannot block on
    // a full pipe, and retain its diagnostic for the recorder result shown in
    // the UI.
    let stderr_reader = std::thread::Builder::new()
        .name("patch-rec-stderr".into())
        .spawn(move || {
            let mut tail = Vec::with_capacity(8192);
            let mut buf = [0u8; 1024];
            loop {
                match capture_stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > 8192 {
                            tail.drain(..tail.len() - 8192);
                        }
                    }
                }
            }
            String::from_utf8_lossy(&tail).trim().to_owned()
        })?;

    let shared = Arc::new(Shared::default());
    // Bounded to 8 MiB: a stalled disk cannot consume all system memory.
    let (tx, rx) = mpsc::sync_channel::<(Vec<u8>, usize, bool)>(128);
    let (recycle, free) = mpsc::sync_channel::<Vec<u8>>(128);
    let read_state = shared.clone();

    // Reader: only drains the pipe.
    let reader = std::thread::Builder::new()
        .name("patch-rec-read".into())
        .spawn(move || {
            let mut allocated = 0;
            loop {
                let mut buf = match free.try_recv() {
                    Ok(buf) => buf,
                    Err(_) if allocated < 128 => {
                        allocated += 1;
                        vec![0u8; 1 << 16]
                    }
                    Err(_) => {
                        *read_state.error.lock().unwrap() =
                            Some("Disk cannot keep up: capture queue full; take stopped".into());
                        read_state.stop.store(true, Ordering::Relaxed);
                        break;
                    }
                };
                let n = match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        *read_state.error.lock().unwrap() =
                            Some(format!("Capture read failed: {e}"));
                        read_state.stop.store(true, Ordering::Relaxed);
                        break;
                    }
                };
                if tx
                    .send((buf, n, read_state.paused.load(Ordering::Relaxed)))
                    .is_err()
                {
                    break;
                }
            }
        })?;

    // Writer: convert, write, meter, sync.
    let st = shared.clone();
    let t = take.clone();
    let writer = std::thread::Builder::new()
        .name("patch-rec-write".into())
        .spawn(move || {
            let mut wav = wav;
            let mut dither = Dither::tpdf(0x9e37_79b9 ^ std::process::id());
            let mut conv = ConvStats::default();
            let mut loud = patch_meter::Loudness::new(t.rate);
            let (mut max_m, mut max_s) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            let mut max_tp = 0.0f32;
            let mut carry: Vec<u8> = vec![];
            let mut samples: Vec<f32> = vec![];
            let mut out: Vec<u8> = Vec::with_capacity(1 << 17);
            let started = Instant::now();
            let mut last_sync = Instant::now();
            let mut deficit_flag = false;
            loop {
                let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if let Some((c, n, paused)) = chunk {
                    carry.extend_from_slice(&c[..n]);
                    let _ = recycle.try_send(c);
                    let whole = carry.len() - carry.len() % 8;
                    samples.clear();
                    samples.extend(
                        carry[..whole]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                    carry.drain(..whole);
                    if paused {
                        *st.level.lock().unwrap() = 0.0;
                        st.paused_frames
                            .fetch_add(samples.len() as u64 / 2, Ordering::Relaxed);
                    } else {
                        out.clear();
                        match t.format {
                            SampleFormat::F32 => {
                                let level = patch_meter::stats(&samples);
                                *st.level.lock().unwrap() = level.peak[0].max(level.peak[1]);
                                conv.peak = conv.peak.max(level.peak[0].max(level.peak[1]));
                                conv.clips +=
                                    samples.iter().filter(|x| x.abs() > 1.0).count() as u64;
                                out.extend(samples.iter().flat_map(|x| x.to_le_bytes()));
                            }
                            SampleFormat::S24 => {
                                let mut block = ConvStats::default();
                                convert_s24(&samples, &mut dither, &mut block, &mut out);
                                conv.clips += block.clips;
                                conv.peak = conv.peak.max(block.peak);
                                *st.level.lock().unwrap() = block.peak;
                            }
                        }
                        if let Err(e) = wav.write(&out) {
                            *st.error.lock().unwrap() = Some(e.to_string());
                            break;
                        }
                        loud.push(&samples);
                        max_m = max_m.max(loud.momentary());
                        max_s = max_s.max(loud.short_term());
                        st.frames.store(wav.frames(), Ordering::Relaxed);
                        st.clips.store(conv.clips, Ordering::Relaxed);
                        *st.peak.lock().unwrap() = conv.peak;
                    }
                }
                // Dropout: the source delivered noticeably fewer frames than the
                // clock says it should have (e.g. a suspended or vanished device).
                let expected = started.elapsed().as_secs_f64() * t.rate as f64
                    - st.paused_frames.load(Ordering::Relaxed) as f64;
                let behind = expected - wav.frames() as f64 > t.rate as f64 * 0.25;
                if behind && !deficit_flag {
                    st.dropouts.fetch_add(1, Ordering::Relaxed);
                }
                deficit_flag = behind;
                if last_sync.elapsed() >= Duration::from_secs(1) {
                    last_sync = Instant::now();
                    if let Err(e) = wav.sync() {
                        *st.error.lock().unwrap() = Some(format!("Could not sync take: {e}"));
                        st.stop.store(true, Ordering::Relaxed);
                    }
                    let tp = loud.true_peak.take();
                    max_tp = max_tp.max(tp[0].max(tp[1]));
                    *st.loudness.lock().unwrap() = Some(BextLoudness {
                        integrated: loud.integrated() as f32,
                        range: loud.loudness_range() as f32,
                        max_true_peak: 20.0 * max_tp.max(1e-9).log10(),
                        max_momentary: max_m as f32,
                        max_short_term: max_s as f32,
                    });
                }
            }
            if !carry.is_empty() {
                *st.error.lock().unwrap() =
                    Some("Capture ended with an incomplete audio frame.".into());
            }
            let tp = loud.true_peak.take();
            max_tp = max_tp.max(tp[0].max(tp[1]));
            let l = Some(BextLoudness {
                integrated: loud.integrated() as f32,
                range: loud.loudness_range() as f32,
                max_true_peak: 20.0 * max_tp.max(1e-9).log10(),
                max_momentary: max_m as f32,
                max_short_term: max_s as f32,
            });
            *st.loudness.lock().unwrap() = l;
            if let Err(e) = wav.finish(l) {
                *st.error.lock().unwrap() = Some(format!("Could not finish take: {e}"));
            }
        })?;

    // Control socket (non-blocking accept so we can notice the writer ending).
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, &shared, &take, started),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => break,
        }
        if shared.stop.load(Ordering::Relaxed) || writer.is_finished() {
            break;
        }
    }
    let requested_stop = shared.stop.load(Ordering::Relaxed);
    shared.stop.store(true, Ordering::Relaxed);
    let _ = child.0.kill();
    let _ = child.0.wait();
    let capture_error = stderr_reader.join().unwrap_or_default();
    let reader_ok = reader.join().is_ok();
    let writer_ok = writer.join().is_ok();
    if !reader_ok || !writer_ok {
        *shared.error.lock().unwrap() =
            Some("Recorder worker failed; take may be incomplete.".into());
    } else if !requested_stop && shared.error.lock().unwrap().is_none() {
        *shared.error.lock().unwrap() = Some(if capture_error.is_empty() {
            "Capture source ended unexpectedly.".into()
        } else {
            format!("Capture source ended unexpectedly: {capture_error}")
        });
    }
    let error = shared.error.lock().unwrap().clone();
    // Retain the final outcome after the socket closes, including failures.
    let result_tmp = sock.with_extension("result.tmp");
    let _ = std::fs::write(&result_tmp, serde_json::json!({
        "path": take.path, "frames": shared.frames.load(Ordering::Relaxed), "error": error,
        "rate": take.rate, "seconds": shared.frames.load(Ordering::Relaxed) as f64 / take.rate as f64,
        "format": take.format.label(), "bytes": std::fs::metadata(&take.path).map(|m| m.len()).unwrap_or(0),
        "peak": *shared.peak.lock().unwrap(), "clips": shared.clips.load(Ordering::Relaxed),
        "dropouts": shared.dropouts.load(Ordering::Relaxed), "integrated": shared.loudness.lock().unwrap().map(|l| l.integrated)
    }).to_string());
    let _ = std::fs::rename(result_tmp, sock.with_extension("result.json"));
    let _ = std::fs::remove_file(&sock);
    error.map_or(Ok(()), |e| Err(std::io::Error::other(e)))
}

fn handle(stream: UnixStream, s: &Shared, take: &Take, started: Instant) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
    let mut line = String::new();
    let mut r = BufReader::new(match stream.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    if r.read_line(&mut line).is_err() {
        return;
    }
    let mut w = stream;
    match line.trim() {
        "pause" => {
            s.paused.store(true, Ordering::Relaxed);
            let _ = writeln!(w, "ok");
        }
        "resume" => {
            s.paused.store(false, Ordering::Relaxed);
            let _ = writeln!(w, "ok");
        }
        "stop" => {
            s.stop.store(true, Ordering::Relaxed);
            let _ = writeln!(w, "ok");
        }
        _ => {
            let frames = s.frames.load(Ordering::Relaxed);
            let l = *s.loudness.lock().unwrap();
            let status = serde_json::json!({
                "recording": true,
                "paused": s.paused.load(Ordering::Relaxed),
                "source": take.source.encode(),
                "path": take.path.display().to_string(),
                "label": take.label,
                "format": take.format.label(),
                "rate": take.rate,
                "frames": frames,
                "seconds": frames as f64 / take.rate as f64,
                "wall": started.elapsed().as_secs_f64(),
                "bytes": std::fs::metadata(&take.path).map(|m| m.len()).unwrap_or(0),
                "peak": *s.peak.lock().unwrap(),
                "level": *s.level.lock().unwrap(),
                "clips": s.clips.load(Ordering::Relaxed),
                "dropouts": s.dropouts.load(Ordering::Relaxed),
                "integrated": l.map(|l| l.integrated),
                "error": s.error.lock().unwrap().clone(),
            });
            let _ = writeln!(w, "{status}");
        }
    }
}

// ---- client side (used by Patch's UI and CLI) -----------------------------

fn call(req: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(300))).ok()?;
    writeln!(s, "{req}").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    Some(line)
}

pub fn status() -> Option<serde_json::Value> {
    call("status").and_then(|l| serde_json::from_str(&l).ok())
}

pub fn pause(paused: bool) -> bool {
    call(if paused { "pause" } else { "resume" }).is_some()
}

pub fn stop() -> bool {
    call("stop").is_some()
}

/// Stop and wait for the writer to finalize, with a bounded deadline. Socket
/// removal follows the atomic outcome file, so callers never report a take as
/// saved while queued audio is still being written.
pub fn stop_and_wait() -> Result<serde_json::Value, String> {
    if !stop() {
        return Err("not recording".into());
    }
    let sock = socket_path();
    for _ in 0..200 {
        if !sock.exists() {
            let result: serde_json::Value = std::fs::read(sock.with_extension("result.json"))
                .map_err(|e| format!("Could not read take outcome: {e}"))
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))?;
            if let Some(error) = result["error"].as_str() {
                return Err(error.to_owned());
            }
            return Ok(result);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("Recorder is still finalizing the take.".into())
}

/// Start a detached recorder process (`<exe> record-daemon …`).
pub fn start(exe: &Path, take: &Take) -> Result<(), String> {
    if status().is_some() {
        return Err("already recording".into());
    }
    let fmt = if take.format == SampleFormat::F32 {
        "f32"
    } else {
        "s24"
    };
    Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .args([
            "record-daemon",
            &take.source.encode(),
            fmt,
            &take.rate.to_string(),
        ])
        .arg(&take.path)
        .arg(&take.label)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    for _ in 0..60 {
        if status().is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("the recorder didn't start".into())
}
