//! The broadcast process.
//!
//! Started detached (`setsid`) by Patch, so a stream keeps running when
//! Patch's UI restarts or crashes. One reader thread drains the PipeWire
//! capture (parec), meters it and feeds the encoder (ffmpeg); the control
//! thread answers the socket. Controlled over
//! `$XDG_RUNTIME_DIR/omarchy-patch/broadcast.sock`:
//! `status` → one JSON line, `stop` → end the stream and exit. How it ended
//! is written beside the socket (`broadcast.result.json`), so a broadcast
//! that failed with no window open still says why.

use crate::video::{self, Picture};
use patch_meter::stats;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn socket_path() -> PathBuf {
    patch_core::ipc::runtime_dir().join("broadcast.sock")
}

fn outcome_path() -> PathBuf {
    socket_path().with_extension("result.json")
}

/// What to broadcast: a PipeWire source (bus monitor, output monitor, a
/// stream) the way the Recorder names it, or the engine's own program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A node the PipeWire graph exposes, by name.
    Device(String),
    /// One application's output stream, by its PipeWire index.
    Stream(u32),
}

impl Source {
    fn capture_args(&self) -> Vec<String> {
        match self {
            Source::Device(d) => vec![format!("--device={d}")],
            Source::Stream(i) => vec![format!("--monitor-stream={i}")],
        }
    }

    pub fn encode(&self) -> String {
        match self {
            Source::Device(d) => format!("device:{d}"),
            Source::Stream(i) => format!("stream:{i}"),
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        let (k, v) = s.split_once(':')?;
        match k {
            "device" if !v.is_empty() => Some(Source::Device(v.to_string())),
            "stream" => v.parse().ok().map(Source::Stream),
            _ => None,
        }
    }
}

/// Where the stream goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An Icecast/HTTP server (`icecast://` under the hood).
    Icecast,
    /// An RTMP endpoint (YouTube, Twitch, a local nginx-rtmp).
    Rtmp,
    /// A file or HLS folder on this machine.
    File,
}

impl Kind {
    pub fn encode(self) -> &'static str {
        match self {
            Kind::Icecast => "icecast",
            Kind::Rtmp => "rtmp",
            Kind::File => "file",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Icecast => "Icecast",
            Kind::Rtmp => "RTMP",
            Kind::File => "File",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "icecast" => Kind::Icecast,
            "rtmp" => Kind::Rtmp,
            "file" => Kind::File,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Opus,
    Mp3,
    Aac,
}

impl Codec {
    pub fn encode(self) -> &'static str {
        match self {
            Codec::Opus => "opus",
            Codec::Mp3 => "mp3",
            Codec::Aac => "aac",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Codec::Opus => "Opus",
            Codec::Mp3 => "MP3",
            Codec::Aac => "AAC",
        }
    }
    pub fn parse(s: &str) -> Option<Codec> {
        Some(match s {
            "opus" => Codec::Opus,
            "mp3" => Codec::Mp3,
            "aac" => Codec::Aac,
            _ => return None,
        })
    }
}

/// One broadcast: what to send, where, and how it is coded.
#[derive(Debug, Clone)]
pub struct Config {
    pub source: Source,
    pub label: String,
    pub kind: Kind,
    pub target: String,
    pub mount: String,
    pub user: String,
    pub password: String,
    /// The RTMP stream key, added to `target` as its last path segment
    /// (empty: `target` is the whole ingest URL, key included).
    pub key: String,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub rate: u32,
    pub channels: u16,
    pub name: String,
    /// The picture an RTMP stream carries (YouTube and Twitch need one);
    /// `None`: audio only. Never for Icecast or a file.
    pub picture: Option<Picture>,
}

/// How the picture is coded: NVIDIA's encoder when this machine has one
/// that works (no CPU next to the live audio), else x264.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoEncoder {
    Nvenc,
    X264,
}

impl VideoEncoder {
    pub fn encode(self) -> &'static str {
        match self {
            VideoEncoder::Nvenc => "h264_nvenc",
            VideoEncoder::X264 => "libx264",
        }
    }

    /// Tried once per broadcast: a few black frames through NVENC.
    /// `PATCH_BROADCAST_H264` (`libx264`, `h264_nvenc`) decides instead.
    fn probe() -> VideoEncoder {
        match std::env::var("PATCH_BROADCAST_H264").as_deref() {
            Ok("libx264") => return VideoEncoder::X264,
            Ok("h264_nvenc") => return VideoEncoder::Nvenc,
            _ => {}
        }
        let child = Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i",
                "color=c=black:s=1280x720:r=30", "-frames:v", "3", "-c:v", "h264_nvenc", "-f",
                "null", "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return VideoEncoder::X264 };
        let until = Instant::now() + Duration::from_secs(8);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return VideoEncoder::Nvenc,
                Ok(Some(_)) | Err(_) => return VideoEncoder::X264,
                Ok(None) if Instant::now() >= until => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return VideoEncoder::X264;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// Live H.264 at 1280×720, 30 fps: 4 Mb/s constant rate, a keyframe
    /// every 2 s (YouTube and Twitch ask for ≤ 2–4 s), no lookahead delay.
    fn args(self) -> Vec<String> {
        let mut a: Vec<&str> = match self {
            VideoEncoder::Nvenc => vec![
                "-c:v", "h264_nvenc", "-preset", "p4", "-tune", "ll", "-rc", "cbr",
            ],
            VideoEncoder::X264 => vec![
                "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
                "-keyint_min", "60", "-sc_threshold", "0",
            ],
        };
        a.extend([
            "-b:v", "4000k", "-maxrate", "4000k", "-bufsize", "8000k", "-g", "60",
            "-profile:v", "high",
            // The window's RGB to HD video's own colours (BT.709, TV range),
            // tagged so players show them as drawn.
            "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
            "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
            "-color_range", "tv",
        ]);
        a.into_iter().map(String::from).collect()
    }
}

/// Icecast's own name for the source user when none is given.
const ICECAST_USER: &str = "source";

impl Config {
    /// The destination URL ffmpeg pushes to (credentials inside for Icecast,
    /// the stream key for RTMP).
    fn destination(&self) -> String {
        match self.kind {
            Kind::File => self.target.clone(),
            Kind::Rtmp => self.rtmp_url(),
            Kind::Icecast => {
                let auth = if self.user.is_empty() && self.password.is_empty() {
                    String::new()
                } else {
                    // Escaped: ffmpeg decodes them, and a raw `/`, `#` or `?`
                    // in a password would end the address there.
                    format!(
                        "{}:{}@",
                        url_escape(self.icecast_user()),
                        url_escape(&self.password)
                    )
                };
                format!("icecast://{auth}{}/{}", self.icecast_host(), self.icecast_mount())
            }
        }
    }

