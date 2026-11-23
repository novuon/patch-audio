//! Content identity of an audio file: the file's size plus FNV-1a hashes of
//! three 64 KiB blocks (head, middle, tail). Stable under moves and renames;
//! reading ~192 KiB per file keeps a scan of a large library fast.
//!
//! Known limit: editing tags rewrites the head of most formats, which gives
//! the file a new identity (its old metadata stays under the old identity
//! and can be re-linked). A decoded-audio fingerprint would survive that and
//! is the planned long-term identity.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const BLOCK: u64 = 64 * 1024;

fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

pub fn content_id(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    fnv(&mut h, &size.to_le_bytes());
    let mut buf = vec![0u8; BLOCK as usize];
    for start in [
        0,
        size.saturating_sub(BLOCK) / 2,
        size.saturating_sub(BLOCK),
    ] {
        f.seek(SeekFrom::Start(start))?;
        let n = f.read(&mut buf)?;
        fnv(&mut h, &buf[..n]);
    }
    Ok(format!("{size:x}-{h:016x}"))
}
