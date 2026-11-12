//! Private bytes per worktree and the bytes saved against plain checkouts,
//! measured from the filesystem rather than estimated from allocation.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

mod support;
use support::writable_tempdir as tempdir;

const PAYLOAD: usize = 4 << 20;
const WRITTEN: usize = 1 << 20;

fn riftri(current_directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(current_directory)
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

fn json_stdout(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse JSON report")
}

/// The selected backend for `destination`, or None to skip on volumes
/// without one unless CI said the volume must have one.
fn selected_backend(repository: &Path, destination: &Path) -> Option<String> {
    let doctor = json_stdout(&riftri(
        repository,
        &[
            "doctor",
            "--destination",
            destination.to_str().expect("UTF-8 fixture path"),
            "--json",
        ],
    ));
    if let Some(backend) = doctor["destination_readiness"]["backend"].as_str() {
        return Some(backend.to_owned());
    }
    for required in ["RIFTRI_REQUIRE_REFLINK", "RIFTRI_REQUIRE_OVERLAYFS"] {
        assert_ne!(
            std::env::var_os(required).as_deref(),
            Some(OsStr::new("1")),
            "{required}=1 but doctor selected no backend: {doctor}"
        );
    }
    eprintln!("skipping: no copy-on-write backend for this volume");
    None
}

/// Distinct bytes, so no filesystem can deduplicate or compress them away.
fn pattern(length: usize, seed: usize) -> Vec<u8> {
    (0..length)
        .map(|index| ((index + seed).wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

#[test]
fn status_reports_private_bytes_and_what_the_views_save() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("payload.bin"), pattern(PAYLOAD, 0)).expect("write payload");
    git(&repository, &["add", "--", "payload.bin"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let names = ["clean", "edited", "third"];
    let Some(backend) = selected_backend(&repository, &fixture.path().join(names[0])) else {
        return;
    };
    for name in names {
        json_stdout(&riftri(
            &repository,
            &[
                "worktree",
                "add",
                fixture
                    .path()
                    .join(name)
                    .to_str()
                    .expect("UTF-8 fixture path"),
                "-b",
                name,
                "--no-progress",
                "--json",
            ],
        ));
    }
    // Overwrite the first megabyte of one view's copy of the payload.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(fixture.path().join("edited/payload.bin"))
        .expect("open the edited view's payload");
    file.write_all(&pattern(WRITTEN, 7))
        .expect("overwrite payload");
    file.sync_all().expect("sync payload");
    drop(file);

    let status = json_stdout(&riftri(&repository, &["status", "--json"]));
    let worktrees = status["worktrees"].as_array().expect("worktrees");
    let private = |name: &str| {
        worktrees
            .iter()
            .find(|worktree| worktree["path"].as_str().unwrap().ends_with(name))
            .unwrap_or_else(|| panic!("{name} in {status}"))["private_bytes"]
            .as_u64()
    };
    if backend == "refs-block-clone" {
        // ReFS has no per-file private size, so nothing is claimed.
        assert!(private("clean").is_none(), "{status}");
        assert!(status["saved_bytes"].is_null(), "{status}");
        return;
    }
    let clean = private("clean").expect("clean view measured");
    let edited = private("edited").expect("edited view measured");
    assert!(
        clean < (PAYLOAD / 16) as u64,
        "a clean view owns {clean}: {status}"
    );
    // A block clone owns only the blocks written; OverlayFS copies the whole
    // file up on its first write.
    let overlay = backend == "overlay-fs";
    if overlay {
        assert!(edited >= PAYLOAD as u64, "copied up: {edited}: {status}");
    } else {
        assert!(
            edited >= WRITTEN as u64 && edited < PAYLOAD as u64,
            "only the written megabyte is the edited view's own: {edited}: {status}"
        );
    }

    // The saving is exactly the reported formula, and close to two payloads.
    let logical = worktrees
        .iter()
        .map(|worktree| worktree["logical_bytes"].as_i64().unwrap())
        .sum::<i64>();
    let owned = worktrees
        .iter()
        .map(|worktree| worktree["private_bytes"].as_i64().unwrap())
        .sum::<i64>();
    let bases = status["bases"]
        .as_array()
        .expect("bases")
        .iter()
        .map(|base| base["logical_bytes"].as_i64().unwrap())
        .sum::<i64>();
    let saved = status["saved_bytes"].as_i64().expect("saved bytes");
    assert_eq!(saved, logical - owned - bases, "{status}");
    // Three views of one payload save about two, less what the edit owns.
    let owned_by_edit = if overlay { PAYLOAD } else { WRITTEN };
    assert!(
        saved > (2 * PAYLOAD - owned_by_edit - PAYLOAD / 16) as i64,
        "{saved}: {status}"
    );

    let human = riftri(&repository, &["status"]);
    assert!(
        String::from_utf8_lossy(&human.stdout).contains("Saved against one plain checkout"),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );

    for name in names {
        let removed = riftri(
            &repository,
            &[
                "worktree",
                "remove",
                "--force",
                "--yes",
                fixture
                    .path()
                    .join(name)
                    .to_str()
                    .expect("UTF-8 fixture path"),
                "--no-progress",
            ],
        );
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
}
