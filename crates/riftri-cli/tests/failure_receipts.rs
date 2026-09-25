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

    // Policy refusal: exit 3, mirroring the receipt's category.
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON receipt");
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["operation"], "worktree-add");
    // Standing outside a repository is the caller's mistake, not an
    // operational failure: nothing was attempted, retrying cannot help, and
    // there is no command Riftri could suggest that would change the answer.
    assert_eq!(receipt["code"], "not-a-repository");
    assert_eq!(receipt["category"], "policy");
    assert_eq!(receipt["cleanup"], "not-needed");
    assert_eq!(receipt["recovery"], "not-required");
    assert_eq!(receipt["nextCommand"], serde_json::Value::Null);
    assert_eq!(
        receipt["repository"],
        outside_repository.path().display().to_string()
    );
}

/// A usage error is a failure, so `--json-errors` must report it as one JSON
/// receipt on stderr rather than clap's human-readable text. The receipt keeps
/// clap's usage exit code (2) so automation still tells a usage error apart
/// from an operational command failure (1).
#[test]
fn json_errors_report_usage_errors_as_one_receipt_with_exit_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("--json-errors")
        .args(["worktree", "add"])
        .output()
        .expect("run Riftri with a missing required argument");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON receipt");
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["code"], "usage-error");
    assert_eq!(receipt["category"], "usage");
    assert_eq!(receipt["cleanup"], "not-needed");
    assert_eq!(receipt["recovery"], "not-required");
    assert!(receipt["operation"].is_null());
    assert!(receipt["phase"].is_null());
    assert!(receipt["nextCommand"].is_null());
    assert!(
        receipt["message"]
            .as_str()
            .expect("message")
            .contains("required arguments were not provided"),
        "the receipt carries clap's diagnostic: {}",
        receipt["message"]
    );
}

/// Without `--json-errors`, the very same usage error keeps clap's exact
/// human-readable behavior: plain text on stderr, exit 2, and not JSON.
#[test]
fn usage_errors_without_json_errors_stay_plain_clap_text() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .output()
        .expect("run Riftri with a missing required argument");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("required arguments were not provided"),
        "clap's text is preserved: {stderr}"
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).is_err(),
        "unflagged usage errors must not become JSON: {stderr}"
    );
}

/// `--help` and `--version` route through clap's error path with a success exit
/// too, but they are not failures: `--json-errors` must leave them untouched.
#[test]
fn json_errors_leave_help_and_version_untouched() {
    for flag in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("--json-errors")
            .arg(flag)
            .output()
            .unwrap_or_else(|error| panic!("run Riftri {flag}: {error}"));

        assert_eq!(output.status.code(), Some(0), "{flag} exits successfully");
        assert!(
            output.stderr.is_empty(),
            "{flag} prints to stdout, not stderr"
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err(),
            "{flag} must not become a JSON receipt"
        );
    }
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
    let fixture = support::writable_tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    init_repository_with_commit(&repository);
    write_pending_prune_journal(&repository, &state);

    let (receipt, exit_code) = riftri_json_error(
        &repository,
        &["worktree", "prune", "--state-dir", state.to_str().unwrap()],
    );
    assert_eq!(exit_code, Some(1));
    assert_eq!(receipt["operation"], "worktree-prune");
    assert_pending_recovery_receipt(&receipt, &state);
}

