//! How the Git shim classifies a command line: ordinary Git is never
//! intercepted, and an add it does take over means what Git means by it.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_core::{GitProxyPlan, WorktreeMode};

use crate::common::{Scratch, git, git_ok, riftri};
use crate::contract::process_cases;

fn words(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// Global options, several of which take a value that reads like the start
/// of a worktree command.
fn global_options(tc: &TestCase) -> Vec<OsString> {
    // `-C worktree` at most once, so the directory it names exists, and never
    // with a relative `--git-dir`, which Git resolves after changing into it.
    let enters_directory = tc.draw(gs::booleans());
    let mut choices = vec![
        &["--no-pager"][..],
        &["-p"],
        &["--literal-pathspecs"],
        &["--no-optional-locks"],
        &["-C", "."],
        &["-c", "core.pager=worktree"],
        &["-c", "alias.add=status"],
        &["--namespace", "worktree"],
    ];
    if !enters_directory {
        choices.extend([&["--git-dir", ".git"][..], &["--work-tree", "."]]);
    }
    let mut options = Vec::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(3)) {
        options.extend(words(tc.draw(gs::sampled_from(choices.clone()))));
    }
    if enters_directory {
        options.extend(words(&["-C", "worktree"]));
    }
    options
}

fn enabled_repository(scratch: &Scratch) -> std::path::PathBuf {
    let repository = scratch.repository("repository");
    git_ok(&repository, &["branch", "side"]);
    git_ok(&repository, &["tag", "v1"]);
    for directory in ["worktree", "sub dir"] {
        std::fs::create_dir(repository.join(directory)).expect("create directory");
    }
    let enabled = riftri()
        .arg("enable")
        .arg(&repository)
        .output()
        .expect("enable repository");
    assert!(enabled.status.success());
    repository
}

#[test]
fn ordinary_git_commands_are_never_intercepted() {
    let scratch = Scratch::new();
    let repository = enabled_repository(&scratch);

    hegel::Hegel::new(|tc: TestCase| {
        let mut arguments = global_options(&tc);
        arguments.extend(match tc.draw(gs::integers::<u8>().max_value(2)) {
            // Commands that only mention worktrees.
            0 => words(tc.draw(gs::sampled_from(vec![
                &["status"][..],
                &["add", "worktree"],
                &["commit", "-m", "worktree add ../view"],
                &["log", "--", "worktree", "add"],
                &["grep", "worktree", "--", "add"],
                &["branch", "worktree"],
                &["checkout", "worktree", "--", "add"],
                &["rm", "worktree", "remove"],
                &["mv", "worktree", "move"],
                &["prune"],
                &["version"],
            ]))),
            // Worktree commands Riftri does not own.
            1 => words(tc.draw(gs::sampled_from(vec![
                &["worktree"][..],
                &["worktree", "list"],
                &["worktree", "list", "--porcelain", "-z"],
                &["worktree", "lock", "add"],
                &["worktree", "unlock", "remove"],
                &["worktree", "repair", "move"],
                &["worktree", "bogus", "add", "../view"],
            ]))),
            // Anything at all that is not the `worktree` command.
            _ => {
                let mut command = vec![OsString::from(tc.draw(gs::sampled_from(vec![
                    "rev-parse",
                    "ls-files",
                    "show",
                    "no-such-command",
                ])))];
                for _ in 0..tc.draw(gs::integers::<usize>().max_value(3)) {
                    command.push(OsString::from_vec(tc.draw(gs::binary().max_size(12))));
                }
                command
            }
        });

        let plan = riftri_core::plan_git_command(&repository, &arguments);

        assert!(
            matches!(plan, Ok(GitProxyPlan::Passthrough)),
            "git {arguments:?} planned as {plan:?}"
        );
    })
    .settings(process_cases(200))
    .run();
}

/// The worktree Git lists at `path`: its HEAD commit and branch, if attached.
fn listed_worktree(repository: &Path, path: &Path) -> Option<(String, Option<String>)> {
    let listing = git_ok(repository, &["worktree", "list", "--porcelain"]);
    listing.split("\n\n").find_map(|entry| {
        let mut lines = entry.lines();
        (lines.next()?.strip_prefix("worktree ") == path.to_str()).then(|| {
            let head = entry
                .lines()
                .find_map(|line| line.strip_prefix("HEAD "))
                .expect("worktree HEAD");
            let branch = entry
                .lines()
                .find_map(|line| line.strip_prefix("branch refs/heads/"));
            (head.to_owned(), branch.map(str::to_owned))
        })
    })
}

