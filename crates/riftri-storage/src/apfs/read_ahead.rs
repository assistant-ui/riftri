use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

// This is a kernel-cache hint, not a validation shortcut. Limit both the amount
// requested for a large file and the total requested by one clone operation.
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct ReadAhead {
    remaining: AtomicU64,
}

impl ReadAhead {
    pub(super) fn new() -> Self {
        Self {
            remaining: AtomicU64::new(MAX_TREE_BYTES),
        }
    }

    fn reserve(&self, length: u64) -> Option<i32> {
        let count = length.min(MAX_FILE_BYTES);
        if count == 0 {
            return None;
        }
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(count)
            })
            .ok()
            .map(|_| count as i32)
    }

    pub(super) fn advise(&self, path: &Path, length: u64) {
        if let Some(count) = self.reserve(length) {
            // Unsupported or failed hints must not turn a successful native
            // clone into an error. Git still performs its full content check.
            let _ = advise_read(path, count);
        }
    }
}

fn advise_read(path: &Path, count: i32) -> io::Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "read-ahead input is not a regular file",
        ));
    }
    let advice = libc::radvisory {
        ra_offset: 0,
        ra_count: count,
    };
    // SAFETY: the descriptor and initialized, correctly aligned advice remain
    // live throughout fcntl. F_RDADVISE requests asynchronous reads without a
    // userspace copy; its outcome is never evidence of file contents or safety.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_RDADVISE, &advice) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn reserves_only_nonempty_bounded_ranges() {
        let hint = ReadAhead::new();
        assert_eq!(hint.reserve(0), None);
        assert_eq!(hint.remaining.load(Ordering::Relaxed), MAX_TREE_BYTES);
        assert_eq!(hint.reserve(u64::MAX), Some(MAX_FILE_BYTES as i32));
        assert_eq!(hint.reserve(7), Some(7));
        assert_eq!(
            hint.remaining.load(Ordering::Relaxed),
            MAX_TREE_BYTES - MAX_FILE_BYTES - 7
        );
    }

    #[test]
    fn concurrent_workers_cannot_overdraw_the_tree_budget() {
        let hint = ReadAhead::new();
        let requested = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1024 {
                        if let Some(count) = hint.reserve(u64::MAX) {
                            requested.fetch_add(count as u64, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(requested.load(Ordering::Relaxed), MAX_TREE_BYTES);
        assert_eq!(hint.remaining.load(Ordering::Relaxed), 0);
        assert_eq!(hint.reserve(1), None);
    }

    #[test]
    fn hint_preserves_native_names_bytes_and_permissions() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("read-名前");
        fs::write(&path, b"private bytes").unwrap();
        let before = fs::symlink_metadata(&path).unwrap();
        advise_read(&path, 13).unwrap();
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!(before.mode(), after.mode());
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.blocks(), after.blocks());
        assert_eq!(fs::read(&path).unwrap(), b"private bytes");
    }

    #[test]
    fn refuses_links_directories_and_fifos_without_blocking() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("target");
        fs::write(&target, b"unchanged").unwrap();
        let link = fixture.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(advise_read(&link, 9).is_err());
        assert!(advise_read(fixture.path(), 9).is_err());
        let fifo = fixture.path().join("fifo");
        let native = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        assert!(advise_read(&fifo, 9).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
    }

    #[test]
    fn failed_advice_is_optional_and_still_consumes_its_budget() {
        let fixture = tempfile::tempdir().unwrap();
        let missing = fixture.path().join("missing");
        let hint = ReadAhead::new();
        assert!(advise_read(&missing, 10).is_err());
        hint.advise(&missing, 10);
        assert!(!missing.exists());
        assert_eq!(hint.remaining.load(Ordering::Relaxed), MAX_TREE_BYTES - 10);
        // APFS rejects invalid UTF-8 names itself. Keep the native path intact
        // and ignore that failed hint too, without a conversion or a panic.
        let native = fixture.path().join(OsStr::from_bytes(b"read-\xff"));
        hint.advise(&native, 1);
        assert_eq!(hint.remaining.load(Ordering::Relaxed), MAX_TREE_BYTES - 11);
    }
}
