//! The broadcast's picture (RTMP): the live frames of a Patch window — the
//! Performer or the Visualiser, UI and GPU stage as drawn — sent to the
//! broadcast process, which paces them onto the audio clock. The encoder
//! gets exactly 30 frames for every second of sound, whatever rate the
//! window draws at, so picture and sound never drift apart; while no frame
//! comes (the window is closed or not on screen) the picture is black.
//!
//! Window → broadcast, over `broadcast.picture.sock`: `PATCHVF1`, width and
//! height (u32, little-endian), then frames of width × height × 4 bytes
//! (R, G, B, unused; top row first). Broadcast → ffmpeg, over
//! `broadcast.video.sock`: the paced frames, raw.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
pub const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
const MAGIC: &[u8; 8] = b"PATCHVF1";
/// A frame older than this is no longer the picture (the window stopped
/// drawing): black instead of a frozen image.
const FRESH: Duration = Duration::from_millis(1500);

/// Where a window sends its frames.
pub fn picture_socket() -> PathBuf {
    patch_core::ipc::runtime_dir().join("broadcast.picture.sock")
}

/// Where ffmpeg reads the paced frames.
pub fn encoder_socket() -> PathBuf {
    patch_core::ipc::runtime_dir().join("broadcast.video.sock")
}

/// Which window's picture goes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picture {
    /// The Broadcast DJ display (`performer`): the audience picture.
    Performer,
    Visualiser,
}

impl Picture {
    pub fn encode(self) -> &'static str {
        match self {
            Picture::Performer => "performer",
            Picture::Visualiser => "visualiser",
        }
    }
    pub fn parse(s: &str) -> Option<Picture> {
        match s {
            "performer" => Some(Picture::Performer),
            "visualiser" => Some(Picture::Visualiser),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Picture::Performer => "Performer",
            Picture::Visualiser => "Visualiser",
        }
    }
    /// The surface whose window it is.
    pub fn surface(self) -> &'static str {
        self.encode()
    }
}

// ---- window side ----------------------------------------------------------

