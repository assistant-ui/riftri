use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt, symlink_dir, symlink_file};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceW, GetVolumePathNameW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DUPLICATE_EXTENTS_DATA, FILE_SET_SPARSE_BUFFER, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
    FSCTL_GET_INTEGRITY_INFORMATION, FSCTL_GET_INTEGRITY_INFORMATION_BUFFER,
    FSCTL_SET_INTEGRITY_INFORMATION, FSCTL_SET_INTEGRITY_INFORMATION_BUFFER, FSCTL_SET_SPARSE,
};

use crate::StorageError;

const PROBE_BYTES: usize = 64 * 1024;
const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn probe(directory: &Path) -> std::io::Result<()> {
    let source_path = probe_path(directory, "source")?;
    let destination_path = probe_path(directory, "destination")?;
    let mut source = open_delete_on_close(&source_path)?;
    let mut destination = open_delete_on_close(&destination_path)?;

    let contents = vec![0x5a; PROBE_BYTES];
    source.write_all(&contents)?;
    source.sync_all()?;
    destination.set_len(PROBE_BYTES as u64)?;
    clone_extent(&source, &destination, 0, PROBE_BYTES as u64)?;
    destination.seek(SeekFrom::Start(0))?;
    destination.write_all(b"private")?;
    destination.sync_all()?;

    source.seek(SeekFrom::Start(0))?;
    let mut preserved = [0_u8; 7];
    source.read_exact(&mut preserved)?;
    if preserved != contents[..preserved.len()] {
        return Err(std::io::Error::other(
            "ReFS block-clone probe did not preserve private writes",
        ));
    }
    Ok(())
}

fn probe_path(directory: &Path, role: &str) -> std::io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let nonce = PROBE_NONCE.fetch_add(1, Ordering::Relaxed);
    Ok(directory.join(format!(
        ".riftri-refs-probe-{}-{timestamp}-{nonce}-{role}",
        std::process::id()
    )))
}

fn open_delete_on_close(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

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
    let cluster_size = cluster_size(source)
        .map_err(|source_error| io("determine ReFS block-clone alignment", source, source_error))?;

    let result = clone_directory(source, destination, cluster_size);
    if result.is_err() && destination.exists() {
        let _ = make_tree_owner_writable(destination);
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn clone_directory(
    source: &Path,
    destination: &Path,
    cluster_size: u64,
) -> Result<(), StorageError> {
    fs::create_dir(destination)
        .map_err(|source_error| io("create clone directory", destination, source_error))?;

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
            clone_directory(&source_path, &destination_path, cluster_size)?;
        } else if file_type.is_file() {
            clone_file(&source_path, &destination_path, cluster_size)?;
        } else if file_type.is_symlink() {
            clone_symlink(&source_path, &destination_path)?;
        } else {
            return Err(StorageError::UnsupportedEntry(source_path));
        }
    }

    let permissions = fs::metadata(source)
        .map_err(|source_error| io("inspect source directory permissions", source, source_error))?
        .permissions();
    fs::set_permissions(destination, permissions).map_err(|source_error| {
        io(
            "restore clone directory permissions",
            destination,
            source_error,
        )
    })?;
    Ok(())
}

fn clone_file(source: &Path, destination: &Path, cluster_size: u64) -> Result<(), StorageError> {
    let mut source_file = File::open(source).map_err(|source_error| StorageError::Clone {
        source_path: source.to_path_buf(),
        destination: destination.to_path_buf(),
        source: source_error,
    })?;
    let mut destination_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination)
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

    match prepare_destination_attributes(&source_file, &destination_file, &metadata) {
        Ok(()) => {}
        Err(source_error) => {
            drop(destination_file);
            let _ = fs::remove_file(destination);
            return Err(StorageError::Clone {
                source_path: source.to_path_buf(),
                destination: destination.to_path_buf(),
                source: source_error,
            });
        }
    }
    let length = metadata.file_size();
    destination_file
        .set_len(length)
        .map_err(|source_error| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: source_error,
        })?;

    let aligned_length = length / cluster_size * cluster_size;
    let max_chunk = (FOUR_GIB - cluster_size) / cluster_size * cluster_size;
    let mut offset = 0_u64;
    while offset < aligned_length {
        let byte_count = (aligned_length - offset).min(max_chunk);
        if let Err(source_error) = clone_extent(&source_file, &destination_file, offset, byte_count)
        {
            drop(destination_file);
            let _ = fs::remove_file(destination);
            return Err(StorageError::Clone {
                source_path: source.to_path_buf(),
                destination: destination.to_path_buf(),
                source: source_error,
            });
        }
        offset += byte_count;
    }

    if aligned_length < length {
        source_file
            .seek(SeekFrom::Start(aligned_length))
            .and_then(|_| destination_file.seek(SeekFrom::Start(aligned_length)))
            .and_then(|_| {
                std::io::copy(
                    &mut Read::by_ref(&mut source_file).take(length - aligned_length),
                    &mut destination_file,
                )
            })
            .map_err(|source_error| StorageError::Clone {
                source_path: source.to_path_buf(),
                destination: destination.to_path_buf(),
                source: source_error,
            })?;
    }
    destination_file
        .sync_all()
        .map_err(|source_error| StorageError::Clone {
            source_path: source.to_path_buf(),
            destination: destination.to_path_buf(),
            source: source_error,
        })?;
    fs::set_permissions(destination, metadata.permissions())
        .map_err(|source_error| io("restore cloned file permissions", destination, source_error))?;
    Ok(())
}

