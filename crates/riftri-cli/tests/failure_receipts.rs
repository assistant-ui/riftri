use std::process::Command;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod support;

#[test]
fn json_errors_emit_one_parseable_lifecycle_receipt_on_stderr() {
    let outside_repository = tempfile::tempdir().expect("temporary non-repository");
    let destination = outside_repository.path().join("view");
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("--json-errors")
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/receipt", "--repository"])
        .arg(outside_repository.path())
        .output()
        .expect("run Riftri with JSON failures enabled");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON receipt");
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["operation"], "worktree-add");
    assert_eq!(receipt["code"], "git-failed");
    assert_eq!(receipt["category"], "operational");
    assert_eq!(receipt["cleanup"], "unknown");
    assert_eq!(receipt["recovery"], "inspect");
    assert_eq!(receipt["nextCommand"], "riftri status");
}

#[test]
fn policy_refusals_still_report_no_recovery() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let plain = repository.join("plain");
    std::fs::create_dir(&repository).expect("create repository");
    std::fs::create_dir(&state).expect("create state directory");
    git(&repository, &["init", "--quiet"]);
    std::fs::create_dir(&plain).expect("create unmanaged directory");

    let (receipt, exit_code) = riftri_json_error(
        &repository,
        &[
            "worktree",
            "remove",
            plain.to_str().expect("utf-8 path"),
            "--state-dir",
            state.to_str().expect("utf-8 path"),
        ],
    );

    assert_eq!(exit_code, Some(3));
    assert_eq!(receipt["operation"], "worktree-remove");
    assert_eq!(receipt["code"], "invalid-request");
    assert_eq!(receipt["category"], "policy");
    assert_eq!(receipt["cleanup"], "not-needed");
    assert_eq!(receipt["recovery"], "not-required");
    assert!(receipt["nextCommand"].is_null());
    assert!(
        receipt["message"]
            .as_str()
            .expect("message")
            .contains("is not an active Riftri-managed worktree")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pending_move_receipts_require_repair_with_the_state_directory() {
    let fixture = support::writable_tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let source = fixture.path().join("mover");
    let destination = fixture.path().join("moved");
    init_repository_with_commit(&repository);
    add_managed_worktree(&repository, &source, &state);

    // A move that Git rejects mid-flight leaves a durable pending move
    // journal, exactly as an interrupted process would.
    git(&repository, &["worktree", "lock", source.to_str().unwrap()]);
    let (_, exit_code) = riftri_json_error(
        &repository,
        &[
            "worktree",
            "move",
            source.to_str().unwrap(),
            destination.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ],
    );
    assert_ne!(exit_code, Some(0));
    git(
        &repository,
        &["worktree", "unlock", source.to_str().unwrap()],
    );

    // Every lifecycle command blocked by the pending move must report the
    // recovery obligation, not a generic policy refusal (guards from #178).
    let move_again = [
        "worktree",
        "move",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
    ];
    let compact = [
        "worktree",
        "compact",
        source.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
    ];
    let remove = [
        "worktree",
        "remove",
        source.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
    ];
    let prune = ["worktree", "prune", "--state-dir", state.to_str().unwrap()];
    for (operation, arguments) in [
        ("worktree-move", move_again.as_slice()),
        ("worktree-compact", compact.as_slice()),
        ("worktree-remove", remove.as_slice()),
        ("worktree-prune", prune.as_slice()),
    ] {
        let (receipt, exit_code) = riftri_json_error(&repository, arguments);
        assert_eq!(exit_code, Some(1), "{operation} exits as operational");
        assert_eq!(receipt["operation"], operation);
        assert_pending_recovery_receipt(&receipt, &state);
    }
    assert!(source.join("tracked.txt").is_file(), "source is preserved");

    // The advertised nextCommand actually clears the pending state.
    let repair = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["repair", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run riftri repair");
    assert!(
        repair.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(destination.join("tracked.txt").is_file(), "move completed");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pending_removal_receipts_require_repair_with_the_state_directory() {
    let fixture = support::writable_tempdir().expect("fixture directory");
    if !native_cow_supported(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("goner");
    init_repository_with_commit(&repository);
    add_managed_worktree(&repository, &worktree, &state);

    // Git refuses to remove a locked worktree, stranding the removal journal
    // after its intent was durably recorded.
    git(
        &repository,
        &["worktree", "lock", worktree.to_str().unwrap()],
    );
    let remove = [
        "worktree",
        "remove",
        worktree.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
    ];
    let (_, exit_code) = riftri_json_error(&repository, &remove);
    assert_ne!(exit_code, Some(0));
    git(
        &repository,
        &["worktree", "unlock", worktree.to_str().unwrap()],
    );

    let (receipt, exit_code) = riftri_json_error(&repository, &remove);
    assert_eq!(exit_code, Some(1));
    assert_eq!(receipt["operation"], "worktree-remove");
    assert_pending_recovery_receipt(&receipt, &state);
    assert!(
        receipt["message"]
            .as_str()
            .expect("message")
            .contains("a removal of")
    );

    let repair = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["repair", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run riftri repair");
    assert!(
        repair.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(!worktree.exists(), "repair completed the pending removal");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pending_prune_journal_blocks_prune_with_a_recovery_receipt() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = support::writable_tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository_with_commit(&repository);

    // A durable prune journal frozen at intent-recorded, exactly as an
    // interrupted prune would leave it. No copy-on-write support is needed.
    let prunes = state.join("prunes");
    std::fs::create_dir_all(&prunes).expect("create prune journal directory");
    let journal = serde_json::json!({
        "format_version": 1,
        "operation_id": "prune-1",
        "repository": {
            "encoding": "unix-bytes",
            "units": std::fs::canonicalize(&repository)
                .expect("canonical repository")
                .as_os_str()
                .as_bytes(),
        },
        "phase": "intent-recorded",
    });
    std::fs::write(
        prunes.join("prune-1.json"),
        serde_json::to_string_pretty(&journal).expect("encode prune journal"),
    )
    .expect("write prune journal");

    let (receipt, exit_code) = riftri_json_error(
        &repository,
        &["worktree", "prune", "--state-dir", state.to_str().unwrap()],
    );
    assert_eq!(exit_code, Some(1));
    assert_eq!(receipt["operation"], "worktree-prune");
    assert_pending_recovery_receipt(&receipt, &state);
}

/// The receipt must direct the caller to repair, and its `nextCommand` must
/// quote the exact command the human-readable message names, including the
/// state directory that holds the pending journal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn assert_pending_recovery_receipt(receipt: &serde_json::Value, state: &std::path::Path) {
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["code"], "recovery-pending");
    assert_eq!(receipt["category"], "operational");
    assert_eq!(receipt["cleanup"], "not-needed");
    assert_eq!(receipt["recovery"], "required");
    let next_command = receipt["nextCommand"].as_str().expect("next command");
    let state_directory = next_command
        .strip_prefix("riftri repair --state-dir ")
        .expect("nextCommand names the repair command with a state directory");
    assert_eq!(
        std::fs::canonicalize(state_directory).expect("receipt state directory exists"),
        std::fs::canonicalize(state).expect("fixture state directory exists")
    );
    assert!(
        receipt["message"]
            .as_str()
            .expect("message")
            .contains(next_command),
        "human guidance must name the same command as nextCommand"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn native_cow_supported(probe_path: &std::path::Path) -> bool {
    riftri_storage::probe_backends(probe_path)
        .iter()
        .any(|backend| {
            matches!(
                backend.kind,
                riftri_storage::BackendKind::ApfsClone | riftri_storage::BackendKind::Reflink
            ) && backend.status == riftri_storage::CapabilityStatus::Supported
        })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn init_repository_with_commit(repository: &std::path::Path) {
    std::fs::create_dir(repository).expect("create repository");
    git(repository, &["init", "--quiet"]);
    git(repository, &["config", "user.name", "Riftri Tests"]);
    git(
        repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(repository, &["config", "core.autocrlf", "false"]);
    std::fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    git(repository, &["add", "--", "tracked.txt"]);
    git(repository, &["commit", "--quiet", "-m", "initial"]);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn add_managed_worktree(
    repository: &std::path::Path,
    destination: &std::path::Path,
    state: &std::path::Path,
) {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(destination)
        .args(["--detach", "HEAD", "--state-dir"])
        .arg(state)
        .current_dir(repository)
        .output()
        .expect("run riftri worktree add");
    assert!(
        output.status.success(),
        "worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(repository: &std::path::Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .expect("run git");
    assert!(status.success(), "git {arguments:?} failed");
}

fn riftri_json_error(
    current_directory: &std::path::Path,
    arguments: &[&str],
) -> (serde_json::Value, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("--json-errors")
        .args(arguments)
        .current_dir(current_directory)
        .output()
        .expect("run Riftri with JSON failures enabled");
    assert!(
        !output.status.success(),
        "expected {arguments:?} to fail, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let receipt = serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr is one JSON receipt for {arguments:?}: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (receipt, output.status.code())
}