    /// A password with no user is still sent: as Icecast's `source`.
    fn icecast_user(&self) -> &str {
        if self.user.is_empty() { ICECAST_USER } else { &self.user }
    }

    fn icecast_host(&self) -> &str {
        let host = self
            .target
            .trim()
            .trim_start_matches("icecast://")
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        // Credentials typed into the address are never sent twice (or shown).
        host.rsplit('@').next().unwrap_or(host)
    }

    fn icecast_mount(&self) -> &str {
        self.mount.trim().trim_start_matches('/')
    }

    /// `https://` asks for TLS: it is never quietly sent in the clear.
    fn tls(&self) -> bool {
        self.kind == Kind::Icecast && self.target.trim_start().starts_with("https://")
    }

    fn rtmp_url(&self) -> String {
        let key = self.key.trim();
        if key.is_empty() {
            self.target.trim().to_owned()
        } else {
            format!("{}/{}", self.target.trim().trim_end_matches('/'), key.trim_start_matches('/'))
        }
    }

    /// The destination with any credentials masked, for the UI and logs.
    pub fn display_destination(&self) -> String {
        match self.kind {
            Kind::File => self.target.clone(),
            Kind::Rtmp => mask_rtmp(&self.rtmp_url()),
            Kind::Icecast => {
                let (host, mount) = (self.icecast_host(), self.icecast_mount());
                if self.user.is_empty() && self.password.is_empty() {
                    format!("{host}/{mount}")
                } else {
                    format!("{}@{host}/{mount}", self.icecast_user())
                }
            }
        }
    }

    /// ffmpeg's own words with the credentials taken out: it prints the URL
    /// it was given (password, stream key) when it fails.
    fn redact(&self, text: &str) -> String {
        let mut out = text.replace(&self.destination(), &self.display_destination());
        if self.kind == Kind::Rtmp {
            out = out.replace(&self.rtmp_url(), &self.display_destination());
        }
        for secret in [
            self.password.clone(),
            url_escape(&self.password),
            self.key.trim().to_owned(),
        ] {
            // A secret this short would garble every word it occurs in.
            if secret.chars().count() >= 4 {
                out = out.replace(&secret, "••••");
            }
        }
        out
    }

    /// The picture this stream carries (RTMP only).
    fn picture(&self) -> Option<Picture> {
        if self.kind == Kind::Rtmp { self.picture } else { None }
    }

    /// RTMP cannot carry Opus in practice: an FLV stream is AAC.
    fn effective_codec(&self) -> Codec {
        match self.kind {
            Kind::Rtmp => Codec::Aac,
            _ => self.codec,
        }
    }

    fn muxer(&self) -> &'static str {
        match (self.effective_codec(), self.kind) {
            (Codec::Opus, _) => "ogg",
            (Codec::Mp3, _) => "mp3",
            (Codec::Aac, Kind::Rtmp) => "flv",
            (Codec::Aac, _) => "adts",
        }
    }

    fn encoder(&self) -> &'static str {
        match self.effective_codec() {
            Codec::Opus => "libopus",
            Codec::Mp3 => "libmp3lame",
            Codec::Aac => "aac",
        }
    }

    /// The full ffmpeg argument list (`video`: how the picture is coded,
    /// when the stream carries one).
    fn encoder_args(&self, video: Option<VideoEncoder>) -> Vec<String> {
        let video = video.filter(|_| self.picture().is_some());
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "warning".into(),
            "-nostdin".into(),
            "-nostats".into(),
            // What the destination has taken, twice a second on stdout: a
            // server has accepted the stream once any bytes reach it.
            "-progress".into(),
            "pipe:1".into(),
            "-stats_period".into(),
            "0.5".into(),
            // (No -thread_queue_size: ffmpeg 9 refuses it on an input, and
            // every broadcast failed with "Invalid argument".)
            "-f".into(),
            "f32le".into(),
            "-ar".into(),
            self.rate.to_string(),
            "-ac".into(),
            self.channels.to_string(),
            "-ch_layout".into(),
            if self.channels == 1 { "mono" } else { "stereo" }.into(),
            "-i".into(),
            "pipe:0".into(),
        ];
        if let Some(v) = video {
            // The picture, paced onto the audio clock by the broadcast
            // process (`video::pace`): R, G, B, unused, 30 frames a second.
            args.extend(
                [
                    // (ffmpeg's own queue of 8 frames: ~30 MB at most.)
                    "-f", "rawvideo", "-pix_fmt", "rgb0",
                    "-video_size", format!("{}x{}", video::WIDTH, video::HEIGHT).as_str(),
                    "-framerate", video::FPS.to_string().as_str(),
                    "-i", format!("unix:{}", video::encoder_socket().display()).as_str(),
                    "-map", "1:v:0", "-map", "0:a:0",
                ]
                .map(String::from),
            );
            args.extend(v.args());
        }
        args.extend([
            // Never clip the encoder: a little headroom before coding.
            // `level=0`: alimiter's auto level would scale its output back
            // up to full scale (and lift everything by 1/limit).
            "-af".into(),
            "alimiter=limit=0.97:level=0".into(),
            "-c:a".into(),
            self.encoder().into(),
            "-b:a".into(),
            format!("{}k", self.bitrate_kbps.max(32)),
        ]);
        if self.kind != Kind::File {
            // A server that stops answering ends this encoder instead of
            // holding the stream (and its stop) forever; a stream that was
            // up then reconnects.
            args.push("-rw_timeout".into());
            args.push("10000000".into());
        }
        args.push("-f".into());
        args.push(self.muxer().into());
        if self.kind == Kind::Rtmp {
            // A live FLV has no length to write back at the end.
            args.push("-flvflags".into());
            args.push("no_duration_filesize".into());
        }
        if self.kind == Kind::Icecast {
            // Icecast picks the content type by the muxer name it gets.
            let content = match self.effective_codec() {
                Codec::Opus => "application/ogg",
                Codec::Mp3 => "audio/mpeg",
                Codec::Aac => "audio/aac",
            };
            args.push("-content_type".into());
            args.push(content.into());
            if !self.name.is_empty() {
                args.push("-ice_name".into());
                args.push(self.name.clone());
            }
            if self.tls() {
                args.push("-tls".into());
                args.push("1".into());
            }
        }
        args.push(self.destination());
        args
    }
}

