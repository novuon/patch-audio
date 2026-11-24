//! Meters in shared memory: the engine writes, the UI reads, with no
//! request, reply or JSON in between (the UI asked the engine over its
//! socket every frame before; that round trip cost both sides).
//!
//! One file, `meters.shm` in Patch's private runtime folder (0600): a
//! header, a ring of meter blocks, and one "now" section (analysis,
//! mixer, channel names). Each block and the section are seqlocks: the
//! writer makes the sequence odd while it writes and even when done; a
//! reader keeps a copy only when the number was even and unchanged across
//! its copy. One engine writes; at a handover the new one makes a new file
//! (the UI sees the file change and maps the new one).

use crate::{BUSES, MAX_CHANNELS, MAX_DECKS, MeterBlock};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering, fence};

const MAGIC: u64 = 0x5041_5443_484d_5452; // "PATCHMTR"
const VERSION: u32 = 9;
/// Blocks kept: at 1024 frames and 48 kHz, 5 s (the UI keeps ~3 s).
pub const SLOTS: usize = 256;
pub const BANDS: usize = 64;
const NAMES: usize = 1024;

/// One meter block as the engine measured it (linear values).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Block {
    pub nsec: u64,
    pub rate: u32,
    pub frames: u32,
    pub channels: u32,
    pub decks: u32,
    pub ch_peak: [[f32; 2]; MAX_CHANNELS],
    pub ch_abs: [[f32; 2]; MAX_CHANNELS],
    pub ch_gain: [[f32; 2]; MAX_CHANNELS],
    pub out_peak: [f32; 2],
    /// Deck position in seconds (NaN: no track) and source frames per
    /// output frame.
    pub deck_secs: [f64; MAX_DECKS],
    pub deck_step: [f32; MAX_DECKS],
    /// Per deck, which track `deck_secs` is in (its load count, v4).
    pub deck_load: [u32; MAX_DECKS],
    /// The DJ Booth's sum: peak and mean |x| per side (v2).
    pub dj_peak: [f32; 2],
    pub dj_abs: [f32; 2],
    /// The Mixer's buses as they leave (v7): AUX 1, 2, FX 1, 2, peak per side.
    pub bus_peak: [[f32; 2]; BUSES],
}

/// The engine's state now: program analysis and the DJ mixer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Now {
    pub vu: [f32; 2],
    /// Momentary, short-term, integrated, range, true peak max.
    pub loudness: [f32; 5],
    pub bands: u32,
    pub spectrum: [f32; BANDS],
    pub xfader: f32,
    pub faders: [f32; 4],
    /// The decks' EQ knobs as they move (v5): per deck A–D: high, mid,
    /// low (−1 kill … 1) and filter (−1 low-pass … 1 high-pass).
    pub eq: [[f32; 4]; 4],
    /// The decks' stem gains (0…2) and levels (0…1) as they move (v6), per
    /// deck A–D in canonical order: vocals, drums, bass, other. NaN gains:
    /// the deck has no stems attached.
    pub stems: [[f32; 4]; 4],
    pub stem_levels: [[f32; 4]; 4],
    /// Channel names, comma-separated (UTF-8, zero-padded).
    pub names: [u8; NAMES],
}

#[repr(C)]
struct Slot<T> {
    seq: AtomicU64,
    data: std::cell::UnsafeCell<T>,
}

#[repr(C)]
struct Header {
    magic: u64,
    version: u32,
    slots: u32,
    /// The newest block's number (block n lives in slot n % SLOTS).
    newest: AtomicU64,
    /// Bumped by the writer at least every few ms while it runs.
    beat: AtomicU64,
    now: Slot<Now>,
}

fn size() -> usize {
    std::mem::size_of::<Header>() + SLOTS * std::mem::size_of::<Slot<Block>>()
}

pub fn path() -> PathBuf {
    patch_core::ipc::runtime_dir().join("meters.shm")
}

struct Map {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain shared memory, accessed through atomics and
// seqlocked copies only.
unsafe impl Send for Map {}

impl Map {
    fn header(&self) -> &Header {
        // SAFETY: the mapping is at least size() long and page-aligned.
        unsafe { &*(self.ptr as *const Header) }
    }

    fn slot(&self, n: u64) -> &Slot<Block> {
        let i = (n % SLOTS as u64) as usize;
        // SAFETY: i < SLOTS; the ring follows the header, within size().
        unsafe { &*((self.ptr.add(std::mem::size_of::<Header>()) as *const Slot<Block>).add(i)) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: ptr/len are the mmap result.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

fn map(fd: i32, len: usize, write: bool) -> Option<Map> {
    let prot = if write {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_READ
    };
    // SAFETY: a shared mapping of an open fd of at least `len` bytes.
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, libc::MAP_SHARED, fd, 0) };
    (ptr != libc::MAP_FAILED).then(|| Map {
        ptr: ptr as *mut u8,
        len,
    })
}

/// The engine's side: makes a new file (replacing a predecessor's).
pub struct Writer {
    map: Map,
    newest: u64,
}

impl Writer {
    pub fn create() -> Option<Writer> {
        Self::create_at(&path())
    }

