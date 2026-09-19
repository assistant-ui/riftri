#![cfg(target_os = "macos")]

//! End-to-end coverage for the documented escape from a wedged state
//! directory: the exact command sequence that used to leave `remove` and
//! `repair` failing forever must now succeed.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn riftri(repository: &Path, state: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .args(["--state-dir"])
        .arg(state)
        .current_dir(repository)
        .output()
        .expect("run Riftri CLI")
}

fn create_repository(repository: &Path) {
    fs::create_dir_all(repository).expect("create repository");
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
fn a_hand_deleted_worktree_no_longer_wedges_remove_and_repair() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let worktree = root.join("worktree");
    create_repository(&repository);

    let added = riftri(
        &repository,
        &state,
        &[
            "worktree",
            "add",
            worktree.to_str().expect("worktree path"),
            "-b",
            "feature/first",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    fs::remove_dir_all(&worktree).expect("delete the worktree by hand");
    assert!(git(&repository, &["worktree", "prune"]).status.success());

    // repair reports the retirement instead of claiming the state is clean.
    let repaired = riftri(&repository, &state, &["repair", ".", "--json"]);
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&repaired.stdout).expect("parse repair JSON");
    assert_eq!(report["retired_adds"], 1);
    assert_eq!(report["errors"], serde_json::json!([]));

    // A second add at the same path used to create a duplicate active journal.
    let added = riftri(
        &repository,
        &state,
        &[
            "worktree",
            "add",
            worktree.to_str().expect("worktree path"),
            "-b",
            "feature/second",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let status = riftri(&repository, &state, &["status", ".", "--json"]);
    assert!(status.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("parse status JSON");
    assert_eq!(
        report["worktrees"].as_array().expect("worktrees").len(),
        1,
        "one path may only be claimed once"
    );
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));

    // This used to exit 3 with "multiple active Riftri journals reference".
    let removed = riftri(
        &repository,
        &state,
        &[
            "worktree",
            "remove",
            worktree.to_str().expect("worktree path"),
        ],
    );
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );

    // And the freed base is collectible, with nothing left protecting it.
    let collected = riftri(
        &repository,
        &state,
        &["gc", ".", "--apply", "--yes", "--json"],
    );
    assert!(
        collected.status.success(),
        "{}",
        String::from_utf8_lossy(&collected.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&collected.stdout).expect("parse gc JSON");
    assert_eq!(report["skipped_protected"], serde_json::json!([]));
    assert_eq!(report["collected"].as_array().expect("collected").len(), 1);
}

#[test]
fn repair_names_a_relocated_worktree_in_human_and_json_output() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let original = root.join("original");
    let relocated = root.join("relocated");
    create_repository(&repository);

    let added = riftri(
        &repository,
        &state,
        &[
            "worktree",
            "add",
            original.to_str().expect("worktree path"),
            "-b",
            "feature/relocated",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    fs::write(original.join("precious.txt"), "precious\n").expect("write user data");

    fs::rename(&original, &relocated).expect("relocate the worktree");
    assert!(
        git(
            &repository,
            &[
                "worktree",
                "repair",
                relocated.to_str().expect("relocated path")
            ]
        )
        .status
        .success()
    );

    let human = riftri(&repository, &state, &["repair", "."]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let text = String::from_utf8(human.stdout).expect("human repair output");
    assert!(
        text.contains("Relocated worktrees Riftri no longer tracks"),
        "{text}"
    );
    assert!(
        text.contains(relocated.to_str().expect("relocated path")),
        "{text}"
    );

    let json = riftri(&repository, &state, &["repair", ".", "--json"]);
    assert!(json.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("parse repair JSON");
    let relocations = report["relocated_worktrees"]
        .as_array()
        .expect("relocated worktrees");
    assert_eq!(relocations.len(), 1);
    assert_eq!(
        relocations[0]["registered_path"],
        relocated.to_string_lossy().as_ref()
    );
    assert_eq!(report["retired_adds"], 0);

    // Reporting only: the live worktree and its contents survive untouched.
    assert_eq!(
        fs::read_to_string(relocated.join("precious.txt")).expect("read user data"),
        "precious\n"
    );
}
