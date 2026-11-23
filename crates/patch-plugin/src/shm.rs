//! The audio path between the engine and the plugin host process: one
//! shared memory, lock-free both ways, so a plugin runs in a process of its
//! own and its crash never reaches the engine.
//!
//! Each insert point ([`crate::POINTS`]: the master, the buses, channels)
//! has lanes of its own. In block n the engine writes each point's input
//! into its lane n % SLOTS, then says block n is complete (`submitted`);
//! it plays each point's output for block n − 1 if it is there (dry if
//! not). The host takes block n at every point it runs. Each lane is
//! a seqlock: its sequence reads [`WRITING`] while it is written and the
//! block's number when done; a reader keeps a copy only when the number
//! was the one it wanted, unchanged across its copy.
//!
//! The header also says what the host is doing (`busy`: the slot whose
//! plugin is running now), so the supervisor can name the plugin that took
//! the host down.
//!
//! Nothing costs memory or time until a plugin is wanted: a point's lanes
//! are only touched (faulted in and locked, off the audio thread) when the
//! supervisor [`Mapping::prepare`]s it, and given back to the system when
//! it is done. The host does not poll: it sleeps on a futex the engine
//! wakes as it completes a block.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

const MAGIC: u64 = 0x5041_5443_4856_5354; // "PATCHVST"
const VERSION: u32 = 3;
use crate::POINTS;
/// Blocks in flight each way: the engine waits for none of them.
pub const SLOTS: usize = 4;
const WRITING: u64 = u64::MAX;
/// The audio follows the header from here (one page).
const DATA: usize = 4096;

#[repr(C)]
struct Header {
    magic: u64,
    version: u32,
    max_frames: u32,
    /// The points where the host has plugins (bit per point): the engine
    /// routes those through it.
    engaged: AtomicU32,
    /// The rack slot whose plugin runs on the host now (0: none), on the
    /// audio worker and on the loader.
    busy_audio: AtomicU32,
    busy_loader: AtomicU32,
    _pad: u32,
    /// Bumped by the host's audio worker every pass, for the watchdog.
    beat: AtomicU64,
    /// The newest block the engine submitted (0: none yet).
    submitted: AtomicU64,
    /// Blocks the engine played dry because the host had not answered.
    late: AtomicU64,
    /// Bumped with each completed block: the futex the host sleeps on.
    kick: AtomicU32,
    /// The host is asleep on `kick` (the engine wakes it only then).
    sleeping: AtomicU32,
}

#[repr(C)]
struct Lane {
    seq: AtomicU64,
    frames: AtomicU64,
}

/// A block's place in the mapping: its lane, then left and right.
fn lane_size(max_frames: usize) -> usize {
    (std::mem::size_of::<Lane>() + 2 * max_frames * std::mem::size_of::<f32>()).next_multiple_of(64)
}

/// One point's lanes, whole pages: one is given back without touching its
/// neighbours.
fn point_size(max_frames: usize) -> usize {
    (2 * SLOTS * lane_size(max_frames)).next_multiple_of(4096)
}

fn size(max_frames: usize) -> usize {
    DATA + POINTS * point_size(max_frames)
}

pub struct Mapping {
    ptr: *mut u8,
    len: usize,
    max_frames: usize,
    /// The engine's memfd, kept open for hosts to come.
    file: Option<std::fs::File>,
}

// SAFETY: plain shared memory, accessed through atomics and seqlocked
// copies only.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: ptr/len are the mmap result.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

fn map(file: &std::fs::File, len: usize) -> Result<*mut u8> {
    use std::os::fd::AsRawFd;
    // SAFETY: a shared mapping of an open file at least `len` bytes long.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        bail!("map plugin audio: {}", std::io::Error::last_os_error());
    }
    Ok(ptr as *mut u8)
}

