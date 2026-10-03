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
    marker_v1(root, SpecialEntries::Refuse)
}

/// The v1 digest of a live worktree rather than a base. FIFOs, sockets, and
/// device nodes, which a base can never hold but a worktree may (a dev
/// server's socket), are hashed by kind and mode instead of refused, so
/// forced removal and compaction can tell whether one appeared or changed.
/// A tree without them digests exactly as [`marker`] does.
pub(crate) fn worktree_marker(root: &Path) -> io::Result<Vec<u8>> {
    marker_v1(root, SpecialEntries::Hash)
}

fn marker_v1(root: &Path, special: SpecialEntries) -> io::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    let mut scratch = HashScratch::new();
    digest.update(b"riftri-base-content-v1\0");
    hash_entry(root, &mut digest, MarkerVersion::V1, special, &mut scratch)?;
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
    let mut scratch = HashScratch::new();
    digest.update(b"riftri-base-content-v2\0");
    hash_entry(
        root,
        &mut digest,
        MarkerVersion::V2,
        SpecialEntries::Refuse,
        &mut scratch,
    )?;
    Ok(format!("riftri-base-sha256-v2\n{}\n", hex_lower(digest.finalize())).into_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerVersion {
    V1,
    V2,
}

/// What a walk does with an entry that is neither a file, a directory, nor a
/// symlink.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpecialEntries {
    Refuse,
    Hash,
}

/// Operation-local buffers reused across every entry in one integrity walk.
/// A representative base has thousands of files, so allocating the file and
/// extended-attribute buffers per entry adds allocator work without adding any
/// isolation: each read fully overwrites the relevant buffer before hashing.
struct HashScratch {
    file_bytes: Vec<u8>,
    #[cfg(unix)]
    xattr_names: Vec<u8>,
    #[cfg(unix)]
    xattr_value: Vec<u8>,
}

impl HashScratch {
    fn new() -> Self {
        Self {
            file_bytes: vec![0; 64 * 1024],
            #[cfg(unix)]
            xattr_names: Vec::with_capacity(64 * 1024),
            #[cfg(unix)]
            xattr_value: Vec::with_capacity(256 * 1024),
        }
    }
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
        let units = value.encode_wide();
        digest.update((units.clone().count() as u64).to_le_bytes());
        for unit in units {
            digest.update(unit.to_le_bytes());
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use sha2::{Digest, Sha256};

    use super::hash_native;

    fn hash_native_allocating(value: &OsStr, digest: &mut Sha256) {
        let units = value.encode_wide().collect::<Vec<_>>();
        digest.update((units.len() as u64).to_le_bytes());
        for unit in units {
            digest.update(unit.to_le_bytes());
        }
    }

    fn digest_names(names: &[OsString], allocating: bool) -> Vec<u8> {
        let mut digest = Sha256::new();
        for name in names {
            if allocating {
                hash_native_allocating(name, &mut digest);
            } else {
                hash_native(name, &mut digest);
            }
        }
        digest.finalize().to_vec()
    }

    #[test]
    fn streamed_native_names_preserve_the_persisted_wide_layout() {
        let values = [
            OsString::from("ascii"),
            OsString::from("emoji-🦀"),
            OsString::from_wide(&[b'x' as u16, 0xd800, b'y' as u16]),
        ];
        assert_eq!(digest_names(&values, false), digest_names(&values, true));
    }
}

/// Hash the metadata a v2 marker covers beyond file contents: the full native
/// Unix mode, extended attributes, and macOS ACL presence. Windows covers the
/// read-only attribute only — the ReFS cloner propagates no other permission
/// metadata from a base into a view. Symlink entries are included: APFS
/// clonefile copies their mode and extended attributes verbatim.
fn hash_v2_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    digest: &mut Sha256,
    scratch: &mut HashScratch,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.mode().to_le_bytes());
        hash_xattrs(path, digest, scratch)?;
    }
    #[cfg(target_os = "macos")]
    digest.update([u8::from(
        riftri_storage::has_macos_acl(path).map_err(io::Error::other)?,
    )]);
    #[cfg(windows)]
    {
        let _ = path;
        let _ = scratch;
        digest.update([u8::from(metadata.permissions().readonly())]);
    }
    Ok(())
}

