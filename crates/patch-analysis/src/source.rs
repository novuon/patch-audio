//! Verified source identities: a full SHA-256 of a file, and the metadata
//! revision it was taken at.
//!
//! Hash only on a worker. A cache record keeps the digest of the file it was
//! made from and the revision it was last checked against; a lookup whose
//! file still has that revision needs no hashing (see `trust` in lib.rs).
//! Library `content_id` is a sparse hash and is not this identity.

use sha2::{Digest, Sha256};
use std::fs::{Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Metadata revision of an opened local file, for inexpensive revalidation.
/// Includes inode and ctime so same-size replacement or restored mtime alone
/// cannot masquerade as the previously hashed revision on ordinary Unix FSes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Revision {
    device: u64,
    inode: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Revision {
    fn from_metadata(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            len: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
    /// The same file revision, wherever its filesystem is mounted: all but
    /// the device number, which a remount can change (btrfs gives each
    /// mount its own). 2026-11-13: a drive back after a reboot had every
    /// one of its 15,000 tracks checksummed again (over 100 GB read) only
    /// because the device number had changed. Inode, size and both times to
    /// the nanosecond still have to match.
    pub fn same_file(&self, other: &Revision) -> bool {
        self.inode == other.inode && self.len == other.len && self.mtime == other.mtime && self.ctime == other.ctime
    }

    pub fn of(path: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "analysis source must be a regular file",
            ));
        }
        Ok(Self::from_metadata(&meta))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSource {
    pub revision: Revision,
    digest: [u8; 32],
}

impl VerifiedSource {
    pub fn sha256(&self) -> String {
        use std::fmt::Write;
        let mut hex = String::with_capacity(64);
        for b in self.digest {
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    /// Namespace distinct from all old path and sparse-content entries.
    pub fn cache_name(&self, schema: u32) -> String {
        format!("sha256-v{schema}-{}.json", self.sha256())
    }

    pub fn still_matches(&self, path: &Path) -> bool {
        Revision::of(path).is_ok_and(|revision| revision.same_file(&self.revision))
    }
}

/// Bounded-memory full hash with source-revision checks before and after.
/// Cancellation is checked between 64 KiB reads. Filesystem reads themselves
/// may block: the future worker/service owns job deadlines and admission.
pub fn verify(path: &Path, mut cancelled: impl FnMut() -> bool) -> io::Result<VerifiedSource> {
    let revision = Revision::of(path)?;
    // Nonblocking open prevents a regular-file-to-FIFO replacement race
    // from blocking before the descriptor metadata can be checked.
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if Revision::from_metadata(&input.metadata()?) != revision {
        return Err(changed());
    }
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    let mut read = 0u64;
    loop {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "source verification cancelled",
            ));
        }
        let count = match input.read(&mut bytes) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > revision.len {
            return Err(changed());
        }
        digest.update(&bytes[..count]);
    }
    if read != revision.len
        || Revision::from_metadata(&input.metadata()?) != revision
        || Revision::of(path)? != revision
    {
        return Err(changed());
    }
    Ok(VerifiedSource {
        revision,
        digest: digest.finalize().into(),
    })
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "analysis source changed during verification",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "patch-source-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn full_identity_follows_copies_but_detects_unsampled_edits() {
        let temp = Temp::new();
        let a = temp.0.join("a");
        let b = temp.0.join("b");
        let mut bytes = vec![0; 1024 * 1024];
        std::fs::write(&a, &bytes).unwrap();
        std::fs::copy(&a, &b).unwrap();
        let original = verify(&a, || false).unwrap();
        assert_eq!(original.sha256(), verify(&b, || false).unwrap().sha256());
        assert!(original.still_matches(&a));
        bytes[200_000] = 1;
        std::fs::write(&b, &bytes).unwrap();
        assert_ne!(original.sha256(), verify(&b, || false).unwrap().sha256());
        assert_ne!(original.cache_name(7), original.cache_name(8));
    }

    #[test]
    fn known_digest_and_cancellation() {
        let temp = Temp::new();
        let path = temp.0.join("abc");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            verify(&path, || false).unwrap().sha256(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            verify(&path, || true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(verify(&temp.0, || false).is_err());
    }

    #[test]
    fn replacing_the_source_during_hashing_rejects_the_result() {
        let temp = Temp::new();
        let path = temp.0.join("source");
        let next = temp.0.join("next");
        std::fs::write(&path, vec![0; 128 * 1024]).unwrap();
        std::fs::write(&next, vec![1; 128 * 1024]).unwrap();
        let mut calls = 0;
        let result = verify(&path, || {
            calls += 1;
            if calls == 2 {
                std::fs::rename(&next, &path).unwrap();
            }
            false
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}

#[cfg(test)]
mod remount_tests {
    use super::*;

    #[test]
    fn a_remount_keeps_the_revision_but_a_changed_file_does_not() {
        let a = Revision { device: 64, inode: 9, len: 100, mtime: (5, 1), ctime: (6, 2) };
        assert!(a.same_file(&Revision { device: 71, ..a }), "another device number: the same file");
        assert!(!a.same_file(&Revision { ctime: (6, 3), ..a }));
        assert!(!a.same_file(&Revision { inode: 10, ..a }));
        assert!(!a.same_file(&Revision { len: 101, ..a }));
    }
}
