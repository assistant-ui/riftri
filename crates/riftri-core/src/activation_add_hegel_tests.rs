//! Property tests for what the proxy does with `git worktree add` and with
//! everything it does not claim.
//!
//! An optimized add replaces Git's own, so it has to mean what Git means:
//! whenever the proxy plans one, real Git given the same arguments creates
//! the worktree the plan describes. Anything the optimized add cannot honor
//! is refused, and anything else reaches Git untouched.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use hegel::generators as gs;
use hegel::{Hegel, Settings, TestCase};
use tempfile::tempdir;

use super::hegel_tests::{GIT_CASES, global_option, os, real_git, run, stdout_line};
use super::tests::{git, repository_fixture};
use super::{
    ActivationError, GitProxyPlan, GlobalOptionScope, enable_repository, plan_git_command,
};
use crate::WorktreeMode;

/// One enabled repository with a second local branch and a tag, reset to that
/// shape before every case.
struct AddFixture {
    repository: tempfile::TempDir,
}

impl AddFixture {
    const VIEW: &str = "view";
    const NEW_BRANCH: &str = "feature/new";

    fn new() -> Self {
        let repository = repository_fixture();
        git(repository.path(), &["branch", "existing"]);
        git(repository.path(), &["tag", "v1"]);
        enable_repository(repository.path()).expect("enable repository");
        Self { repository }
    }

    fn path(&self) -> &Path {
        self.repository.path()
    }

    fn reset(&self) {
        let view = self.path().join(Self::VIEW);
        if view.exists() {
            fs::remove_dir_all(view).expect("remove view");
        }
        run(real_git(self.path()).args(["worktree", "prune"]));
        run(real_git(self.path()).args(["branch", "-D", Self::NEW_BRANCH]));
    }

    fn plan_add(&self, options: &[OsString]) -> Result<GitProxyPlan, ActivationError> {
        let mut arguments = vec![os("worktree"), os("add")];
        arguments.extend_from_slice(options);
        plan_git_command(self.path(), &arguments)
    }
}

/// `git worktree add` arguments from the syntax the optimized add supports:
/// spaced, bundled, and stuck short options, long options, explicit defaults,
/// options before or after the path, and an optional `--`.
fn supported_add_arguments<'a>(tc: &TestCase, absolute_view: &'a str) -> (Vec<OsString>, bool) {
    let mut units: Vec<Vec<&'a str>> = Vec::new();
    let mode = tc.draw(gs::integers::<u8>().max_value(8));
    let quiet_cluster = tc.draw(gs::booleans());
    match (mode, quiet_cluster) {
        (0, _) => {}
        (1, _) => units.push(vec!["--detach"]),
        (2, false) => units.push(vec!["-d"]),
        (2, true) => units.push(vec![tc.draw(gs::sampled_from(vec!["-qd", "-dq"]))]),
        (3 | 4, false) => units.push(vec!["-b", AddFixture::NEW_BRANCH]),
        (3 | 4, true) => units.push(vec!["-qb", AddFixture::NEW_BRANCH]),
        (5 | 6, false) => units.push(vec!["-bfeature/new"]),
        (_, _) => units.push(vec!["-qbfeature/new"]),
    }
    for flag in tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "-q",
            "--quiet",
            "--no-quiet",
            "--checkout",
            "--no-lock",
            "--no-force",
            "--no-orphan",
            "--no-guess-remote",
        ]))
        .max_size(3),
    ) {
        units.push(vec![flag]);
    }
    let units = tc.draw(gs::permutations(units));

    let path = tc.draw(gs::sampled_from(vec![
        "view",
        "./view",
        "sub/../view",
        absolute_view,
    ]));
    let commit = tc.draw(gs::sampled_from(vec![
        None,
        Some("HEAD"),
        Some("existing"),
        Some("v1"),
    ]));
    let leading = tc.draw(gs::integers::<usize>().max_value(units.len()));
    let separator = leading == units.len() && tc.draw(gs::booleans());
    let mut arguments = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        if index == leading {
            arguments.push(path);
            arguments.extend(commit);
        }
        arguments.extend(unit);
    }
    if leading == units.len() {
        arguments.extend(separator.then_some("--"));
        arguments.push(path);
        arguments.extend(commit);
    }
    // Without a mode option only an existing local branch is optimized: any
    // other revision makes Git detach or guess, which the proxy refuses.
    let optimized = mode != 0 || commit == Some("existing");
    (arguments.into_iter().map(os).collect(), optimized)
}

