//! `riftri history`: every worktree a state directory has recorded, including
//! ones whose journals `riftri gc --apply` retired, with creation and removal
//! times.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

mod support;
use support::writable_tempdir as tempdir;

fn riftri(current_directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(current_directory)
        .env_remove("RIFTRI_WORKTREE_METADATA")
        .output()
        .expect("run Riftri CLI")
}

fn git(path: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn initialize_repository(repository: &Path) {
    fs::create_dir(repository).expect("create repository");
    git(repository, &["init", "--quiet", "--initial-branch=main"]);
    git(repository, &["config", "user.name", "Riftri Tests"]);
    git(
        repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    git(repository, &["add", "--", "tracked.txt"]);
    git(repository, &["commit", "--quiet", "-m", "initial"]);
}

fn json_stdout(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse JSON report")
}

fn copy_on_write_available(repository: &Path, destination: &Path) -> bool {
    let doctor = json_stdout(&riftri(
        repository,
        &[
            "doctor",
            "--destination",
            destination.to_str().expect("UTF-8 fixture path"),
            "--json",
        ],
    ));
    if !doctor["destination_readiness"]["backend"].is_null() {
        return true;
    }
    for required in ["RIFTRI_REQUIRE_REFLINK", "RIFTRI_REQUIRE_OVERLAYFS"] {
        assert_ne!(
            std::env::var_os(required).as_deref(),
            Some(OsStr::new("1")),
            "{required}=1 but doctor selected no backend: {doctor}"
        );
    }
    eprintln!("skipping: no copy-on-write backend for this volume");
    false
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn is_rfc3339_utc(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'.',
            23 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

#[test]
fn history_of_a_repository_without_riftri_state_is_empty() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    initialize_repository(&repository);
    let report = json_stdout(&riftri(&repository, &["history", "--json"]));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["worktrees"], json!([]));
    assert_eq!(
        report["counts"],
        json!({"active": 0, "pending": 0, "removed": 0, "rolled_back": 0})
    );
    assert_eq!(report["diagnostic_issues"], json!([]));
    assert!(
        !repository.join(".git/riftri").exists(),
        "history must not create state"
    );
}

#[test]
fn history_keeps_removed_worktrees_after_gc_retires_their_journals() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    initialize_repository(&repository);
    let kept = fixture.path().join("kept");
    if !copy_on_write_available(&repository, &kept) {
        return;
    }
    let gone = fixture.path().join("gone");
    for (path, branch, agent) in [(&gone, "gone", "codex"), (&kept, "kept", "claude-code")] {
        json_stdout(&riftri(
            &repository,
            &[
                "worktree",
                "add",
                path.to_str().expect("UTF-8 fixture path"),
                "-b",
                branch,
                "--meta",
                &format!("agent={agent}"),
                "--no-progress",
                "--json",
            ],
        ));
    }
    let removed = riftri(
        &repository,
        &[
            "worktree",
            "remove",
            gone.to_str().expect("UTF-8 fixture path"),
            "--no-progress",
        ],
    );
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );

    let check = |report: &Value, archived: bool| {
        assert_eq!(
            report["counts"],
            json!({"active": 1, "pending": 0, "removed": 1, "rolled_back": 0}),
            "{report}"
        );
        let worktrees = report["worktrees"].as_array().expect("worktrees");
        assert_eq!(worktrees.len(), 2, "{report}");
        let (first, second) = (&worktrees[0], &worktrees[1]);
        assert!(
            first["path"].as_str().unwrap().ends_with("gone"),
            "{report}"
        );
        assert_eq!(first["state"], "removed");
        assert_eq!(first["branch"], "refs/heads/gone");
        assert_eq!(first["metadata"], json!({"agent": "codex"}));
        assert_eq!(first["archived"], archived);
        assert!(is_rfc3339_utc(&first["created_at"]), "{first}");
        assert!(is_rfc3339_utc(&first["ended_at"]), "{first}");
        assert!(first["created_at"].as_str() <= first["ended_at"].as_str());
        assert!(
            second["path"].as_str().unwrap().ends_with("kept"),
            "{report}"
        );
        assert_eq!(second["state"], "active");
        assert_eq!(second["metadata"], json!({"agent": "claude-code"}));
        assert_eq!(second["archived"], false);
        assert!(second["ended_at"].is_null());
        assert_eq!(report["diagnostic_issues"], json!([]));
    };
    let before = json_stdout(&riftri(&repository, &["history", "--json"]));
    check(&before, false);

    let collected = json_stdout(&riftri(
        &repository,
        &["gc", "--apply", "--no-progress", "--json"],
    ));
    assert!(
        collected["retired_journals"].as_u64().unwrap() >= 2,
        "{collected}"
    );
    let after = json_stdout(&riftri(&repository, &["history", "--json"]));
    check(&after, true);
    assert_eq!(
        before["worktrees"][0]["created_at"],
        after["worktrees"][0]["created_at"]
    );

    // `worktree list` and `status` carry the same creation time.
    let list = json_stdout(&riftri(&repository, &["worktree", "list", "--json"]));
    assert_eq!(
        list["worktrees"][0]["created_at"],
        after["worktrees"][1]["created_at"]
    );
    let status = json_stdout(&riftri(&repository, &["status", "--json"]));
    assert_eq!(
        status["worktrees"][0]["created_at"],
        after["worktrees"][1]["created_at"]
    );
    assert_eq!(status["diagnostic_issues"], json!([]), "{status}");

    let human = riftri(&repository, &["history"]);
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(
        human.contains("Worktrees: 2 (1 active, 0 pending, 1 removed, 0 rolled-back)"),
        "{human}"
    );
    assert!(human.contains("Metadata: agent=codex"), "{human}");

    let removed = riftri(
        &repository,
        &[
            "worktree",
            "remove",
            kept.to_str().expect("UTF-8 fixture path"),
            "--no-progress",
        ],
    );
    assert!(removed.status.success());
}
