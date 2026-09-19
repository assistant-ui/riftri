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

/// Stored completion-marker prefixes. A marker names its own scheme so reuse
/// verification recomputes exactly the digest that materialization recorded,
/// and anything else fails closed as corruption.
pub(crate) const MARKER_V1_PREFIX: &[u8] = b"riftri-base-sha256-v1\n";
pub(crate) const MARKER_V2_PREFIX: &[u8] = b"riftri-base-sha256-v2\n";

/// The v1 (content-only) marker: bytes, tree shape, symlink targets, and
/// `mode & 0o777`. It is blind to special permission bits, extended
/// attributes, and macOS ACLs. New completion markers use [`marker_v2`]; this
/// digest is retained verbatim because the persisted compaction and
/// forced-removal snapshot formats compose it.
pub(crate) fn marker(root: &Path) -> io::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    digest.update(b"riftri-base-content-v1\0");
    hash_entry(root, &mut digest, MarkerVersion::V1)?;
    Ok(format!("riftri-base-sha256-v1\n{}\n", hex_lower(digest.finalize())).into_bytes())
}

/// The v2 marker recorded for newly materialized bases: everything v1 covers
/// plus, per entry, metadata the native cloners propagate but Git does not
/// reproduce — the full native Unix mode (setuid, setgid, sticky), every
/// extended attribute name and value (symlinks included), and macOS ACL
/// presence. On Windows only the read-only attribute is covered, matching
/// what the ReFS cloner propagates.
pub(crate) fn marker_v2(root: &Path) -> io::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    digest.update(b"riftri-base-content-v2\0");
    hash_entry(root, &mut digest, MarkerVersion::V2)?;
    Ok(format!("riftri-base-sha256-v2\n{}\n", hex_lower(digest.finalize())).into_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerVersion {
    V1,
    V2,
}

/// sha2 0.11 digest outputs no longer implement `LowerHex`, so render the
/// canonical lowercase hexadecimal form explicitly.
pub(crate) fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut rendered = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        rendered.push(char::from(HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    rendered
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

/// Hash the metadata a v2 marker covers beyond file contents: the full native
/// Unix mode, extended attributes, and macOS ACL presence. Windows covers the
/// read-only attribute only — the ReFS cloner propagates no other permission
/// metadata from a base into a view. Symlink entries are included: APFS
/// clonefile copies their mode and extended attributes verbatim.
fn hash_v2_metadata(path: &Path, metadata: &fs::Metadata, digest: &mut Sha256) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.mode().to_le_bytes());
        hash_xattrs(path, digest)?;
    }
    #[cfg(target_os = "macos")]
    digest.update([u8::from(
        riftri_storage::has_macos_acl(path).map_err(io::Error::other)?,
    )]);
    #[cfg(windows)]
    {
        let _ = path;
        digest.update([u8::from(metadata.permissions().readonly())]);
    }
    Ok(())
}

/// Hash every extended attribute name and value without following symlinks,
/// in sorted-name order. The byte layout matches the forced-removal metadata
/// snapshot so both features attest the same facts the same way.
#[cfg(unix)]
fn hash_xattrs(path: &Path, digest: &mut Sha256) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let mut name_buffer: Vec<u8> = Vec::with_capacity(64 * 1024);
    rustix::fs::llistxattr(path, rustix::buffer::spare_capacity(&mut name_buffer))
        .map_err(io::Error::from)?;
    let mut names = name_buffer
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    names.sort_unstable();
    digest.update((names.len() as u64).to_le_bytes());
    for name in names {
        let mut value_buffer: Vec<u8> = Vec::with_capacity(256 * 1024);
        rustix::fs::lgetxattr(
            path,
            OsStr::from_bytes(&name),
            rustix::buffer::spare_capacity(&mut value_buffer),
        )
        .map_err(io::Error::from)?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(&name);
        digest.update((value_buffer.len() as u64).to_le_bytes());
        digest.update(value_buffer);
    }
    Ok(())
}

fn hash_entry(path: &Path, digest: &mut Sha256, version: MarkerVersion) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if version == MarkerVersion::V2 {
        hash_v2_metadata(path, &metadata, digest)?;
    }
    if metadata.file_type().is_symlink() {
        digest.update(b"link");
        hash_native(fs::read_link(path)?.as_os_str(), digest);
        return Ok(());
    }
    if version == MarkerVersion::V1 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update((metadata.permissions().mode() & 0o777).to_le_bytes());
        }
        #[cfg(windows)]
        digest.update([u8::from(metadata.permissions().readonly())]);
    }

    if metadata.is_dir() {
        digest.update(b"directory");
        let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable_by_key(|entry| entry.file_name());
        digest.update((entries.len() as u64).to_le_bytes());
        for entry in entries {
            hash_native(&entry.file_name(), digest);
            hash_entry(&entry.path(), digest, version)?;
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

    #[test]
    fn v2_integrity_covers_special_bits_and_xattrs_that_v1_ignores() {
        #[cfg(target_os = "macos")]
        const ATTRIBUTE: &str = "com.riftri.base-integrity-test";
        #[cfg(not(target_os = "macos"))]
        const ATTRIBUTE: &str = "user.riftri.base-integrity-test";

        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("tool");
        fs::write(&path, b"#!/bin/sh\n").expect("file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let v1 = marker(fixture.path()).expect("v1 digest");
        let v2 = marker_v2(fixture.path()).expect("v2 digest");
        assert!(v1.starts_with(MARKER_V1_PREFIX));
        assert!(v2.starts_with(MARKER_V2_PREFIX));

        // A setuid bit leaves mode & 0o777 unchanged: invisible to v1.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o4755)).unwrap();
        assert_eq!(marker(fixture.path()).unwrap(), v1);
        assert_ne!(marker_v2(fixture.path()).unwrap(), v2);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(marker_v2(fixture.path()).unwrap(), v2);

        // A sticky bit on the root directory is equally invisible to v1.
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o1755)).unwrap();
        assert_eq!(marker(fixture.path()).unwrap(), v1);
        assert_ne!(marker_v2(fixture.path()).unwrap(), v2);
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(marker_v2(fixture.path()).unwrap(), v2);

        // Extended attribute names and values are covered by v2 only.
        rustix::fs::setxattr(
            &path,
            ATTRIBUTE,
            b"injected",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("set xattr");
        assert_eq!(marker(fixture.path()).unwrap(), v1);
        let with_attribute = marker_v2(fixture.path()).unwrap();
        assert_ne!(with_attribute, v2);
        rustix::fs::setxattr(
            &path,
            ATTRIBUTE,
            b"changed",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("change xattr value");
        assert_ne!(marker_v2(fixture.path()).unwrap(), with_attribute);
        rustix::fs::removexattr(&path, ATTRIBUTE).expect("remove xattr");
        assert_eq!(marker_v2(fixture.path()).unwrap(), v2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn v2_integrity_covers_macos_acl_presence() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("file");
        fs::write(&path, b"contents\n").expect("file");
        let v1 = marker(fixture.path()).expect("v1 digest");
        let v2 = marker_v2(fixture.path()).expect("v2 digest");
        assert!(
            std::process::Command::new("chmod")
                .args(["+a", "everyone deny write"])
                .arg(&path)
                .status()
                .expect("run chmod +a")
                .success()
        );
        assert_eq!(marker(fixture.path()).unwrap(), v1);
        assert_ne!(marker_v2(fixture.path()).unwrap(), v2);
    }
}
