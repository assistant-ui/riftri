use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use crate::StorageError;

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

    let result = clone_directory(
        source,
        destination,
        metadata.permissions().mode(),
        owner_writable,
    );
    if result.is_err() && destination.exists() {
        let _ = make_tree_owner_writable(destination);
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn clone_directory(
    source: &Path,
    destination: &Path,
    final_mode: u32,
    owner_writable: bool,
) -> Result<(), StorageError> {
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
                owner_writable,
            )?;
        } else if file_type.is_file() {
            clone_file(&source_path, &destination_path)?;
            if owner_writable {
                // Read the clone's actual mode, just like the former second
                // pass: clonefile/umask/inherited ACL semantics remain intact.
                let cloned = fs::symlink_metadata(&destination_path)
                    .map_err(|error| io("inspect cloned permissions", &destination_path, error))?;
                set_mode(&destination_path, cloned.permissions().mode() | 0o200)?;
            }
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

    let final_mode = if owner_writable {
        final_mode | 0o700
    } else {
        final_mode
    };
    fs::set_permissions(destination, fs::Permissions::from_mode(final_mode))
        .map_err(|source_error| io("restore clone directory mode", destination, source_error))?;
    Ok(())
}

fn clone_file(source: &Path, destination: &Path) -> Result<(), StorageError> {
    clone_path(source, destination)
}

