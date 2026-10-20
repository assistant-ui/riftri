#![cfg(target_os = "macos")]

//! Guard the number of Git processes each lifecycle command may spawn.
//!
//! Every Git invocation costs a process spawn on the critical path of a
//! worktree lifecycle command, so the budgets below are part of the
//! performance contract: raising one needs the same scrutiny as weakening a
//! safety check.
//! Older Git retains the private attribute index; Git 2.43+ must save its
//! read-tree process without weakening the lifecycle checks.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;
#[path = "support/real_git.rs"]
mod real_git;
use real_git::real_git;

/// Git invocations for an add that must build the immutable base first.
const COLD_ADD_BUDGET: usize = 18;
/// Git invocations for an add that reuses a verified immutable base.
const CACHED_ADD_BUDGET: usize = 13;
/// Git invocations for a cached add that checks out an existing local branch.
const CACHED_EXISTING_BRANCH_ADD_BUDGET: usize = 15;
/// Named revisions share HEAD's lookup; branch creation keeps its safety checks.
const CACHED_NEW_BRANCH_ADD_BUDGET: usize = 15;

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
        // An inherited real-Git override would bypass this counting wrapper.
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", shim_directory.join("git"))
        .env_remove("RIFTRI_BYPASS")
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
fn index_refresh_is_deferred_only_for_attribute_free_full_checkouts() {
    for (name, attributes, sparse, deferred) in [
        ("plain", None, false, true),
        ("empty-attributes", Some("# no attributes\n"), false, true),
        ("text-auto", Some("*.txt text=auto\n"), false, false),
        ("text-lf", Some("*.txt text eol=lf\n"), false, false),
        ("text-crlf", Some("*.txt text eol=crlf\n"), false, false),
        ("binary", Some("*.txt binary\n"), false, false),
        (
            "metadata",
            Some("*.txt linguist-generated=true\n"),
            false,
            false,
        ),
        ("sparse", None, true, false),
    ] {
        let fixture = tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let shim_directory = fixture.path().join("shim");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&shim_directory).unwrap();
        for arguments in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Riftri Tests"][..],
            &["config", "user.email", "riftri@example.invalid"][..],
            &["config", "core.autocrlf", "false"][..],
        ] {
            git(&repository, arguments);
        }
        fs::create_dir(repository.join("src")).unwrap();
        fs::write(repository.join("src/tracked.txt"), "tracked\n").unwrap();
        if let Some(attributes) = attributes {
            fs::write(repository.join(".gitattributes"), attributes).unwrap();
        }
        git(&repository, &["add", "--all"]);
        git(
            &repository,
            &["commit", "--quiet", "-m", "index refresh fixture"],
        );
        install_counting_shim(&shim_directory);
        let real_git = real_git();
        for kind in ["cold", "cached"] {
            let destination = fixture.path().join(kind);
            let log = fixture.path().join(format!("{kind}.log"));
            let arguments = if sparse {
                vec!["--detach", "--sparse-dir", "src", "HEAD"]
            } else {
                vec!["--detach", "HEAD"]
            };
            let commands = add_worktree_with_counted_git(
                &repository,
                &destination,
                &state,
                &arguments,
                &shim_directory,
                &real_git,
                &log,
            );
            let resets = commands
                .iter()
                .filter(|line| line.starts_with("reset "))
                .collect::<Vec<_>>();
            assert_eq!(resets.len(), 1, "{name}/{kind}: {commands:?}");
            assert_eq!(
                resets[0].contains("--no-refresh"),
                deferred,
                "{name}/{kind}: {resets:?}"
            );
            assert_eq!(
                commands
                    .iter()
                    .filter(|line| line.starts_with("status "))
                    .count(),
                1,
                "{name}/{kind}: the final content check must remain"
            );
            assert!(
                git(&destination, &["status", "--porcelain=v1", "-z"])
                    .stdout
                    .is_empty()
            );
        }
    }
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

    let legacy_attribute_index = usize::from(
        !riftri_git::Git::new(&real_git)
            .detect()
            .unwrap()
            .supports_attribute_source(),
    );

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
        cold.len() <= COLD_ADD_BUDGET + legacy_attribute_index,
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
        cached.len() <= CACHED_ADD_BUDGET + legacy_attribute_index,
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
        existing.len() <= CACHED_EXISTING_BRANCH_ADD_BUDGET + legacy_attribute_index,
        "cached existing-branch add spawned {} Git processes, budget is \
         {CACHED_EXISTING_BRANCH_ADD_BUDGET}:\n{}",
        existing.len(),
        existing.join("\n")
    );
    let named_log = fixture.path().join("named-invocations");
    let named = add_worktree_with_counted_git(
        &repository,
        &fixture.path().join("named-view"),
        &state,
        &["--detach", "refs/heads/existing"],
        &shim_directory,
        &real_git,
        &named_log,
    );
    assert!(
        named.len() <= CACHED_ADD_BUDGET + legacy_attribute_index,
        "named add: {}",
        named.join("\n")
    );
    let new_log = fixture.path().join("new-branch-invocations");
    let new_branch = add_worktree_with_counted_git(
        &repository,
        &fixture.path().join("new-view"),
        &state,
        &["-b", "new-branch", "refs/heads/existing"],
        &shim_directory,
        &real_git,
        &new_log,
    );
    assert!(
        new_branch.len() <= CACHED_NEW_BRANCH_ADD_BUDGET + legacy_attribute_index,
        "new branch add: {}",
        new_branch.join("\n")
    );
    for view in [
        "cold-view",
        "cached-view",
        "existing-view",
        "named-view",
        "new-view",
    ] {
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
    for view in [
        "new-view",
        "named-view",
        "existing-view",
        "cached-view",
        "cold-view",
    ] {
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
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", shim_directory.join("git"))
        .env_remove("RIFTRI_BYPASS")
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
        .expect("counting wrapper must have run")
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
            (1..=2).contains(&listings),
            "{name} listed Git worktrees {listings} times for 6 worktrees:\n{}",
            calls.join("\n")
        );
    }
}

