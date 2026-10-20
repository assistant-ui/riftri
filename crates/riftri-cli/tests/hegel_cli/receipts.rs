//! Every refused invocation leaves exactly one receipt that matches its exit
//! code, names what the caller selected, and changes nothing about the code
//! the same invocation exits with when receipts are off.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;

use crate::common::{Scratch, git_ok, receipt, riftri};
use crate::contract::process_cases;
use crate::contract::{decode_hex, path_from_bytes};

/// One command shape: its words with placeholders, the operation a receipt
/// names for it, and whether it takes `--repository` and `--state-dir`.
struct Shape {
    words: &'static [&'static str],
    operation: &'static str,
    repository: bool,
    state_directory: bool,
}

const fn shape(
    words: &'static [&'static str],
    operation: &'static str,
    repository: bool,
    state_directory: bool,
) -> Shape {
    Shape {
        words,
        operation,
        repository,
        state_directory,
    }
}

/// `VIEW`, `OTHER`, `REPOSITORY`, and `STATE` stand for generated paths.
const SHAPES: [Shape; 24] = [
    shape(&["status"], "status", true, true),
    shape(&["repair"], "repair", true, true),
    shape(&["gc"], "garbage-collection", true, true),
    shape(
        &["gc", "--apply", "--yes"],
        "garbage-collection",
        true,
        true,
    ),
    shape(&["worktree", "list"], "worktree-list", true, true),
    shape(
        &["worktree", "list", "--all-states"],
        "worktree-list",
        true,
        false,
    ),
    shape(
        &["worktree", "add", "VIEW", "-b", "topic"],
        "worktree-add",
        true,
        true,
    ),
    shape(
        &["worktree", "add", "VIEW", "--detach"],
        "worktree-add",
        true,
        true,
    ),
    shape(
        &["worktree", "add", "VIEW", "main"],
        "worktree-add",
        true,
        true,
    ),
    shape(
        &["worktree", "remove", "VIEW"],
        "worktree-remove",
        true,
        true,
    ),
    shape(
        &["worktree", "remove", "VIEW", "--force", "--yes"],
        "worktree-remove",
        true,
        true,
    ),
    shape(
        &["worktree", "move", "VIEW", "OTHER"],
        "worktree-move",
        true,
        true,
    ),
    shape(
        &["worktree", "compact", "VIEW"],
        "worktree-compact",
        true,
        true,
    ),
    shape(&["worktree", "prune"], "worktree-prune", true, true),
    shape(
        &["worktree", "owner", "VIEW"],
        "worktree-owner",
        true,
        false,
    ),
    shape(
        &["worktree", "inspect", "VIEW", "OTHER"],
        "worktree-inspect",
        true,
        false,
    ),
    shape(&["enable"], "enable", true, false),
    shape(&["disable"], "disable", true, false),
    shape(&["doctor", "--destination", "VIEW"], "doctor", true, false),
    shape(&["state", "unregister", "STATE"], "state", true, false),
    shape(&["setup"], "setup", true, false),
    shape(
        &["exec", "--worktree", "VIEW", "--", "true"],
        "exec",
        false,
        false,
    ),
    shape(&["shell", "status", "REPOSITORY"], "shell", false, false),
    shape(&["man", "FILE/pages"], "man", false, false),
];

/// A path under `root`, sometimes with bytes no shell or UTF-8 string holds.
fn odd_path(tc: &TestCase, root: &Path, stem: &str) -> PathBuf {
    let suffix: &[u8] = tc.draw(gs::sampled_from(vec![
        &b""[..],
        b" with spaces",
        b" it's quoted",
        b"-\xff\xfe",
        b"\ttab",
    ]));
    let mut name = stem.as_bytes().to_vec();
    name.extend_from_slice(suffix);
    root.join(OsString::from_vec(name))
}

fn receipt_path(receipt: &serde_json::Value, field: &str) -> Option<PathBuf> {
    let hex = receipt[format!("{field}NativeHex")].as_str()?;
    let path = path_from_bytes(decode_hex(hex));
    assert_eq!(
        receipt[field],
        path.display().to_string(),
        "{field} is the lossy rendering of its exact bytes"
    );
    Some(path)
}

