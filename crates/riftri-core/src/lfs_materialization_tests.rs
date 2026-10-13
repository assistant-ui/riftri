use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

use super::{GitLfsObject, GitLfsPointer, materialize_git_lfs_objects};

fn fixture() -> (TempDir, PathBuf, Vec<GitLfsObject>) {
    let fixture = tempdir().unwrap();
    let staging = fixture.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let objects = (0..8)
        .map(|index| {
            // Duplicate OIDs still have independent checkout destinations.
            let payload = vec![(index % 2) as u8; 2 * 1024 * 1024];
            let oid = crate::base_integrity::hex_lower(Sha256::digest(&payload));
            let source_path = fixture.path().join(&oid);
            fs::write(&source_path, &payload).unwrap();
            let checkout_path = PathBuf::from(format!("asset {index}-é.bin"));
            fs::write(staging.join(&checkout_path), b"pointer\n").unwrap();
            GitLfsObject {
                checkout_path,
                source_path,
                pointer: GitLfsPointer {
                    oid,
                    size: payload.len() as u64,
                },
            }
        })
        .collect();
    (fixture, staging, objects)
}

#[test]
fn bounded_materialization_preserves_duplicate_objects_modes_and_private_writes() {
    let (_fixture, staging, objects) = fixture();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            staging.join(&objects[0].checkout_path),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    materialize_git_lfs_objects(&staging, &objects).unwrap();
    for object in &objects {
        assert_eq!(
            fs::read(staging.join(&object.checkout_path)).unwrap(),
            fs::read(&object.source_path).unwrap()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(staging.join(&objects[0].checkout_path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    fs::write(staging.join(&objects[0].checkout_path), b"private").unwrap();
    assert_eq!(
        fs::read(staging.join(&objects[2].checkout_path)).unwrap(),
        fs::read(&objects[0].source_path).unwrap()
    );
    assert_eq!(
        fs::metadata(&objects[0].source_path).unwrap().len(),
        objects[0].pointer.size
    );
}

#[test]
fn parallel_materialization_rejects_corruption_missing_objects_and_size_changes() {
    for failure in ["corrupt", "missing", "size"] {
        let (fixture, staging, objects) = fixture();
        let victim = &objects[1];
        match failure {
            "corrupt" => fs::write(
                &victim.source_path,
                vec![0xff; victim.pointer.size as usize],
            )
            .unwrap(),
            "missing" => fs::remove_file(&victim.source_path).unwrap(),
            _ => fs::write(&victim.source_path, b"wrong size").unwrap(),
        }
        let error = materialize_git_lfs_objects(&staging, &objects)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(match failure {
                "corrupt" => "failed SHA-256 verification",
                "missing" => "could not open local Git LFS object",
                _ => "changed size before materialization",
            }),
            "{failure}: {error}"
        );
        // Safe for the transaction to remove the unpublished stage immediately:
        // the scheduler must not return while another writer is still using it.
        fs::remove_dir_all(&staging).unwrap();
        assert!(!staging.exists());
        assert!(!fixture.path().join("active").exists());
        if failure == "corrupt" {
            assert_eq!(
                fs::read(&victim.source_path).unwrap(),
                vec![0xff; victim.pointer.size as usize]
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn parallel_materialization_does_not_follow_source_or_destination_symlinks() {
    use std::os::unix::fs::symlink;
    for source_link in [false, true] {
        let (fixture, staging, objects) = fixture();
        let outside = fixture.path().join("outside");
        fs::write(&outside, b"untouched").unwrap();
        let link = if source_link {
            objects[0].source_path.clone()
        } else {
            staging.join(&objects[0].checkout_path)
        };
        fs::remove_file(&link).unwrap();
        symlink(&outside, &link).unwrap();
        assert!(materialize_git_lfs_objects(&staging, &objects).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
        fs::remove_dir_all(&staging).unwrap();
    }
}

#[test]
fn small_lfs_workloads_stay_inline() {
    let (_fixture, _staging, mut objects) = fixture();
    assert_eq!(super::lfs_materialization_workers(&[]), 1);
    assert_eq!(super::lfs_materialization_workers(&objects[..3]), 1);
    for object in &mut objects {
        object.pointer.size = 17;
    }
    assert_eq!(super::lfs_materialization_workers(&objects), 1);
}

// Native data sharing must not turn local object-store metadata into checkout
// metadata. In particular, two paths with the same OID can have different Git
// executable bits and destination attributes. Keep this contract before trying
// a clone-based replacement for the existing verified streaming expansion.
#[cfg(unix)]
#[test]
fn lfs_expansion_preserves_destination_metadata_not_object_store_metadata() {
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "macos")]
    const SHARED: &str = "com.riftri.lfs-shared";
    #[cfg(target_os = "macos")]
    const SOURCE_ONLY: &str = "com.riftri.lfs-source-only";
    #[cfg(not(target_os = "macos"))]
    const SHARED: &str = "user.riftri.lfs-shared";
    #[cfg(not(target_os = "macos"))]
    const SOURCE_ONLY: &str = "user.riftri.lfs-source-only";
    let (_fixture, staging, objects) = fixture();
    let source = &objects[0].source_path;
    fs::set_permissions(source, fs::Permissions::from_mode(0o640)).unwrap();
    for name in [SHARED, SOURCE_ONLY] {
        rustix::fs::setxattr(
            source,
            name,
            b"object-store",
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
    }
    let destinations = [
        (0, 0o751, b"first".as_slice()),
        (2, 0o600, b"second".as_slice()),
    ];
    for (index, mode, value) in destinations {
        let path = staging.join(&objects[index].checkout_path);
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        rustix::fs::setxattr(&path, SHARED, value, rustix::fs::XattrFlags::empty()).unwrap();
    }
    materialize_git_lfs_objects(&staging, &objects).unwrap();
    for (index, mode, value) in destinations {
        let path = staging.join(&objects[index].checkout_path);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            mode
        );
        let mut buffer = Vec::with_capacity(64);
        rustix::fs::getxattr(&path, SHARED, rustix::buffer::spare_capacity(&mut buffer)).unwrap();
        assert_eq!(buffer, value);
        let error = rustix::fs::getxattr(&path, SOURCE_ONLY, &mut [0_u8; 64]).unwrap_err();
        #[cfg(target_os = "macos")]
        assert_eq!(error, rustix::io::Errno::NOATTR);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(error, rustix::io::Errno::NODATA);
        assert_eq!(fs::read(&path).unwrap(), fs::read(source).unwrap());
    }
    assert_eq!(
        fs::metadata(source).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    let mut buffer = Vec::with_capacity(64);
    rustix::fs::getxattr(
        source,
        SOURCE_ONLY,
        rustix::buffer::spare_capacity(&mut buffer),
    )
    .unwrap();
    assert_eq!(buffer, b"object-store");
}

#[cfg(target_os = "macos")]
#[test]
fn lfs_expansion_preserves_checkout_acls_without_importing_object_store_acls() {
    let (_fixture, staging, objects) = fixture();
    let source = &objects[0].source_path;
    let output = std::process::Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"])
        .arg(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(riftri_storage::has_macos_acl(source).unwrap());
    let destination = staging.join(&objects[0].checkout_path);
    let destination_with_acl = staging.join(&objects[2].checkout_path);
    assert!(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,execute"])
            .arg(&destination_with_acl)
            .status()
            .unwrap()
            .success()
    );
    let acl = |path: &std::path::Path| {
        let output = std::process::Command::new("/bin/ls")
            .arg("-lde")
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let checkout_acl = acl(&destination_with_acl);
    assert!(!checkout_acl.is_empty());
    assert!(!riftri_storage::has_macos_acl(&destination).unwrap());
    materialize_git_lfs_objects(&staging, &objects).unwrap();
    assert!(!riftri_storage::has_macos_acl(&destination).unwrap());
    assert_eq!(acl(&destination_with_acl), checkout_acl);
    assert!(riftri_storage::has_macos_acl(source).unwrap());
    assert_eq!(fs::read(destination).unwrap(), fs::read(source).unwrap());
    assert_eq!(
        fs::read(destination_with_acl).unwrap(),
        fs::read(source).unwrap()
    );
}
