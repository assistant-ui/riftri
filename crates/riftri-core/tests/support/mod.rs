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
