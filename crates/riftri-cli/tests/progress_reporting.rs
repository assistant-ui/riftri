//! Lifecycle progress lines report real state transitions on stderr without
//! corrupting the machine-readable stdout and stderr contracts.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod support;
use support::writable_tempdir as tempdir;

fn native_cow_supported(path: &Path) -> bool {
    riftri_storage::probe_backends(path).iter().any(|backend| {
        matches!(
            backend.kind,
            riftri_storage::BackendKind::ApfsClone | riftri_storage::BackendKind::Reflink
        ) && backend.status == riftri_storage::CapabilityStatus::Supported
    })
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run fixture git command")
}

fn init_repository(repository: &Path) {
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

fn riftri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
}

fn progress_lines(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.starts_with("riftri: "))
        .map(str::to_owned)
        .collect()
}

fn assert_plain_lines(stderr: &[u8]) {
    let text = String::from_utf8_lossy(stderr);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\r'),
        "progress used control sequences: {text:?}"
    );
}

#[test]
fn cold_then_cached_adds_report_real_phase_transitions() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let cold = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view-cold"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run cold worktree add");
    assert!(
        cold.status.success(),
        "cold add failed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert_plain_lines(&cold.stderr);
    let lines = progress_lines(&cold.stderr);
    for expected in [
        "riftri: worktree-add: intent-recorded",
        "riftri: worktree-add: git-metadata-created",
        "riftri: worktree-add: materializing new immutable base",
        "riftri: worktree-add: base-ready",
        "riftri: worktree-add: index-synchronized",
        "riftri: worktree-add: active",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "missing {expected:?} in {lines:?}"
        );
    }
    // Progress is emitted per durable transition, never per file or on a
    // timer, so a single add stays within a small fixed number of lines.
    assert!(
        lines.len() <= 12,
        "progress output is not bounded: {lines:?}"
    );

    let cached = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view-cached"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run cached worktree add");
    assert!(
        cached.status.success(),
        "cached add failed: {}",
        String::from_utf8_lossy(&cached.stderr)
    );
    let lines = progress_lines(&cached.stderr);
    assert!(
        lines
            .iter()
            .any(|line| line == "riftri: worktree-add: immutable base cached; reusing it"),
        "cached add never reported base reuse: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("materializing new immutable base")),
        "cached add claimed to materialize a base: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "riftri: worktree-add: active"),
        "cached add never reported completion: {lines:?}"
    );
}

#[test]
fn json_stdout_stays_one_valid_report_while_progress_uses_stderr() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let output = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--json", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run worktree add --json");
    assert!(
        output.status.success(),
        "add --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON report");
    assert_eq!(report["schema_version"], 1);
    assert!(
        !progress_lines(&output.stderr).is_empty(),
        "progress never reached stderr during --json"
    );
}

#[test]
fn no_progress_flag_suppresses_progress_lines() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let output = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--no-progress", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run worktree add --no-progress");
    assert!(
        output.status.success(),
        "add --no-progress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "suppressed run still wrote stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn json_errors_receives_one_receipt_with_no_progress_lines() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    // Fail mid-operation, after several journal phases would already have
    // reported progress, and require stderr to still be one JSON receipt.
    let wrapper = fixture.path().join("failing-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = checkout-index ]; then exit 65; fi\ndone\nexec git \"$@\"\n",
    )
    .expect("write failing git wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("mark executable");

    let output = riftri()
        .arg("--json-errors")
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .output()
        .expect("run failing add with --json-errors");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr is exactly one JSON receipt");
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["operation"], "worktree-add");

    // A successful command with --json-errors writes nothing to stderr.
    let quiet = riftri()
        .arg("--json-errors")
        .args(["repair", "--state-dir"])
        .arg(&state)
        .arg(&repository)
        .output()
        .expect("run repair with --json-errors");
    assert!(
        quiet.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert!(
        quiet.stderr.is_empty(),
        "--json-errors run wrote progress to stderr: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
}

#[test]
fn failed_add_reports_rollback_progress() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let wrapper = fixture.path().join("failing-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = checkout-index ]; then exit 65; fi\ndone\nexec git \"$@\"\n",
    )
    .expect("write failing git wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("mark executable");

    let output = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .output()
        .expect("run failing add");
    assert_eq!(output.status.code(), Some(1));
    let lines = progress_lines(&output.stderr);
    for expected in [
        "riftri: worktree-add: git-metadata-created",
        "riftri: worktree-add: materializing new immutable base",
        "riftri: worktree-add: rollback-pending",
        "riftri: worktree-add: rolled-back",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "missing {expected:?} in {lines:?}"
        );
    }
}

#[test]
fn repair_reports_scanning_and_recovery_of_an_interrupted_add() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let ready = fixture.path().join("ready");
    let release = fixture.path().join("release");
    init_repository(&repository);

    // Pause at checkout-index, then exit without materializing anything once
    // released, so the interrupted add leaves a mid-flight journal behind and
    // no stray Git child races the subsequent repair.
    let wrapper = fixture.path().join("paused-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = checkout-index ]; then\n  touch \"$RIFTRI_TEST_READY\"\n  cd / || exit 1\n  attempt=0\n  while [ ! -e \"$RIFTRI_TEST_RELEASE\" ] && [ \"$attempt\" -lt 600 ]; do sleep 0.05; attempt=$((attempt + 1)); done\n  exit 70\n fi\ndone\nexec git \"$@\"\n",
    )
    .expect("write paused git wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("mark executable");

    // Armed before the pause can begin: the assertion below must not be able
    // to leave `paused-git` spinning after the test binary exits.
    let paused = support::PausedGit::new(&release);
    let mut add = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .env("RIFTRI_TEST_READY", &ready)
        .env("RIFTRI_TEST_RELEASE", &release)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start pausable add");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() && Instant::now() < deadline && add.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "add never reached its pause point");
    add.kill().expect("interrupt the paused add");
    add.wait().expect("collect the interrupted add");
    paused.release();

    let repair = riftri()
        .arg("repair")
        .arg(&repository)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("run repair");
    assert!(
        repair.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    let lines = progress_lines(&repair.stderr);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("riftri: repair: scanned ")),
        "repair never reported its scan: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("riftri: repair: recovering add operation ")),
        "repair never reported the recovered add: {lines:?}"
    );
}