/// Percent-escape everything but RFC 3986's unreserved characters.
fn url_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `rtmp://host/app/KEY?x=y` → `host/app/••••`: a stream key is a password
/// (anyone holding it can stream as you), so only the server shows.
fn mask_rtmp(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let app = path.split(['/', '?']).next().unwrap_or("");
    match (app.is_empty(), path.len() > app.len()) {
        (true, _) => host.to_owned(),
        (false, false) => format!("{host}/{app}"),
        (false, true) => format!("{host}/{app}/••••"),
    }
}

/// A file destination never overwrites a set already there:
/// `set.ogg` taken → `set-2.ogg`, then `set-3.ogg`, …
fn free_path(path: &str) -> String {
    let p = Path::new(path);
    if !p.exists() {
        return path.to_owned();
    }
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("broadcast");
    let ext = p.extension().and_then(|s| s.to_str());
    (2..10_000u32)
        .map(|n| {
            p.with_file_name(match ext {
                Some(e) => format!("{stem}-{n}.{e}"),
                None => format!("{stem}-{n}"),
            })
        })
        .find(|c| !c.exists())
        .map_or_else(|| path.to_owned(), |c| c.to_string_lossy().into_owned())
}

/// How long to wait before reconnect attempt `attempt` (0-based, counted
/// per outage): 1, 2, 4, 8, then every 15 s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(4)).min(15))
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Notices the capture falling behind the clock: a suspended or vanished
/// source delivers well under what the time says it should have. Each gap
/// counts once and the clock starts again from it, so a later gap counts
/// too; while the audio is on time it starts again every 10 s, so the drift
/// between the sound card's clock and this one never adds up to a gap.
struct GapDetector {
    rate: f64,
    frames: u64,
    base: Option<(Instant, u64)>,
}

impl GapDetector {
    fn new(rate: u32) -> GapDetector {
        GapDetector { rate: rate.max(1) as f64, frames: 0, base: None }
    }

    /// `frames` more arrived at `now`; true when they came after a gap.
    fn observe(&mut self, now: Instant, frames: u64) -> bool {
        self.frames += frames;
        // The clock starts with the first audio, not with the capture
        // starting up.
        let Some((at, base)) = self.base else {
            self.base = Some((now, self.frames));
            return false;
        };
        let elapsed = now.saturating_duration_since(at);
        let behind = elapsed.as_secs_f64() * self.rate - (self.frames - base) as f64;
        if behind > self.rate * 0.25 {
            self.base = Some((now, self.frames));
            return true;
        }
        if behind < self.rate * 0.05 && elapsed > Duration::from_secs(10) {
            self.base = Some((now, self.frames));
        }
        false
    }
}

#[derive(Default)]
struct Shared {
    /// Audio handed to an encoder (what went out), in frames.
    frames: AtomicU64,
    /// The same, counted from the current encoder's start: the clock the
    /// picture is paced on.
    encoder_frames: Arc<AtomicU64>,
    /// Pictures handed to an encoder.
    video_frames: Arc<AtomicU64>,
    /// Bytes the destination has taken, every encoder together.
    bytes: AtomicU64,
    dropouts: AtomicU64,
    stop: AtomicBool,
    /// The current encoder's destination has taken the stream.
    accepted: AtomicBool,
    /// A network stream lost its server and is coming back (`note` says why).
    reconnecting: AtomicBool,
    reconnects: AtomicU64,
    level: Mutex<f32>,
    peak: Mutex<f32>,
    error: Mutex<Option<String>>,
}

/// The encoder's stdin, while one runs. Shared so the reader can write
/// without holding the lock: a write into an encoder that stopped reading
/// (a stalled server) must never block the stop.
type EncoderInput = Arc<Mutex<Option<Arc<ChildStdin>>>>;

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// One ffmpeg run: the process and the threads draining its diagnostics
/// (stderr) and progress (stdout).
struct Encoder {
    child: Option<Child>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Encoder {
    /// Start the encoder (raw float32le on stdin → the destination), hand
    /// its stdin to the reader and drain its diagnostics into `tail` (a
    /// failing push never blocks on a full pipe; the tail is what the UI
    /// shows) and its progress into `shared`.
    fn spawn(
        config: &Config,
        video: Option<VideoEncoder>,
        input: &EncoderInput,
        tail: &Arc<Mutex<String>>,
        shared: &Arc<Shared>,
    ) -> std::io::Result<Encoder> {
        shared.accepted.store(false, Ordering::Relaxed);
        shared.encoder_frames.store(0, Ordering::Relaxed);
        let mut child = Command::new("ffmpeg")
            .args(config.encoder_args(video))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| std::io::Error::other(format!("could not start ffmpeg: {e}")))?;
        *input.lock().unwrap() = child.stdin.take().map(Arc::new);
        let mut err = child.stderr.take().expect("piped");
        let out = child.stdout.take().expect("piped");
        let mut guard = Encoder { child: Some(child), threads: Vec::new() };
        let tail = tail.clone();
        guard.threads.push(
            std::thread::Builder::new()
                .name("patch-bc-stderr".into())
                .spawn(move || {
                    let mut buf = [0u8; 1024];
                    loop {
                        match err.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut t) = tail.lock() {
                                    t.push_str(&String::from_utf8_lossy(&buf[..n]));
                                    keep_tail(&mut t, 4096);
                                }
                            }
                        }
                    }
                })?,
        );
        let shared = shared.clone();
        guard.threads.push(
            std::thread::Builder::new()
                .name("patch-bc-progress".into())
                .spawn(move || {
                    let mut last = 0u64;
                    for line in BufReader::new(out).lines() {
                        let Ok(line) = line else { break };
                        let Some(total) = progress_bytes(&line) else { continue };
                        if total > last {
                            shared.bytes.fetch_add(total - last, Ordering::Relaxed);
                            last = total;
                        }
                        if total > 0 && !shared.accepted.swap(true, Ordering::Relaxed) {
                            shared.reconnecting.store(false, Ordering::Relaxed);
                        }
                    }
                })?,
        );
        Ok(guard)
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Let the encoder end by itself (its input closed: a file gets its last
    /// pages) for up to `grace`, then kill it; either way its threads are
    /// joined when this returns.
    fn finish(mut self, grace: Duration) {
        let until = Instant::now() + grace;
        while self.running() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// `total_size=123` from ffmpeg's progress (`N/A` before any output).
fn progress_bytes(line: &str) -> Option<u64> {
    line.trim().strip_prefix("total_size=")?.parse().ok()
}

/// Keep the last `max` bytes of `t`, from a line start and on a character
/// boundary (draining mid-character would panic, and panics abort).
fn keep_tail(t: &mut String, max: usize) {
    if t.len() <= max {
        return;
    }
    let mut cut = t.len() - max;
    while !t.is_char_boundary(cut) {
        cut += 1;
    }
    if let Some(nl) = t[cut..].find('\n') {
        cut += nl + 1;
    }
    t.drain(..cut);
}

struct SocketCleanup(PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run one broadcast until `stop`. Body of `omarchy-patch broadcast-daemon`.
pub fn run(config: Config) -> std::io::Result<()> {
    let sock = socket_path();
    std::fs::create_dir_all(sock.parent().unwrap())?;
    if UnixStream::connect(&sock).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "a broadcast is already running",
        ));
    }
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sock.with_extension("lock"))?;
    lock.try_lock()
        .map_err(|e| std::io::Error::other(format!("a broadcast is already running: {e}")))?;
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(outcome_path());
    let listener = UnixListener::bind(&sock)?;
    listener.set_nonblocking(true)?;
    patch_core::ipc::private_socket(&sock);
    let _cleanup = SocketCleanup(sock.clone());
    // The bar's icon shows a stream running with every window closed.
    let _beat = patch_core::ipc::heartbeat("broadcasting");

    let mut config = config;
    if config.kind == Kind::File {
        config.target = free_path(&config.target);
    }
    let since = unix_now();
    let shared = Arc::new(Shared::default());
    if let Err(e) = broadcast(&config, &listener, &shared, since) {
        *shared.error.lock().unwrap() = Some(config.redact(&e.to_string()));
    }

    let error = shared.error.lock().unwrap().clone();
    let frames = shared.frames.load(Ordering::Relaxed);
    let tmp = sock.with_extension("result.tmp");
    let _ = std::fs::write(
        &tmp,
        serde_json::json!({
            "target": config.display_destination(),
            "kind": config.kind.encode(),
            "codec": config.effective_codec().encode(),
            "seconds": frames as f64 / config.rate as f64,
            "frames": frames,
            "bytes": sent_bytes(&config, &shared),
            "peak": *shared.peak.lock().unwrap(),
            "dropouts": shared.dropouts.load(Ordering::Relaxed),
            "error": error,
            "since": since,
            "ended": unix_now(),
        })
        .to_string(),
    );
    let _ = std::fs::rename(tmp, outcome_path());
    let _ = std::fs::remove_file(&sock);
    error.map_or(Ok(()), |e| Err(std::io::Error::other(e)))
}

