//! macOS ACL inspection. Extended ACLs are not exposed by listxattr and
//! cannot be reconstructed from a Git tree's ordinary permission bits.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::StorageError;

// A getattrlist result starts with its length, followed by the requested
// attribute. Extended security is an attrreference (signed offset + length).
// We only need its length, not the variable-sized ACL contents.
#[repr(C)]
#[derive(Default)]
struct SecurityHeader {
    length: u32,
    offset: i32,
    acl_length: u32,
}

pub(crate) fn has_extended_acl(path: &Path) -> Result<bool, StorageError> {
    let error = |source| StorageError::Io {
        operation: "inspect macOS ACL",
        path: path.to_path_buf(),
        source,
    };
    let native = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        error(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ACL path contains NUL",
        ))
    })?;
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
    // SAFETY: native is NUL-terminated, attributes contains one valid request,
    // and header is writable for its supplied size. Darwin may truncate the
    // variable ACL payload; the fixed attrreference remains available.
    if unsafe {
        libc::getattrlist(
            native.as_ptr(),
            (&mut attributes as *mut libc::attrlist).cast(),
            (&mut header as *mut SecurityHeader).cast(),
            std::mem::size_of::<SecurityHeader>(),
            libc::FSOPT_NOFOLLOW,
        )
    } != 0
    {
        return Err(error(std::io::Error::last_os_error()));
    }
    if header.length < std::mem::size_of::<SecurityHeader>() as u32 {
        return Err(error(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete macOS security attribute",
        )));
    }
    // No ACL produces an empty reference. Any ACL (including an empty one
    // with inheritance flags) is private metadata that Git cannot reconstruct.
    Ok(header.acl_length != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{c_char, c_int, c_void};
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::process::Command;

    #[test]
    fn detects_file_and_directory_acls_without_following_symlinks() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("file with spaces");
        fs::write(&file, "private\n").unwrap();
        assert!(!has_extended_acl(&file).unwrap());
        assert!(
            Command::new("chmod")
                .args(["+a", "everyone deny write"])
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );
        assert!(has_extended_acl(&file).unwrap());
        let link = fixture.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(!has_extended_acl(&link).unwrap());
        let dangling = fixture.path().join("dangling");
        symlink("absent-target", &dangling).unwrap();
        assert!(!has_extended_acl(&dangling).unwrap());
        assert!(has_extended_acl(&fixture.path().join("missing")).is_err());
        assert!(
            Command::new("chmod")
                .args(["+a", "everyone allow read,file_inherit,directory_inherit"])
                .arg(fixture.path())
                .status()
                .unwrap()
                .success()
        );
        assert!(has_extended_acl(fixture.path()).unwrap());
        let inherited = fixture.path().join("inherited");
        fs::write(&inherited, "inherited ACL\n").unwrap();
        assert!(has_extended_acl(&inherited).unwrap());
    }

    #[test]
    fn detects_empty_acl_with_private_inheritance_flags() {
        unsafe extern "C" {
            fn acl_init(count: c_int) -> *mut c_void;
            fn acl_get_flagset_np(acl: *mut c_void, flags: *mut *mut c_void) -> c_int;
            fn acl_add_flag_np(flags: *mut c_void, flag: c_int) -> c_int;
            fn acl_set_file(path: *const c_char, kind: c_int, acl: *mut c_void) -> c_int;
            fn acl_free(acl: *mut c_void) -> c_int;
        }
        let fixture = tempfile::tempdir().unwrap();
        let native = CString::new(fixture.path().as_os_str().as_bytes()).unwrap();
        // SAFETY: create a private empty ACL, add Darwin's NO_INHERIT flag,
        // apply it only to our disposable fixture, then free the owned ACL.
        unsafe {
            let acl = acl_init(0);
            assert!(!acl.is_null());
            let mut flags = std::ptr::null_mut();
            assert_eq!(acl_get_flagset_np(acl, &mut flags), 0);
            assert_eq!(acl_add_flag_np(flags, 1 << 17), 0);
            let result = acl_set_file(native.as_ptr(), 0x100, acl);
            acl_free(acl);
            assert_eq!(result, 0);
        }
        assert!(has_extended_acl(fixture.path()).unwrap());
    }

    #[test]
    fn non_utf8_paths_reach_the_native_api() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture
            .path()
            .join(std::ffi::OsString::from_vec(b"missing-\xff".to_vec()));
        // APFS rejects such names, but the low-level API must not require UTF-8.
        let StorageError::Io { source, .. } = has_extended_acl(&path).unwrap_err() else {
            panic!("expected a native filesystem error");
        };
        assert!(source.raw_os_error().is_some());
    }
}
