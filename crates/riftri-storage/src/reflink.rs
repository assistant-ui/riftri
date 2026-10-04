use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::StorageError;
use crate::parallel::{file_clone_parallelism, try_for_each_bounded};

struct FileClone {
    source: PathBuf,
    destination: PathBuf,
}

pub(crate) fn probe(directory: &Path) -> std::io::Result<()> {
    let mut source = unnamed_file(directory)?;
    source.write_all(b"riftri-reflink-probe")?;
    source.sync_all()?;
    let mut destination = unnamed_file(directory)?;
    rustix::fs::ioctl_ficlone(&destination, &source)
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;

    destination.seek(SeekFrom::Start(0))?;
    destination.write_all(b"private")?;
    source.seek(SeekFrom::Start(0))?;
    let mut contents = Vec::new();
    source.read_to_end(&mut contents)?;
    if contents != b"riftri-reflink-probe" {
        return Err(std::io::Error::other(
            "FICLONE probe did not preserve private writes",
        ));
    }
    Ok(())
}

fn unnamed_file(directory: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_TMPFILE | libc::O_CLOEXEC)
        .mode(0o600)
        .open(directory)
}

pub(crate) fn clone_tree(source: &Path, destination: &Path) -> Result<(), StorageError> {
    clone_tree_with_permissions(source, destination, false)
}

pub(crate) fn clone_tree_owner_writable(
    source: &Path,
    destination: &Path,
) -> Result<(), StorageError> {
    clone_tree_with_permissions(source, destination, true)
}

fn clone_tree_with_permissions(
    source: &Path,
    destination: &Path,
    owner_writable: bool,
) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|source_error| io("inspect clone source", source, source_error))?;
    if !metadata.is_dir() {
        return Err(StorageError::InvalidSource(source.to_path_buf()));
    }
    if destination
        .try_exists()
        .map_err(|source_error| io("inspect clone destination", destination, source_error))?
    {
        return Err(StorageError::DestinationExists(destination.to_path_buf()));
    }

    let writable_bits = owner_writable
        .then(crate::umask_writable_bits)
        .transpose()
        .map_err(|error| io("read process umask", destination, error))?;
    let result = (|| {
        let mut files = Vec::new();
        let mut directories = Vec::new();
        prepare_clone_directory(
            source,
            destination,
            metadata.permissions().mode(),
            &mut files,
            &mut directories,
            writable_bits,
        )?;
        try_for_each_bounded(files, file_clone_parallelism(), |file| {
            clone_file(&file.source, &file.destination, writable_bits)
        })?;
        for (path, mode) in directories.into_iter().rev() {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(|source_error| io("restore clone directory mode", &path, source_error))?;
        }
        Ok(())
    })();
    if result.is_err() && destination.exists() {
        let _ = make_tree_owner_writable(destination);
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn prepare_clone_directory(
    source: &Path,
    destination: &Path,
    final_mode: u32,
    files: &mut Vec<FileClone>,
    directories: &mut Vec<(PathBuf, u32)>,
    writable_bits: Option<u32>,
) -> Result<(), StorageError> {
    fs::create_dir(destination)
        .map_err(|source_error| io("create clone directory", destination, source_error))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(final_mode | 0o700))
        .map_err(|source_error| io("prepare clone directory mode", destination, source_error))?;
    let final_mode = if let Some(writable_bits) = writable_bits {
        final_mode | 0o700 | writable_bits
    } else {
        final_mode
    };
    directories.push((destination.to_path_buf(), final_mode));

    for entry in fs::read_dir(source)
        .map_err(|source_error| io("read clone source directory", source, source_error))?
    {
        let entry =
            entry.map_err(|source_error| io("read clone source entry", source, source_error))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        // Linux directory entries normally carry their type. Regular-file
        // metadata is read from the already-open source handle in the bounded
        // clone worker, so only directories need a serial path metadata read
        // here for their final mode.
        let file_type = entry.file_type().map_err(|source_error| {
            io(
                "inspect clone source entry type",
                &source_path,
                source_error,
            )
        })?;

        if file_type.is_dir() {
            let metadata = fs::symlink_metadata(&source_path).map_err(|source_error| {
                io("inspect clone source directory", &source_path, source_error)
            })?;
            prepare_clone_directory(
                &source_path,
                &destination_path,
                metadata.permissions().mode(),
                files,
                directories,
                writable_bits,
            )?;
        } else if file_type.is_file() {
            files.push(FileClone {
                source: source_path,
                destination: destination_path,
            });
        } else if file_type.is_symlink() {
            let target = fs::read_link(&source_path)
                .map_err(|source_error| io("read source symlink", &source_path, source_error))?;
            symlink(target, &destination_path).map_err(|source_error| {
                io("create cloned symlink", &destination_path, source_error)
            })?;
        } else {
            return Err(StorageError::UnsupportedEntry(source_path));
        }
    }

    Ok(())
}