/// A file's size on disk; for a network stream what its servers took.
fn sent_bytes(config: &Config, shared: &Shared) -> u64 {
    match config.kind {
        Kind::File => std::fs::metadata(&config.target).map_or(0, |m| m.len()),
        _ => shared.bytes.load(Ordering::Relaxed),
    }
}

/// The broadcast itself: capture, encoder, control loop, and the end.
/// Errors are setup failures (a tool missing); how a running stream ended is
/// left in `shared.error`.
fn broadcast(
    config: &Config,
    listener: &UnixListener,
    shared: &Arc<Shared>,
    since: f64,
) -> std::io::Result<()> {
    if let Kind::File = config.kind
        && let Some(dir) = Path::new(&config.target).parent()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| std::io::Error::other(format!("could not create {}: {e}", dir.display())))?;
    }

    // Capture: raw float32le, the same shape the encoder reads.
    let mut capture = ChildGuard(Some(
        Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                &format!("--channels={}", config.channels),
                "--latency-msec=20",
                "--client-name=Patch broadcast",
                &format!("--stream-name=Patch · {}", config.label),
                &format!("--rate={}", config.rate),
            ])
            .args(config.source.capture_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| std::io::Error::other(format!("could not start parec: {e}")))?,
    ));

    let mut pipe = capture.0.as_mut().unwrap().stdout.take().expect("piped");
    let tail = Arc::new(Mutex::new(String::new()));
    let encoder_in: EncoderInput = Arc::new(Mutex::new(None));
    // The picture (RTMP): windows send frames in, the pacer feeds ffmpeg.
    let picture = match config.picture() {
        Some(_) => Some(Pictures::start(shared, config.rate)?),
        None => None,
    };
    let video = picture.as_ref().map(|p| p.encoder);
    let mut encoder = Some(Encoder::spawn(config, video, &encoder_in, &tail, shared)?);

    let read_state = shared.clone();
    let read_into = encoder_in.clone();
    let (rate, channels) = (config.rate, config.channels as usize);
    // Reader: capture → encoder, then the meter.
    let reader = std::thread::Builder::new()
        .name("patch-bc-read".into())
        .spawn(move || {
            let frame = 4 * channels.max(1);
            let mut buf = vec![0u8; 1 << 16];
            let mut have = 0usize;
            let mut samples: Vec<f32> = Vec::with_capacity(buf.len() / 4);
            let mut gaps = GapDetector::new(rate);
            loop {
                if read_state.stop.load(Ordering::Relaxed) {
                    break;
                }
                let n = match pipe.read(&mut buf[have..]) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        *read_state.error.lock().unwrap() = Some(format!("Capture failed: {e}"));
                        break;
                    }
                };
                have += n;
                // Whole frames only: an encoder started on a reconnect
                // begins on a frame, and the meter reads real samples.
                let whole = have - have % frame;
                if whole == 0 {
                    continue;
                }
                let input = read_into.lock().unwrap().clone();
                if let Some(w) = input {
                    if (&*w).write_all(&buf[..whole]).is_ok() {
                        let n = (whole / frame) as u64;
                        read_state.frames.fetch_add(n, Ordering::Relaxed);
                        read_state.encoder_frames.fetch_add(n, Ordering::Relaxed);
                    } else {
                        // The encoder went away: the control loop decides,
                        // with its own last words, whether that ends the
                        // broadcast or reconnects.
                        let mut slot = read_into.lock().unwrap();
                        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &w)) {
                            *slot = None;
                        }
                    }
                }
                samples.clear();
                samples.extend(
                    buf[..whole]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b)),
                );
                let s = stats(&samples);
                {
                    let mut level = read_state.level.lock().unwrap();
                    *level = s.peak[0].max(s.peak[1]);
                    let mut peak = read_state.peak.lock().unwrap();
                    *peak = peak.max(s.peak[0].max(s.peak[1]));
                }
                if gaps.observe(Instant::now(), (whole / frame) as u64) {
                    read_state.dropouts.fetch_add(1, Ordering::Relaxed);
                }
                buf.copy_within(whole..have, 0);
                have -= whole;
            }
            read_into.lock().unwrap().take();
        })?;

    // Control loop: answer the socket, notice an encoder that died. A
    // network stream that reached its server comes back by itself (1, 2,
    // 4, 8, then every 15 s, per outage); a file, refused credentials or a
    // server never reached end it.
    let started = Instant::now();
    let mut retry_at: Option<Instant> = None;
    let mut ever_up = false;
    let mut attempt = 0u32;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                handle(stream, shared, config, started, since, &tail, picture.as_ref())
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => break,
        }
        if shared.stop.load(Ordering::Relaxed) || shared.error.lock().unwrap().is_some() {
            break;
        }
        // The capture ended (its source went away): the encoder stopping
        // after it is not a lost connection, and nothing is left to send.
        if reader.is_finished() {
            break;
        }
        if let Some(at) = retry_at {
            if Instant::now() >= at {
                tail.lock().unwrap().clear();
                match Encoder::spawn(config, video, &encoder_in, &tail, shared) {
                    Ok(e) => {
                        encoder = Some(e);
                        retry_at = None;
                    }
                    Err(e) => *shared.error.lock().unwrap() = Some(e.to_string()),
                }
            }
            continue;
        }
        if encoder.as_mut().is_some_and(|e| !e.running()) {
            encoder_in.lock().unwrap().take();
            // Joined first: its last words and progress are all in.
            drop(encoder.take());
            if reader.is_finished() {
                break;
            }
            let was_up = shared.accepted.swap(false, Ordering::Relaxed);
            ever_up |= was_up;
            // The encoder's own last words beat a generic diagnosis.
            let why = config.redact(&last_line(&tail.lock().unwrap()));
            if config.kind == Kind::File || !ever_up || refused(&why) {
                *shared.error.lock().unwrap() = Some(if why.is_empty() {
                    "The encoder stopped unexpectedly.".into()
                } else {
                    format!("The encoder stopped: {why}")
                });
                break;
            }
            if was_up {
                // One outage, however many attempts it takes.
                shared.dropouts.fetch_add(1, Ordering::Relaxed);
                attempt = 0;
            }
            shared.reconnects.fetch_add(1, Ordering::Relaxed);
            shared.reconnecting.store(true, Ordering::Relaxed);
            let wait = backoff(attempt);
            attempt += 1;
            *tail.lock().unwrap() = format!(
                "Connection lost{}; reconnecting in {} s (attempt {attempt}).",
                if why.is_empty() { String::new() } else { format!(": {why}") },
                wait.as_secs(),
            );
            retry_at = Some(Instant::now() + wait);
        }
    }

    // The end. The encoder's input closes, so it finishes by itself (a file
    // gets its last pages); one that cannot in time (a stalled server) is
    // killed, which also frees a reader blocked writing to it.
    let requested_stop = shared.stop.load(Ordering::Relaxed);
    shared.stop.store(true, Ordering::Relaxed);
    encoder_in.lock().unwrap().take();
    drop(capture);
    if let Some(e) = encoder.take() {
        e.finish(Duration::from_secs(5));
    }
    let _ = reader.join();
    drop(picture);
    if !requested_stop && shared.error.lock().unwrap().is_none() {
        let why = config.redact(tail.lock().unwrap().trim());
        *shared.error.lock().unwrap() = Some(if why.is_empty() {
            "Capture ended unexpectedly.".into()
        } else {
            format!("Capture ended unexpectedly: {why}")
        });
    }
    Ok(())
}