/// Whenever the proxy plans an optimized add, running the same arguments
/// through real Git creates the worktree the plan describes: same path, same
/// commit, same branch or detached HEAD, and the same quietness.
#[test]
fn optimized_adds_mean_what_git_means() {
    let fixture = AddFixture::new();
    fs::create_dir(fixture.path().join("sub")).expect("subdirectory");
    let absolute_view = fixture.path().join(AddFixture::VIEW);
    let absolute_view = absolute_view
        .to_str()
        .expect("UTF-8 fixture path")
        .to_owned();
    Hegel::new(|tc: TestCase| {
        fixture.reset();
        let (options, optimized) = supported_add_arguments(&tc, &absolute_view);
        let plan = fixture.plan_add(&options);
        if !optimized {
            assert!(
                matches!(plan, Err(ActivationError::UnsupportedWorktreeCommand(_))),
                "{options:?} was planned as {plan:?}"
            );
            return;
        }
        let Ok(GitProxyPlan::OptimizedAdd { request, quiet }) = plan else {
            panic!("{options:?} was not optimized: {plan:?}");
        };

        let added = run(real_git(fixture.path())
            .args(["worktree", "add"])
            .args(&options));
        assert!(
            added.status.success(),
            "Git refused {options:?}: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert_eq!(added.stderr.is_empty(), quiet, "{options:?}");
        assert_eq!(request.repository, fixture.path());
        assert!(
            request.destination.join(".git").is_file(),
            "Git did not create {}",
            request.destination.display()
        );
        let view = fixture.path().join(AddFixture::VIEW);
        assert_eq!(
            fs::canonicalize(&request.destination).expect("planned destination"),
            fs::canonicalize(&view).expect("created view")
        );
        let commit = |directory: &Path, revision: &OsStr| {
            let mut revision = revision.to_os_string();
            revision.push("^{commit}");
            stdout_line(real_git(directory).arg("rev-parse").arg(revision))
        };
        assert_eq!(
            commit(&view, OsStr::new("HEAD")),
            commit(fixture.path(), &request.revision)
        );
        let branch =
            stdout_line(real_git(&view).args(["symbolic-ref", "--quiet", "--short", "HEAD"]));
        let expected = match &request.mode {
            WorktreeMode::Detached => None,
            WorktreeMode::NewBranch(branch) | WorktreeMode::ExistingBranch(branch) => {
                Some(branch.to_string_lossy().into_owned())
            }
        };
        assert_eq!(branch, expected, "{options:?}");
    })
    .settings(Settings::new().test_cases(60))
    .run();
}

/// One argument the optimized add cannot honor makes the whole request a
/// visible refusal, never a silently different worktree, unless the command
/// only asks Git for help.
#[test]
fn adds_the_optimized_path_cannot_honor_are_refused() {
    let fixture = AddFixture::new();
    Hegel::new(|tc: TestCase| {
        let unsupported = tc.draw(gs::sampled_from(vec![
            "--force",
            "-f",
            "--lock",
            "--orphan",
            "--track",
            "--no-track",
            "--guess-remote",
            "--no-checkout",
            "--detach=now",
            "--sparse",
            "--sparse-dir",
            "--sparse-dir=src",
            "-B",
            "-x",
            "-qf",
            "-qx",
            "--det",
        ]));
        let mut options = match tc.draw(gs::integers::<u8>().max_value(4)) {
            // A short-option cluster that is not UTF-8 cannot be read letter
            // by letter.
            0 => vec![OsString::from_vec(b"-q\xff".to_vec())],
            1 => vec![os("-d"), os("--detach")],
            2 => vec![os("-b"), os("one"), os("-d")],
            _ => vec![os(unsupported)],
        };
        options.extend([os("-b"), os(AddFixture::NEW_BRANCH), os(AddFixture::VIEW)]);
        let positionals = tc.draw(gs::integers::<u8>().max_value(9));
        match positionals {
            0 => {
                // No path at all, or one positional too many.
                options = vec![os("-d")];
            }
            1 => options = vec![os("-d"), os("a"), os("HEAD"), os("extra")],
            _ => {}
        }
        let refused = fixture.plan_add(&options);
        assert!(
            matches!(refused, Err(ActivationError::UnsupportedWorktreeCommand(_))),
            "{options:?} was planned as {refused:?}"
        );

        let help = tc.draw(gs::sampled_from(vec!["-h", "--help"]));
        let position = tc.draw(gs::integers::<usize>().max_value(options.len()));
        options.insert(position, os(help));
        assert!(
            matches!(fixture.plan_add(&options), Ok(GitProxyPlan::Passthrough)),
            "help in {options:?} did not reach Git"
        );
    })
    .settings(Settings::new().test_cases(GIT_CASES))
    .run();
}

/// What an optimized add may be preceded by: nothing that Git would apply
/// and the add would drop. Checkout-neutral options hand the add to Git
/// unchanged; anything else is refused.
#[test]
fn global_options_decide_whether_an_add_is_optimized() {
    let fixture = AddFixture::new();
    Hegel::new(|tc: TestCase| {
        let options = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
        let options = (0..options)
            .map(|_| global_option(&tc))
            .filter(|option| {
                option.change_directory.is_none()
                    && option.git_directory.is_none()
                    && option.work_tree.is_none()
            })
            .collect::<Vec<_>>();
        tc.assume(!options.is_empty());
        let mut arguments = options
            .iter()
            .flat_map(|option| option.words.clone())
            .collect::<Vec<_>>();
        arguments.extend(["worktree", "add", "-d", AddFixture::VIEW].map(os));

        let plan = plan_git_command(fixture.path(), &arguments);
        let scopes = options.iter().map(|option| option.scope);
        if scopes
            .clone()
            .any(|scope| scope == GlobalOptionScope::Significant)
        {
            assert!(matches!(
                plan,
                Err(ActivationError::UnsupportedWorktreeCommand(_))
            ));
        } else if scopes
            .clone()
            .any(|scope| scope == GlobalOptionScope::CheckoutNeutral)
        {
            assert!(matches!(plan, Ok(GitProxyPlan::Passthrough)));
        } else {
            assert!(matches!(plan, Ok(GitProxyPlan::OptimizedAdd { .. })));
        }
    })
    .settings(Settings::new().test_cases(GIT_CASES))
    .run();
}

/// Everything that is not a lifecycle command on a managed worktree is
/// ordinary Git, in a repository that opted in as much as in one that did
/// not, a bare one, or no repository at all.
#[test]
fn commands_outside_the_proxys_claim_reach_git_unchanged() {
    let enabled = AddFixture::new();
    let disabled = repository_fixture();
    let bare = tempdir().expect("bare fixture");
    git(bare.path(), &["init", "--quiet", "--bare"]);
    let plain = tempdir().expect("plain directory");
    // Stored aliases the proxy must leave alone: an empty body, a body that
    // is not UTF-8, and a chain that never reaches `worktree`.
    git(enabled.path(), &["config", "alias.empty", ""]);
    git(enabled.path(), &["config", "alias.hop", "log --oneline"]);
    let raw = run(real_git(enabled.path())
        .args(["config", "alias.raw"])
        .arg(OsStr::from_bytes(b"worktree \xff")));
    assert!(raw.status.success());

    Hegel::new(|tc: TestCase| {
        let subcommand = tc.draw(gs::sampled_from(vec![
            "add", "remove", "move", "prune", "list", "lock", "unlock", "repair",
        ]));
        let mut worktree = vec![os("worktree"), os(subcommand)];
        worktree.extend(
            tc.draw(gs::vecs(gs::sampled_from(vec!["-f", "--", "a", "b", "-v", "-n"])).max_size(3))
                .into_iter()
                .map(os),
        );
        for directory in [disabled.path(), bare.path(), plain.path()] {
            assert!(
                matches!(
                    plan_git_command(directory, &worktree),
                    Ok(GitProxyPlan::Passthrough)
                ),
                "{worktree:?} in {}",
                directory.display()
            );
        }

        let arguments = |choices: Vec<&'static str>| {
            tc.draw(gs::vecs(gs::sampled_from(choices)).max_size(4))
                .into_iter()
                .map(os)
        };
        let ordinary: Vec<OsString> = match tc.draw(gs::integers::<u8>().max_value(11)) {
            0 => Vec::new(),
            1 => vec![os("-C")],
            2 => vec![os("--no-pager"), os("-c")],
            3 => vec![OsString::from_vec(b"st\xff".to_vec())],
            4 => vec![os("empty")],
            5 => vec![os("hop")],
            6 => vec![os("raw")],
            7 => vec![os(tc.draw(gs::sampled_from(vec!["status", "unknown"])))],
            8 => vec![os("worktree")],
            9 => {
                let subcommand =
                    tc.draw(gs::sampled_from(vec!["list", "lock", "unlock", "repair"]));
                vec![os("worktree"), os(subcommand), os("a")]
            }
            // Nothing in this repository is managed, so removing, moving, and
            // pruning are Git's business however they are spelled.
            10 => [os("worktree"), os("prune")]
                .into_iter()
                .chain(arguments(vec!["-v", "--expire", "now", "--unknown", "a"]))
                .collect(),
            _ => {
                let subcommand = tc.draw(gs::sampled_from(vec!["remove", "move"]));
                [os("worktree"), os(subcommand)]
                    .into_iter()
                    .chain(arguments(vec!["--force", "-x", "--", "a", "b", "-v"]))
                    .collect()
            }
        };
        assert!(
            matches!(
                plan_git_command(enabled.path(), &ordinary),
                Ok(GitProxyPlan::Passthrough)
            ),
            "{ordinary:?} did not pass through"
        );
    })
    .settings(Settings::new().test_cases(GIT_CASES))
    .run();
}
