//! How much of a file's data no copy-on-write clone shares.
//!
//! A cloned worktree file shares its blocks with the immutable base until it
//! is written. The bytes that are its own are what the worktree really costs
//! on disk; `st_blocks` cannot tell them apart, because it counts shared
//! blocks in full for every clone.

use std::io;
use std::path::Path;

/// Bytes of the regular file at `path` whose data no clone shares, or `None`
/// where the filesystem cannot say. A symlink is not followed.
///
/// macOS asks APFS for `ATTR_CMNEXT_PRIVATESIZE`; a volume that does not
/// report it, such as HFS+, gives `None`. Linux maps the file's extents with
/// `FIEMAP` and counts each one without `FIEMAP_EXTENT_SHARED`; data not yet
/// allocated is counted, because only a write can have produced it. A
/// filesystem without `FIEMAP` gives `None`. Other platforms give `None`.
#[cfg(target_os = "macos")]
pub fn private_data_bytes(path: &Path) -> io::Result<Option<u64>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // The reply: a u32 length, the returned attribute_set_t (five u32 groups:
    // common, volume, directory, file, fork), then the private size as off_t.
    const FORK_GROUP: usize = 4 + 4 * 4;
    const PRIVATE_SIZE: usize = 4 + 5 * 4;

    #[repr(C, align(8))]
    struct Reply([u8; 64]);

    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        // With FSOPT_ATTR_CMN_EXTENDED, the fork group carries ATTR_CMNEXT_*.
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE,
    };
    let mut reply = Reply([0; 64]);
    // SAFETY: `path` is NUL-terminated and outlives the call, `request` is a
    // valid attrlist, and the reply buffer is writable for the length passed.
    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut request).cast(),
            reply.0.as_mut_ptr().cast(),
            reply.0.len(),
            libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    let word = |offset: usize| {
        u32::from_ne_bytes(
            reply.0[offset..offset + 4]
                .try_into()
                .expect("four reply bytes"),
        )
    };
    if word(FORK_GROUP) & libc::ATTR_CMNEXT_PRIVATESIZE == 0 {
        return Ok(None);
    }
    let private = i64::from_ne_bytes(
        reply.0[PRIVATE_SIZE..PRIVATE_SIZE + 8]
            .try_into()
            .expect("eight reply bytes"),
    );
    Ok(u64::try_from(private).ok())
}

#[cfg(target_os = "linux")]
pub fn private_data_bytes(path: &Path) -> io::Result<Option<u64>> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    /// `_IOWR('f', 11, struct fiemap)` on every architecture Riftri ships for.
    const FS_IOC_FIEMAP: u32 = 0xC020_660B;
    const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
    const FIEMAP_EXTENT_SHARED: u32 = 0x0000_2000;
    const EXTENTS_PER_CALL: usize = 64;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }

    #[repr(C)]
    struct Request {
        start: u64,
        length: u64,
        flags: u32,
        mapped_extents: u32,
        extent_count: u32,
        reserved: u32,
        extents: [Extent; EXTENTS_PER_CALL],
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let mut private = 0_u64;
    let mut start = 0_u64;
    loop {
        let mut request = Request {
            start,
            length: u64::MAX - start,
            flags: 0,
            mapped_extents: 0,
            extent_count: EXTENTS_PER_CALL as u32,
            reserved: 0,
            extents: [Extent::default(); EXTENTS_PER_CALL],
        };
        // SAFETY: the descriptor is open for the call, and `request` is a
        // `struct fiemap` followed by room for exactly `extent_count`
        // extents, which is all the kernel writes.
        let status = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                FS_IOC_FIEMAP as libc::Ioctl,
                &raw mut request,
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EOPNOTSUPP | libc::ENOTTY | libc::EINVAL) => Ok(None),
                _ => Err(error),
            };
        }
        let mapped = (request.mapped_extents as usize).min(EXTENTS_PER_CALL);
        if mapped == 0 {
            return Ok(Some(private));
        }
        for extent in &request.extents[..mapped] {
            if extent.flags & FIEMAP_EXTENT_SHARED == 0 {
                private = private.saturating_add(extent.length);
            }
            if extent.flags & FIEMAP_EXTENT_LAST != 0 {
                return Ok(Some(private));
            }
        }
        let last = request.extents[mapped - 1];
        let next = last.logical.saturating_add(last.length);
        if next <= start {
            return Ok(Some(private));
        }
        start = next;
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn private_data_bytes(_path: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;

    use super::private_data_bytes;

    const SIZE: usize = 1 << 20;

    fn write_synced(path: &Path, bytes: &[u8]) {
        let mut file = fs::File::create(path).expect("create fixture file");
        file.write_all(bytes).expect("write fixture file");
        file.sync_all().expect("sync fixture file");
    }

    /// Distinct bytes, so no filesystem can deduplicate or compress them away.
    fn pattern() -> Vec<u8> {
        (0..SIZE)
            .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn clone_supported(directory: &Path) -> bool {
        use crate::{CapabilityStatus, ReflinkCloner};
        let capability = ReflinkCloner::probe(directory);
        if capability.status == CapabilityStatus::Supported {
            return true;
        }
        assert_ne!(
            std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "reflink test volume is required but unavailable: {}",
            capability.explanation
        );
        eprintln!("skipping: {}", capability.explanation);
        false
    }

    #[cfg(target_os = "macos")]
    fn clone_supported(_directory: &Path) -> bool {
        true
    }

    #[cfg(target_os = "linux")]
    fn clone_tree(source: &Path, destination: &Path) {
        crate::ReflinkCloner::clone_tree_owner_writable(source, destination).expect("clone tree");
    }

    #[cfg(target_os = "macos")]
    fn clone_tree(source: &Path, destination: &Path) {
        crate::ApfsCloner::clone_tree_owner_writable(source, destination).expect("clone tree");
    }

    #[test]
    fn data_no_clone_shares_is_private() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let file = directory.path().join("plain");
        write_synced(&file, &pattern());
        match private_data_bytes(&file).expect("measure plain file") {
            Some(private) => assert!(private >= SIZE as u64, "{private}"),
            // Only a filesystem that cannot map extents, such as tmpfs.
            None => eprintln!("this volume does not report private data"),
        }
    }

    #[test]
    fn a_clone_is_private_only_where_it_was_written() {
        let directory = tempfile::tempdir().expect("fixture directory");
        if !clone_supported(directory.path()) {
            return;
        }
        let base = directory.path().join("base");
        fs::create_dir(&base).expect("create base");
        write_synced(&base.join("data"), &pattern());
        let view = directory.path().join("view");
        clone_tree(&base, &view);
        let cloned = view.join("data");

        let before = private_data_bytes(&cloned)
            .expect("measure clone")
            .expect("a clone-capable volume reports private data");
        assert!(
            before < (SIZE / 16) as u64,
            "an unwritten clone owns {before}"
        );

        let mut file = OpenOptions::new()
            .write(true)
            .open(&cloned)
            .expect("open clone");
        file.write_all(&[0xA5; 64 * 1024])
            .expect("overwrite the start of the clone");
        file.sync_all().expect("sync clone");
        let after = private_data_bytes(&cloned)
            .expect("measure written clone")
            .expect("still reported");
        assert!(after >= 64 * 1024, "the written range is private: {after}");
        assert!(after < SIZE as u64, "the rest is still shared: {after}");

        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&cloned, &link).expect("symlink");
        let followed = private_data_bytes(&link).ok().flatten();
        assert_ne!(followed, Some(after), "a symlink is not followed");
    }
}
