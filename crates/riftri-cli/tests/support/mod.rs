use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
use riftri_storage::ApfsCloner as NativeCowCloner;
#[cfg(target_os = "linux")]
use riftri_storage::ReflinkCloner as NativeCowCloner;
#[cfg(target_os = "windows")]
use riftri_storage::RefsBlockCloner as NativeCowCloner;
use tempfile::TempDir;

pub(crate) struct WritableTempDir {
    directory: Option<TempDir>,
}

impl WritableTempDir {
    pub(crate) fn path(&self) -> &Path {
        self.directory
            .as_ref()
            .expect("fixture directory is available")
            .path()
    }
}

impl Drop for WritableTempDir {
    fn drop(&mut self) {
        let Some(directory) = self.directory.take() else {
            return;
        };
        let path = directory.path().to_path_buf();
        if let Err(error) = NativeCowCloner::make_tree_owner_writable(&path) {
            if std::thread::panicking() {
                eprintln!(
                    "failed to restore writable fixture permissions for {}: {error}",
                    path.display()
                );
                return;
            }
            panic!(
                "failed to restore writable fixture permissions for {}: {error}",
                path.display()
            );
        }
        if let Err(error) = directory.close() {
            if std::thread::panicking() {
                eprintln!(
                    "failed to remove fixture directory {}: {error}",
                    path.display()
                );
                return;
            }
            panic!(
                "failed to remove fixture directory {}: {error}",
                path.display()
            );
        }
    }
}

pub(crate) fn writable_tempdir() -> io::Result<WritableTempDir> {
    tempfile::tempdir().map(|directory| WritableTempDir {
        directory: Some(directory),
    })
}

/// Releases a paused Git wrapper when it goes out of scope.
///
/// The wrappers in these tests spin for up to 30 seconds waiting for a release
/// file, spawning a `sleep` each pass. Writing that file only on the success
/// path means any panic before it leaves `paused-git` and its `sleep` running
/// after the test binary exits. On the Linux reflink jobs those orphans keep
/// the disposable volume busy and the unmount step fails, which fails the job
/// even though every test passed.
pub(crate) struct PausedGit {
    release: std::path::PathBuf,
}

impl PausedGit {
    pub(crate) fn new(release: impl Into<std::path::PathBuf>) -> Self {
        PausedGit {
            release: release.into(),
        }
    }

    /// Release it now, on the normal path, and keep the guard harmless.
    pub(crate) fn release(&self) {
        let _ = std::fs::write(&self.release, "release");
    }
}

impl Drop for PausedGit {
    fn drop(&mut self) {
        // Writing twice is fine: the wrapper only checks that the file exists.
        self.release();
    }
}

#[cfg(test)]
mod paused_git_guard_tests {
    use super::PausedGit;

    #[test]
    fn dropping_the_guard_releases_a_panicking_test() {
        let directory = tempfile::tempdir().expect("scratch");
        let release = directory.path().join("release");
        let caught = std::panic::catch_unwind({
            let release = release.clone();
            move || {
                let _paused = PausedGit::new(&release);
                panic!("a test failing while the wrapper is paused");
            }
        });
        assert!(caught.is_err(), "the panic must still propagate");
        assert!(
            release.exists(),
            "unwinding must release the wrapper, or paused-git outlives the test",
        );
    }
}
