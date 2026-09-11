#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use riftri_storage::{CapabilityStatus, OverlayFsMounter, ReflinkCloner};

mod support;
use support::writable_tempdir as tempdir;

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn require_reflink(path: &Path) -> bool {
    let capability = ReflinkCloner::probe(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref(),
        Some(OsStr::new("1")),
        "Linux reflink test volume is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Linux reflink CLI test on this volume: {}",
        capability.explanation
    );
    false
}

fn require_overlayfs(path: &Path) -> bool {
    if std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref() != Some(OsStr::new("1")) {
        return false;
    }
    let capability = OverlayFsMounter::probe_current_namespace(path);
    assert_eq!(
        capability.status,
        CapabilityStatus::Supported,
        "Linux OverlayFS test namespace is required but unavailable: {}",
        capability.explanation
    );
    true
}

fn initialize_repository(repository: &Path) {
    fs::create_dir(repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    assert!(
        git(repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );
}

#[test]
fn explicit_and_transparent_commands_create_linux_reflink_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_reflink(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let explicit = fixture.path().join("explicit");
    let transparent = fixture.path().join("transparent");
    initialize_repository(&repository);

    let explicit_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&explicit)
        .args(["-b", "feature/linux-explicit", "HEAD"])
        .current_dir(&repository)
        .output()
        .expect("run explicit Riftri add");
    assert!(
        explicit_output.status.success(),
        "explicit add failed: {}",
        String::from_utf8_lossy(&explicit_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit_output.stdout)
            .contains("Created Linux reflink-backed Git worktree")
    );
    assert!(
        git(&explicit, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    let enable_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("enable")
        .current_dir(&repository)
        .output()
        .expect("enable repository");
    assert!(enable_output.status.success());

    let transparent_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/linux-transparent",
        ])
        .arg(&transparent)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("run transparent Riftri add");
    assert!(
        transparent_output.status.success(),
        "transparent add failed: {}",
        String::from_utf8_lossy(&transparent_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&transparent_output.stderr)
            .contains("optimized Linux reflink worktree")
    );
    assert!(
        git(&transparent, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn explicit_and_transparent_commands_manage_linux_overlayfs_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_overlayfs(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let explicit = fixture.path().join("explicit");
    let transparent = fixture.path().join("transparent");
    initialize_repository(&repository);

    let explicit_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&explicit)
        .args(["-b", "feature/overlay-explicit", "HEAD"])
        .current_dir(&repository)
        .output()
        .expect("run explicit OverlayFS add");
    assert!(
        explicit_output.status.success(),
        "explicit OverlayFS add failed: {}",
        String::from_utf8_lossy(&explicit_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit_output.stdout)
            .contains("Created OverlayFS-backed Git worktree")
    );
    assert!(
        git(&explicit, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    assert!(
        Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("enable")
            .current_dir(&repository)
            .status()
            .expect("enable repository")
            .success()
    );
    let transparent_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/overlay-transparent",
        ])
        .arg(&transparent)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("run transparent OverlayFS add");
    assert!(
        transparent_output.status.success(),
        "transparent OverlayFS add failed: {}",
        String::from_utf8_lossy(&transparent_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&transparent_output.stderr)
            .contains("optimized OverlayFS worktree")
    );
    assert!(
        git(&transparent, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    let transparent_remove = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&transparent)
        .current_dir(&repository)
        .output()
        .expect("run transparent OverlayFS removal");
    assert!(
        transparent_remove.status.success(),
        "transparent OverlayFS removal failed: {}",
        String::from_utf8_lossy(&transparent_remove.stderr)
    );
    let explicit_remove = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "remove"])
        .arg(&explicit)
        .current_dir(&repository)
        .output()
        .expect("run explicit OverlayFS removal");
    assert!(
        explicit_remove.status.success(),
        "explicit OverlayFS removal failed: {}",
        String::from_utf8_lossy(&explicit_remove.stderr)
    );
    assert!(!explicit.exists());
    assert!(!transparent.exists());
}
