#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn command(program: &Path, path: &Path, arguments: &[&str]) -> Output {
    Command::new(program)
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run fixture command")
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    command(Path::new("git"), path, arguments)
}

#[test]
fn documented_command_creates_a_clean_real_worktree() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("worktree");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/cli", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run Riftri CLI");

    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created APFS-backed Git worktree"));
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(
        String::from_utf8_lossy(&git(&repository, &["worktree", "list", "--porcelain"]).stdout)
            .contains("refs/heads/feature/cli")
    );
}
