#![cfg(target_os = "macos")]

//! Guard the number of Git processes one `riftri worktree add` may spawn.
//!
//! Every Git invocation costs a process spawn on the critical path of
//! worktree creation, so the budget below is part of the performance
//! contract: raising it needs the same scrutiny as weakening a safety check.
//! The counts are driven entirely by Riftri's own code path, not by the
//! installed Git version, which keeps the assertions stable.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

/// Git invocations for an add that must build the immutable base first.
const COLD_ADD_BUDGET: usize = 19;
/// Git invocations for an add that reuses a verified immutable base.
const CACHED_ADD_BUDGET: usize = 14;
/// Git invocations for a cached add that checks out an existing local branch.
const CACHED_EXISTING_BRANCH_ADD_BUDGET: usize = 17;

fn git(path: &Path, arguments: &[&str]) -> Output {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn real_git() -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate the real git executable");
    assert!(output.status.success(), "no git executable on PATH");
    PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("UTF-8 git path")
            .trim(),
    )
}

fn install_counting_shim(directory: &Path) -> PathBuf {
    let shim = directory.join("git");
    fs::write(
        &shim,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RIFTRI_TEST_GIT_LOG\"\nexec \"$RIFTRI_TEST_REAL_GIT\" \"$@\"\n",
    )
    .expect("write counting git shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("mark shim executable");
    shim
}

fn add_worktree_with_counted_git(
    repository: &Path,
    destination: &Path,
    state: &Path,
    add_arguments: &[&str],
    shim_directory: &Path,
    real_git: &Path,
    log: &Path,
) -> Vec<String> {
    let original_path = std::env::var_os("PATH").expect("PATH is set");
    let mut shim_path = shim_directory.as_os_str().to_os_string();
    shim_path.push(":");
    shim_path.push(&original_path);
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(destination)
        .args(add_arguments)
        .arg("--state-dir")
        .arg(state)
        .current_dir(repository)
        .env("PATH", shim_path)
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_GIT_LOG", log)
        .output()
        .expect("run Riftri CLI");
    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(log)
        .expect("read Git invocation log")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn worktree_add_stays_within_its_git_invocation_budget() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let shim_directory = fixture.path().join("shim");
    fs::create_dir(&repository).expect("create repository");
    fs::create_dir(&shim_directory).expect("create shim directory");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        git(&repository, arguments);
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    // Many small files exercise the metadata-heavy shape the one-large-file
    // allocation benchmark cannot cover. Process budgets must stay constant.
    for directory in 0..16 {
        let path = repository.join(format!("package-{directory:02}"));
        fs::create_dir(&path).unwrap();
        for file in 0..64 {
            fs::write(
                path.join(format!("file-{file:02}")),
                format!("{directory}:{file}\n"),
            )
            .unwrap();
        }
    }
    git(&repository, &["add", "--all"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let real_git = real_git();
    install_counting_shim(&shim_directory);

    let cold_log = fixture.path().join("cold-invocations");
    let cold = add_worktree_with_counted_git(
        &repository,
        &fixture.path().join("cold-view"),
        &state,
        &["--detach", "HEAD"],
        &shim_directory,
        &real_git,
        &cold_log,
    );
    assert!(
        cold.len() <= COLD_ADD_BUDGET,
        "cold add spawned {} Git processes, budget is {COLD_ADD_BUDGET}:\n{}",
        cold.len(),
        cold.join("\n")
    );

    let cached_log = fixture.path().join("cached-invocations");
    let cached = add_worktree_with_counted_git(
        &repository,
        &fixture.path().join("cached-view"),
        &state,
        &["--detach", "HEAD"],
        &shim_directory,
        &real_git,
        &cached_log,
    );
    assert!(
        cached.len() <= CACHED_ADD_BUDGET,
        "cached add spawned {} Git processes, budget is {CACHED_ADD_BUDGET}:\n{}",
        cached.len(),
        cached.join("\n")
    );

    // The cached view must still be a clean, fully populated worktree.
    assert!(
        git(
            &fixture.path().join("cached-view"),
            &["status", "--porcelain=v1"]
        )
        .stdout
        .is_empty()
    );

    git(&repository, &["branch", "existing", "HEAD"]);
    let existing_log = fixture.path().join("existing-branch-invocations");
    let existing = add_worktree_with_counted_git(
        &repository,
        &fixture.path().join("existing-view"),
        &state,
        &["existing"],
        &shim_directory,
        &real_git,
        &existing_log,
    );
    assert!(
        existing.len() <= CACHED_EXISTING_BRANCH_ADD_BUDGET,
        "cached existing-branch add spawned {} Git processes, budget is \
         {CACHED_EXISTING_BRANCH_ADD_BUDGET}:\n{}",
        existing.len(),
        existing.join("\n")
    );
    for view in ["cold-view", "cached-view", "existing-view"] {
        let destination = fixture.path().join(view);
        assert!(
            git(
                &destination,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .stdout
            .is_empty()
        );
        for directory in 0..16 {
            for file in 0..64 {
                assert_eq!(
                    fs::read_to_string(
                        destination.join(format!("package-{directory:02}/file-{file:02}"))
                    )
                    .unwrap(),
                    format!("{directory}:{file}\n")
                );
            }
        }
    }
    fs::write(
        fixture.path().join("cached-view/package-00/file-00"),
        "private\n",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.path().join("cold-view/package-00/file-00")).unwrap(),
        "0:0\n"
    );
    fs::write(
        fixture.path().join("cached-view/package-00/file-00"),
        "0:0\n",
    )
    .unwrap();
    for view in ["existing-view", "cached-view", "cold-view"] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["worktree", "remove"])
            .arg(fixture.path().join(view))
            .arg("--state-dir")
            .arg(&state)
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.path().join(view).exists());
    }
}