#[test]
fn contended_base_lock_reports_wait_and_resume() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let first = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view-first"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run first add");
    assert!(
        first.status.success(),
        "first add failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let lock_path = find_base_lock(&state.join("bases/v1")).expect("locate immutable-base lock");
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open immutable-base lock");
    fs2::FileExt::lock_exclusive(&held).expect("hold immutable-base lock");

    let mut second = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view-second"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start contended add");
    let mut stderr = second.stderr.take().expect("piped stderr");
    let collected = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&collected);
    let reader = std::thread::spawn(move || {
        let mut buffer = [0_u8; 256];
        loop {
            match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => sink.lock().unwrap().extend_from_slice(&buffer[..count]),
            }
        }
    });

    let waiting_line = "riftri: waiting: read-lock immutable base (held by another process)";
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_wait = false;
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&collected.lock().unwrap())
            .lines()
            .any(|line| line == waiting_line)
        {
            saw_wait = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !saw_wait {
        let _ = second.kill();
    }
    drop(held);
    let status = second.wait().expect("collect contended add");
    reader.join().expect("join stderr reader");
    let stderr = collected.lock().unwrap().clone();
    assert!(
        saw_wait,
        "contended add never reported the lock wait: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        status.success(),
        "contended add failed after the lock was released: {}",
        String::from_utf8_lossy(&stderr)
    );
    let lines = progress_lines(&stderr);
    assert!(
        lines
            .iter()
            .any(|line| line == "riftri: resumed: read-lock immutable base"),
        "contended add never reported resuming: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "riftri: worktree-add: active"),
        "contended add never reported completion: {lines:?}"
    );
}

fn find_base_lock(base_root: &Path) -> Option<PathBuf> {
    let mut pending = vec![base_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "lock")
            {
                return Some(path);
            }
        }
    }
    None
}

#[test]
fn garbage_collection_reports_plan_and_phase_transitions() {
    let fixture = tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository(&repository);

    let added = riftri()
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run worktree add");
    assert!(
        added.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let removed = riftri()
        .args(["worktree", "remove"])
        .arg(fixture.path().join("view"))
        .args(["--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run worktree remove");
    assert!(
        removed.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );

    let plan = riftri()
        .args(["gc", "--json", "--state-dir"])
        .arg(&state)
        .arg(&repository)
        .output()
        .expect("run gc plan");
    assert!(
        plan.status.success(),
        "gc plan failed: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&plan.stdout).expect("gc plan stdout is exactly one JSON report");
    assert_eq!(report["applied"], false);
    assert!(
        progress_lines(&plan.stderr)
            .iter()
            .any(|line| line == "riftri: garbage-collection: 1 candidate base(s)"),
        "gc plan never reported its candidates: {}",
        String::from_utf8_lossy(&plan.stderr)
    );

    let apply = riftri()
        .args(["gc", "--apply", "--yes", "--state-dir"])
        .arg(&state)
        .arg(&repository)
        .output()
        .expect("run gc apply");
    assert!(
        apply.status.success(),
        "gc apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let lines = progress_lines(&apply.stderr);
    for expected in [
        "riftri: garbage-collection: intent-recorded",
        "riftri: garbage-collection: marker-removed",
        "riftri: garbage-collection: base-quarantined",
        "riftri: garbage-collection: complete",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "missing {expected:?} in {lines:?}"
        );
    }
}