fn clone_path(source: &Path, destination: &Path) -> Result<(), StorageError> {
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

#[cfg(test)]
fn clone_tree_bulk_for_evaluation(source: &Path, destination: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|source_error| io("inspect bulk clone source", source, source_error))?;
    if !metadata.is_dir() {
        return Err(StorageError::InvalidSource(source.to_path_buf()));
    }
    if destination
        .try_exists()
        .map_err(|source_error| io("inspect bulk clone destination", destination, source_error))?
    {
        return Err(StorageError::DestinationExists(destination.to_path_buf()));
    }
    clone_path(source, destination)
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

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use serde::Serialize;
    use tempfile::tempdir;

    use super::{clone_tree, clone_tree_bulk_for_evaluation};

    #[test]
    fn writable_clone_matches_the_two_pass_path_and_preserves_the_base() {
        let fixture = tempdir().expect("writable clone fixture");
        let source = fixture.path().join("source");
        let old = fixture.path().join("old");
        let fused = fixture.path().join("fused");
        write_fixture(&source, 2, 3, 1024);
        let file = "directory-00/file-0000.bin";
        set_xattr(&source.join(file), "com.riftri.clone-test", b"keep");
        super::make_tree_read_only(&source).expect("protect base");
        clone_tree(&source, &old).expect("old clone");
        super::make_tree_owner_writable(&old).expect("old permission pass");
        super::clone_tree_owner_writable(&source, &fused).expect("fused clone");
        assert_tree_matches(&old, &fused);
        assert_eq!(
            get_xattr(&fused.join(file), "com.riftri.clone-test"),
            Some(b"keep".to_vec())
        );
        fs::write(fused.join(file), b"private").expect("write private clone");
        assert_eq!(
            fs::read(source.join(file)).unwrap(),
            fs::read(old.join(file)).unwrap()
        );
        assert_eq!(
            fs::metadata(source.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o222,
            0
        );
        super::make_tree_owner_writable(&source).expect("fixture cleanup");
    }

    #[test]
    fn writable_clone_cleans_up_after_an_unsupported_entry() {
        let fixture = tempdir().expect("unsupported clone fixture");
        let source = fixture.path().join("source");
        let destination = fixture.path().join("destination");
        write_fixture(&source, 1, 1, 128);
        let socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
        assert!(matches!(
            super::clone_tree_owner_writable(&source, &destination),
            Err(crate::StorageError::UnsupportedEntry(_))
        ));
        assert!(!destination.exists());
        assert!(source.join("socket").exists());
        drop(socket);
    }

    #[derive(Serialize)]
    struct BulkCloneEvaluation {
        schema_version: u8,
        file_count: usize,
        logical_bytes: u64,
        iterative_microseconds: u64,
        bulk_microseconds: u64,
        iterative_allocated_bytes: u64,
        bulk_allocated_bytes: u64,
    }

    #[test]
    fn bulk_directory_candidate_matches_supported_tree_semantics() {
        let fixture = tempdir().expect("bulk clone fixture");
        let source = fixture.path().join("source");
        let destination = fixture.path().join("bulk");
        write_fixture(&source, 2, 3, 1024);

        clone_tree_bulk_for_evaluation(&source, &destination).expect("bulk directory clone");

        assert_tree_matches(&source, &destination);
        fs::write(destination.join("directory-00/file-0000.bin"), b"private")
            .expect("write private clone");
        assert_ne!(
            fs::read(source.join("directory-00/file-0000.bin")).expect("read source"),
            b"private"
        );
    }

    #[test]
    fn bulk_directory_candidate_records_the_directory_xattr_difference() {
        let fixture = tempdir().expect("bulk clone metadata fixture");
        let source = fixture.path().join("source");
        let iterative = fixture.path().join("iterative");
        let bulk = fixture.path().join("bulk");
        write_fixture(&source, 1, 1, 128);
        set_xattr(&source, "com.riftri.bulk-evaluation", b"source-only");

        clone_tree(&source, &iterative).expect("iterative APFS clone");
        clone_tree_bulk_for_evaluation(&source, &bulk).expect("bulk APFS clone");

        assert_eq!(
            get_xattr(&bulk, "com.riftri.bulk-evaluation"),
            Some(b"source-only".to_vec())
        );
        assert_eq!(get_xattr(&iterative, "com.riftri.bulk-evaluation"), None);
    }

    #[test]
    #[ignore = "manual APFS bulk-directory comparison; reports measurements without a timing threshold"]
    fn reports_bulk_directory_clone_comparison() {
        let fixture = tempdir().expect("bulk clone benchmark fixture");
        let source = fixture.path().join("source");
        let iterative = fixture.path().join("iterative");
        let bulk = fixture.path().join("bulk");
        let directory_count = 32;
        let files_per_directory = 64;
        let bytes_per_file = 4096;
        write_fixture(
            &source,
            directory_count,
            files_per_directory,
            bytes_per_file,
        );

        let iterative_started = Instant::now();
        clone_tree(&source, &iterative).expect("iterative APFS clone");
        let iterative_duration = iterative_started.elapsed();
        let bulk_started = Instant::now();
        clone_tree_bulk_for_evaluation(&source, &bulk).expect("bulk APFS clone");
        let bulk_duration = bulk_started.elapsed();

        assert_tree_matches(&source, &iterative);
        assert_tree_matches(&source, &bulk);
        let report = BulkCloneEvaluation {
            schema_version: 1,
            file_count: directory_count * files_per_directory + 1,
            logical_bytes: u64::try_from(
                directory_count * files_per_directory * bytes_per_file + 18,
            )
            .expect("logical byte count"),
            iterative_microseconds: elapsed_microseconds(iterative_duration),
            bulk_microseconds: elapsed_microseconds(bulk_duration),
            iterative_allocated_bytes: allocated_bytes(&iterative),
            bulk_allocated_bytes: allocated_bytes(&bulk),
        };
        println!(
            "RIFTRI_APFS_BULK_EVALUATION {}",
            serde_json::to_string(&report).expect("serialize evaluation")
        );
    }

    fn write_fixture(
        source: &Path,
        directory_count: usize,
        files_per_directory: usize,
        bytes_per_file: usize,
    ) {
        fs::create_dir(source).expect("create source");
        let mut payload = vec![0_u8; bytes_per_file];
        for directory_index in 0..directory_count {
            let directory = source.join(format!("directory-{directory_index:02}"));
            fs::create_dir(&directory).expect("create fixture directory");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o750))
                .expect("set fixture directory mode");
            for file_index in 0..files_per_directory {
                payload.fill(u8::try_from((directory_index + file_index) % 251).unwrap());
                let path = directory.join(format!("file-{file_index:04}.bin"));
                let mut file = fs::File::create(&path).expect("create fixture file");
                file.write_all(&payload).expect("write fixture file");
                if file_index == 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                        .expect("set executable fixture mode");
                }
            }
        }
        fs::write(source.join("readme.txt"), b"bulk clone fixture\n")
            .expect("write fixture readme");
        symlink("readme.txt", source.join("readme-link")).expect("create fixture symlink");
    }

    fn assert_tree_matches(source: &Path, destination: &Path) {
        let source_metadata = fs::symlink_metadata(source).expect("source metadata");
        let destination_metadata = fs::symlink_metadata(destination).expect("destination metadata");
        assert_eq!(
            source_metadata.file_type().is_dir(),
            destination_metadata.file_type().is_dir()
        );
        assert_eq!(
            source_metadata.permissions().mode() & 0o777,
            destination_metadata.permissions().mode() & 0o777
        );

        let mut names = fs::read_dir(source)
            .expect("read source tree")
            .map(|entry| entry.expect("read source entry").file_name())
            .collect::<Vec<_>>();
        names.sort();
        let mut destination_names = fs::read_dir(destination)
            .expect("read destination tree")
            .map(|entry| entry.expect("read destination entry").file_name())
            .collect::<Vec<_>>();
        destination_names.sort();
        assert_eq!(names, destination_names);

        for name in names {
            let source_path = source.join(&name);
            let destination_path = destination.join(name);
            let metadata = fs::symlink_metadata(&source_path).expect("source entry metadata");
            if metadata.is_dir() {
                assert_tree_matches(&source_path, &destination_path);
            } else if metadata.is_file() {
                let cloned =
                    fs::symlink_metadata(&destination_path).expect("destination file metadata");
                assert!(cloned.is_file());
                assert_eq!(
                    metadata.permissions().mode() & 0o777,
                    cloned.permissions().mode() & 0o777
                );
                assert_eq!(
                    fs::read(&source_path).expect("read source file"),
                    fs::read(&destination_path).expect("read destination file")
                );
                assert_ne!(metadata.ino(), cloned.ino());
            } else {
                assert!(metadata.file_type().is_symlink());
                assert_eq!(
                    fs::read_link(&source_path).expect("read source link"),
                    fs::read_link(&destination_path).expect("read destination link")
                );
            }
        }
    }

    fn allocated_bytes(path: &Path) -> u64 {
        let metadata = fs::symlink_metadata(path).expect("allocation metadata");
        let mut bytes = metadata.blocks().saturating_mul(512);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("read allocation tree") {
                bytes =
                    bytes.saturating_add(allocated_bytes(&entry.expect("allocation entry").path()));
            }
        }
        bytes
    }

    fn elapsed_microseconds(duration: Duration) -> u64 {
        u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
    }

    fn set_xattr(path: &Path, name: &str, value: &[u8]) {
        let path = CString::new(path.as_os_str().as_bytes()).expect("xattr path");
        let name = CString::new(name).expect("xattr name");
        // SAFETY: the path/name are NUL-terminated and value points to the
        // supplied number of readable bytes for this synchronous call.
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(result, 0, "set xattr: {}", std::io::Error::last_os_error());
    }

    fn get_xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
        let path = CString::new(path.as_os_str().as_bytes()).expect("xattr path");
        let name = CString::new(name).expect("xattr name");
        // SAFETY: the path/name are NUL-terminated and a null value pointer
        // requests only the required buffer length.
        let length =
            unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        if length < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOATTR),
                "read xattr length"
            );
            return None;
        }
        let mut value = vec![0_u8; usize::try_from(length).expect("xattr length")];
        // SAFETY: value owns a writable buffer of exactly the requested size.
        let read = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(read, length, "read xattr value");
        Some(value)
    }
}
