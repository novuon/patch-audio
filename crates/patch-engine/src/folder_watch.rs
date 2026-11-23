//! Directory events for local Linux filesystems. Unsupported mounts or
//! watch limits fall back to the librarian's existing signature polling.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub struct Watch(OwnedFd);

impl Watch {
    pub fn new(root: &Path) -> Option<Self> {
        let root_name = std::ffi::CString::new(root.as_os_str().as_bytes()).ok()?;
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: valid NUL-terminated path and writable statfs storage.
        if unsafe { libc::statfs(root_name.as_ptr(), fs.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: statfs succeeded. Only filesystems with local event semantics.
        let kind = unsafe { fs.assume_init() }.f_type;
        if !matches!(
            kind,
            0xef53 | 0x9123683e | 0x01021994 | 0x794c7630 | 0x58465342 | 0xf2f52010
        ) {
            return None;
        }
        // SAFETY: no pointer arguments; descriptor is nonblocking and not inherited.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: newly created fd, exactly one owner.
        let watch = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            let name = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
            let mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_CLOSE_WRITE
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF
                | libc::IN_ONLYDIR;
            // SAFETY: descriptor and NUL-terminated path remain valid for the call.
            if unsafe { libc::inotify_add_watch(watch.0.as_raw_fd(), name.as_ptr(), mask) } < 0 {
                return None;
            }
            let entries = std::fs::read_dir(dir).ok()?;
            for e in entries {
                let e = e.ok()?;
                if !e.file_name().to_string_lossy().starts_with('.') && e.file_type().ok()?.is_dir()
                {
                    dirs.push(e.path());
                }
            }
        }
        Some(watch)
    }

    /// Overflow and invalidated watches are changes too: rebuild after scanning.
    pub fn changed(&self) -> bool {
        let mut bytes = [0u8; 8192];
        let mut changed = false;
        loop {
            // SAFETY: bytes is writable storage for at most bytes.len() bytes.
            let n =
                unsafe { libc::read(self.0.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if n <= 0 {
                return changed
                    || (n < 0
                        && std::io::Error::last_os_error().kind()
                            != std::io::ErrorKind::WouldBlock);
            }
            changed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_nested_changes_and_new_directory_contents() {
        let root = std::env::temp_dir().join(format!("patch-watch-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let Some(watch) = Watch::new(&root) else {
            std::fs::remove_dir_all(root).unwrap();
            return; // filesystem without local event semantics
        };
        assert!(!watch.changed());
        std::fs::write(root.join("nested/track"), b"audio").unwrap();
        assert!(watch.changed());
        assert!(!watch.changed());
        std::fs::create_dir(root.join("new")).unwrap();
        assert!(watch.changed());
        let watch = Watch::new(&root).unwrap();
        std::fs::write(root.join("new/track"), b"audio").unwrap();
        assert!(watch.changed());
        std::fs::remove_dir_all(root).unwrap();
    }
}
