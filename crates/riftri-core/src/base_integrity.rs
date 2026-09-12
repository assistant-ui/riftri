use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("integrity input is not a regular file"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::other("integrity input is a reparse point"));
        }
    }
    Ok(file)
}

pub(crate) fn marker(root: &Path) -> io::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    digest.update(b"riftri-base-content-v1\0");
    hash_entry(root, &mut digest)?;
    Ok(format!("riftri-base-sha256-v1\n{:x}\n", digest.finalize()).into_bytes())
}

fn hash_native(value: &OsStr, digest: &mut Sha256) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = value.as_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units = value.encode_wide().collect::<Vec<_>>();
        digest.update((units.len() as u64).to_le_bytes());
        for unit in units {
            digest.update(unit.to_le_bytes());
        }
    }
}

fn hash_entry(path: &Path, digest: &mut Sha256) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        digest.update(b"link");
        hash_native(fs::read_link(path)?.as_os_str(), digest);
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        digest.update((metadata.permissions().mode() & 0o777).to_le_bytes());
    }
    #[cfg(windows)]
    digest.update([u8::from(metadata.permissions().readonly())]);

    if metadata.is_dir() {
        digest.update(b"directory");
        let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable_by_key(|entry| entry.file_name());
        digest.update((entries.len() as u64).to_le_bytes());
        for entry in entries {
            hash_native(&entry.file_name(), digest);
            hash_entry(&entry.path(), digest)?;
        }
    } else if metadata.is_file() {
        digest.update(b"file");
        digest.update(metadata.len().to_le_bytes());
        let mut file = open_regular(path)?;
        let mut buffer = [0; 64 * 1024];
        let mut length = 0;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            digest.update(&buffer[..count]);
        }
        if length != metadata.len() {
            return Err(io::Error::other("integrity input changed while reading"));
        }
    } else {
        return Err(io::Error::other("unsupported entry in immutable base"));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn integrity_covers_bytes_names_modes_and_link_targets() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("file");
        fs::write(&path, b"one").expect("file");
        let original = marker(fixture.path()).expect("digest");
        fs::write(&path, b"two").expect("same-length corruption");
        assert_ne!(marker(fixture.path()).unwrap(), original);
        fs::write(&path, b"one").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(marker(fixture.path()).unwrap(), original);
        let link = fixture.path().join("link");
        symlink("first", &link).unwrap();
        let linked = marker(fixture.path()).unwrap();
        fs::remove_file(&link).unwrap();
        symlink("second", &link).unwrap();
        assert_ne!(marker(fixture.path()).unwrap(), linked);
        fs::remove_file(&link).unwrap();
        let named = marker(fixture.path()).unwrap();
        fs::rename(path, fixture.path().join("renamed")).unwrap();
        assert_ne!(marker(fixture.path()).unwrap(), named);
        let mut native = Sha256::new();
        let mut lossy = Sha256::new();
        hash_native(
            &std::ffi::OsString::from_vec(b"file-\xff".to_vec()),
            &mut native,
        );
        hash_native(OsStr::new("file-\u{fffd}"), &mut lossy);
        assert_ne!(native.finalize(), lossy.finalize());
    }
}
