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
    assert!(relative["state_directory"].is_null());
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
