#![cfg(any(target_os = "macos", target_os = "linux"))]

#[cfg(target_os = "macos")]
use riftri_storage::ApfsCloner as Native;
#[cfg(target_os = "linux")]
use riftri_storage::ReflinkCloner as Native;
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::PathBuf, process::Command};

fn exercise(phase: &str, test: &str) {
    const ROOT: &str = "RIFTRI_TEST_NATIVE_LOW_FD_ROOT";
    let mut relative = PathBuf::new();
    for _ in 0..128 {
        relative.push("n");
    }
    relative.push("leaf");
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let source = root.join("source");
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            // SAFETY: writable rlimit in the isolated child, never the parent suite.
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit.rlim_cur = limit.rlim_cur.min(64);
        // SAFETY: initialized rlimit, lowering only this child's soft limit.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        match phase {
            "clone" => {
                let view = root.join("view");
                Native::clone_tree_owner_writable(&source, &view).unwrap();
                assert_eq!(fs::read(view.join(&relative)).unwrap(), b"base");
                fs::write(view.join(&relative), b"private").unwrap();
                assert_eq!(fs::read(source.join(&relative)).unwrap(), b"base");
            }
            "readonly" => {
                Native::make_tree_read_only(&source).unwrap();
                assert_eq!(
                    fs::metadata(source.join(&relative))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o222,
                    0
                );
            }
            "writable" => {
                Native::make_tree_owner_writable(&source).unwrap();
                fs::write(source.join(&relative), b"updated").unwrap();
            }
            _ => unreachable!(),
        }
        return;
    }
    let fixture = tempfile::tempdir().unwrap();
    #[cfg(target_os = "linux")]
    if phase == "clone"
        && Native::probe(fixture.path()).status != riftri_storage::CapabilityStatus::Supported
    {
        assert_ne!(
            std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "required reflink volume unavailable"
        );
        return;
    }
    let source = fixture.path().join("source");
    fs::create_dir_all(source.join(relative.parent().unwrap())).unwrap();
    fs::write(source.join(&relative), b"base").unwrap();
    if phase != "readonly" {
        Native::make_tree_read_only(&source).unwrap();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(ROOT, fixture.path())
        .output()
        .unwrap();
    // Cleanup runs with the parent's normal descriptor limit even on failure.
    Native::make_tree_owner_writable(&source).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn deep_clone_bounds_directory_handles() {
    exercise("clone", "deep_clone_bounds_directory_handles");
}
#[test]
fn deep_readonly_walk_bounds_directory_handles() {
    exercise("readonly", "deep_readonly_walk_bounds_directory_handles");
}
#[test]
fn deep_writable_walk_bounds_directory_handles() {
    exercise("writable", "deep_writable_walk_bounds_directory_handles");
}