fn clone_file(
    source: &Path,
    destination: &Path,
    writable_bits: Option<u32>,
) -> Result<(), StorageError> {
    let source_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(source)
        .map_err(|source_error| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: source_error,
        })?;
    let metadata = source_file
        .metadata()
        .map_err(|source_error| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: source_error,
        })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StorageError::UnsupportedEntry(source.to_path_buf()));
    }
    let mode = if let Some(writable_bits) = writable_bits {
        metadata.permissions().mode() | writable_bits
    } else {
        metadata.permissions().mode()
    };
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|source_error| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: source_error,
        })?;

    if let Err(error) = rustix::fs::ioctl_ficlone(&destination_file, &source_file) {
        drop(destination_file);
        let _ = fs::remove_file(destination);
        return Err(StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        });
    }
    destination_file
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source_error| io("restore cloned file mode", destination, source_error))?;
    Ok(())
}

pub(crate) fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
    update_modes(path, ModeUpdate::ReadOnly, 0)
}

pub(crate) fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
    let writable_bits =
        crate::umask_writable_bits().map_err(|error| io("read process umask", path, error))?;
    update_modes(path, ModeUpdate::OwnerWritable, writable_bits)
}

#[derive(Clone, Copy)]
enum ModeUpdate {
    ReadOnly,
    OwnerWritable,
}

fn update_modes(path: &Path, update: ModeUpdate, writable_bits: u32) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source_error| io("inspect tree permissions", path, source_error))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    if metadata.is_dir() {
        if matches!(update, ModeUpdate::OwnerWritable) {
            // Owner rwx (`0o700`) guarantees traversal and edits; the
            // umask-appropriate bits restore the group/other write access a
            // plain `git worktree add` keeps under `umask 002` or
            // `core.sharedRepository=group`, which `make_tree_read_only`
            // stripped when it cleared every write bit.
            set_mode(path, metadata.permissions().mode() | 0o700 | writable_bits)?;
        }
        for entry in fs::read_dir(path)
            .map_err(|source_error| io("read tree permissions", path, source_error))?
        {
            let entry = entry.map_err(|source_error| io("read tree entry", path, source_error))?;
            update_modes(&entry.path(), update, writable_bits)?;
        }
        if matches!(update, ModeUpdate::ReadOnly) {
            set_mode(path, metadata.permissions().mode() & !0o222)?;
        }
    } else if metadata.is_file() {
        let mode = match update {
            ModeUpdate::ReadOnly => metadata.permissions().mode() & !0o222,
            ModeUpdate::OwnerWritable => metadata.permissions().mode() | writable_bits,
        };
        set_mode(path, mode)?;
    } else if matches!(update, ModeUpdate::ReadOnly) {
        return Err(StorageError::UnsupportedEntry(path.to_path_buf()));
    }
    // A FIFO, socket, or device node needs no mode change to be deleted:
    // unlinking it takes write access to its parent, which is granted above.
    // Worktrees hold them (a dev server's socket), and refusing here left a
    // removed worktree's quarantine undeletable.
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), StorageError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source_error| io("set tree permissions", path, source_error))
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::{
        clone_tree, clone_tree_owner_writable, make_tree_owner_writable, make_tree_read_only, probe,
    };

    #[test]
    fn writable_clone_matches_the_two_pass_path_and_preserves_the_base() {
        let fixture = tempdir().expect("writable clone fixture");
        if probe(fixture.path()).is_err() {
            return;
        }
        let source = fixture.path().join("source");
        let old = fixture.path().join("old");
        let fused = fixture.path().join("fused");
        fs::create_dir(&source).expect("source");
        fs::create_dir(source.join("nested")).expect("nested source");
        fs::write(source.join("nested/file"), b"base contents\n").expect("regular file");
        fs::write(source.join("tool"), b"#!/bin/sh\n").expect("executable file");
        fs::set_permissions(source.join("tool"), fs::Permissions::from_mode(0o755))
            .expect("executable mode");
        symlink("nested/file", source.join("link")).expect("source symlink");
        make_tree_read_only(&source).expect("immutable source");

        clone_tree(&source, &old).expect("old clone");
        make_tree_owner_writable(&old).expect("old permission pass");
        clone_tree_owner_writable(&source, &fused).expect("fused clone");

        for relative in ["", "nested", "nested/file", "tool"] {
            let old_mode = fs::symlink_metadata(old.join(relative))
                .expect("old metadata")
                .permissions()
                .mode()
                & 0o7777;
            let fused_mode = fs::symlink_metadata(fused.join(relative))
                .expect("fused metadata")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(fused_mode, old_mode, "mode differs for {relative:?}");
        }
        assert_eq!(
            fs::read_link(fused.join("link")).expect("fused symlink"),
            fs::read_link(old.join("link")).expect("old symlink")
        );
        fs::write(fused.join("nested/file"), b"private change\n").expect("private write");
        assert_eq!(
            fs::read(source.join("nested/file")).expect("source contents"),
            b"base contents\n"
        );
        assert_eq!(
            fs::symlink_metadata(source.join("nested/file"))
                .expect("source metadata")
                .permissions()
                .mode()
                & 0o222,
            0,
            "fused clone changed immutable source permissions"
        );

        make_tree_owner_writable(&source).expect("fixture cleanup");
    }
}
