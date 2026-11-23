//! Sample-stamped program capture. One SPSC producer (RT) and one socket consumer.
//! Socket writes, allocation and serialization happen only on a control thread.
use crate::{Params, ring::Ring};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};

pub const FRAMES: usize = 256;
pub const MAGIC: u64 = 0x504154434850434d;
#[derive(Clone, Copy)]
pub struct Packet {
    pub sample: u64,
    pub rate: u32,
    pub frames: u32,
    pub audio: [f32; FRAMES * 2],
}
impl Default for Packet {
    fn default() -> Self {
        Self {
            sample: 0,
            rate: 0,
            frames: 0,
            audio: [0.0; FRAMES * 2],
        }
    }
}
pub struct Capture {
    pub active: AtomicBool,
    pub packets: Ring<Packet>,
}
impl Default for Capture {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            packets: Ring::new(4096, Packet::default()),
        }
    }
}
impl Capture {
    /// The only producer; called with the pre-device-volume program in the callback.
    ///
    /// # Safety
    /// Only the engine callback may call this.
    pub unsafe fn push(&self, sample: u64, rate: u32, audio: &[f32]) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        for (i, chunk) in audio.chunks(FRAMES * 2).enumerate() {
            let mut p = Packet {
                sample: sample + (i * FRAMES) as u64,
                rate,
                frames: (chunk.len() / 2) as u32,
                ..Default::default()
            };
            p.audio[..chunk.len()].copy_from_slice(chunk);
            // SAFETY: the callback is the only producer (this fn's contract).
            unsafe { self.packets.push(p) };
        }
    }
}
struct Lease<'a>(&'a Capture);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}
pub fn stream(params: &Params, mut stream: UnixStream) -> Result<(), String> {
    if params
        .capture
        .active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return Err("Program capture already in use".into());
    }
    let _lease = Lease(&params.capture);
    // Prior abandoned packets are distinguishable by sample continuity. Drain once before streaming.
    // SAFETY: the lease above makes this connection the only consumer.
    while unsafe { params.capture.packets.pop() }.is_some() {}
    let dropped = params.capture.packets.dropped.load(Ordering::Relaxed);
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(32 + FRAMES * 8);
    loop {
        if params.capture.packets.dropped.load(Ordering::Relaxed) != dropped {
            return Err("Program capture queue overflow".into());
        }
        // SAFETY: as above, the lease holder is the only consumer.
        let Some(p) = (unsafe { params.capture.packets.pop() }) else {
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        };
        bytes.clear();
        bytes.extend(MAGIC.to_le_bytes());
        bytes.extend(p.sample.to_le_bytes());
        bytes.extend(p.rate.to_le_bytes());
        bytes.extend(p.frames.to_le_bytes());
        bytes.extend(
            p.audio[..p.frames as usize * 2]
                .iter()
                .flat_map(|v| v.to_le_bytes()),
        );
        stream.write_all(&bytes).map_err(|e| e.to_string())?;
    }
}
pub fn read_packet(stream: &mut impl Read) -> std::io::Result<Packet> {
    let mut header = [0u8; 24];
    stream.read_exact(&mut header)?;
    let u64_at = |i| u64::from_le_bytes(header[i..i + 8].try_into().unwrap());
    let u32_at = |i| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
    let (magic, sample, rate, frames) = (u64_at(0), u64_at(8), u32_at(16), u32_at(20));
    if magic != MAGIC || frames == 0 || frames as usize > FRAMES || rate == 0 {
        return Err(std::io::Error::other("Invalid program packet"));
    }
    let mut bytes = [0u8; FRAMES * 8];
    stream.read_exact(&mut bytes[..frames as usize * 8])?;
    let mut p = Packet {
        sample,
        rate,
        frames,
        ..Default::default()
    };
    for (v, b) in p
        .audio
        .iter_mut()
        .zip(bytes[..frames as usize * 8].chunks_exact(4))
    {
        *v = f32::from_le_bytes(b.try_into().unwrap());
    }
    Ok(p)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_program_precedes_device_volume_and_keeps_graph_stamp() {
        let p = std::sync::Arc::new(Params::new(&["bus"]));
        p.master_limiter.store(false, Ordering::Relaxed);
        p.capture.active.store(true, Ordering::Relaxed);
        p.set_system(0.5, 0.5, false);
        p.system_post.store(true, Ordering::Relaxed);
        let mut rt = crate::Rt::new(p.clone());
        let input = vec![0.25; 256];
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        let clock = crate::Clock {
            position: 1234,
            nsec: 0,
            rate: 48000,
            frames: 256,
            delay: 0,
            ..Default::default()
        };
        rt.process(
            &clock,
            &[Some(&input), Some(&input)],
            [&mut l, &mut r],
            None,
        );
        let packet = unsafe { p.capture.packets.pop() }.unwrap();
        assert_eq!(packet.sample, 1234);
        assert_eq!(packet.rate, 48000);
        assert!(
            packet
                .audio
                .iter()
                .zip(l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]))
                .all(|(captured, device)| (*captured - device * 2.0).abs() < 1e-6)
        );
    }
    #[test]
    fn packets_preserve_samples_and_partial_quantums() {
        let c = Capture::default();
        c.active.store(true, Ordering::Relaxed);
        let samples: Vec<f32> = (0..600).map(|i| i as f32 / 600.0).collect();
        unsafe { c.push(1000, 48000, &samples) };
        let a = unsafe { c.packets.pop() }.unwrap();
        let b = unsafe { c.packets.pop() }.unwrap();
        assert_eq!(
            (a.sample, a.frames, b.sample, b.frames),
            (1000, 256, 1256, 44)
        );
        assert_eq!(&a.audio[..512], &samples[..512]);
        assert_eq!(&b.audio[..88], &samples[512..]);
    }
}