/// Hash every extended attribute name and value without following symlinks,
/// in sorted-name order. The byte layout matches the forced-removal metadata
/// snapshot so both features attest the same facts the same way.
#[cfg(unix)]
fn hash_xattrs(path: &Path, digest: &mut Sha256, scratch: &mut HashScratch) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    scratch.xattr_names.clear();
    rustix::fs::llistxattr(
        path,
        rustix::buffer::spare_capacity(&mut scratch.xattr_names),
    )
    .map_err(io::Error::from)?;
    let names = sorted_xattr_names(&scratch.xattr_names);
    digest.update((names.len() as u64).to_le_bytes());
    for name in names {
        scratch.xattr_value.clear();
        rustix::fs::lgetxattr(
            path,
            OsStr::from_bytes(name),
            rustix::buffer::spare_capacity(&mut scratch.xattr_value),
        )
        .map_err(io::Error::from)?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name);
        digest.update((scratch.xattr_value.len() as u64).to_le_bytes());
        digest.update(&scratch.xattr_value);
    }
    Ok(())
}

#[cfg(unix)]
fn sorted_xattr_names(buffer: &[u8]) -> Vec<&[u8]> {
    // The list buffer stays fixed while values are read into a separate field.
    // Borrow native bytes to avoid one allocation and copy per attribute name.
    let mut names = buffer
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

fn hash_entry(
    path: &Path,
    digest: &mut Sha256,
    version: MarkerVersion,
    special: SpecialEntries,
    scratch: &mut HashScratch,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if version == MarkerVersion::V2 {
        hash_v2_metadata(path, &metadata, digest, scratch)?;
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
        // Keep each native name and its path once. `sort_unstable_by_key` on
        // `DirEntry::file_name()` rebuilt an owned `OsString` for every key
        // comparison, then the hashing loop allocated both values again.
        let mut entries = fs::read_dir(path)?
            .map(|entry| {
                entry.map(|entry| {
                    let name = entry.file_name();
                    let path = path.join(&name);
                    (name, path)
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        digest.update((entries.len() as u64).to_le_bytes());
        for (name, path) in entries {
            hash_native(&name, digest);
            hash_entry(&path, digest, version, special, scratch)?;
        }
    } else if metadata.is_file() {
        digest.update(b"file");
        digest.update(metadata.len().to_le_bytes());
        hash_file_bytes(path, metadata.len(), digest, &mut scratch.file_bytes)?;
    } else if special == SpecialEntries::Hash {
        digest.update(b"special");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            digest.update(metadata.mode().to_le_bytes());
            digest.update(metadata.rdev().to_le_bytes());
        }
    } else {
        return Err(io::Error::other("unsupported entry in immutable base"));
    }
    Ok(())
}

/// Hash a regular file's bytes. The caller owns one heap buffer for the whole
/// operation because `hash_entry` recurses once per directory level: a 64 KiB
/// stack buffer in every frame overflowed the stack a few hundred levels deep.
#[inline(never)]
fn hash_file_bytes(
    path: &Path,
    expected_length: u64,
    digest: &mut Sha256,
    buffer: &mut [u8],
) -> io::Result<()> {
    let mut file = open_regular(path)?;
    let mut length = 0;
    loop {
        let count = file.read(buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        digest.update(&buffer[..count]);
    }
    if length != expected_length {
        return Err(io::Error::other(ChangedWhileReading));
    }
    Ok(())
}

/// A file's length changed between its metadata and the end of reading it:
/// something wrote to it during the walk. Typed so a caller walking a live
/// worktree can tell a concurrent change from a real I/O failure.
#[derive(Debug)]
pub(crate) struct ChangedWhileReading;

impl std::fmt::Display for ChangedWhileReading {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("integrity input changed while reading")
    }
}

impl std::error::Error for ChangedWhileReading {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::Instant;

    /// Repeatable microbenchmark for the many-file immutable-base integrity
    /// path. Kept threshold-free because filesystem caches and host load move
    /// absolute timings; before/after work should compare alternating release
    /// builds on the same fixture and retain every sample.
    #[test]
    #[ignore = "manual release-mode immutable-base integrity benchmark"]
    fn reports_many_file_integrity_latency() {
        const DIRECTORIES: usize = 96;
        const FILES_PER_DIRECTORY: usize = 64;
        const PAYLOAD_BYTES: usize = 12 * 1024;
        const ROUNDS: usize = 5;

        let fixture = tempfile::tempdir().expect("fixture");
        let payload = vec![b'x'; PAYLOAD_BYTES];
        for directory in 0..DIRECTORIES {
            let directory = fixture.path().join(format!("directory-{directory:03}"));
            fs::create_dir(&directory).expect("benchmark directory");
            for file in 0..FILES_PER_DIRECTORY {
                fs::write(directory.join(format!("file-{file:03}")), &payload)
                    .expect("benchmark file");
            }
        }

        let expected = marker_v2(fixture.path()).expect("warm integrity marker");
        let mut samples = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let started = Instant::now();
            assert_eq!(marker_v2(fixture.path()).unwrap(), expected);
            samples.push(started.elapsed().as_micros());
        }
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        println!(
            "RIFTRI_INTEGRITY_BENCHMARK files={} logical_bytes={} marker={} median_microseconds={} samples_microseconds={samples:?}",
            DIRECTORIES * FILES_PER_DIRECTORY,
            DIRECTORIES * FILES_PER_DIRECTORY * PAYLOAD_BYTES,
            String::from_utf8_lossy(&expected).replace('\n', ":"),
            sorted[ROUNDS / 2],
        );
    }

    /// Every recursion level used to reserve the 64 KiB read buffer on the
    /// stack, so a tree a few hundred directories deep, which Git checks out
    /// without trouble, aborted Riftri with a stack overflow mid-add.
    #[test]
    fn integrity_hashes_a_very_deep_tree() {
        let fixture = tempfile::tempdir().expect("fixture");
        let mut deepest = fixture.path().to_path_buf();
        for _ in 0..300 {
            deepest.push("n");
        }
        fs::create_dir_all(&deepest).expect("deep tree");
        fs::write(deepest.join("leaf"), b"leaf\n").expect("leaf");

        // Test threads get a 2 MiB stack, a quarter of the main thread's.
        let first = marker(fixture.path()).expect("digest a deep tree");
        assert_eq!(marker(fixture.path()).unwrap(), first);
    }

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
    fn xattr_names_borrow_original_bytes_in_sorted_order() {
        let bytes = b"z\0\0a\0\xff\0";
        let names = super::sorted_xattr_names(bytes);
        assert_eq!(names, vec![&b"a"[..], &b"z"[..], &b"\xff"[..]]);
        for (name, offset) in names.iter().zip([3, 0, 5]) {
            assert_eq!(
                name.as_ptr(),
                bytes[offset..].as_ptr(),
                "name must not be copied"
            );
        }
        assert!(super::sorted_xattr_names(b"").is_empty());
    }

    #[test]
    fn xattr_digest_preserves_the_owned_name_layout() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file");
        fs::write(&path, b"contents").unwrap();
        #[cfg(target_os = "macos")]
        let prefix = "com.riftri";
        #[cfg(not(target_os = "macos"))]
        let prefix = "user.riftri";
        for (suffix, value) in [
            ("z", &b"\0\xffbinary"[..]),
            ("a", &b""[..]),
            ("unicode-\u{e9}", &b"value"[..]),
        ] {
            let name = format!("{prefix}.{suffix}");
            rustix::fs::setxattr(&path, name.as_str(), value, rustix::fs::XattrFlags::empty())
                .unwrap();
        }
        // Independent reference retains the previous owned-name implementation.
        let mut buffer = Vec::with_capacity(64 * 1024);
        rustix::fs::llistxattr(&path, rustix::buffer::spare_capacity(&mut buffer)).unwrap();
        let mut names = buffer
            .split(|b| *b == 0)
            .filter(|n| !n.is_empty())
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert!(names.len() >= 3);
        let mut expected = Sha256::new();
        expected.update((names.len() as u64).to_le_bytes());
        for name in names {
            let mut value = Vec::with_capacity(256 * 1024);
            rustix::fs::lgetxattr(
                &path,
                OsStr::from_bytes(&name),
                rustix::buffer::spare_capacity(&mut value),
            )
            .unwrap();
            expected.update((name.len() as u64).to_le_bytes());
            expected.update(&name);
            expected.update((value.len() as u64).to_le_bytes());
            expected.update(value);
        }
        let mut actual = Sha256::new();
        let mut scratch = super::HashScratch::new();
        super::hash_xattrs(&path, &mut actual, &mut scratch).unwrap();
        assert_eq!(actual.finalize(), expected.finalize());
        assert!(
            super::hash_xattrs(
                &fixture.path().join("missing"),
                &mut Sha256::new(),
                &mut scratch
            )
            .is_err()
        );
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
