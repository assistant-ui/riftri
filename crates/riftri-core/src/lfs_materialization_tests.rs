use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

use super::{GitLfsObject, GitLfsPointer, materialize_git_lfs_objects};

fn fixture() -> (TempDir, PathBuf, Vec<GitLfsObject>) {
    fixture_with_size(2 * 1024 * 1024)
}

fn fixture_with_size(size: usize) -> (TempDir, PathBuf, Vec<GitLfsObject>) {
    let fixture = tempdir().unwrap();
    let staging = fixture.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let objects = (0..8)
        .map(|index| {
            // Duplicate OIDs still have independent checkout destinations.
            let payload = vec![(index % 2) as u8; size];
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

#[cfg(target_os = "macos")]
#[test]
fn native_lfs_expansion_verifies_the_private_snapshot_before_publication() {
    use crate::test_hooks::{FilesystemRacePoint, install};
    use std::cell::Cell;
    use std::rc::Rc;
    for action in [
        "success",
        "corrupt-clone",
        "source-write",
        "checkout-write",
        "source-corrupt",
    ] {
        let (fixture, staging, mut objects) = fixture();
        objects.truncate(1);
        let object = &mut objects[0];
        let payload = vec![0x5a; 8 * 1024 * 1024];
        object.pointer.size = payload.len() as u64;
        object.pointer.oid = crate::base_integrity::hex_lower(Sha256::digest(&payload));
        fs::write(&object.source_path, &payload).unwrap();
        let checkout = staging.join(&object.checkout_path);
        let observed = Rc::new(Cell::new(false));
        let seen = observed.clone();
        let source = object.source_path.clone();
        let destination = checkout.clone();
        if action == "source-corrupt" {
            fs::write(&source, vec![0xff; payload.len()]).unwrap();
        }
        let _guard = install(FilesystemRacePoint::LfsClonePrepared, move |clone| {
            seen.set(true);
            match action {
                "corrupt-clone" => fs::write(clone, vec![0xff; 8 * 1024 * 1024]).unwrap(),
                "source-write" => fs::write(source, b"later source write").unwrap(),
                "checkout-write" => fs::write(destination, b"later checkout write").unwrap(),
                _ => {}
            }
        });
        let result = materialize_git_lfs_objects(&staging, &objects);
        assert!(
            observed.get(),
            "fixture must exercise the native path: {action}"
        );
        if ["success", "source-write"].contains(&action) {
            result.unwrap();
            assert_eq!(fs::read(&checkout).unwrap(), payload);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains(if action == "checkout-write" {
                    "changed before publication"
                } else {
                    "failed SHA-256 verification"
                }),
                "{error}"
            );
            assert_eq!(
                fs::read(&checkout).unwrap(),
                if action == "checkout-write" {
                    b"later checkout write".as_slice()
                } else {
                    b"pointer\n"
                }
            );
        }
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            8,
            "temporary clones must be gone"
        );
        assert!(!fixture.path().join("active").exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_lfs_clone_process_exit_is_recovered_by_the_add_journal() {
    use crate::test_hooks::{FilesystemRacePoint, install};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    const CHILD_ROOT: &str = "RIFTRI_TEST_LFS_CLONE_EXIT_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let sentinel = root.join("clone-path");
        let _guard = install(FilesystemRacePoint::LfsClonePrepared, move |path| {
            fs::write(sentinel, path.as_os_str().as_encoded_bytes()).unwrap();
            std::process::exit(86); // deliberately bypass every Drop/rollback
        });
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: root.join("repository"),
            destination: root.join("view"),
            revision: "HEAD".into(),
            mode: crate::WorktreeMode::Detached,
            state_dir: Some(root.join("state")),
            sparse_directories: Vec::new(),
        })
        .unwrap();
        panic!("native clone exit point was not reached");
    }
    let root = tempdir().unwrap();
    let repository = root.path().join("repository");
    fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repository)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.name", "LFS clone fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    let payload = vec![0x5a; 8 * 1024 * 1024];
    let oid = crate::base_integrity::hex_lower(Sha256::digest(&payload));
    let object = repository.join(format!(
        ".git/lfs/objects/{}/{}/{oid}",
        &oid[..2],
        &oid[2..4]
    ));
    fs::create_dir_all(object.parent().unwrap()).unwrap();
    fs::write(&object, &payload).unwrap();
    fs::write(
        repository.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    fs::write(
        repository.join("asset.bin"),
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
            payload.len()
        ),
    )
    .unwrap();
    git(&["add", "--all"]);
    git(&["commit", "--quiet", "-m", "LFS clone fixture"]);
    for (key, value) in [
        ("filter.lfs.clean", "git-lfs clean -- %f"),
        ("filter.lfs.smudge", "git-lfs smudge -- %f"),
        ("filter.lfs.required", "true"),
    ] {
        git(&["config", key, value]);
    }
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    // Only version discovery can run before the clone hook exits. Real-filter
    // cleanliness and lifecycle checks are covered by the cold-LFS runner.
    fs::write(bin.join("git-lfs"), "#!/bin/sh\ncase \"$1\" in version) printf '%s\\n' 'git-lfs/3.7.0 (fixture)' ;; *) exit 1 ;; esac\n").unwrap();
    fs::set_permissions(bin.join("git-lfs"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "worktree::lfs_materialization_tests::native_lfs_clone_process_exit_is_recovered_by_the_add_journal", "--nocapture"])
        .env(CHILD_ROOT, root.path()).env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    use std::os::unix::ffi::OsStringExt;
    let clone = PathBuf::from(std::ffi::OsString::from_vec(
        fs::read(root.path().join("clone-path")).unwrap(),
    ));
    let state = root.path().join("state");
    assert!(clone.starts_with(fs::canonicalize(&state).unwrap()));
    assert!(
        clone.is_file(),
        "crash must leave an unpublished native clone"
    );
    let report = crate::recover_incomplete_operations(&state).unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.recovered, 1);
    assert!(!clone.exists());
    assert!(!root.path().join("view").exists());
    assert_eq!(fs::read(object).unwrap(), payload);
    let repeated = crate::recover_incomplete_operations(&state).unwrap();
    assert_eq!(repeated.recovered, 0);
    assert!(repeated.errors.is_empty());
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
    let (_fixture, staging, objects) = fixture_with_size(8 * 1024 * 1024);
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
    let (_fixture, staging, objects) = fixture_with_size(8 * 1024 * 1024);
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
