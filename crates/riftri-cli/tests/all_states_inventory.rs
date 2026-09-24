//! Cross-platform coverage for the all-registered-states worktree inventory.
//!
//! These tests exercise state-directory discovery, deduplication, diagnostic
//! reporting, and default-scope backward compatibility without requiring a
//! copy-on-write capable volume.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run git")
}

fn riftri(current_directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(current_directory)
        .output()
        .expect("run Riftri CLI")
}

fn init_repository(path: &Path) {
    fs::create_dir_all(path).expect("create repository directory");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
    ] {
        assert!(
            git(path, arguments).status.success(),
            "git {arguments:?} failed"
        );
    }
}

fn register_state(repository: &Path, state: &Path) {
    let value = state.to_str().expect("UTF-8 state path");
    assert!(
        git(
            repository,
            &["config", "--add", "riftri.stateDirectory", value]
        )
        .status
        .success()
    );
}

fn parse_report(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse inventory JSON")
}

fn canonical_display(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonicalize fixture path")
        .display()
        .to_string()
}

#[test]
fn all_states_inventory_lists_default_and_registered_state_directories() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    let default_state = repository.join(".git").join("riftri");
    fs::create_dir(&default_state).expect("create default state directory");
    let custom_state = fixture.path().join("custom-state");
    fs::create_dir(&custom_state).expect("create custom state directory");
    // Duplicate registrations of an equivalent path must collapse to one entry.
    register_state(&repository, &custom_state);
    register_state(&repository, &custom_state);

    let output = riftri(&repository, &["worktree", "list", "--all-states", "--json"]);
    let report = parse_report(&output);

    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["scope"], "all-registered-states");
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["worktrees"], serde_json::json!([]));
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));

    let states = report["state_directories"]
        .as_array()
        .expect("state_directories array");
    assert_eq!(
        states.len(),
        2,
        "expected default plus one registered state"
    );
    assert_eq!(states[0]["path"], canonical_display(&default_state));
    assert_eq!(states[0]["source"], "default");
    assert_eq!(states[1]["path"], canonical_display(&custom_state));
    assert_eq!(states[1]["source"], "registered");
}

#[test]
fn all_states_inventory_reports_missing_and_malformed_registrations() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    let default_state = repository.join(".git").join("riftri");
    fs::create_dir(&default_state).expect("create default state directory");
    let missing_state = fixture.path().join("missing-state");
    register_state(&repository, &missing_state);
    register_state(&repository, &PathBuf::from("relative-state"));

    let output = riftri(&repository, &["worktree", "list", "--all-states", "--json"]);
    let report = parse_report(&output);

    assert_eq!(report["schema_version"], 2);
    let states = report["state_directories"]
        .as_array()
        .expect("state_directories array");
    assert_eq!(states.len(), 1, "only the default state remains usable");
    assert_eq!(states[0]["source"], "default");

    let issues = report["diagnostic_issues"]
        .as_array()
        .expect("diagnostic_issues array");
    assert_eq!(issues.len(), 2, "both bad registrations are reported");
    let relative = issues
        .iter()
        .find(|issue| issue["path"] == "relative-state")
        .expect("relative registration diagnostic");
    assert!(
        relative["reason"]
            .as_str()
            .expect("reason string")
            .contains("not absolute")
    );
    // Registration-level issues belong to no state directory, so the display
    // string and its native-hex twin are both present and explicitly null.
    assert!(relative["state_directory"].is_null());
    assert!(
        relative
            .as_object()
            .expect("diagnostic object")
            .contains_key("state_directory_native_hex")
    );
    assert!(relative["state_directory_native_hex"].is_null());
    let missing = issues
        .iter()
        .find(|issue| issue["path"] == missing_state.display().to_string())
        .expect("missing registration diagnostic");
    assert!(
        missing["reason"]
            .as_str()
            .expect("reason string")
            .contains("missing")
    );

    // Discovery is strictly read-only: both registrations must survive.
    let config = git(
        &repository,
        &["config", "--get-all", "riftri.stateDirectory"],
    );
    let values = String::from_utf8_lossy(&config.stdout).into_owned();
    assert!(values.contains("missing-state"));
    assert!(values.contains("relative-state"));
}

