//! Cross-platform JSON report coverage for read-only lifecycle commands.
//!
//! These tests run against an explicit empty state directory so they exercise
//! the stable JSON shapes on every supported platform without requiring a
//! copy-on-write capable volume.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn riftri(current_directory: &Path, arguments: &[&str], state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .args(["--state-dir"])
        .arg(state)
        .current_dir(current_directory)
        .output()
        .expect("run Riftri CLI")
}

#[test]
fn status_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["status", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse status JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["operations"]["active_views"], 0);
    assert_eq!(report["operations"]["pending_adds"], 0);
    assert_eq!(report["operations"]["coordination_locks"], 0);
    assert_eq!(report["bases"], serde_json::json!([]));
    assert_eq!(report["worktrees"], serde_json::json!([]));
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));
    assert_eq!(report["total_logical_bytes"], 0);
    assert_eq!(report["total_allocated_bytes"], 0);
}

#[test]
fn repair_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["repair", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri repair failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse repair JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["scanned"], 0);
    assert_eq!(report["busy_adds"], 0);
    assert_eq!(report["recovered_adds"], 0);
    assert_eq!(report["errors"], serde_json::json!([]));
}

#[test]
fn gc_plan_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["gc", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri gc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse gc JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["applied"], false);
    assert_eq!(report["candidates"], serde_json::json!([]));
    assert_eq!(report["collected"], serde_json::json!([]));
    assert_eq!(report["skipped_in_use"], serde_json::json!([]));
    assert_eq!(report["removed_logical_bytes"], 0);
    assert_eq!(report["removed_allocated_bytes"], 0);
}
