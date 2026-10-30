//! Permissions a Unix tree needs before `remove_dir_all` can delete it.
//!
//! Unlinking an entry needs write and search permission on its parent
//! directory and nothing on the entry itself, so a read-only file or symlink
//! never blocks deletion. Listing a directory, which recursive deletion must do
//! first, also needs read permission on it. Only directories are therefore
//! inspected and changed: the entry type comes from `readdir`, so files and
//! symlinks are neither stat'ed nor chmod'ed, and a directory is chmod'ed only
//! when it lacks one of the owner `rwx` bits. Symlinks are never followed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::StorageError;

/// Owner read, write, and search: what listing a directory and unlinking its
/// entries require.
const OWNER_REMOVAL_BITS: u32 = 0o700;

pub(crate) fn make_tree_removable(path: &Path) -> Result<(), StorageError> {
    let metadata = symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        // A file or symlink is deleted through its parent's permissions.
        return Ok(());
    }
    make_directory_removable(path, metadata.permissions().mode())
}

fn make_directory_removable(path: &Path, mode: u32) -> Result<(), StorageError> {
    if mode & OWNER_REMOVAL_BITS != OWNER_REMOVAL_BITS {
        set_mode(path, mode | OWNER_REMOVAL_BITS)?;
    }
    // Collect child directory names and close the handle before descending,
    // so a deep tree holds one directory handle at a time.
    let mut directories = Vec::new();
    for entry in fs::read_dir(path).map_err(|source| io("read tree for removal", path, source))? {
        let entry = entry.map_err(|source| io("read tree entry for removal", path, source))?;
        let file_type = entry
            .file_type()
            .map_err(|source| io("inspect tree entry for removal", &entry.path(), source))?;
        if file_type.is_dir() {
            directories.push(entry.file_name());
        }
    }
    let mut child = path.to_path_buf();
    for name in directories {
        child.push(name);
        // Re-read without following links: the entry type above came from
        // `readdir`, and only a real directory's own mode is ever changed.
        let metadata = symlink_metadata(&child)?;
        if metadata.file_type().is_dir() {
            make_directory_removable(&child, metadata.permissions().mode())?;
        }
        child.pop();
    }
    Ok(())
}

fn symlink_metadata(path: &Path) -> Result<fs::Metadata, StorageError> {
    #[cfg(test)]
    counters::record(&counters::LSTATS, path);
    fs::symlink_metadata(path).map_err(|source| io("inspect tree for removal", path, source))
}

fn set_mode(path: &Path, mode: u32) -> Result<(), StorageError> {
    #[cfg(test)]
    counters::record(&counters::CHMODS, path);
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| io("prepare directory for removal", path, source))
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

/// Per-thread records of the paths this module stat'ed and chmod'ed.
#[cfg(test)]
mod counters {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::thread::LocalKey;