impl Mapping {
    /// The engine's side: anonymous memory (a memfd, nothing on disk),
    /// zeroed, every page touched and locked (no page fault on the audio
    /// callback later). The host opens it by [`Mapping::path`].
    pub fn create(max_frames: usize) -> Result<Self> {
        use std::os::fd::FromRawFd;
        let len = size(max_frames);
        // SAFETY: a plain syscall; the fd is owned by the File below.
        let fd = unsafe { libc::memfd_create(c"patch-plugins".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            bail!("plugin audio memory: {}", std::io::Error::last_os_error());
        }
        // SAFETY: fd is new and ours.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        // Sparse: a memfd's pages are zero and take no memory until used.
        file.set_len(len as u64)?;
        let ptr = map(&file, len)?;
        // SAFETY: the mapping is fresh and ours alone until the host opens
        // it; only the header page is touched and locked now.
        unsafe {
            let h = ptr as *mut Header;
            (*h).magic = MAGIC;
            (*h).version = VERSION;
            (*h).max_frames = max_frames as u32;
            libc::mlock(ptr as *const libc::c_void, DATA);
        }
        Ok(Self { ptr, len, max_frames, file: Some(file) })
    }

    /// Where another process of this user opens the engine's memory.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        use std::os::fd::AsRawFd;
        let file = self.file.as_ref()?;
        Some(format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd()).into())
    }

    /// The host's side.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata()?.len() as usize;
        if len < DATA {
            bail!("{} is not Patch's plugin audio", path.display());
        }
        let ptr = map(&file, len)?;
        // SAFETY: at least DATA bytes are mapped; the header is plain data.
        let (magic, version, max_frames) = unsafe {
            let h = &*(ptr as *const Header);
            (h.magic, h.version, h.max_frames as usize)
        };
        let mapping = Self { ptr, len, max_frames, file: None };
        if magic != MAGIC || version != VERSION || len < size(max_frames) {
            bail!("{} is not this version's plugin audio", path.display());
        }
        Ok(mapping)
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping holds the header at its start, page-aligned.
        unsafe { &*(self.ptr as *const Header) }
    }

    /// Lane `n` of a point's inputs (`output` false) or outputs.
    fn lane(&self, point: usize, output: bool, n: u64) -> (&Lane, *mut f32) {
        let point = point.min(POINTS - 1);
        let i = (n % SLOTS as u64) as usize + if output { SLOTS } else { 0 };
        // SAFETY: i < 2 * SLOTS; every point's lanes lie within size(max_frames).
        unsafe {
            let at = self.ptr.add(DATA + point * point_size(self.max_frames) + i * lane_size(self.max_frames));
            (&*(at as *const Lane), at.add(std::mem::size_of::<Lane>()) as *mut f32)
        }
    }

    /// Write block `seq` into its lane.
    fn put(&self, point: usize, output: bool, seq: u64, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len()).min(self.max_frames);
        let (lane, data) = self.lane(point, output, seq);
        lane.seq.store(WRITING, Ordering::Relaxed);
        fence(Ordering::Release);
        // SAFETY: one writer per direction; readers check the sequence
        // around their copy. Both halves fit the lane (frames ≤ max).
        unsafe {
            std::ptr::copy_nonoverlapping(left.as_ptr(), data, frames);
            std::ptr::copy_nonoverlapping(right.as_ptr(), data.add(self.max_frames), frames);
        }
        lane.frames.store(frames as u64, Ordering::Relaxed);
        lane.seq.store(seq, Ordering::Release);
    }

    /// Copy block `seq` out if its lane holds it whole: its frame count.
    fn get(&self, point: usize, output: bool, seq: u64, left: &mut [f32], right: &mut [f32]) -> Option<usize> {
        let (lane, data) = self.lane(point, output, seq);
        if lane.seq.load(Ordering::Acquire) != seq {
            return None;
        }
        let frames = (lane.frames.load(Ordering::Relaxed) as usize)
            .min(left.len())
            .min(right.len())
            .min(self.max_frames);
        // SAFETY: within the lane; a torn copy is discarded below.
        unsafe {
            std::ptr::copy_nonoverlapping(data, left.as_mut_ptr(), frames);
            std::ptr::copy_nonoverlapping(data.add(self.max_frames), right.as_mut_ptr(), frames);
        }
        fence(Ordering::Acquire);
        (lane.seq.load(Ordering::Relaxed) == seq).then_some(frames)
    }

    // The engine's and the supervisor's view of the host.

    /// The host runs plugins somewhere.
    pub fn engaged(&self) -> bool {
        self.header().engaged.load(Ordering::Acquire) != 0
    }

    /// The host runs plugins at `point`.
    pub fn engaged_at(&self, point: usize) -> bool {
        self.header().engaged.load(Ordering::Acquire) & (1 << point) != 0
    }

    /// The host is gone: route around it until a new one engages.
    pub fn disengage(&self) {
        let h = self.header();
        h.engaged.store(0, Ordering::Release);
        h.busy_audio.store(0, Ordering::Relaxed);
        h.busy_loader.store(0, Ordering::Relaxed);
    }

    /// The slots that were running when the host stopped (audio, loader).
    pub fn busy(&self) -> (u32, u32) {
        let h = self.header();
        (h.busy_audio.load(Ordering::Acquire), h.busy_loader.load(Ordering::Acquire))
    }

    pub fn beat(&self) -> u64 {
        self.header().beat.load(Ordering::Relaxed)
    }

    pub fn submitted(&self) -> u64 {
        self.header().submitted.load(Ordering::Relaxed)
    }

    pub fn late(&self) -> u64 {
        self.header().late.load(Ordering::Relaxed)
    }

    // The engine's audio callback.

    /// A point's input for block `seq`; the host sees it once the block
    /// is [`Mapping::publish`]ed.
    pub(crate) fn submit(&self, point: usize, seq: u64, left: &[f32], right: &[f32]) {
        self.put(point, false, seq, left, right);
    }

    /// Block `seq` is complete at every point: wake the host if it sleeps
    /// (one futex call, only while a plugin runs).
    pub(crate) fn publish(&self, seq: u64) {
        let h = self.header();
        h.submitted.store(seq, Ordering::Release);
        h.kick.fetch_add(1, Ordering::SeqCst);
        if h.sleeping.load(Ordering::SeqCst) != 0 {
            // SAFETY: a futex wake on a u32 of this shared mapping.
            unsafe {
                libc::syscall(libc::SYS_futex, h.kick.as_ptr(), libc::FUTEX_WAKE, 1);
            }
        }
    }

    /// The host's side of [`Mapping::publish`]: the block count to sleep
    /// on, read before looking for a block.
    pub(crate) fn kicks(&self) -> u32 {
        self.header().kick.load(Ordering::SeqCst)
    }

    /// Sleep until the engine completes a block after `seen` (or
    /// `timeout`).
    pub(crate) fn wait(&self, seen: u32, timeout: std::time::Duration) {
        let h = self.header();
        h.sleeping.store(1, Ordering::SeqCst);
        if h.kick.load(Ordering::SeqCst) == seen {
            let ts = libc::timespec {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_nsec: libc::c_long::from(timeout.subsec_nanos() as i32),
            };
            // SAFETY: a futex wait on a u32 of this shared mapping (shared
            // between processes: not the private variant).
            unsafe {
                libc::syscall(libc::SYS_futex, h.kick.as_ptr(), libc::FUTEX_WAIT, seen, &ts as *const libc::timespec);
            }
        }
        h.sleeping.store(0, Ordering::SeqCst);
    }

    /// Make a point's lanes ready (`on`: faulted in and locked, so the
    /// audio thread never faults on them) or give their memory back. Off
    /// the audio thread, and only once nothing reads them.
    pub fn prepare(&self, point: usize, on: bool) {
        let Some(file) = &self.file else { return };
        if point >= POINTS {
            return;
        }
        let stride = point_size(self.max_frames);
        let offset = DATA + point * stride;
        // SAFETY: the range is this point's own pages of the mapping.
        unsafe {
            let at = self.ptr.add(offset) as *const libc::c_void;
            if on {
                libc::mlock(at, stride);
            } else {
                use std::os::fd::AsRawFd;
                libc::munlock(at, stride);
                libc::fallocate(
                    file.as_raw_fd(),
                    libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                    offset as libc::off_t,
                    stride as libc::off_t,
                );
            }
        }
    }

    pub(crate) fn processed(&self, point: usize, seq: u64, left: &mut [f32], right: &mut [f32]) -> Option<usize> {
        let got = self.get(point, true, seq, left, right);
        if got.is_none() {
            self.header().late.fetch_add(1, Ordering::Relaxed);
        }
        got
    }

    // The host.

    /// The newest complete block the host has not taken yet.
    pub(crate) fn next(&self, after: u64) -> Option<u64> {
        let seq = self.header().submitted.load(Ordering::Acquire);
        (seq != 0 && seq != after).then_some(seq)
    }

    /// A point's input for block `seq`, copied out (none: the engine did
    /// not pass that point in this block).
    pub(crate) fn take(&self, point: usize, seq: u64, left: &mut [f32], right: &mut [f32]) -> Option<usize> {
        self.get(point, false, seq, left, right)
    }

    pub(crate) fn finish(&self, point: usize, seq: u64, left: &[f32], right: &[f32]) {
        self.put(point, true, seq, left, right);
    }

    /// The points the host runs plugins at (bit per point).
    pub(crate) fn set_engaged(&self, points: u32) {
        self.header().engaged.store(points, Ordering::Release);
    }

    pub(crate) fn set_busy_audio(&self, slot: u32) {
        self.header().busy_audio.store(slot, Ordering::Release);
    }

    pub(crate) fn set_busy_loader(&self, slot: u32) {
        self.header().busy_loader.store(slot, Ordering::Release);
    }

    pub(crate) fn tick(&self) {
        self.header().beat.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_crosses_and_a_stale_one_does_not() {
        let engine = Mapping::create(512).unwrap();
        let host = Mapping::open(&engine.path().unwrap()).unwrap();
        let (l, r) = (vec![0.25f32; 256], vec![-0.5f32; 256]);
        engine.submit(0, 1, &l, &r);
        assert_eq!(host.next(0), None, "not before the block is complete");
        engine.publish(1);
        let (mut hl, mut hr) = (vec![0.0; 512], vec![0.0; 512]);
        assert_eq!(host.next(0), Some(1));
        assert_eq!(host.next(1), None, "taken once");
        let frames = host.take(0, 1, &mut hl, &mut hr).unwrap();
        assert_eq!(frames, 256);
        host.finish(0, 1, &hl[..frames], &hr[..frames]);
        let (mut el, mut er) = (vec![0.0; 256], vec![0.0; 256]);
        assert_eq!(engine.processed(0, 1, &mut el, &mut er), Some(256));
        assert_eq!((el, er), (l.clone(), r.clone()));
        // Block 5 shares block 1's lane: a reader asking for 1 gets nothing.
        host.finish(0, 5, &hl[..frames], &hr[..frames]);
        assert_eq!(engine.processed(0, 1, &mut [0.0; 256], &mut [0.0; 256]), None);
        assert_eq!(engine.late(), 1);
    }

    /// Memory resident in this process's mapping (pages, by mincore).
    fn resident(m: &Mapping) -> usize {
        let pages = m.len.div_ceil(4096);
        let mut v = vec![0u8; pages];
        // SAFETY: the vector holds one byte per page of the mapping.
        unsafe { libc::mincore(m.ptr as *mut libc::c_void, m.len, v.as_mut_ptr()) };
        v.iter().filter(|b| *b & 1 != 0).count()
    }

    #[test]
    fn a_point_costs_memory_only_while_prepared() {
        let engine = Mapping::create(8192).unwrap();
        assert!(resident(&engine) <= 1, "only the header at start: {} pages", resident(&engine));
        engine.prepare(3, true);
        let one = resident(&engine);
        assert!(one >= point_size(8192) / 4096, "point 3 is in: {one} pages");
        engine.submit(3, 1, &[0.5; 64], &[0.5; 64]);
        engine.prepare(3, false);
        assert!(resident(&engine) <= 1, "given back: {} pages", resident(&engine));
        let (mut l, mut r) = ([1.0f32; 64], [1.0f32; 64]);
        assert_eq!(engine.get(3, false, 1, &mut l, &mut r), None, "zero again");
    }

    #[test]
    fn the_host_sleeps_until_a_block_is_complete() {
        let engine = Mapping::create(64).unwrap();
        let host = Mapping::open(&engine.path().unwrap()).unwrap();
        let seen = host.kicks();
        let waiter = std::thread::spawn(move || {
            let t = std::time::Instant::now();
            host.wait(seen, std::time::Duration::from_secs(5));
            t.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        engine.publish(1);
        let slept = waiter.join().unwrap();
        assert!(slept < std::time::Duration::from_secs(1), "woken by the block, not the timeout: {slept:?}");
        assert!(slept >= std::time::Duration::from_millis(40), "slept until then: {slept:?}");
    }

    #[test]
    fn points_keep_their_own_audio() {
        let engine = Mapping::create(64).unwrap();
        let host = Mapping::open(&engine.path().unwrap()).unwrap();
        engine.submit(0, 1, &[0.5; 64], &[0.5; 64]);
        engine.submit(POINTS - 1, 1, &[-0.25; 64], &[-0.25; 64]);
        engine.publish(1);
        let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
        assert_eq!(host.take(POINTS - 1, 1, &mut l, &mut r), Some(64));
        assert_eq!(l, [-0.25; 64]);
        assert_eq!(host.take(0, 1, &mut l, &mut r), Some(64));
        assert_eq!(l, [0.5; 64]);
        assert_eq!(host.take(3, 1, &mut l, &mut r), None, "a point the engine did not pass");
        host.set_engaged(1 << 3);
        assert!(engine.engaged_at(3) && !engine.engaged_at(0) && engine.engaged());
    }
}
