#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

#[test]
fn worktree_add_uses_the_revision_resolved_before_mutation() {
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
    fs::write(repository.join("tracked.txt"), "first\n").expect("write first revision");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "first"])
            .status
            .success()
    );
    let first = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("first commit is UTF-8")
        .trim()
        .to_owned();
    fs::write(repository.join("tracked.txt"), "second\n").expect("write second revision");
    assert!(
        git(&repository, &["commit", "-am", "second", "--quiet"])
            .status
            .success()
    );
    let second = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("second commit is UTF-8")
        .trim()
        .to_owned();
    assert!(
        git(&repository, &["branch", "moving", &first])
            .status
            .success()
    );

    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate real Git")
            .stdout,
    )
    .expect("Git path is UTF-8")
    .trim()
    .to_owned();
    let wrapper = fixture.path().join("moving-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = add ]; then\n  \"$RIFTRI_TEST_REAL_GIT\" update-ref refs/heads/moving \"$RIFTRI_TEST_MOVED_COMMIT\" || exit $?\nfi\nexec \"$RIFTRI_TEST_REAL_GIT\" \"$@\"\n",
    )
    .expect("write Git wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/pinned", "moving", "--state-dir"])
        .arg(&state)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_MOVED_COMMIT", &second)
        .current_dir(&repository)
        .output()
        .expect("run Riftri CLI through moving-ref wrapper");

    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(git(&destination, &["rev-parse", "HEAD"]).stdout)
            .expect("worktree commit is UTF-8")
            .trim(),
        first
    );
    assert_eq!(
        fs::read_to_string(destination.join("tracked.txt")).expect("read worktree file"),
        "first\n"
    );
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}