fn prepare_destination_attributes(
    source: &File,
    destination: &File,
    metadata: &fs::Metadata,
) -> std::io::Result<()> {
    if metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE != 0 {
        let sparse = FILE_SET_SPARSE_BUFFER { SetSparse: true };
        device_io_control_input(destination, FSCTL_SET_SPARSE, &sparse)?;
    }

    let source_integrity = integrity_information(source)?;
    let destination_integrity = integrity_information(destination)?;
    if source_integrity.ChecksumAlgorithm != destination_integrity.ChecksumAlgorithm
        || source_integrity.Flags != destination_integrity.Flags
    {
        let requested = FSCTL_SET_INTEGRITY_INFORMATION_BUFFER {
            ChecksumAlgorithm: source_integrity.ChecksumAlgorithm,
            Reserved: 0,
            Flags: source_integrity.Flags,
        };
        device_io_control_input(destination, FSCTL_SET_INTEGRITY_INFORMATION, &requested)?;
    }
    Ok(())
}

fn integrity_information(file: &File) -> std::io::Result<FSCTL_GET_INTEGRITY_INFORMATION_BUFFER> {
    let mut output = FSCTL_GET_INTEGRITY_INFORMATION_BUFFER::default();
    let mut returned = 0_u32;
    // SAFETY: the file handle is live, the output points to correctly sized
    // writable storage, and this synchronous call supplies no OVERLAPPED.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_GET_INTEGRITY_INFORMATION,
            std::ptr::null(),
            0,
            (&mut output as *mut FSCTL_GET_INTEGRITY_INFORMATION_BUFFER).cast(),
            std::mem::size_of::<FSCTL_GET_INTEGRITY_INFORMATION_BUFFER>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(output)
}

fn device_io_control_input<T>(file: &File, code: u32, input: &T) -> std::io::Result<()> {
    let mut returned = 0_u32;
    // SAFETY: the file handle is live, `input` points to an initialized value
    // of the declared size, and the synchronous call supplies no output or
    // OVERLAPPED buffer.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            code,
            (input as *const T).cast(),
            std::mem::size_of::<T>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn clone_extent(
    source: &File,
    destination: &File,
    offset: u64,
    byte_count: u64,
) -> std::io::Result<()> {
    let request = DUPLICATE_EXTENTS_DATA {
        FileHandle: source.as_raw_handle() as HANDLE,
        SourceFileOffset: offset as i64,
        TargetFileOffset: offset as i64,
        ByteCount: byte_count as i64,
    };
    device_io_control_input(destination, FSCTL_DUPLICATE_EXTENTS_TO_FILE, &request)
}

fn cluster_size(path: &Path) -> std::io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    const WINDOWS_MAX_PATH: usize = 32_768;
    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_path.push(0);
    let mut volume_path = vec![0_u16; WINDOWS_MAX_PATH];
    // SAFETY: the input is NUL-terminated and the output buffer is writable
    // for the supplied length.
    let succeeded = unsafe {
        GetVolumePathNameW(
            wide_path.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut sectors_per_cluster = 0_u32;
    let mut bytes_per_sector = 0_u32;
    // SAFETY: the volume path is a Windows-produced NUL-terminated root path;
    // both requested outputs point to writable values.
    let succeeded = unsafe {
        GetDiskFreeSpaceW(
            volume_path.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let size = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
    if size == 0 || size > PROBE_BYTES as u64 || PROBE_BYTES as u64 % size != 0 {
        return Err(std::io::Error::other(format!(
            "unsupported ReFS cluster size {size}"
        )));
    }
    Ok(size)
}

fn clone_symlink(source: &Path, destination: &Path) -> Result<(), StorageError> {
    let target = fs::read_link(source)
        .map_err(|source_error| io("read source symlink", source, source_error))?;
    let result = match fs::metadata(source) {
        Ok(metadata) if metadata.is_dir() => symlink_dir(target, destination),
        _ => symlink_file(target, destination),
    };
    result.map_err(|source_error| io("create cloned symlink", destination, source_error))
}

pub(crate) fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
    update_permissions(path, true)
}

pub(crate) fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
    update_permissions(path, false)
}

fn update_permissions(path: &Path, read_only: bool) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source_error| io("inspect tree permissions", path, source_error))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() && !read_only {
        set_read_only(path, &metadata, false)?;
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|source_error| io("read tree permissions", path, source_error))?
        {
            let entry = entry.map_err(|source_error| io("read tree entry", path, source_error))?;
            update_permissions(&entry.path(), read_only)?;
        }
    } else if !metadata.is_file() {
        return Err(StorageError::UnsupportedEntry(path.to_path_buf()));
    }
    if metadata.is_file() || read_only {
        set_read_only(path, &metadata, read_only)?;
    }
    Ok(())
}

fn set_read_only(
    path: &Path,
    metadata: &fs::Metadata,
    read_only: bool,
) -> Result<(), StorageError> {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions)
        .map_err(|source_error| io("set tree permissions", path, source_error))
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
