use std::process::Command;

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