/// The regression that matters: take the `nextCommand` a receipt advertises,
/// run it through a real shell from an unrelated working directory, and prove
/// it reaches the pending journal.
///
/// A repository and a state directory under paths containing a space and a
/// quote are exactly what an unquoted, `Path::display`-rendered suggestion
/// breaks: the shell splits the path, repair inspects a directory that does
/// not exist, and a missing directory used to report a confident all-clear.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_suggested_command_recovers_state_under_paths_with_spaces_and_quotes() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let awkward = fixture.path().join("My Projects and 'quotes'");
    let repository = awkward.join("app");
    let state = awkward.join("state dir");
    std::fs::create_dir(&awkward).expect("create awkward parent directory");
    init_repository_with_commit(&repository);
    write_pending_prune_journal(&repository, &state);

    let (receipt, exit_code) = riftri_json_error(
        &repository,
        &["worktree", "prune", "--state-dir", state.to_str().unwrap()],
    );
    assert_eq!(exit_code, Some(1));
    assert_pending_recovery_receipt(&receipt, &state);

    // Run the advertised command verbatim, only substituting the binary under
    // test for `riftri`, and from the fixture root rather than the repository
    // so nothing but the command itself can select the right state.
    let next_command = receipt["nextCommand"].as_str().expect("next command");
    let program = next_command
        .strip_prefix("riftri ")
        .expect("nextCommand invokes riftri");
    let quoted_binary =
        riftri_core::shell_quoted_path(std::path::Path::new(env!("CARGO_BIN_EXE_riftri")))
            .expect("the test binary path is representable");
    let repair = Command::new("sh")
        .arg("-c")
        .arg(format!("{quoted_binary} {program}"))
        .current_dir(fixture.path())
        .output()
        .expect("run the advertised recovery command");

    let stdout = String::from_utf8_lossy(&repair.stdout);
    assert!(
        repair.status.success(),
        "advertised command failed: {}\n{stdout}",
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(
        stdout.contains(&format!(
            "State: {}",
            receipt["stateDirectory"].as_str().expect("state directory")
        )),
        "repair inspected a different state directory: {stdout}"
    );
    assert!(
        stdout.contains("Scanned operations: 1"),
        "repair found no journal, which is the false all-clear: {stdout}"
    );
    assert!(
        stdout.contains("Recovered prunes: 1"),
        "repair did not resume the pending prune: {stdout}"
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.join("prunes/prune-1.json")).expect("read prune journal"),
    )
    .expect("parse prune journal");
    assert_eq!(journal["phase"], "complete");
}

/// A `--state-dir` the caller named explicitly must exist. Answering "nothing
/// needs attention" for a directory Riftri never found is how a mistyped or
/// shell-split path hides a real pending journal.
#[test]
fn an_explicitly_named_missing_state_directory_never_reports_an_all_clear() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let missing = fixture.path().join("absent state");

    for subcommand in [
        vec!["repair"],
        vec!["status"],
        vec!["gc"],
        vec!["worktree", "list"],
    ] {
        let mut arguments = subcommand.clone();
        arguments.push("--state-dir");
        arguments.push(missing.to_str().expect("utf-8 path"));
        let (receipt, exit_code) = riftri_json_error(fixture.path(), &arguments);

        assert_eq!(exit_code, Some(3), "{subcommand:?} must refuse");
        assert_eq!(receipt["code"], "invalid-request");
        let message = receipt["message"].as_str().expect("message");
        assert!(message.contains("does not exist"), "{message}");
        assert!(message.contains("not an all-clear"), "{message}");
        assert_eq!(
            receipt["stateDirectory"],
            missing.display().to_string(),
            "the refused directory stays machine-readable"
        );
    }
}

/// The legitimate case must keep working: a repository that has simply never
/// created Riftri state reports an all-clear and exits zero.
#[test]
fn a_repository_without_any_riftri_state_still_reports_an_all_clear() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    std::fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);

    let repair = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("repair")
        .current_dir(&repository)
        .output()
        .expect("run riftri repair");

    assert!(
        repair.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(
        String::from_utf8_lossy(&repair.stdout)
            .contains("No journaled operation needs manual attention")
    );
}

