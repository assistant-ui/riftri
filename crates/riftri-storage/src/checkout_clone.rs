//! Staged APFS data clones for private, unpublished checkout files.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::darwin::fs::MetadataExt as _;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// An unpublished clone. Dropping it removes only its private temporary tree.
pub struct CheckoutClone {
    directory: TempDir,
    file: File,
    destination: PathBuf,
    original: Metadata,
    prepared: Metadata,
}

impl CheckoutClone {
    /// Read this isolated snapshot to verify its complete content before publish.
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Path to the unpublished snapshot, for contextual errors and verification.
    pub fn path(&self) -> PathBuf {
        self.directory.path().join("data")
    }

    /// Atomically replace a still-unchanged private checkout after verification.
    /// The caller must own the unpublished staging tree and verify all bytes;
    /// this is not a replacement API for live worktrees or untrusted directories.
    pub fn publish(self, checkout: &File) -> io::Result<()> {
        if !unchanged(&self.original, &checkout.metadata()?)
            || !unchanged(&self.original, &fs::symlink_metadata(&self.destination)?)
            || !unchanged(&self.prepared, &self.file.metadata()?)
            || !unchanged(&self.prepared, &fs::symlink_metadata(self.path())?)
        {
            return Err(io::Error::other(
                "LFS clone or checkout changed before publication",
            ));
        }
        fs::rename(self.path(), &self.destination)?;
        // An interrupted preparation/publication remains entirely beneath the
        // journal-owned base staging tree. Cleanup failure stops base publication.
        self.directory.close()
    }
}