/// The picture's two sockets and threads (see `video`), for one broadcast.
struct Pictures {
    encoder: VideoEncoder,
    latest: video::SharedLatest,
    /// Ends its threads when it goes (also when the broadcast never got
    /// going: a setup step after it failed).
    halt: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    _sockets: [SocketCleanup; 2],
}

impl Pictures {
    fn start(shared: &Arc<Shared>, rate: u32) -> std::io::Result<Pictures> {
        let (from_windows, to_encoder) = (video::picture_socket(), video::encoder_socket());
        let bind = |path: &Path| -> std::io::Result<UnixListener> {
            let _ = std::fs::remove_file(path);
            let l = UnixListener::bind(path)?;
            patch_core::ipc::private_socket(path);
            Ok(l)
        };
        let windows = bind(&from_windows)?;
        let ffmpeg = bind(&to_encoder)?;
        let sockets = [SocketCleanup(from_windows), SocketCleanup(to_encoder)];
        let latest = video::latest();
        let halt = Arc::new(AtomicBool::new(false));
        let stopped = |s: &Arc<Shared>| {
            let (s, halt) = (s.clone(), halt.clone());
            move || s.stop.load(Ordering::Relaxed) || halt.load(Ordering::Relaxed)
        };
        let threads = vec![
            video::serve_pictures(windows, latest.clone(), stopped(shared))?,
            video::pace(
                ffmpeg,
                latest.clone(),
                shared.encoder_frames.clone(),
                rate,
                shared.video_frames.clone(),
                stopped(shared),
            )?,
        ];
        Ok(Pictures { encoder: VideoEncoder::probe(), latest, halt, threads, _sockets: sockets })
    }
}

