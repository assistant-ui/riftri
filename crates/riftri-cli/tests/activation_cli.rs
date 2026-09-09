use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

struct RepositoryFixture {
    directory: TempDir,
    repository: PathBuf,
}

impl RepositoryFixture {
    fn new() -> Self {
        let directory = tempdir().expect("fixture directory");
        let repository = directory.path().join("repository");
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
        Self {
            directory,
            repository,
        }
    }
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn riftri(path: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Riftri CLI")
}

#[test]
fn enable_and_disable_change_only_repository_local_config() {
    let fixture = RepositoryFixture::new();

    let enabled = riftri(&fixture.repository, &["enable"]);
    assert!(
        enabled.status.success(),
        "enable failed: {}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        git(
            &fixture.repository,
            &["config", "--local", "--get", "riftri.enabled"]
        )
        .stdout,
        b"true\n"
    );
    let doctor = riftri(&fixture.repository, &["doctor"]);
    assert!(doctor.status.success());
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("Repository enabled: yes"));

    let disabled = riftri(&fixture.repository, &["disable"]);
    assert!(
        disabled.status.success(),
        "disable failed: {}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert_eq!(
        git(
            &fixture.repository,
            &["config", "--local", "--get", "riftri.enabled"]
        )
        .status
        .code(),
        Some(1)
    );
}

#[test]
fn exec_delegates_normal_git_to_the_real_executable() {
    let fixture = RepositoryFixture::new();
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = riftri(
        &fixture.repository,
        &["exec", "--", "git", "rev-parse", "--show-toplevel"],
    );
    assert!(
        output.status.success(),
        "scoped Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        fixture
            .repository
            .canonicalize()
            .expect("canonical repository")
    );

    let direct_failure = git(
        &fixture.repository,
        &["rev-parse", "--verify", "refs/heads/does-not-exist"],
    );
    let scoped_failure = riftri(
        &fixture.repository,
        &[
            "exec",
            "--",
            "git",
            "rev-parse",
            "--verify",
            "refs/heads/does-not-exist",
        ],
    );
    assert_eq!(scoped_failure.status.code(), direct_failure.status.code());
    assert_eq!(scoped_failure.stdout, direct_failure.stdout);
    assert_eq!(scoped_failure.stderr, direct_failure.stderr);
}

#[test]
fn exec_leaves_worktree_add_untouched_for_a_disabled_repository() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("ordinary-view");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/ordinary",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run ordinary Git worktree add");

    assert!(
        output.status.success(),
        "ordinary add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.repository.join(".git/riftri").exists());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn enabled_unsupported_add_fails_without_falling_back() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("unsupported-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "add"])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run unsupported enabled add");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires `-b <branch>`"));
    assert!(!destination.exists());
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn exec_routes_enabled_git_worktree_add_through_apfs() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("optimized-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/enabled",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run enabled Git worktree add");

    assert!(
        output.status.success(),
        "enabled add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("optimized APFS worktree"));
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(fixture.repository.join(".git/riftri/operations").is_dir());
}