/// Prepare a native clone when both files have ordinary, compatible metadata.
/// `None` leaves the existing checkout untouched and declines the optimization.
pub fn prepare(
    source: &File,
    checkout: &File,
    destination: &Path,
) -> io::Result<Option<CheckoutClone>> {
    let original = checkout.metadata()?;
    let source_metadata = source.metadata()?;
    if !unchanged(&original, &fs::symlink_metadata(destination)?) {
        return Err(io::Error::other("LFS checkout path changed before cloning"));
    }
    if !ordinary(&original)
        || !ordinary(&source_metadata)
        || original.dev() != source_metadata.dev()
        || original.uid() != source_metadata.uid()
        || original.gid() != source_metadata.gid()
        // SAFETY: geteuid has no arguments or side effects.
        || original.uid() != unsafe { libc::geteuid() }
        || has_private_metadata(source)?
        || has_private_metadata(checkout)?
    {
        return Ok(None);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("checkout has no parent"))?;
    let directory = tempfile::Builder::new()
        .prefix(".riftri-lfs-")
        .tempdir_in(parent)?;
    let parent_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.path())?;
    // Inherited ACLs/attributes on the temporary directory could alter the new
    // inode. Decline before cloning; the original streaming path remains valid.
    if has_private_metadata(&parent_file)? {
        directory.close()?;
        return Ok(None);
    }
    // SAFETY: descriptors stay live and `data` is an absent child of our private
    // directory. Source identity comes from the already validated descriptor,
    // not a second pathname lookup. fclonefileat has no byte-copy fallback.
    if unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            parent_file.as_raw_fd(),
            c"data".as_ptr(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(directory.path().join("data"))?;
    // SAFETY: both descriptors are live regular files. Metadata-only copy
    // preserves the cloned data, including when the object was read-only.
    if unsafe {
        libc::fcopyfile(
            checkout.as_raw_fd(),
            file.as_raw_fd(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // Expansion is a new content write, not reuse of the object-store mtime.
    file.set_modified(std::time::SystemTime::now())?;
    let prepared = file.metadata()?;
    if !ordinary(&prepared)
        || prepared.uid() != original.uid()
        || prepared.gid() != original.gid()
        || prepared.mode() != original.mode()
        || prepared.len() != source_metadata.len()
        || has_private_metadata(&file)?
        || !unchanged(&original, &checkout.metadata()?)
        || !unchanged(&source_metadata, &source.metadata()?)
    {
        return Err(io::Error::other(
            "LFS native clone metadata changed during preparation",
        ));
    }
    Ok(Some(CheckoutClone {
        directory,
        file,
        destination: destination.to_path_buf(),
        original,
        prepared,
    }))
}

fn ordinary(metadata: &Metadata) -> bool {
    metadata.is_file()
        && metadata.nlink() == 1
        && metadata.mode() & 0o7000 == 0
        && metadata.st_flags() == 0
}

fn unchanged(before: &Metadata, after: &Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.mode() == after.mode()
        && before.uid() == after.uid()
        && before.gid() == after.gid()
        && before.len() == after.len()
        && before.nlink() == after.nlink()
        && before.st_flags() == after.st_flags()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn has_private_metadata(file: &File) -> io::Result<bool> {
    // SAFETY: the descriptor is live; a null buffer requests the xattr-list size.
    let length = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    if length != 0 {
        return Ok(true);
    }
    // Like macos_acl's pathname inspection, retain empty ACLs carrying private
    // inheritance flags. Inspect the open file, never reopen the object by path.
    #[repr(C)]
    #[derive(Default)]
    struct SecurityHeader {
        length: u32,
        offset: i32,
        acl_length: u32,
    }
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_EXTENDED_SECURITY,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut header = SecurityHeader::default();
    // SAFETY: one valid attribute request and a writable fixed-size header;
    // Darwin can truncate the ACL payload but retains its length reference.
    if unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&mut attributes as *mut libc::attrlist).cast(),
            (&mut header as *mut SecurityHeader).cast(),
            std::mem::size_of::<SecurityHeader>(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if header.length < std::mem::size_of::<SecurityHeader>() as u32 {
        return Err(io::Error::other("incomplete macOS security attribute"));
    }
    Ok(header.acl_length != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ordinary_checkout_can_prepare_a_private_native_clone() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("object");
        let destination = root.path().join("checkout");
        fs::write(&source, vec![0x5a; 8 * 1024 * 1024]).unwrap();
        fs::write(&destination, b"pointer\n").unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o751)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).unwrap();
        let checkout = OpenOptions::new().write(true).open(&destination).unwrap();
        let mut prepared = prepare(&File::open(&source).unwrap(), &checkout, &destination)
            .unwrap()
            .expect("ordinary files should take the native path");
        assert_eq!(fs::read(&destination).unwrap(), b"pointer\n");
        assert_eq!(
            fs::metadata(prepared.path()).unwrap().permissions().mode() & 0o7777,
            0o751
        );
        let temporary = prepared.directory.path().to_path_buf();
        let mut content = Vec::new();
        std::io::Read::read_to_end(prepared.file_mut(), &mut content).unwrap();
        assert_eq!(content, vec![0x5a; 8 * 1024 * 1024]);
        prepared.publish(&checkout).unwrap();
        assert!(!temporary.exists());
        assert_eq!(fs::read(&destination).unwrap(), content);
        assert_ne!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&destination).unwrap().ino()
        );
        fs::write(&destination, b"private").unwrap();
        assert_eq!(fs::read(&source).unwrap(), content);
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o7777,
            0o444
        );
    }

    #[test]
    fn private_metadata_declines_before_mutating_the_checkout() {
        for target in ["source", "checkout"] {
            for kind in ["xattr", "acl", "special-mode", "hardlink", "flags"] {
                let root = tempfile::tempdir().unwrap();
                let source = root.path().join("source");
                let destination = root.path().join("checkout");
                fs::write(&source, b"payload").unwrap();
                fs::write(&destination, b"pointer\n").unwrap();
                let path = root.path().join(target);
                match kind {
                    "xattr" => assert!(
                        std::process::Command::new("/usr/bin/xattr")
                            .args(["-w", "com.riftri.private", "keep"])
                            .arg(&path)
                            .status()
                            .unwrap()
                            .success()
                    ),
                    "acl" => assert!(
                        std::process::Command::new("/bin/chmod")
                            .args(["+a", "everyone allow read"])
                            .arg(&path)
                            .status()
                            .unwrap()
                            .success()
                    ),
                    "special-mode" => {
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o1644)).unwrap()
                    }
                    "hardlink" => fs::hard_link(&path, root.path().join("alias")).unwrap(),
                    _ => assert!(
                        std::process::Command::new("/usr/bin/chflags")
                            .arg("nodump")
                            .arg(&path)
                            .status()
                            .unwrap()
                            .success()
                    ),
                }
                assert!(
                    prepare(
                        &File::open(&source).unwrap(),
                        &File::open(&destination).unwrap(),
                        &destination
                    )
                    .unwrap()
                    .is_none(),
                    "{target} {kind}"
                );
                assert_eq!(fs::read(&destination).unwrap(), b"pointer\n");
                assert_eq!(
                    fs::read_dir(root.path()).unwrap().count(),
                    if kind == "hardlink" { 3 } else { 2 }
                );
            }
        }
    }

    #[test]
    fn replacements_and_later_changes_are_preserved_and_temporary_clones_are_removed() {
        for change in ["replace-checkout", "write-checkout", "write-clone", "drop"] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let destination = root.path().join("checkout");
            fs::write(&source, b"payload").unwrap();
            fs::write(&destination, b"pointer\n").unwrap();
            let checkout = File::open(&destination).unwrap();
            let prepared = prepare(&File::open(&source).unwrap(), &checkout, &destination)
                .unwrap()
                .unwrap();
            let temporary = prepared.directory.path().to_path_buf();
            match change {
                "replace-checkout" => {
                    fs::rename(&destination, root.path().join("old-checkout")).unwrap();
                    fs::write(&destination, b"changed").unwrap();
                }
                "write-checkout" => fs::write(&destination, b"changed").unwrap(),
                "write-clone" => fs::write(prepared.path(), b"changed").unwrap(),
                _ => {}
            }
            if change == "drop" {
                drop(prepared);
            } else {
                assert!(prepared.publish(&checkout).is_err());
            }
            assert!(!temporary.exists());
            assert_eq!(
                fs::read(&destination).unwrap(),
                if change.ends_with("checkout") {
                    b"changed".as_slice()
                } else {
                    b"pointer\n"
                }
            );
            assert_eq!(fs::read(&source).unwrap(), b"payload");
        }
    }

    #[test]
    fn source_path_replacement_cannot_redirect_the_open_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("checkout");
        fs::write(&source, b"payload").unwrap();
        fs::write(&destination, b"pointer\n").unwrap();
        let opened_source = File::open(&source).unwrap();
        fs::rename(&source, root.path().join("old-source")).unwrap();
        fs::write(&source, b"foreign").unwrap();
        let checkout = File::open(&destination).unwrap();
        let prepared = prepare(&opened_source, &checkout, &destination)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(prepared.path()).unwrap(), b"payload");
        prepared.publish(&checkout).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"foreign");
        assert_eq!(fs::read(&destination).unwrap(), b"payload");
    }
}