#[cfg(unix)]
#[test]
fn all_states_inventory_reports_symlinked_registrations_without_traversal() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    let real_state = fixture.path().join("real-state");
    fs::create_dir(&real_state).expect("create real state directory");
    let linked_state = fixture.path().join("linked-state");
    std::os::unix::fs::symlink(&real_state, &linked_state).expect("create state symlink");
    register_state(&repository, &linked_state);

    let output = riftri(&repository, &["worktree", "list", "--all-states", "--json"]);
    let report = parse_report(&output);

    assert_eq!(report["state_directories"], serde_json::json!([]));
    let issues = report["diagnostic_issues"]
        .as_array()
        .expect("diagnostic_issues array");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["path"], linked_state.display().to_string());
    assert!(
        issues[0]["reason"]
            .as_str()
            .expect("reason string")
            .contains("not a real directory")
    );
}

/// Every diagnostic entry's `state_directory` display string carries its
/// `state_directory_native_hex` sibling: the exact native bytes for a
/// state-level issue, and explicit null beside null for a registration-level
/// issue. The display string alone is lossy for non-UTF-8 paths, and this was
/// the only display path in the CLI without a hex partner.
#[cfg(unix)]
#[test]
fn all_states_diagnostic_issues_pair_state_directory_with_its_native_hex() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    // A registered state directory that is real but contains a symlinked
    // immutable-base parent produces state-level diagnostics.
    let custom_state = fixture.path().join("custom-state");
    fs::create_dir(&custom_state).expect("create custom state directory");
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    std::os::unix::fs::symlink(&outside, custom_state.join("bases"))
        .expect("redirect the immutable-base parent");
    register_state(&repository, &custom_state);
    // And one registration-level issue that no state directory owns.
    register_state(&repository, &PathBuf::from("relative-state"));

    let output = riftri(&repository, &["worktree", "list", "--all-states", "--json"]);
    let report = parse_report(&output);

    assert_eq!(report["schema_version"], 2, "additive keys keep schema v2");
    let issues = report["diagnostic_issues"]
        .as_array()
        .expect("diagnostic_issues array");
    let state_level = issues
        .iter()
        .find(|issue| issue["state_directory"].is_string())
        .expect("a state-level diagnostic for the symlinked base parent");
    let display = state_level["state_directory"]
        .as_str()
        .expect("state_directory display string");
    assert_eq!(display, canonical_display(&custom_state));
    let hex = state_level["state_directory_native_hex"]
        .as_str()
        .expect("state_directory_native_hex sibling");
    assert_eq!(
        decode_hex(hex),
        fs::canonicalize(&custom_state)
            .expect("canonicalize fixture state")
            .as_os_str()
            .as_bytes(),
        "the hex twin carries the exact native bytes of the same directory"
    );

    let registration = issues
        .iter()
        .find(|issue| issue["path"] == "relative-state")
        .expect("registration-level diagnostic");
    assert!(registration["state_directory"].is_null());
    assert!(registration["state_directory_native_hex"].is_null());

    // Both keys are siblings on every entry: never one without the other.
    for issue in issues {
        let issue = issue.as_object().expect("diagnostic object");
        assert!(issue.contains_key("state_directory"));
        assert!(issue.contains_key("state_directory_native_hex"));
        assert_eq!(
            issue["state_directory"].is_null(),
            issue["state_directory_native_hex"].is_null(),
            "display string and hex twin must be null together or set together"
        );
    }
}

#[cfg(unix)]
fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
        .collect()
}

#[test]
fn default_scope_ignores_registered_states_and_keeps_schema_version_one() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    let default_state = repository.join(".git").join("riftri");
    fs::create_dir(&default_state).expect("create default state directory");
    let custom_state = fixture.path().join("custom-state");
    fs::create_dir(&custom_state).expect("create custom state directory");
    register_state(&repository, &custom_state);

    let output = riftri(&repository, &["worktree", "list", "--json"]);
    let report = parse_report(&output);

    assert_eq!(report["schema_version"], 1);
    assert!(report["scope"].is_null());
    assert!(report["state_directories"].is_null());
    assert_eq!(report["worktrees"], serde_json::json!([]));
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));
    let state_directory = report["state_directory"].as_str().expect("state path");
    assert_eq!(
        fs::canonicalize(state_directory).expect("canonicalize reported state"),
        fs::canonicalize(&default_state).expect("canonicalize default state"),
    );
    assert!(
        !state_directory.contains("custom-state"),
        "default scope must not silently widen to registered states"
    );
}