/// The window side: the UI thread fills a frame and swaps it in; a thread
/// of its own sends the newest one (never blocking the UI). Dropped, the
/// thread ends and the connection closes.
pub struct FrameSink {
    back: std::cell::RefCell<Vec<u8>>,
    shared: Arc<(Mutex<SinkSlot>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct SinkSlot {
    front: Vec<u8>,
    seq: u64,
    closed: bool,
}

impl FrameSink {
    pub fn new() -> FrameSink {
        let shared = Arc::new((Mutex::new(SinkSlot::default()), Condvar::new()));
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("patch-bc-picture".into())
            .spawn(move || send_frames(&s))
            .ok();
        FrameSink { back: Default::default(), shared, thread }
    }

    /// Hand over the next frame: `fill` writes all `FRAME_BYTES` of it.
    pub fn submit(&self, fill: impl FnOnce(&mut [u8])) {
        let mut back = self.back.borrow_mut();
        back.resize(FRAME_BYTES, 0);
        fill(&mut back);
        let (slot, cv) = &*self.shared;
        let mut slot = slot.lock().unwrap();
        std::mem::swap(&mut slot.front, &mut *back);
        slot.seq += 1;
        cv.notify_one();
    }
}

impl Default for FrameSink {
    fn default() -> Self {
        FrameSink::new()
    }
}

impl Drop for FrameSink {
    fn drop(&mut self) {
        let (slot, cv) = &*self.shared;
        slot.lock().unwrap().closed = true;
        cv.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn send_frames(shared: &(Mutex<SinkSlot>, Condvar)) {
    let (slot, cv) = shared;
    let mut frame = Vec::new();
    let mut sent = 0u64;
    let mut conn: Option<UnixStream> = None;
    let mut tried = Instant::now() - Duration::from_secs(1);
    loop {
        {
            let mut s = slot.lock().unwrap();
            while s.seq == sent && !s.closed {
                s = cv.wait(s).unwrap();
            }
            if s.closed {
                return;
            }
            std::mem::swap(&mut frame, &mut s.front);
            sent = s.seq;
        }
        if conn.is_none() && tried.elapsed() >= Duration::from_secs(1) {
            tried = Instant::now();
            conn = UnixStream::connect(picture_socket()).ok().and_then(|mut c| {
                let mut head = MAGIC.to_vec();
                head.extend(WIDTH.to_le_bytes());
                head.extend(HEIGHT.to_le_bytes());
                c.write_all(&head).ok().map(|_| c)
            });
        }
        if let Some(c) = conn.as_mut()
            && frame.len() == FRAME_BYTES
            && c.write_all(&frame).is_err()
        {
            conn = None;
        }
    }
}

// ---- broadcast side -------------------------------------------------------

/// The newest frame a window sent, and when.
pub struct Latest {
    frame: Vec<u8>,
    at: Option<Instant>,
    /// The connection whose window is the picture: it keeps it while it
    /// draws; another takes over once it stops (two windows never take
    /// turns).
    owner: u64,
}

/// A window that sent nothing for this long has stopped drawing.
const TAKE_OVER: Duration = Duration::from_millis(500);

pub type SharedLatest = Arc<Mutex<Latest>>;

pub fn latest() -> SharedLatest {
    Arc::new(Mutex::new(Latest { frame: vec![0; FRAME_BYTES], at: None, owner: 0 }))
}

/// A window is sending frames now.
pub fn is_fed(latest: &SharedLatest) -> bool {
    latest.lock().unwrap().at.is_some_and(|at| at.elapsed() < FRESH)
}

/// Take frames from windows on `listener` until `stop()`: one thread per
/// connection; the window drawing keeps the picture.
pub fn serve_pictures(
    listener: UnixListener,
    latest: SharedLatest,
    stop: impl Fn() -> bool + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("patch-bc-pictures".into())
        .spawn(move || {
            let mut next = 0u64;
            while !stop() {
                match listener.accept() {
                    Ok((conn, _)) => {
                        next += 1;
                        let (id, latest) = (next, latest.clone());
                        let _ = std::thread::Builder::new()
                            .name("patch-bc-frames".into())
                            .spawn(move || take_frames(conn, id, &latest));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    Err(_) => break,
                }
            }
        })
}

fn take_frames(mut conn: UnixStream, id: u64, latest: &SharedLatest) {
    let _ = conn.set_nonblocking(false);
    let mut head = [0u8; 16];
    if conn.read_exact(&mut head).is_err()
        || &head[..8] != MAGIC
        || u32::from_le_bytes(head[8..12].try_into().unwrap()) != WIDTH
        || u32::from_le_bytes(head[12..16].try_into().unwrap()) != HEIGHT
    {
        return;
    }
    let mut frame = vec![0u8; FRAME_BYTES];
    while conn.read_exact(&mut frame).is_ok() {
        let mut l = latest.lock().unwrap();
        let free = l.at.is_none_or(|at| at.elapsed() >= TAKE_OVER);
        if l.owner == id || free {
            l.owner = id;
            std::mem::swap(&mut l.frame, &mut frame);
            l.at = Some(Instant::now());
        }
    }
}

/// Feed ffmpeg's video input until `stop()`: each encoder connects to
/// `listener` once; from then on it gets one frame per 1/30 s of the audio
/// it has been given (`audio_frames`, counted from its start, at `rate`),
/// one frame ahead — the newest picture, or black when there is none.
pub fn pace(
    listener: UnixListener,
    latest: SharedLatest,
    audio_frames: Arc<AtomicU64>,
    rate: u32,
    sent: Arc<AtomicU64>,
    stop: impl Fn() -> bool + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("patch-bc-video".into())
        .spawn(move || {
            let black = vec![0u8; FRAME_BYTES];
            let mut conn: Option<UnixStream> = None;
            let mut written = 0u64;
            while !stop() {
                if let Ok((c, _)) = listener.accept() {
                    // A new encoder (a reconnect): its own count from 0.
                    let _ = c.set_nonblocking(false);
                    conn = Some(c);
                    written = 0;
                }
                let Some(c) = conn.as_mut() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let due = frames_due(audio_frames.load(Ordering::Relaxed), rate);
                if written >= due {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let ok = {
                    let l = latest.lock().unwrap();
                    let fresh = l.at.is_some_and(|at| at.elapsed() < FRESH);
                    c.write_all(if fresh { &l.frame } else { &black }).is_ok()
                };
                if ok {
                    written += 1;
                    sent.fetch_add(1, Ordering::Relaxed);
                } else {
                    conn = None;
                }
            }
        })
}

/// Video frames owed for `audio` frames of sound at `rate`: 30 per second,
/// one ahead (the muxer never waits for a picture).
fn frames_due(audio: u64, rate: u32) -> u64 {
    audio * FPS as u64 / rate.max(1) as u64 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_frames_for_every_second_of_sound() {
        assert_eq!(frames_due(0, 48_000), 1);
        assert_eq!(frames_due(48_000, 48_000), 31);
        assert_eq!(frames_due(44_100 * 60, 44_100), 30 * 60 + 1);
        // An hour stays exact: no drift from rounding.
        assert_eq!(frames_due(48_000 * 3600, 48_000), 30 * 3600 + 1);
    }

    #[test]
    fn the_drawing_window_keeps_the_picture() {
        let dir = std::env::temp_dir().join(format!("patch-bc-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.sock");
        let _ = std::fs::remove_file(&path);
        let latest = latest();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        let server = serve_pictures(UnixListener::bind(&path).unwrap(), latest.clone(), move || {
            s.load(Ordering::Relaxed)
        })
        .unwrap();
        let send = |value: u8| {
            let mut c = UnixStream::connect(&path).unwrap();
            let mut head = MAGIC.to_vec();
            head.extend(WIDTH.to_le_bytes());
            head.extend(HEIGHT.to_le_bytes());
            c.write_all(&head).unwrap();
            c.write_all(&vec![value; FRAME_BYTES]).unwrap();
            c
        };
        let wait = |value: u8| {
            for _ in 0..200 {
                if latest.lock().unwrap().frame[0] == value {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("frame {value} never arrived");
        };
        let mut first = send(7);
        wait(7);
        assert!(is_fed(&latest));
        // A second window while the first draws: the first keeps it.
        let mut second = send(9);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(latest.lock().unwrap().frame[0], 7);
        first.write_all(&vec![8; FRAME_BYTES]).unwrap();
        wait(8);
        // The first stops drawing: the second takes over.
        std::thread::sleep(TAKE_OVER);
        second.write_all(&vec![9; FRAME_BYTES]).unwrap();
        wait(9);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_size_is_refused() {
        let (mut a, b) = UnixStream::pair().unwrap();
        let latest = latest();
        let mut head = MAGIC.to_vec();
        head.extend(640u32.to_le_bytes());
        head.extend(360u32.to_le_bytes());
        a.write_all(&head).unwrap();
        take_frames(b, 1, &latest);
        assert!(!is_fed(&latest));
    }
}