impl Drop for Pictures {
    fn drop(&mut self) {
        self.halt.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn last_line(tail: &str) -> String {
    tail.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_owned()
}

/// The server said no (credentials, mount, path): retrying cannot help.
fn refused(why: &str) -> bool {
    ["401", "403", "Unauthorized", "Forbidden", "No such file", "Permission denied"]
        .iter()
        .any(|w| why.contains(w))
}

fn handle(
    stream: UnixStream,
    s: &Shared,
    config: &Config,
    started: Instant,
    since: f64,
    tail: &Arc<Mutex<String>>,
    picture: Option<&Pictures>,
) {
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
        "stop" => {
            s.stop.store(true, Ordering::Relaxed);
            let _ = writeln!(w, "ok");
        }
        _ => {
            let frames = s.frames.load(Ordering::Relaxed);
            let state = if s.error.lock().unwrap().is_some() {
                "error"
            } else if s.reconnecting.load(Ordering::Relaxed) {
                "reconnecting"
            } else if s.accepted.load(Ordering::Relaxed) {
                "live"
            } else {
                "connecting"
            };
            let status = serde_json::json!({
                "live": state == "live",
                "state": state,
                "source": config.source.encode(),
                "label": config.label,
                "kind": config.kind.encode(),
                "target": config.display_destination(),
                "codec": config.effective_codec().encode(),
                "bitrate": config.bitrate_kbps,
                "rate": config.rate,
                "channels": config.channels,
                "seconds": frames as f64 / config.rate as f64,
                "frames": frames,
                "wall": started.elapsed().as_secs_f64(),
                "since": since,
                "bytes": sent_bytes(config, s),
                "level": *s.level.lock().unwrap(),
                "peak": *s.peak.lock().unwrap(),
                "dropouts": s.dropouts.load(Ordering::Relaxed),
                "reconnects": s.reconnects.load(Ordering::Relaxed),
                "error": s.error.lock().unwrap().clone(),
                "note": config.redact(tail.lock().unwrap().trim()),
                // The picture: which window, whether one is sending frames
                // (else the stream shows black), how it is coded.
                "picture": config.picture().map(Picture::encode),
                "picture_live": picture.is_some_and(|p| video::is_fed(&p.latest)),
                "video_encoder": picture.map(|p| p.encoder.encode()),
                "video_frames": s.video_frames.load(Ordering::Relaxed),
            });
            let _ = writeln!(w, "{status}");
        }
    }
}

// ---- client side (used by Patch's UI and the CLI) -------------------------

fn call(req: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(400))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(400))).ok()?;
    writeln!(s, "{req}").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    Some(line)
}

pub fn status() -> Option<serde_json::Value> {
    call("status").and_then(|l| serde_json::from_str(&l).ok())
}

pub fn stop() -> bool {
    call("stop").is_some()
}