    thread_local! {
        pub(super) static LSTATS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
        pub(super) static CHMODS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(counter: &'static LocalKey<RefCell<Vec<PathBuf>>>, path: &Path) {
        counter.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
    }

    pub(super) fn take(counter: &'static LocalKey<RefCell<Vec<PathBuf>>>) -> Vec<PathBuf> {
        counter.with(|paths| std::mem::take(&mut *paths.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{counters, make_tree_removable};

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn sorted(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths.sort();
        paths
    }

    #[test]
    fn read_only_tree_is_deleted_without_touching_files_or_link_targets() {
        let root = tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("target");
        fs::write(&outside_file, b"keep").unwrap();
        set_mode(&outside_file, 0o444);
        set_mode(&outside, 0o555);

        let tree = root.path().join("tree");
        let nested = tree.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let writable = tree.join("writable");
        fs::create_dir(&writable).unwrap();
        fs::write(tree.join("top"), b"top").unwrap();
        fs::write(nested.join("leaf"), b"leaf").unwrap();
        fs::write(writable.join("plain"), b"plain").unwrap();
        symlink("leaf", nested.join("inside-link")).unwrap();
        symlink(&outside, tree.join("a").join("outside-dir-link")).unwrap();
        symlink(&outside_file, writable.join("outside-file-link")).unwrap();
        set_mode(&tree.join("top"), 0o444);
        set_mode(&nested.join("leaf"), 0o444);
        set_mode(&nested, 0o555);
        set_mode(&tree.join("a"), 0o555);
        set_mode(&tree, 0o555);
        let outside_modes = (mode(&outside), mode(&outside_file));
        let writable_mode = mode(&writable);

        counters::take(&counters::LSTATS);
        counters::take(&counters::CHMODS);
        make_tree_removable(&tree).unwrap();
        let chmods = sorted(counters::take(&counters::CHMODS));
        let lstats = sorted(counters::take(&counters::LSTATS));

        // Only the three directories missing owner bits were changed; the
        // already-writable directory, every file, and every link were not.
        assert_eq!(
            chmods,
            sorted(vec![tree.clone(), tree.join("a"), nested.clone()])
        );
        // Files and symlinks are typed by `readdir` and never stat'ed.
        assert_eq!(
            lstats,
            sorted(vec![
                tree.clone(),
                tree.join("a"),
                nested.clone(),
                writable.clone()
            ])
        );
        assert_eq!(mode(&tree.join("top")), 0o444);
        assert_eq!(mode(&nested.join("leaf")), 0o444);
        assert_eq!(mode(&writable), writable_mode);

        fs::remove_dir_all(&tree).unwrap();
        assert!(!tree.exists());
        assert_eq!((mode(&outside), mode(&outside_file)), outside_modes);
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep");
        set_mode(&outside, 0o755);
    }

    #[test]
    fn writable_tree_needs_no_permission_changes() {
        let root = tempdir().unwrap();
        let tree = root.path().join("tree");
        for directory in ["one/two", "three"] {
            fs::create_dir_all(tree.join(directory)).unwrap();
        }
        for (index, file) in ["one/a", "one/two/b", "three/c", "d"].iter().enumerate() {
            fs::write(tree.join(file), index.to_string()).unwrap();
            set_mode(&tree.join(file), 0o444);
        }
        counters::take(&counters::LSTATS);
        counters::take(&counters::CHMODS);
        make_tree_removable(&tree).unwrap();
        assert!(counters::take(&counters::CHMODS).is_empty());
        assert_eq!(counters::take(&counters::LSTATS).len(), 4);
        fs::remove_dir_all(&tree).unwrap();
    }

    #[test]
    fn missing_owner_read_or_search_is_restored() {
        let root = tempdir().unwrap();
        let tree = root.path().join("tree");
        let unreadable = tree.join("unreadable");
        let unsearchable = tree.join("unsearchable");
        fs::create_dir_all(&unreadable).unwrap();
        fs::create_dir_all(&unsearchable).unwrap();
        fs::write(unreadable.join("file"), b"x").unwrap();
        fs::write(unsearchable.join("file"), b"y").unwrap();
        set_mode(&unreadable, 0o300);
        set_mode(&unsearchable, 0o600);
        make_tree_removable(&tree).unwrap();
        assert_eq!(mode(&unreadable) & 0o700, 0o700);
        assert_eq!(mode(&unsearchable) & 0o700, 0o700);
        fs::remove_dir_all(&tree).unwrap();
    }

    #[test]
    fn a_link_or_file_root_is_left_alone() {
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        set_mode(&target, 0o555);
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        let file = root.path().join("file");
        fs::write(&file, b"x").unwrap();
        set_mode(&file, 0o444);
        counters::take(&counters::CHMODS);
        make_tree_removable(&link).unwrap();
        make_tree_removable(&file).unwrap();
        assert!(counters::take(&counters::CHMODS).is_empty());
        assert_eq!(mode(&target), 0o555);
        assert_eq!(mode(&file), 0o444);
        set_mode(&target, 0o755);
    }
}