    pub fn create_at(p: &std::path::Path) -> Option<Writer> {
        use std::os::unix::fs::OpenOptionsExt;
        // A new file (not the old one truncated): a UI still reading the
        // predecessor's keeps a consistent mapping until it switches.
        let tmp = p.with_extension(format!("shm.{}", std::process::id()));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .ok()?;
        f.set_len(size() as u64).ok()?;
        use std::os::fd::AsRawFd;
        let map = map(f.as_raw_fd(), size(), true)?;
        // SAFETY: the mapping is fresh, zeroed, ours alone until renamed.
        unsafe {
            let h = map.ptr as *mut Header;
            (*h).magic = MAGIC;
            (*h).version = VERSION;
            (*h).slots = SLOTS as u32;
        }
        std::fs::rename(&tmp, p).ok()?;
        Some(Writer { map, newest: 0 })
    }

    fn write<T: Copy>(slot: &Slot<T>, v: &T) {
        let s = slot.seq.load(Ordering::Relaxed);
        slot.seq.store(s + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        // SAFETY: the single writer; readers check the sequence around it.
        unsafe { std::ptr::write_volatile(slot.data.get(), *v) };
        slot.seq.store(s + 2, Ordering::Release);
    }

    /// One block, in the order measured.
    pub fn block(&mut self, b: &MeterBlock, channels: usize, decks: usize) {
        let rate = b.stamp.rate.max(1) as f64;
        let mut deck_secs = [f64::NAN; MAX_DECKS];
        for (d, s) in deck_secs.iter_mut().enumerate().take(decks.min(MAX_DECKS)) {
            *s = b.deck_pos[d] / rate;
        }
        let v = Block {
            nsec: b.stamp.nsec,
            rate: b.stamp.rate,
            frames: b.stamp.frames,
            channels: channels.min(MAX_CHANNELS) as u32,
            decks: decks.min(MAX_DECKS) as u32,
            ch_peak: b.ch_peak,
            ch_abs: b.ch_abs,
            ch_gain: b.ch_gain,
            out_peak: b.out_peak,
            deck_secs,
            deck_step: b.deck_step,
            deck_load: b.deck_load,
            dj_peak: b.dj_peak,
            dj_abs: b.dj_abs,
            bus_peak: b.bus_peak,
        };
        let n = self.newest + 1;
        Self::write(self.map.slot(n), &v);
        self.newest = n;
        self.map.header().newest.store(n, Ordering::Release);
    }

    pub fn now(&mut self, v: &Now) {
        Self::write(&self.map.header().now, v);
    }

    /// Still alive (a reader takes a writer that stopped for gone).
    pub fn beat(&self) {
        self.map.header().beat.fetch_add(1, Ordering::Relaxed);
    }
}

impl Now {
    pub fn empty() -> Now {
        Now {
            vu: [patch_model::audio::SILENCE_DB; 2],
            loudness: [-120.0; 5],
            bands: 0,
            spectrum: [-120.0; BANDS],
            xfader: 0.5,
            faders: [1.0, 1.0, 0.0, 0.0],
            eq: [[0.0; 4]; 4],
            stems: [[f32::NAN; 4]; 4],
            stem_levels: [[0.0; 4]; 4],
            names: [0; NAMES],
        }
    }

    pub fn set_names(&mut self, names: &[String]) {
        let s = names.join(",");
        let b = s.as_bytes();
        let n = b.len().min(NAMES);
        self.names = [0; NAMES];
        self.names[..n].copy_from_slice(&b[..n]);
    }

    pub fn names(&self) -> Vec<String> {
        let end = self.names.iter().position(|b| *b == 0).unwrap_or(NAMES);
        let s = String::from_utf8_lossy(&self.names[..end]);
        if s.is_empty() {
            vec![]
        } else {
            s.split(',').map(String::from).collect()
        }
    }
}

/// The UI's side: maps the file, and maps it again when it's replaced.
pub struct Reader {
    map: Map,
    path: PathBuf,
    inode: u64,
    beat: (u64, std::time::Instant),
}

fn inode(p: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.ino())
}

impl Reader {
    pub fn open() -> Option<Reader> {
        Self::open_at(&path())
    }

    pub fn open_at(p: &std::path::Path) -> Option<Reader> {
        use std::os::fd::AsRawFd;
        let f = std::fs::File::open(p).ok()?;
        if f.metadata().ok()?.len() < size() as u64 {
            return None;
        }
        let map = map(f.as_raw_fd(), size(), false)?;
        let h = map.header();
        if h.magic != MAGIC || h.version != VERSION || h.slots as usize != SLOTS {
            return None;
        }
        let beat = h.beat.load(Ordering::Relaxed);
        Some(Reader {
            map,
            path: p.to_path_buf(),
            inode: inode(p)?,
            beat: (beat, std::time::Instant::now()),
        })
    }