/// A durable prune journal frozen at intent-recorded, exactly as an
/// interrupted prune would leave it. No copy-on-write support is needed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_pending_prune_journal(repository: &std::path::Path, state: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;

    let prunes = state.join("prunes");
    std::fs::create_dir_all(&prunes).expect("create prune journal directory");
    let journal = serde_json::json!({
        "format_version": 1,
        "operation_id": "prune-1",
        "repository": {
            "encoding": "unix-bytes",
            "units": std::fs::canonicalize(repository)
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
}

/// The receipt must direct the caller to repair, and its `nextCommand` must be
/// the exact command the human-readable message names, with the state
/// directory holding the pending journal quoted as one shell argument.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn assert_pending_recovery_receipt(receipt: &serde_json::Value, state: &std::path::Path) {
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["code"], "recovery-pending");
    assert_eq!(receipt["category"], "operational");
    assert_eq!(receipt["cleanup"], "not-needed");
    assert_eq!(receipt["recovery"], "required");
    let next_command = receipt["nextCommand"].as_str().expect("next command");
    // Automation never has to parse the shell string: the directory is a
    // field. The command is that same directory, quoted.
    let reported = receipt["stateDirectory"].as_str().expect("state directory");
    assert_eq!(
        next_command,
        riftri_core::repair_command(std::path::Path::new(reported))
            .expect("the fixture path is representable")
    );
    assert_eq!(
        std::fs::canonicalize(reported).expect("receipt state directory exists"),
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

/// Every command that inspects a repository must classify "there is no
/// repository here" the same way. They did not: the worktree commands wrapped
/// the cause in a `WorktreeError`, while `status`, `gc`, and `repair` wrapped
/// it in an `ActivationError` that never reached the receipt mapping and fell
/// through to an operational default with an unknown cleanup. A harness
/// following that receipt retries a command whose answer cannot change.
#[test]
fn an_absent_repository_is_a_policy_failure_for_every_command() {
    let outside_repository = tempfile::tempdir().expect("temporary non-repository");
    let moved = outside_repository.path().join("moved");
    let view = outside_repository.path().join("view");
    let view = view.to_str().expect("UTF-8 fixture path");
    let moved = moved.to_str().expect("UTF-8 fixture path");

    for arguments in [
        &["status"][..],
        &["gc"][..],
        &["repair"][..],
        &["worktree", "list"][..],
        &["worktree", "prune"][..],
        &["worktree", "add", view, "-b", "feature/absent"][..],
        &["worktree", "remove", view][..],
        &["worktree", "compact", view][..],
        &["worktree", "move", view, moved][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("--json-errors")
            .args(arguments)
            .current_dir(outside_repository.path())
            .output()
            .expect("run Riftri with JSON failures enabled");
        // `custom-harness.md` tells a runner to branch on the exit code before
        // parsing anything, so a policy receipt delivered with exit 1 would
        // still be retried. The code and the category must agree.
        assert_eq!(output.status.code(), Some(3), "{arguments:?}");
        let receipt: serde_json::Value = serde_json::from_slice(&output.stderr)
            .unwrap_or_else(|error| panic!("{arguments:?} receipt is not JSON: {error}"));
        assert_eq!(receipt["code"], "not-a-repository", "{arguments:?}");
        assert_eq!(receipt["category"], "policy", "{arguments:?}");
        assert_eq!(receipt["cleanup"], "not-needed", "{arguments:?}");
        assert_eq!(receipt["recovery"], "not-required", "{arguments:?}");
    }
}

/// The counterpart: a repository Git accepts, where a Git command fails for a
/// real reason, must stay operational. Only Git's own "not a git repository"
/// verdict becomes a policy refusal — a damaged object store is still
/// something worth inspecting.
#[test]
fn a_git_failure_inside_a_real_repository_stays_operational() {
    let fixture = tempfile::tempdir().expect("temporary repository");
    let repository = fixture.path().join("repository");
    std::fs::create_dir_all(&repository).expect("create repository directory");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "initial"][..],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(&repository)
                .status()
                .expect("run git")
                .success(),
            "git {arguments:?} failed"
        );
    }
    // Git still discovers the repository; its objects are simply gone.
    for entry in std::fs::read_dir(repository.join(".git/objects")).expect("read object store") {
        let entry = entry.expect("read object store entry");
        if entry.file_type().expect("stat object entry").is_dir() {
            std::fs::remove_dir_all(entry.path()).expect("gut the object store");
        }
    }

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("--json-errors")
        .args(["worktree", "add"])
        .arg(fixture.path().join("view"))
        .args(["-b", "feature/damaged"])
        .current_dir(&repository)
        .output()
        .expect("run Riftri with JSON failures enabled");

    assert_eq!(output.status.code(), Some(1));
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON receipt");
    assert_eq!(receipt["code"], "git-failed");
    assert_eq!(receipt["category"], "operational");
}
