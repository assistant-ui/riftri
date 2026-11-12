//! `--meta` labels: recorded at creation, reported by `worktree list` and
//! `status`, inherited through `riftri exec`, and refused before anything is
//! created when malformed.

#[cfg(unix)]
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
#[cfg(unix)]
use serde_json::json;

mod support;
use support::writable_tempdir as tempdir;

const METADATA_ENV: &str = "RIFTRI_WORKTREE_METADATA";

fn riftri(current_directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(current_directory)
        .env_remove(METADATA_ENV)
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

#[cfg(unix)]
fn json_stdout(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse JSON report")
}

fn receipt(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr is not one JSON receipt ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Whether doctor selects a copy-on-write backend for `destination`. Skips
/// the caller on volumes without one, unless CI said the volume must have
/// one.
#[cfg(unix)]
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

#[cfg(unix)]
fn metadata_by_path(report: &Value) -> Value {
    let mut entries = serde_json::Map::new();
    for worktree in report["worktrees"].as_array().expect("worktrees array") {
        let name = Path::new(worktree["path"].as_str().expect("path"))
            .file_name()
            .expect("worktree name")
            .to_string_lossy()
            .into_owned();
        entries.insert(name, worktree["metadata"].clone());
    }
    Value::Object(entries)
}

#[test]
fn malformed_metadata_is_refused_before_anything_is_created() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    initialize_repository(&repository);
    let destination = fixture.path().join("view");
    let destination_arg = destination.to_str().expect("UTF-8 fixture path");

    let refusals: [(&[&str], &str); 5] = [
        (&["--meta", "agent"], "KEY=VALUE"),
        (&["--meta", "=codex"], "metadata key"),
        (
            &["--meta", "agent=a", "--meta", "agent=b"],
            "more than once",
        ),
        (&["--meta", "agent=line\nbreak"], "control character"),
        (&["--meta", "bad key=x"], "metadata key"),
    ];
    for (meta, expected) in refusals {
        let mut arguments = vec!["worktree", "add", destination_arg, "-b", "labeled"];
        arguments.extend_from_slice(meta);
        arguments.push("--json-errors");
        let output = riftri(&repository, &arguments);
        assert_eq!(output.status.code(), Some(3), "{meta:?}");
        let receipt = receipt(&output);
        assert_eq!(receipt["code"], "invalid-request", "{meta:?}");
        assert_eq!(receipt["category"], "policy", "{meta:?}");
        assert_eq!(receipt["operation"], "worktree-add", "{meta:?}");
        assert_eq!(receipt["cleanup"], "not-needed", "{meta:?}");
        assert!(
            receipt["message"]
                .as_str()
                .expect("receipt message")
                .contains(expected),
            "{meta:?}: {receipt}"
        );
    }

    let inherited = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add", destination_arg, "-b", "labeled"])
        .arg("--json-errors")
        .current_dir(&repository)
        .env(METADATA_ENV, "not json")
        .output()
        .expect("run Riftri CLI");
    assert_eq!(inherited.status.code(), Some(3));
    let inherited = receipt(&inherited);
    assert_eq!(inherited["code"], "invalid-request");
    assert!(
        inherited["message"]
            .as_str()
            .expect("receipt message")
            .contains(METADATA_ENV),
        "{inherited}"
    );

    assert!(
        !destination.exists(),
        "a refused add created its destination"
    );
    assert!(
        !repository.join(".git/riftri").exists(),
        "a refused add created Riftri state"
    );
    let branches = Command::new("git")
        .args(["branch", "--list", "labeled"])
        .current_dir(&repository)
        .output()
        .expect("list branches");
    assert!(
        branches.stdout.is_empty(),
        "a refused add created its branch"
    );
}

#[test]
fn exec_refuses_malformed_metadata_without_running_the_command() {
    let fixture = tempdir().expect("fixture directory");
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--meta", "agent", "--json-errors", "--"])
        .arg(env!("CARGO_BIN_EXE_riftri"))
        .args(["completions", "bash"])
        .current_dir(fixture.path())
        .env_remove(METADATA_ENV)
        .output()
        .expect("run Riftri CLI");
    assert_eq!(output.status.code(), Some(3));
    let receipt = receipt(&output);
    assert_eq!(receipt["code"], "invalid-request");
    assert_eq!(receipt["operation"], "exec");
    assert!(output.stdout.is_empty(), "the scoped command ran");
}

#[cfg(unix)]
#[test]
fn metadata_is_recorded_reported_and_inherited_through_exec() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    initialize_repository(&repository);
    let explicit = fixture.path().join("explicit");
    if !copy_on_write_available(&repository, &explicit) {
        return;
    }
    let explicit_arg = explicit.to_str().expect("UTF-8 fixture path");

    // Explicit add: labels in the add report, the journal, and every report.
    let added = json_stdout(&riftri(
        &repository,
        &[
            "worktree",
            "add",
            explicit_arg,
            "-b",
            "explicit",
            "--meta",
            "task=42",
            "--meta",
            "agent=codex",
            "--no-progress",
            "--json",
        ],
    ));
    assert_eq!(added["metadata"], json!({"agent": "codex", "task": "42"}));
    let journal: Value = serde_json::from_slice(
        &fs::read(added["journal_path"].as_str().expect("journal path")).expect("read journal"),
    )
    .expect("parse journal");
    assert_eq!(journal["metadata"], json!({"agent": "codex", "task": "42"}));

    let human = riftri(&repository, &["worktree", "list"]);
    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout).contains("Metadata: agent=codex, task=42"),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );

    // An unlabeled add reports an empty object, never a missing key.
    let unlabeled = fixture.path().join("unlabeled");
    json_stdout(&riftri(
        &repository,
        &[
            "worktree",
            "add",
            unlabeled.to_str().expect("UTF-8 fixture path"),
            "-b",
            "unlabeled",
            "--no-progress",
            "--json",
        ],
    ));

    // `riftri exec --meta` labels a worktree the scoped command creates with
    // plain Git, and an explicit add inside it inherits the labels while its
    // own `--meta` wins for a repeated key.
    let enabled = riftri(&repository, &["enable"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    let scoped = fixture.path().join("scoped");
    let inherited = fixture.path().join("inherited");
    let script = format!(
        "set -eu\n\
         git worktree add -b scoped '{scoped}' >/dev/null 2>&1\n\
         \"$RIFTRI_TEST_BINARY\" worktree add '{inherited}' -b inherited \
         --meta agent=claude-code --no-progress >/dev/null\n",
        scoped = scoped.display(),
        inherited = inherited.display(),
    );
    let exec = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--meta", "agent=codex", "--meta", "team=core", "--"])
        .args(["sh", "-c", &script])
        .current_dir(&repository)
        .env("RIFTRI_TEST_BINARY", env!("CARGO_BIN_EXE_riftri"))
        .env_remove(METADATA_ENV)
        .output()
        .expect("run scoped command");
    assert!(
        exec.status.success(),
        "scoped command failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    let expected = json!({
        "explicit": {"agent": "codex", "task": "42"},
        "inherited": {"agent": "claude-code", "team": "core"},
        "scoped": {"agent": "codex", "team": "core"},
        "unlabeled": {},
    });
    let list = json_stdout(&riftri(&repository, &["worktree", "list", "--json"]));
    assert_eq!(metadata_by_path(&list), expected);
    let all_states = json_stdout(&riftri(
        &repository,
        &["worktree", "list", "--all-states", "--json"],
    ));
    assert_eq!(metadata_by_path(&all_states), expected);
    let status = json_stdout(&riftri(&repository, &["status", "--json"]));
    assert_eq!(metadata_by_path(&status), expected);

    for name in ["explicit", "unlabeled", "scoped", "inherited"] {
        let path = fixture.path().join(name);
        let removed = riftri(
            &repository,
            &[
                "worktree",
                "remove",
                path.to_str().expect("UTF-8 fixture path"),
                "--no-progress",
            ],
        );
        assert!(
            removed.status.success(),
            "remove {name}: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
}