/// Git invocations for `riftri worktree prune`: one repository inspection
/// (three processes), then a state-directory read and one worktree listing in
/// each of the three verification passes, and the prune itself. Neither the
/// number of managed worktrees nor of registered state directories adds to it.
const PRUNE_BUDGET: usize = 10;
/// The shim also reads the opt-in, the state directories it plans from, and
/// the inventory each one's accounting lists, but never inspects twice. This
/// is the count for one registered state directory.
const SHIM_PRUNE_BUDGET: usize = 13;
/// One inspection, one listing under the metadata lock, the cleanliness
/// checks before and after quarantine, and Git's removal.
const REMOVE_BUDGET: usize = 11;
/// The shim adds the opt-in, path resolution, and the ownership lookup.
const SHIM_REMOVE_BUDGET: usize = 15;
/// One inspection, the inventory, the locked registration check, Git's move,
/// and the check that Git moved it.
const MOVE_BUDGET: usize = 7;
const SHIM_MOVE_BUDGET: usize = 11;
/// `--version`, the report inspection (three processes), the opt-in, one
/// configuration snapshot, and the tree and attribute reads.
const DOCTOR_BUDGET: usize = 9;

fn run_shim_with_counted_git(
    repository: &Path,
    arguments: &[&str],
    shim_directory: &Path,
    real_git: &Path,
    log: &Path,
) -> Vec<String> {
    let riftri_as_git = shim_directory.join("riftri-as-git");
    if !riftri_as_git.exists() {
        fs::create_dir(&riftri_as_git).expect("create Riftri shim directory");
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_riftri"), riftri_as_git.join("git"))
            .expect("link Riftri as git");
    }
    let output = Command::new(riftri_as_git.join("git"))
        .args(arguments)
        .current_dir(repository)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", shim_directory.join("git"))
        .env_remove("RIFTRI_BYPASS")
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_GIT_LOG", log)
        .output()
        .expect("run the Riftri Git shim");
    assert!(
        output.status.success(),
        "git {arguments:?} through the shim failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(log)
        .expect("counting wrapper must have run")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_budget(name: &str, calls: &[String], budget: usize) {
    assert_eq!(
        calls.len(),
        budget,
        "{name} spawned {} Git processes, expected exactly {budget}; update the \
         budget only with the same scrutiny as a safety check:\n{}",
        calls.len(),
        calls.join("\n")
    );
}

/// Prune, remove, move, and doctor inspected the same repository several
/// times per command, and prune once more per verification pass and per
/// state directory: 31 Git processes for a prune, 40 through the shim. Each
/// budget is exact, so a regression fails here, and so does an improvement
/// that forgets to tighten it.
#[test]
fn lifecycle_commands_stay_within_their_git_invocation_budgets() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
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
    let riftri = |arguments: &[&std::ffi::OsStr]| {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(arguments)
            .current_dir(&repository)
            .env_remove("RIFTRI_SHIM_ACTIVE")
            .env_remove("RIFTRI_REAL_GIT")
            .output()
            .expect("run Riftri CLI");
        assert!(
            output.status.success(),
            "riftri {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    riftri(&["enable".as_ref()]);
    for view in ["view-1", "view-2", "view-3"] {
        riftri(&[
            "worktree".as_ref(),
            "add".as_ref(),
            "--detach".as_ref(),
            fixture.path().join(view).as_os_str(),
            "HEAD".as_ref(),
            "--no-progress".as_ref(),
        ]);
    }
    // A second registered state directory must not add a listing per pass.
    riftri(&[
        "worktree".as_ref(),
        "add".as_ref(),
        "--detach".as_ref(),
        fixture.path().join("elsewhere-view").as_os_str(),
        "HEAD".as_ref(),
        "--no-progress".as_ref(),
        "--state-dir".as_ref(),
        fixture.path().join("elsewhere-state").as_os_str(),
    ]);
    let real_git = real_git();
    install_counting_shim(&shim_directory);
    let legacy_attribute_index = usize::from(
        !riftri_git::Git::new(&real_git)
            .detect()
            .unwrap()
            .supports_attribute_source(),
    );
    let view = |name: &str| fixture.path().join(name).to_str().unwrap().to_owned();
    let log = |name: &str| fixture.path().join(format!("{name}-invocations"));

    let calls = run_with_counted_git(
        &repository,
        &["worktree", "prune"],
        &shim_directory,
        &real_git,
        &log("prune"),
    );
    assert_budget("prune", &calls, PRUNE_BUDGET);
    let calls = run_shim_with_counted_git(
        &repository,
        &["worktree", "prune"],
        &shim_directory,
        &real_git,
        &log("shim-prune"),
    );
    // Planning accounts for each state directory, listing once for each.
    assert_budget("prune through the shim", &calls, SHIM_PRUNE_BUDGET + 1);
    assert!(
        calls.iter().any(|call| call == "worktree prune"),
        "the shim must run the journaled prune, not pass it through:\n{}",
        calls.join("\n")
    );

    let calls = run_with_counted_git(
        &repository,
        &["doctor"],
        &shim_directory,
        &real_git,
        &log("doctor"),
    );
    assert_budget("doctor", &calls, DOCTOR_BUDGET + legacy_attribute_index);

    let calls = run_with_counted_git(
        &repository,
        &["worktree", "move", &view("view-1"), &view("moved-1")],
        &shim_directory,
        &real_git,
        &log("move"),
    );
    assert_budget("move", &calls, MOVE_BUDGET);
    let calls = run_shim_with_counted_git(
        &repository,
        &["worktree", "move", &view("view-2"), &view("moved-2")],
        &shim_directory,
        &real_git,
        &log("shim-move"),
    );
    assert_budget("move through the shim", &calls, SHIM_MOVE_BUDGET);

    let calls = run_with_counted_git(
        &repository,
        &["worktree", "remove", &view("moved-1")],
        &shim_directory,
        &real_git,
        &log("remove"),
    );
    assert_budget("remove", &calls, REMOVE_BUDGET);
    let calls = run_shim_with_counted_git(
        &repository,
        &["worktree", "remove", &view("moved-2")],
        &shim_directory,
        &real_git,
        &log("shim-remove"),
    );
    assert_budget("remove through the shim", &calls, SHIM_REMOVE_BUDGET);

    for removed in ["view-1", "view-2", "moved-1", "moved-2"] {
        assert!(!fixture.path().join(removed).exists(), "{removed} remains");
    }
    for kept in ["view-3", "elsewhere-view"] {
        assert!(
            git(
                &fixture.path().join(kept),
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .stdout
            .is_empty(),
            "{kept} must stay a clean worktree"
        );
    }
    let listed = String::from_utf8(git(&repository, &["worktree", "list", "--porcelain"]).stdout)
        .expect("UTF-8 listing");
    assert_eq!(listed.matches("\nworktree ").count(), 2, "{listed}");
}