// An enabled repository has no state directory until its first managed add.
// `worktree prune` used to fail there with a `filesystem-io-failed` receipt,
// telling a harness to inspect a repository that was perfectly healthy, while
// `status`, `doctor`, `gc`, `repair`, and `worktree list` all reported success
// at the same moment.
//
// Reporting success while skipping the prune would be a worse answer than the
// error it replaces: `worktree prune` exists to run Git's own prune, and
// ordinary Git worktrees can leave stale registrations long before Riftri
// manages one. The prune must actually happen.
#[test]
fn worktree_prune_succeeds_before_any_managed_add_and_still_prunes_git_metadata() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    assert!(
        git(
            &repository,
            &["commit", "--quiet", "--allow-empty", "-m", "initial"]
        )
        .status
        .success()
    );

    let stale = fixture.path().join("stale");
    assert!(
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/stale",
                stale.to_str().expect("UTF-8 worktree path"),
            ],
        )
        .status
        .success()
    );
    fs::remove_dir_all(&stale).expect("remove the worktree directory behind Git's back");
    assert!(
        !repository.join(".git/riftri").exists(),
        "no managed add has run, so there is no state directory yet"
    );

    let output = riftri(&repository, &["worktree", "prune"]);
    assert!(
        output.status.success(),
        "prune must succeed before any managed add: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let listed =
        String::from_utf8_lossy(&git(&repository, &["worktree", "list"]).stdout).to_string();
    assert!(
        !listed.contains("feature/stale"),
        "the stale registration survived the prune: {listed}"
    );
}

// `remove`, `move`, and `compact` operate on an existing managed worktree, so
// a repository with no state directory simply manages nothing and the worktree
// named cannot be managed either. That is a policy error, and it is the one
// these commands already report once any managed add has run. Before the fix
// they reported the absent directory as `filesystem-io-failed` with a recovery
// of `inspect` and an unknown `cleanup`, so the same request was classified
// two different ways depending on whether an unrelated add had happened.
//
// Unlike `worktree prune`, these must not create a state directory as a side
// effect of failing: they have no work to do.
#[test]
fn lifecycle_commands_report_an_unmanaged_worktree_as_policy_without_a_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    init_repository(&repository);
    assert!(
        git(
            &repository,
            &["commit", "--quiet", "--allow-empty", "-m", "initial"]
        )
        .status
        .success()
    );
    let unmanaged = fixture.path().join("unmanaged");
    let moved = fixture.path().join("moved");
    assert!(
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/unmanaged",
                unmanaged.to_str().expect("UTF-8 worktree path"),
            ],
        )
        .status
        .success()
    );
    let state = repository.join(".git/riftri");
    assert!(!state.exists(), "no managed add has run");

    let unmanaged = unmanaged.to_str().expect("UTF-8 worktree path");
    let moved = moved.to_str().expect("UTF-8 destination path");
    for arguments in [
        &["--json-errors", "worktree", "remove", unmanaged][..],
        &["--json-errors", "worktree", "compact", unmanaged][..],
        &["--json-errors", "worktree", "move", unmanaged, moved][..],
    ] {
        let output = riftri(&repository, arguments);
        assert!(!output.status.success(), "{arguments:?} must fail");
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
            .or_else(|_| serde_json::from_slice(&output.stderr))
            .unwrap_or_else(|error| panic!("{arguments:?} receipt is not JSON: {error}"));
        assert_eq!(receipt["code"], "invalid-request", "{arguments:?}");
        assert_eq!(receipt["category"], "policy", "{arguments:?}");
        assert_eq!(receipt["recovery"], "not-required", "{arguments:?}");
        assert_eq!(receipt["cleanup"], "not-needed", "{arguments:?}");
        assert!(
            !state.exists(),
            "{arguments:?} must not create a state directory just to fail"
        );
    }
}
