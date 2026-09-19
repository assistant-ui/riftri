use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const OVERRIDES: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

fn isolated(command: &str, repository: &Path) -> Command {
    let mut command = Command::new(command);
    command
        .current_dir(repository)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for name in OVERRIDES {
        command.env_remove(name);
    }
    command
}

fn git(repository: &Path, args: &[&str]) -> Output {
    let output = isolated("git", repository).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fixture(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    fs::create_dir(&repository).unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"],
        &["config", "user.email", "riftri@example.invalid"],
        &["config", "core.autocrlf", "false"],
    ] {
        git(&repository, args);
    }
    fs::write(repository.join("tracked.txt"), "original\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    fs::write(repository.join("tracked.txt"), "private staged data\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    fs::write(repository.join("tracked.txt"), "original\n").unwrap();
    repository
}

#[test]
fn explicit_and_intercepted_adds_reject_repository_overrides_without_mutation() {
    for name in OVERRIDES {
        for intercepted in [false, true] {
            let fixture_dir = tempfile::tempdir().unwrap();
            let repository = fixture(fixture_dir.path());
            git(&repository, &["config", "riftri.enabled", "true"]);
            let destination = fixture_dir.path().join("view");
            let value = match name {
                "GIT_INDEX_FILE" => repository.join(".git/index"),
                "GIT_WORK_TREE" => repository.clone(),
                _ => repository.join(".git"),
            };
            let before_index = fs::read(repository.join(".git/index")).unwrap();
            let before_config = fs::read(repository.join(".git/config")).unwrap();
            let mut command = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
            if intercepted {
                command.args(["exec", "--", "git"]);
            }
            command
                .args(["worktree", "add"])
                .arg(&destination)
                .args(["-b", "feature/environment", "HEAD"])
                .env(name, &value);
            let output = command.output().unwrap();
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{name}, intercepted={intercepted}: {message}"
            );
            assert!(message.contains(name), "{message}");
            assert!(message.contains("unset"), "{message}");
            assert!(!destination.exists());
            assert!(!repository.join(".git/riftri").exists());
            assert!(!repository.join(".git/worktrees").exists());
            assert_eq!(
                fs::read(repository.join(".git/index")).unwrap(),
                before_index
            );
            assert_eq!(
                fs::read(repository.join(".git/config")).unwrap(),
                before_config
            );
            assert_eq!(
                git(&repository, &["show", ":tracked.txt"]).stdout,
                b"private staged data\n"
            );
            assert_eq!(
                git(&repository, &["branch", "--list", "feature/environment"]).stdout,
                b""
            );
        }
    }
}

#[test]
fn lifecycle_commands_reject_even_empty_repository_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    // The state directory exists so that inspection commands reach the Git
    // environment guard rather than stopping earlier on a missing
    // `--state-dir`; the guard must still refuse before writing anything into
    // it.
    let state = fixture_dir.path().join("state");
    fs::create_dir(&state).unwrap();
    for args in [
        &["worktree", "add", "../view", "--detach"][..],
        &["worktree", "remove", "../view"],
        &["worktree", "remove", "../view", "--force", "--yes"],
        &["worktree", "move", "../view", "../moved"],
        &["worktree", "compact", "../view"],
        &["worktree", "prune"],
        &["repair"],
        &["gc", "--apply"],
    ] {
        let output = isolated(env!("CARGO_BIN_EXE_riftri"), &repository)
            .args(args)
            .arg("--state-dir")
            .arg(&state)
            .env("GIT_INDEX_FILE", "")
            .output()
            .unwrap();
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {message}");
        assert!(
            message.contains("GIT_INDEX_FILE") && message.contains("unset"),
            "{args:?}: {message}"
        );
        assert_eq!(
            fs::read_dir(&state).unwrap().count(),
            0,
            "{args:?}: refused commands must not write state"
        );
    }
}

#[test]
fn normal_git_passthrough_preserves_repository_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    git(&repository, &["config", "riftri.enabled", "true"]);
    let output = isolated(env!("CARGO_BIN_EXE_riftri"), fixture_dir.path())
        .args(["exec", "--", "git", "show", ":tracked.txt"])
        .env("GIT_DIR", repository.join(".git"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"private staged data\n");
}