    /// The file was replaced (a new engine), or its writer stopped (no
    /// heartbeat for 2 s): open again.
    pub fn stale(&mut self) -> bool {
        if inode(&self.path) != Some(self.inode) {
            return true;
        }
        let b = self.map.header().beat.load(Ordering::Relaxed);
        if b != self.beat.0 {
            self.beat = (b, std::time::Instant::now());
            return false;
        }
        self.beat.1.elapsed() > std::time::Duration::from_secs(2)
    }

    pub fn newest(&self) -> u64 {
        self.map.header().newest.load(Ordering::Acquire)
    }

    fn read<T: Copy>(slot: &Slot<T>) -> Option<(u64, T)> {
        for _ in 0..4 {
            let s1 = slot.seq.load(Ordering::Acquire);
            if s1 % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            // SAFETY: a copy checked against the sequence below; a torn
            // copy is thrown away.
            let v = unsafe { std::ptr::read_volatile(slot.data.get()) };
            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Relaxed) == s1 {
                return Some((s1, v));
            }
        }
        None
    }

    /// Blocks after `since` (block numbers), oldest first, at most a ring.
    pub fn blocks_since(&self, since: u64) -> (u64, Vec<Block>) {
        let newest = self.newest();
        // Block numbers start at 1; the ring holds the newest SLOTS.
        let from = (since + 1)
            .max((newest + 1).saturating_sub(SLOTS as u64))
            .max(1);
        let mut out = Vec::with_capacity((newest + 1).saturating_sub(from) as usize);
        for n in from..=newest {
            // Written in slot order: a slot overwritten meanwhile (n far
            // behind) or still being written is skipped.
            if let Some((_, b)) = Self::read(self.map.slot(n)) {
                out.push(b);
            }
        }
        (newest, out)
    }

    pub fn now(&self) -> Option<Now> {
        Self::read(&self.map.header().now)
            .filter(|(s, _)| *s > 0)
            .map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_now_round_trip() {
        let dir = std::env::temp_dir().join(format!("patch-shm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("meters.shm");
        let mut w = Writer::create_at(&at).unwrap();
        let mut r = Reader::open_at(&at).unwrap();
        assert_eq!(r.blocks_since(0).1.len(), 0);
        let mut b = MeterBlock::default();
        for i in 0..300u64 {
            b.stamp.nsec = i;
            b.ch_peak[1] = [i as f32, 0.5];
            b.deck_pos[0] = 48_000.0 * i as f64;
            b.deck_load[1] = i as u32;
            b.stamp.rate = 48_000;
            w.block(&b, 3, 2);
        }
        let (newest, got) = r.blocks_since(0);
        assert_eq!(newest, 300);
        assert_eq!(got.len(), SLOTS, "a full ring at most");
        assert_eq!(got.last().unwrap().nsec, 299);
        assert_eq!(got.last().unwrap().ch_peak[1][0], 299.0);
        assert_eq!(got.last().unwrap().deck_secs[0], 299.0);
        assert_eq!(got.last().unwrap().deck_load[1], 299, "which track, with the playhead");
        assert!(
            got.last().unwrap().deck_secs[2].is_nan(),
            "decks past the count: none"
        );
        let (_, few) = r.blocks_since(298);
        assert_eq!(few.len(), 2);
        assert!(r.now().is_none(), "no state yet");
        let mut n = Now::empty();
        n.set_names(&["bus".into(), "deck-a".into()]);
        n.vu = [1.0, 2.0];
        n.faders = [0.9, 0.8, 0.7, 0.6];
        n.eq[2] = [-0.2, 0.4, -0.6, 0.8];
        n.eq[3] = [0.1, -0.3, 0.5, -0.7];
        n.stems = std::array::from_fn(|d| [d as f32 * 0.25; 4]);
        n.stem_levels = std::array::from_fn(|d| [d as f32 * 0.1; 4]);
        w.now(&n);
        let got = r.now().unwrap();
        assert_eq!(got.names(), ["bus", "deck-a"]);
        assert_eq!(got.vu, [1.0, 2.0]);
        assert_eq!(got.faders, n.faders);
        assert_eq!(got.eq, n.eq);
        assert_eq!(got.stems, n.stems);
        assert_eq!(got.stem_levels, n.stem_levels);
        // A new engine: a new file; the reader notices.
        w.beat();
        assert!(!r.stale());
        let _w2 = Writer::create_at(&at).unwrap();
        assert!(r.stale());
        let r2 = Reader::open_at(&at).unwrap();
        assert_eq!(r2.newest(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