/// How the last broadcast ended (what it sent, `error` if it failed,
/// `since`/`ended` as Unix seconds), while none runs.
pub fn last_outcome() -> Option<serde_json::Value> {
    std::fs::read(outcome_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Stop and wait for the encoder to finalize, with a bounded deadline. The
/// socket's removal follows the atomic outcome file.
pub fn stop_and_wait() -> Result<serde_json::Value, String> {
    if !stop() {
        return Err("not broadcasting".into());
    }
    let sock = socket_path();
    for _ in 0..400 {
        if !sock.exists() {
            return std::fs::read(outcome_path())
                .map_err(|e| format!("Could not read the broadcast outcome: {e}"))
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("the broadcast is still finalizing".into())
}

/// Where `start` hands the detached process its config.
pub const CONFIG_ENV: &str = "PATCH_BROADCAST_CONFIG";

/// Start a detached broadcast process (`<exe> broadcast-daemon`, its config
/// in `CONFIG_ENV`). Returns when it was asked (Unix seconds): its outcome
/// has a `since` no earlier, which tells it from an earlier broadcast's.
pub fn start(exe: &Path, config: &Config) -> Result<f64, String> {
    if status().is_some() {
        return Err("already broadcasting".into());
    }
    let json = serde_json::json!({
        "source": config.source.encode(),
        "label": config.label,
        "kind": config.kind.encode(),
        "target": config.target,
        "mount": config.mount,
        "user": config.user,
        "password": config.password,
        "key": config.key,
        "codec": config.codec.encode(),
        "bitrate": config.bitrate_kbps,
        "rate": config.rate,
        "channels": config.channels,
        "name": config.name,
        "picture": config.picture.map_or("none", Picture::encode),
    });
    let asked = unix_now();
    // The config (with any stream password) goes in the environment, which
    // only this user can read, not the command line every process can.
    Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .arg("broadcast-daemon")
        .env(CONFIG_ENV, json.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    // A first answer within 10 s (each try waits up to 400 ms while the
    // process is still setting up).
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if status().is_some() {
            return Ok(asked);
        }
        // The daemon may have failed at once (a refused server, a missing
        // capture tool): report what it said rather than guessing. Only
        // its own outcome, not one left by an earlier broadcast.
        if let Some(outcome) = outcome_since(asked) {
            return Err(outcome["error"]
                .as_str()
                .filter(|e| !e.is_empty())
                .unwrap_or("the broadcast ended at once")
                .to_string());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if UnixStream::connect(socket_path()).is_ok() {
        return Err("the broadcast is not answering (the last one may still be stopping); try again in a moment".into());
    }
    Err("the broadcast didn't start; is ffmpeg installed?".into())
}

/// The outcome of a broadcast started at `asked` or later (none while it
/// runs or finishes, nor one an earlier broadcast left).
fn outcome_since(asked: f64) -> Option<serde_json::Value> {
    last_outcome().filter(|o| o["since"].as_f64().is_some_and(|s| s >= asked - 0.05))
}

/// After `start` (asked at `asked`): wait until the destination takes the
/// stream (`live`), the broadcast fails, or `timeout` passes (then it is
/// still connecting).
pub fn wait_until_live(asked: f64, timeout: Duration) -> Result<serde_json::Value, String> {
    let until = Instant::now() + timeout;
    loop {
        match status() {
            Some(s) if s["state"] == "live" || Instant::now() >= until => return Ok(s),
            Some(s) if s["state"] == "error" => {
                return Err(s["error"].as_str().unwrap_or("the broadcast failed").to_owned());
            }
            Some(_) => {}
            // Gone, or finishing (it answers nothing then): its own outcome
            // says how it ended once written.
            None => {
                if let Some(o) = outcome_since(asked) {
                    return Err(o["error"]
                        .as_str()
                        .filter(|e| !e.is_empty())
                        .unwrap_or("the broadcast ended")
                        .to_owned());
                }
                if Instant::now() >= until {
                    return Err("the broadcast ended".into());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A config from the JSON the UI builds.
pub fn config_from_json(v: &serde_json::Value) -> Result<Config, String> {
    Ok(Config {
        source: Source::parse(v["source"].as_str().unwrap_or_default())
            .ok_or("Invalid broadcast source")?,
        label: v["label"].as_str().unwrap_or_default().to_owned(),
        kind: Kind::parse(v["kind"].as_str().unwrap_or_default()).ok_or("Invalid destination")?,
        target: v["target"].as_str().unwrap_or_default().to_owned(),
        mount: v["mount"].as_str().unwrap_or_default().to_owned(),
        user: v["user"].as_str().unwrap_or_default().to_owned(),
        password: v["password"].as_str().unwrap_or_default().to_owned(),
        key: v["key"].as_str().unwrap_or_default().to_owned(),
        codec: Codec::parse(v["codec"].as_str().unwrap_or_default()).ok_or("Invalid codec")?,
        bitrate_kbps: v["bitrate"].as_u64().unwrap_or(128) as u32,
        rate: v["rate"].as_u64().unwrap_or(48_000) as u32,
        channels: v["channels"].as_u64().unwrap_or(2) as u16,
        name: v["name"].as_str().unwrap_or_default().to_owned(),
        // RTMP carries the Performer unless told otherwise ("none": sound
        // only, for a server that takes that).
        picture: match v["picture"].as_str() {
            Some("none") => None,
            Some(p) => Some(Picture::parse(p).ok_or("Invalid picture")?),
            None => Some(Picture::Performer),
        },
    })
}

/// The argument to `broadcast-daemon`: the JSON the UI or CLI built.
pub fn run_from_json(json: &str) -> std::io::Result<()> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| std::io::Error::other(format!("bad broadcast config: {e}")))?;
    let config = config_from_json(&v).map_err(std::io::Error::other)?;
    run(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_and_kinds_round_trip() {
        for src in [Source::Device("patch.bus.monitor".into()), Source::Stream(7)] {
            assert_eq!(Source::parse(&src.encode()), Some(src));
        }
        assert_eq!(Source::parse("program:mix"), None);
        assert_eq!(Source::parse("device:"), None);
        for k in [Kind::Icecast, Kind::Rtmp, Kind::File] {
            assert_eq!(Kind::parse(k.encode()), Some(k));
        }
        for c in [Codec::Opus, Codec::Mp3, Codec::Aac] {
            assert_eq!(Codec::parse(c.encode()), Some(c));
        }
    }

    fn icecast_config() -> Config {
        Config {
            source: Source::Device("patch.bus.monitor".into()),
            label: "Patch bus".into(),
            kind: Kind::Icecast,
            target: "http://stream.example:8000".into(),
            mount: "patch".into(),
            user: "source".into(),
            password: "hunter2".into(),
            key: String::new(),
            codec: Codec::Opus,
            bitrate_kbps: 128,
            rate: 48_000,
            channels: 2,
            name: "Patch".into(),
            picture: None,
        }
    }

    fn rtmp_config(target: &str, key: &str) -> Config {
        Config {
            kind: Kind::Rtmp,
            target: target.into(),
            key: key.into(),
            user: String::new(),
            password: String::new(),
            picture: Some(Picture::Performer),
            ..icecast_config()
        }
    }

    /// YouTube and Twitch need a picture: RTMP carries H.264 from the
    /// paced frames with the AAC, keyframes every 2 s, BT.709 colours.
    #[test]
    fn rtmp_carries_the_picture() {
        let c = rtmp_config("rtmp://live.example/app", "key");
        for enc in [VideoEncoder::Nvenc, VideoEncoder::X264] {
            let args = c.encoder_args(Some(enc));
            let inputs: Vec<&String> = args.iter().zip(&args[1..]).filter(|(a, _)| *a == "-i").map(|(_, b)| b).collect();
            assert_eq!(inputs[0], "pipe:0");
            assert!(inputs[1].starts_with("unix:") && inputs[1].ends_with("broadcast.video.sock"), "{inputs:?}");
            let after = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
            assert_eq!(after("-c:v"), enc.encode());
            assert_eq!(after("-video_size"), "1280x720");
            assert_eq!(after("-framerate"), "30");
            assert_eq!(after("-g"), "60");
            assert_eq!(after("-colorspace"), "bt709");
            assert_eq!(after("-c:a"), "aac");
            assert_eq!(after("-f"), "f32le");
            assert_eq!(args[args.len() - 1], "rtmp://live.example/app/key");
        }
        // Audio only when asked, and never a picture for Icecast or a file.
        let sound = Config { picture: None, ..c.clone() };
        assert!(!sound.encoder_args(Some(VideoEncoder::X264)).iter().any(|a| a == "-c:v"));
        let ice = Config { picture: Some(Picture::Performer), ..icecast_config() };
        assert!(!ice.encoder_args(Some(VideoEncoder::X264)).iter().any(|a| a == "-c:v"));
        // The JSON a window or the CLI builds: the Performer unless "none".
        let mut v = serde_json::json!({"source": "device:x", "kind": "rtmp", "codec": "aac", "target": "rtmp://h/app"});
        assert_eq!(config_from_json(&v).unwrap().picture, Some(Picture::Performer));
        v["picture"] = "visualiser".into();
        assert_eq!(config_from_json(&v).unwrap().picture, Some(Picture::Visualiser));
        v["picture"] = "none".into();
        assert_eq!(config_from_json(&v).unwrap().picture, None);
        v["picture"] = "poster".into();
        assert!(config_from_json(&v).is_err());
    }

    #[test]
    fn icecast_url_carries_credentials_but_never_shows_them() {
        let c = icecast_config();
        assert_eq!(
            c.destination(),
            "icecast://source:hunter2@stream.example:8000/patch"
        );
        assert_eq!(c.display_destination(), "source@stream.example:8000/patch");
        assert!(!c.display_destination().contains("hunter2"));
    }

    /// ffmpeg decodes `%XX` in the user and password (checked against
    /// ffmpeg 9.1); raw, a `/`, `#` or `?` ended the address there and the
    /// stream failed with "Invalid argument".
    #[test]
    fn icecast_credentials_are_escaped_and_a_lone_password_is_sent() {
        let c = Config { password: "p@ss/w#rd?:%".into(), ..icecast_config() };
        assert_eq!(
            c.destination(),
            "icecast://source:p%40ss%2Fw%23rd%3F%3A%25@stream.example:8000/patch"
        );
        let lone = Config { user: String::new(), password: "hunter2".into(), ..icecast_config() };
        assert_eq!(lone.destination(), "icecast://source:hunter2@stream.example:8000/patch");
        assert_eq!(lone.display_destination(), "source@stream.example:8000/patch");
        let open = Config { user: String::new(), password: String::new(), ..icecast_config() };
        assert_eq!(open.destination(), "icecast://stream.example:8000/patch");
    }

    #[test]
    fn https_icecast_is_sent_over_tls() {
        let plain = icecast_config().encoder_args(None);
        assert!(!plain.iter().any(|a| a == "-tls"));
        let tls = Config { target: "https://stream.example:8443/".into(), ..icecast_config() };
        let args = tls.encoder_args(None);
        let at = args.iter().position(|a| a == "-tls").unwrap();
        assert_eq!(args[at + 1], "1");
        assert_eq!(args.last().unwrap(), "icecast://source:hunter2@stream.example:8443/patch");
    }

    #[test]
    fn rtmp_is_aac_flv_and_opus_becomes_ogg() {
        let rtmp = Config { codec: Codec::Opus, ..rtmp_config("rtmp://live.example/app/key", "") };
        assert_eq!(rtmp.effective_codec(), Codec::Aac);
        assert_eq!(rtmp.muxer(), "flv");
        assert!(rtmp.encoder_args(None).contains(&"aac".to_string()));

        let file = Config {
            kind: Kind::File,
            target: "/tmp/patch.ogg".into(),
            codec: Codec::Opus,
            password: String::new(),
            ..icecast_config()
        };
        assert_eq!(file.muxer(), "ogg");
        let args = file.encoder_args(None);
        assert!(args.contains(&"libopus".to_string()));
        assert!(args.contains(&"/tmp/patch.ogg".to_string()));
        assert!(!args.iter().any(|a| a.contains("-content_type")));
        // A file has no server to time out on.
        assert!(!args.iter().any(|a| a == "-rw_timeout"));
    }

    /// The stream key is a password: it is never on screen, in the bar or
    /// in the status, only in the URL ffmpeg gets.
    #[test]
    fn the_rtmp_stream_key_never_shows() {
        let split = rtmp_config("rtmp://a.rtmp.youtube.com/live2/", "abcd-efgh-ijkl");
        assert_eq!(split.destination(), "rtmp://a.rtmp.youtube.com/live2/abcd-efgh-ijkl");
        assert_eq!(split.display_destination(), "a.rtmp.youtube.com/live2/••••");
        let whole = rtmp_config("rtmp://live.example/app/STREAMKEY?s=1", "");
        assert_eq!(whole.destination(), "rtmp://live.example/app/STREAMKEY?s=1");
        assert_eq!(whole.display_destination(), "live.example/app/••••");
        assert_eq!(mask_rtmp("rtmp://live.example/app"), "live.example/app");
        assert_eq!(mask_rtmp("rtmp://user:pw@live.example"), "live.example");
    }

    /// ffmpeg prints the URL it was given when it fails (seen with ffmpeg
    /// 9.1: "Error opening output file icecast://source:hunter2@…").
    #[test]
    fn ffmpeg_lines_lose_the_credentials() {
        let c = Config { password: "hunter2/x".into(), ..icecast_config() };
        let line = format!("Error opening output file {}.", c.destination());
        let shown = c.redact(&line);
        assert_eq!(shown, "Error opening output file source@stream.example:8000/patch.");
        assert_eq!(c.redact("bad password hunter2/x"), "bad password ••••");
        let r = rtmp_config("rtmp://live.example/app", "SECRETKEY");
        let shown = r.redact("Error opening output rtmp://live.example/app/SECRETKEY: I/O error");
        assert!(!shown.contains("SECRETKEY"), "{shown}");
    }

    /// alimiter's auto level (on by default) put a 0.999 peak back at
    /// 0.0 dBFS and lifted quiet audio by 1/0.97 (measured, ffmpeg 9.1).
    #[test]
    fn the_limiter_leaves_headroom() {
        let args = icecast_config().encoder_args(None);
        let at = args.iter().position(|a| a == "-af").unwrap();
        assert_eq!(args[at + 1], "alimiter=limit=0.97:level=0");
    }

    #[test]
    fn a_refusing_server_is_final_a_lost_connection_is_retried() {
        assert!(refused("Server returned 401 Unauthorized (authorization failed)"));
        assert!(refused("Server returned 403 Forbidden (access denied)"));
        assert!(!refused("Error opening output files: Connection refused"));
        assert!(!refused("Error closing file: Broken pipe"));
        assert_eq!(last_line("a\nb: why\n\n"), "b: why");
    }

    #[test]
    fn reconnects_back_off_per_outage() {
        let waits: Vec<u64> = (0..7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 15, 15, 15]);
    }

    #[test]
    fn the_encoder_is_told_the_channel_layout() {
        let args = icecast_config().encoder_args(None);
        let at = args.iter().position(|a| a == "-ch_layout").unwrap();
        assert_eq!(args[at + 1], "stereo");
        assert!(at < args.iter().position(|a| a == "-i").unwrap());
    }

    #[test]
    fn progress_reports_what_the_destination_took() {
        assert_eq!(progress_bytes("total_size=27107"), Some(27107));
        assert_eq!(progress_bytes("total_size=N/A"), None);
        assert_eq!(progress_bytes("out_time_us=500000"), None);
        let args = icecast_config().encoder_args(None);
        let at = args.iter().position(|a| a == "-progress").unwrap();
        assert_eq!(args[at + 1], "pipe:1");
    }

    /// Two 1 s gaps count twice (the second was missed: the count stuck
    /// after the first), a slow start counts none, and two hours of a
    /// sound card running 100 ppm slow count none.
    #[test]
    fn every_capture_gap_counts_once() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut g = GapDetector::new(48_000);
        let mut gaps = 0;
        // The first audio comes 0.6 s after the capture starts.
        let mut t = 0.6;
        while t < 12.0 {
            let silent = (3.0..4.0).contains(&t) || (8.0..9.0).contains(&t);
            if !silent && g.observe(at(t), 960) {
                gaps += 1;
            }
            t += 0.02;
        }
        assert_eq!(gaps, 2);

        let mut g = GapDetector::new(48_000);
        let mut drifted = 0;
        for k in 0..(2 * 3600 * 50) {
            // 960 frames every 20 ms by a clock 100 ppm slow.
            if g.observe(at(k as f64 * 0.02 * 1.0001), 960) {
                drifted += 1;
            }
        }
        assert_eq!(drifted, 0);
    }

    #[test]
    fn a_file_destination_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("patch-bc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let set = dir.join("set.ogg");
        let set = set.to_str().unwrap();
        assert_eq!(free_path(set), set);
        std::fs::write(set, b"x").unwrap();
        assert_eq!(free_path(set), dir.join("set-2.ogg").to_str().unwrap());
        std::fs::write(dir.join("set-2.ogg"), b"x").unwrap();
        assert_eq!(free_path(set), dir.join("set-3.ogg").to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stderr_tail_is_cut_on_a_line_and_a_character() {
        let mut t = "é".repeat(3000) + "\nlast line\n";
        keep_tail(&mut t, 4096);
        assert_eq!(t, "last line\n");
        let mut t = "ab".repeat(10);
        keep_tail(&mut t, 4096);
        assert_eq!(t.len(), 20);
    }
}