fn run_with_counted_git(
    repository: &Path,
    arguments: &[&str],
    shim_directory: &Path,
    real_git: &Path,
    log: &Path,
) -> Vec<String> {
    let mut shim_path = shim_directory.as_os_str().to_os_string();
    shim_path.push(":");
    shim_path.push(std::env::var_os("PATH").expect("PATH is set"));
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(repository)
        .env("PATH", shim_path)
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_GIT_LOG", log)
        .output()
        .expect("run Riftri CLI");
    assert!(
        output.status.success(),
        "riftri {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// `status`, `worktree list`, and `repair` listed Git's worktrees once per
/// managed worktree, and each listing names every worktree, so they slowed
/// quadratically: about five seconds at 120 worktrees. One listing per
/// repository answers every healthy worktree.
#[test]
fn inspection_lists_git_worktrees_a_constant_number_of_times() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let shim_directory = fixture.path().join("shim");
    fs::create_dir(&repository).expect("create repository");
    fs::create_dir(&shim_directory).expect("create shim directory");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        git(&repository, arguments);
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    for index in 0..6 {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["worktree", "add", "--detach"])
            .arg(fixture.path().join(format!("view-{index}")))
            .args(["HEAD", "--no-progress", "--state-dir"])
            .arg(&state)
            .current_dir(&repository)
            .output()
            .expect("add a managed worktree");
        assert!(output.status.success());
    }
    let real_git = real_git();
    install_counting_shim(&shim_directory);
    let state_argument = state.to_str().expect("UTF-8 fixture path");

    for (name, arguments) in [
        (
            "status",
            &["status", "--json", "--state-dir", state_argument][..],
        ),
        (
            "list",
            &["worktree", "list", "--json", "--state-dir", state_argument][..],
        ),
        ("repair", &["repair", "--state-dir", state_argument][..]),
    ] {
        let log = fixture.path().join(format!("{name}-invocations"));
        let calls = run_with_counted_git(&repository, arguments, &shim_directory, &real_git, &log);
        let listings = calls
            .iter()
            .filter(|call| call.contains("worktree list"))
            .count();
        assert!(
            listings <= 2,
            "{name} listed Git worktrees {listings} times for 6 worktrees:\n{}",
            calls.join("\n")
        );
    }
}
