#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod support;

#[test]
fn repair_does_not_roll_back_a_live_add() {
    let fixture = support::writable_tempdir().unwrap();
    // Ordinary Linux runners need not provide a native COW filesystem.
    if !riftri_storage::probe_backends(fixture.path())
        .iter()
        .any(|backend| {
            matches!(
                backend.kind,
                riftri_storage::BackendKind::ApfsClone | riftri_storage::BackendKind::Reflink
            ) && backend.status == riftri_storage::CapabilityStatus::Supported
        })
    {
        return;
    }
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("view");
    let state = fixture.path().join("state");
    let ready = fixture.path().join("ready");
    let release = fixture.path().join("release");
    fs::create_dir(&repository).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "--quiet", "-m", "fixture"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    let wrapper = fixture.path().join("paused-git");
    fs::write(&wrapper, "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = checkout-index ]; then\n  touch \"$RIFTRI_TEST_READY\"\n  attempt=0\n  while [ ! -e \"$RIFTRI_TEST_RELEASE\" ] && [ \"$attempt\" -lt 600 ]; do sleep 0.05; attempt=$((attempt + 1)); done\n fi\ndone\nexec git \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let mut add = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .env("RIFTRI_TEST_READY", &ready)
        .env("RIFTRI_TEST_RELEASE", &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() && Instant::now() < deadline && add.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        fs::write(&release, "release").unwrap();
        let output = add.wait_with_output().unwrap();
        panic!(
            "add did not reach pause: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut repairs = Vec::new();
    for _ in 0..2 {
        repairs.push(
            Command::new(env!("CARGO_BIN_EXE_riftri"))
                .arg("repair")
                .arg(&repository)
                .arg("--state-dir")
                .arg(&state)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let outputs = repairs
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    let preserved = destination.join(".git").is_file();
    fs::write(&release, "release").unwrap();
    let output = add.wait_with_output().unwrap();
    assert!(preserved, "repair deleted a live add's worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for output in outputs {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Busy add operations skipped: 1"));
    }
    let clean = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(&destination)
        .output()
        .unwrap();
    assert!(clean.status.success() && clean.stdout.is_empty());
    let repaired = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("repair")
        .arg(&repository)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .unwrap();
    assert!(repaired.status.success());
    assert!(String::from_utf8_lossy(&repaired.stdout).contains("Active worktrees: 1"));
}
