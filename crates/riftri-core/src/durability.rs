//! The flushes behind Riftri's durable writes.
//!
//! Every durable state change is a write to a temporary file, a flush of that
//! file, an atomic rename into place, and a full flush of the directory that
//! holds it. The directory flush is what makes the write durable: it is the
//! last step, and nothing that depends on the new state starts before it
//! returns. The file flush before the rename only has to guarantee that no
//! device can persist the rename without the content it names.
//!
//! On macOS, `File::sync_all` is `fcntl(F_FULLFSYNC)`, which empties the
//! drive's write cache and takes several milliseconds. `fcntl(2)` documents
//! `F_BARRIERFSYNC` as an `fsync(2)` followed by a barrier: the flushed I/O is
//! persisted before any I/O that follows the barrier. The rename follows it,
//! so a power loss can never leave the destination naming a file without its
//! content. The directory's `F_FULLFSYNC` then drains the whole device queue,
//! and `fcntl(2)` guarantees that data fsync'd on the same device before it is
//! persisted when it returns, so the content and the rename are both on stable
//! storage when the write returns, exactly as with two full flushes
//! (decision D046).
//!
//! Linux and Windows keep `fsync` and `FlushFileBuffers`, whose semantics
//! offer no such split.

use std::fs::File;
use std::io;

/// Flush a fully written temporary file so that a rename issued afterwards
/// can never be persisted without its content. The caller must follow the
/// rename with [`sync_full`] on the containing directory, on the same device,
/// before relying on the new state.
#[cfg(target_os = "macos")]
pub(crate) fn sync_before_rename(file: &File) -> io::Result<()> {
    sync_before_rename_with(file, issue_barrier)
}

/// Flush a fully written temporary file before it is renamed into place.
#[cfg(not(target_os = "macos"))]
pub(crate) fn sync_before_rename(file: &File) -> io::Result<()> {
    sync_full(file)
}

/// Flush a file or directory all the way to stable storage. On macOS this is
/// `F_FULLFSYNC`, which also persists everything fsync'd on the same device
/// before it.
pub(crate) fn sync_full(file: &File) -> io::Result<()> {
    file.sync_all()?;
    #[cfg(test)]
    crate::test_hooks::record_flush(crate::test_hooks::Flush::Full);
    Ok(())
}

#[cfg(target_os = "macos")]
fn issue_barrier(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: the descriptor stays open for the borrow of `file`, and
    // F_BARRIERFSYNC ignores its argument.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Issue `barrier`, and take a full flush instead whenever the filesystem or
/// device does not provide one, so durability never drops silently. Any other
/// failure is reported unchanged, as a failed full flush would be.
#[cfg(target_os = "macos")]
fn sync_before_rename_with(
    file: &File,
    mut barrier: impl FnMut(&File) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        match barrier(file) {
            Ok(()) => {
                #[cfg(test)]
                crate::test_hooks::record_flush(crate::test_hooks::Flush::Barrier);
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if barrier_unsupported(&error) => return sync_full(file),
            Err(error) => return Err(error),
        }
    }
}

/// The errors a filesystem or device returns for a flush command it does not
/// implement, as opposed to a flush that was attempted and failed.
#[cfg(target_os = "macos")]
fn barrier_unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL | libc::ENOTTY | libc::ENOSYS)
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::cell::Cell;
    use std::io::{self, Write};

    use super::{sync_before_rename, sync_before_rename_with};
    use crate::test_hooks::{Flush, take_flushes};

    fn written_file() -> (tempfile::TempDir, std::fs::File) {
        let directory = tempfile::tempdir().expect("directory");
        let mut file = std::fs::File::create(directory.path().join("journal.tmp")).expect("file");
        file.write_all(b"{}\n").expect("write");
        (directory, file)
    }

    /// APFS implements the barrier, so the journal's pre-rename flush is one.
    #[test]
    fn a_temporary_file_is_ordered_with_a_barrier_on_apfs() {
        let (_directory, file) = written_file();
        take_flushes();
        sync_before_rename(&file).expect("barrier");
        assert_eq!(take_flushes(), [Flush::Barrier]);
    }

    /// A filesystem or device that does not implement the barrier gets a full
    /// flush instead: durability never drops silently.
    #[test]
    fn an_unsupported_barrier_falls_back_to_a_full_flush() {
        for errno in [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EINVAL,
            libc::ENOTTY,
            libc::ENOSYS,
        ] {
            let (_directory, file) = written_file();
            take_flushes();
            let attempts = Cell::new(0);
            sync_before_rename_with(&file, |_| {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(errno))
            })
            .expect("full flush fallback");
            assert_eq!(attempts.get(), 1, "errno {errno}");
            assert_eq!(take_flushes(), [Flush::Full], "errno {errno}");
        }
    }

    /// An interrupted barrier is issued again, as `sync_all` retries `EINTR`.
    #[test]
    fn an_interrupted_barrier_is_retried() {
        let (_directory, file) = written_file();
        take_flushes();
        let attempts = Cell::new(0);
        sync_before_rename_with(&file, |_| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(())
            }
        })
        .expect("barrier after retry");
        assert_eq!(attempts.get(), 2);
        assert_eq!(take_flushes(), [Flush::Barrier]);
    }

    /// A barrier that was attempted and failed is a failed flush. It is
    /// reported rather than retried as a full flush, which could report
    /// success for writes the failed attempt already dropped.
    #[test]
    fn a_failed_barrier_is_reported_not_masked() {
        let (_directory, file) = written_file();
        take_flushes();
        let error =
            sync_before_rename_with(&file, |_| Err(io::Error::from_raw_os_error(libc::EIO)))
                .expect_err("I/O failure");
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(take_flushes(), []);
    }
}