#[hegel::test(process_cases(20))]
fn refusals_leave_one_receipt_matching_the_exit_code(tc: TestCase) {
    let scratch = Scratch::new();
    let root = scratch.root();
    std::fs::write(root.join("file"), "").expect("create file");
    std::fs::create_dir(root.join("occupied")).expect("create directory");
    std::fs::write(root.join("occupied/kept.txt"), "kept\n").expect("occupy directory");
    std::fs::create_dir(root.join("empty state")).expect("create state directory");

    let repository_kind = tc.draw(gs::sampled_from(vec![
        "repository",
        "unborn",
        "bare",
        "directory",
        "file",
        "missing",
    ]));
    let repository = match repository_kind {
        "repository" => scratch.repository("repository one"),
        "unborn" | "bare" => {
            let repository = root.join(repository_kind);
            std::fs::create_dir(&repository).expect("create repository");
            let flag = if repository_kind == "bare" {
                "--bare"
            } else {
                "--quiet"
            };
            git_ok(&repository, &["init", "--quiet", flag]);
            repository
        }
        "directory" => root.join("occupied"),
        "file" => root.join("file"),
        _ => root.join("missing repository"),
    };
    let view = match tc.draw(gs::sampled_from(vec!["new", "occupied", "repository"])) {
        "new" => odd_path(&tc, &root, "view"),
        "occupied" => root.join("occupied"),
        _ => repository.clone(),
    };
    let other = odd_path(&tc, &root, "other");
    let state_kind = tc.draw(gs::sampled_from(vec![
        "default",
        "missing",
        "empty",
        "file",
        "below-file",
    ]));
    let state = match state_kind {
        "default" => None,
        "missing" => Some(odd_path(&tc, &root, "missing state")),
        "empty" => Some(root.join("empty state")),
        "file" => Some(root.join("file")),
        _ => Some(root.join("file/state")),
    };

    let shape = &SHAPES[tc.draw(gs::integers::<usize>().max_value(SHAPES.len() - 1))];
    let mut arguments = Vec::<OsString>::new();
    for word in shape.words {
        arguments.push(match *word {
            "VIEW" => view.clone().into(),
            "OTHER" => other.clone().into(),
            "REPOSITORY" => repository.clone().into(),
            "STATE" => state
                .clone()
                .unwrap_or_else(|| root.join("missing state"))
                .into(),
            "FILE/pages" => root.join("file/pages").into(),
            word => word.into(),
        });
    }
    if shape.repository {
        arguments.push("--repository".into());
        arguments.push(repository.clone().into());
    }
    let selected_state = state.filter(|_| shape.state_directory);
    if let Some(state) = &selected_state {
        arguments.push("--state-dir".into());
        arguments.push(state.clone().into());
    }
    let run = |json_errors: bool| {
        let mut command = riftri();
        // Run from elsewhere, so nothing is found through the current directory.
        command.current_dir(root.join("occupied"));
        if json_errors {
            command.arg("--json-errors");
        }
        command.args(&arguments).output().expect("run riftri")
    };

    let output = run(true);

    let code = output.status.code().expect("riftri exits");
    assert!((0..=3).contains(&code), "exit code {code}");
    if code == 0 {
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("\"outcome\""),
            "a successful command left a receipt"
        );
        return;
    }
    let receipt = receipt(&output);
    let category = receipt["category"].as_str().expect("category");
    let message = receipt["message"].as_str().expect("message");
    assert_eq!(receipt["operation"], shape.operation, "{receipt}");
    assert_eq!(
        receipt_path(&receipt, "repository"),
        shape.repository.then(|| repository.clone()),
        "{receipt}"
    );
    assert_eq!(
        receipt_path(&receipt, "stateDirectory"),
        selected_state,
        "{receipt}"
    );
    if shape.operation == "repair" {
        let next = receipt["nextCommand"].as_str().unwrap_or_default();
        assert!(!next.starts_with("riftri repair"), "{receipt}");
    }

    // What the documentation promises for the clear-cut refusals.
    if shape.operation == "setup" {
        assert_eq!(receipt["code"], "invalid-request", "{receipt}");
    }
    if message.starts_with("invalid worktree request:")
        || message.starts_with("unsupported optimized checkout:")
    {
        assert_eq!(category, "policy", "{receipt}");
    }
    // Every command that inspects a repository says so when there is none,
    // unless an explicit state directory let it skip the repository.
    let outside_repository = !matches!(repository_kind, "repository" | "unborn" | "bare");
    if outside_repository && shape.repository && selected_state.is_none() {
        assert_eq!(receipt["code"], "not-a-repository", "{receipt}");
    }
    if receipt["code"] == "not-a-repository" {
        assert_eq!(category, "policy", "{receipt}");
        assert!(
            !matches!(repository_kind, "repository" | "unborn" | "bare"),
            "{receipt}"
        );
    }
    if category == "policy" {
        // Nothing was attempted: no cleanup, and nothing on stdout.
        assert_eq!(receipt["cleanup"], "not-needed", "{receipt}");
        assert!(output.stdout.is_empty(), "{receipt}");

        // Asking for a receipt must not change how the command exits, and
        // the human diagnostic carries the same message.
        let plain = run(false);
        assert_eq!(plain.status.code(), Some(code));
        let stderr = String::from_utf8_lossy(&plain.stderr);
        assert!(
            serde_json::from_str::<serde_json::Value>(stderr.trim_end()).is_err(),
            "{stderr}"
        );
        // Setup refuses `--json-errors` itself, so its plain run reaches the
        // next refusal (no interactive terminal) with a different message.
        if shape.operation != "setup" {
            let first_line = message.lines().next().unwrap_or_default();
            assert!(stderr.contains(first_line), "{stderr}\n{receipt}");
        }
    }
}