#[hegel::test(process_cases(20))]
fn an_add_the_shim_takes_over_means_what_git_means(tc: TestCase) {
    let scratch = Scratch::new();
    let repository = enabled_repository(&scratch);
    let current_directory = if tc.draw(gs::booleans()) {
        repository.clone()
    } else {
        repository.join("sub dir")
    };

    let mut arguments = words(tc.draw(gs::sampled_from(vec![
        &[][..],
        &["--no-pager"],
        &["-C", "."],
        &["--literal-pathspecs"],
    ])));
    arguments.extend(words(&["worktree", "add"]));
    let mode = words(tc.draw(gs::sampled_from(vec![
        &[][..],
        &["-b", "topic"],
        &["-b", "side"],
        &["-btopic"],
        &["--detach"],
        &["-d"],
    ])));
    let mut location = words(tc.draw(gs::sampled_from(vec![
        &["../view"][..],
        &["../../view with spaces"],
        &["--", "../view"],
        &["nested/view"],
    ])));
    location.extend(words(tc.draw(gs::sampled_from(vec![
        &[][..],
        &["side"],
        &["main"],
        &["HEAD"],
        &["v1"],
        &["side~0"],
        &["missing"],
    ]))));
    let quiet = words(tc.draw(gs::sampled_from(vec![&[][..], &["-q"], &["--quiet"]])));
    // Git accepts its options before, between, and after the positionals.
    let order = tc.draw(gs::sampled_from(vec![
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [2, 0, 1],
    ]));
    let mut parts = [Some(quiet), Some(mode), Some(location)];
    let dashed = parts[2]
        .as_ref()
        .is_some_and(|location| location[0] == "--");
    for index in if dashed { [0, 1, 2] } else { order } {
        arguments.extend(parts[index].take().expect("each part once"));
    }

    let plan = riftri_core::plan_git_command(&current_directory, &arguments);

    // Refusing a form the optimized path does not support is fail-closed.
    let Ok(plan) = plan else {
        return;
    };
    assert!(
        matches!(
            plan,
            GitProxyPlan::Passthrough | GitProxyPlan::OptimizedAdd { .. }
        ),
        "git {arguments:?} planned as {plan:?}"
    );
    let GitProxyPlan::OptimizedAdd { request, .. } = plan else {
        // Leaving an add to ordinary Git is always safe.
        return;
    };
    let commit_before = git(&current_directory)
        .arg("rev-parse")
        .arg("--verify")
        .arg("--quiet")
        .arg({
            let mut revision = request.revision.clone();
            revision.push("^{commit}");
            revision
        })
        .output()
        .expect("resolve revision");
    let branches_before = git_ok(&repository, &["for-each-ref", "--format=%(refname)"]);

    let real = git(&current_directory)
        .args(&arguments)
        .output()
        .expect("run real git");
    if !real.status.success() {
        // Riftri's own add re-checks the request and may refuse it as well.
        return;
    }

    // Git created the directory, so the planned destination now resolves.
    let created = request
        .destination
        .canonicalize()
        .unwrap_or_else(|_| request.destination.clone());
    let (head, branch) = listed_worktree(&repository, &created).unwrap_or_else(|| {
        panic!(
            "git {arguments:?} did not create {}",
            request.destination.display()
        )
    });
    assert_eq!(
        String::from_utf8_lossy(&commit_before.stdout).trim_end(),
        head,
        "git {arguments:?} starts at another commit than {:?}",
        request.revision
    );
    match &request.mode {
        WorktreeMode::NewBranch(name) => {
            let name = name.to_str().expect("branch name");
            assert_eq!(branch.as_deref(), Some(name), "git {arguments:?}");
            assert!(
                !branches_before.contains(&format!("refs/heads/{name}\n")),
                "git {arguments:?} reused an existing branch"
            );
        }
        WorktreeMode::ExistingBranch(name) => {
            assert_eq!(branch.as_deref(), name.to_str(), "git {arguments:?}");
        }
        WorktreeMode::Detached => assert_eq!(branch, None, "git {arguments:?}"),
    }
}

/// The shim must not answer for Git when Git itself would refuse to start.
#[hegel::test(process_cases(12))]
#[ignore = "bug: the shim exits 1 with its own error where Git cannot enter -C or open --git-dir"]
fn a_repository_git_cannot_open_is_gits_error_to_report(tc: TestCase) {
    let scratch = Scratch::new();
    let root = scratch.root();
    std::fs::write(root.join("file"), "").expect("create file");
    let unusable = tc.draw(gs::sampled_from(vec!["missing", "file", "file/below"]));
    let mut arguments = words(tc.draw(gs::sampled_from(vec![&["-C"][..], &["--git-dir"]])));
    arguments.push(unusable.into());
    arguments.extend(words(tc.draw(gs::sampled_from(vec![
        &["status"][..],
        &["rm", "tracked.txt"],
        &["mv", "a", "b"],
        &["frobnicate"],
        &["worktree", "list"],
    ]))));
    let run = |mut process: std::process::Command| {
        process
            .current_dir(&root)
            .args(&arguments)
            .output()
            .expect("run git")
    };

    let real = run(git(&root));
    let mut shimmed = riftri();
    shimmed.args(["exec", "--", "git"]);
    let shimmed = run(shimmed);

    assert_eq!(
        shimmed.status.code(),
        real.status.code(),
        "git {arguments:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&shimmed.stderr),
        String::from_utf8_lossy(&real.stderr),
        "git {arguments:?}"
    );
    assert_eq!(shimmed.stdout, real.stdout, "git {arguments:?}");
}
