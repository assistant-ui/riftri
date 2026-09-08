use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use crate::StorageError;

pub(crate) fn clone_tree(source: &Path, destination: &Path) -> Result<(), StorageError> {
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

    let result = clone_directory(source, destination, metadata.permissions().mode());
    if result.is_err() && destination.exists() {
        let _ = make_tree_owner_writable(destination);
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn clone_directory(source: &Path, destination: &Path, final_mode: u32) -> Result<(), StorageError> {
    fs::create_dir(destination)
        .map_err(|source_error| io("create clone directory", destination, source_error))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(final_mode | 0o700))
        .map_err(|source_error| io("prepare clone directory mode", destination, source_error))?;

    for entry in fs::read_dir(source)
        .map_err(|source_error| io("read clone source directory", source, source_error))?
    {
        let entry =
            entry.map_err(|source_error| io("read clone source entry", source, source_error))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|source_error| io("inspect clone source entry", &source_path, source_error))?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            clone_directory(
                &source_path,
                &destination_path,
                metadata.permissions().mode(),
            )?;
        } else if file_type.is_file() {
            clone_file(&source_path, &destination_path)?;
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

    fs::set_permissions(destination, fs::Permissions::from_mode(final_mode))
        .map_err(|source_error| io("restore clone directory mode", destination, source_error))?;
    Ok(())
}

fn clone_file(source: &Path, destination: &Path) -> Result<(), StorageError> {
    let source_c =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "source contains NUL"),
        })?;
    let destination_c =
        CString::new(destination.as_os_str().as_bytes()).map_err(|_| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination contains NUL",
            ),
        })?;

    // SAFETY: both paths are valid NUL-terminated C strings. A zero flag set
    // requires clonefile to either create a native COW clone or return an
    // error; clonefile never falls back to copying file data.
    let result = unsafe { libc::clonefile(source_c.as_ptr(), destination_c.as_ptr(), 0) };
    if result != 0 {
        return Err(StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

pub(crate) fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
    update_modes(path, ModeUpdate::ReadOnly)
}

pub(crate) fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
    update_modes(path, ModeUpdate::OwnerWritable)
}

#[derive(Clone, Copy)]
enum ModeUpdate {
    ReadOnly,
    OwnerWritable,
}

fn update_modes(path: &Path, update: ModeUpdate) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source_error| io("inspect tree permissions", path, source_error))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    if metadata.is_dir() {
        if matches!(update, ModeUpdate::OwnerWritable) {
            set_mode(path, metadata.permissions().mode() | 0o700)?;
        }
        for entry in fs::read_dir(path)
            .map_err(|source_error| io("read tree permissions", path, source_error))?
        {
            let entry = entry.map_err(|source_error| io("read tree entry", path, source_error))?;
            update_modes(&entry.path(), update)?;
        }
        if matches!(update, ModeUpdate::ReadOnly) {
            set_mode(path, metadata.permissions().mode() & !0o222)?;
        }
    } else if metadata.is_file() {
        let mode = match update {
            ModeUpdate::ReadOnly => metadata.permissions().mode() & !0o222,
            ModeUpdate::OwnerWritable => metadata.permissions().mode() | 0o200,
        };
        set_mode(path, mode)?;
    } else {
        return Err(StorageError::UnsupportedEntry(path.to_path_buf()));
    }
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
