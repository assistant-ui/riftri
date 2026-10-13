//! Property tests for the proxy's lifecycle guard, against a repository that
//! really has a managed worktree.
//!
//! Once a worktree is managed, removing, moving, or pruning it must go
//! through the journal. However Git can be asked to do one of those, the plan
//! is the journaled operation or a refusal, never a passthrough.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hegel::generators as gs;
use hegel::{Hegel, Settings, TestCase};
use tempfile::tempdir;

use super::hegel_tests::os;
use super::tests::{git, repository_fixture};
use super::{
    ActivationError, GitProxyPlan, enable_repository, plan_git_command, resolve_worktree_binding,
};

/// Planning spawns Git several times and reads the state directory.
const PLAN_CASES: u64 = 60;

/// An enabled repository with one managed worktree inside it.
struct ManagedFixture {
    repository: tempfile::TempDir,
    view: PathBuf,
    /// A second managed worktree whose name contains U+00A0.
    spaced_view: PathBuf,
}

impl ManagedFixture {
    const VIEW: &str = "managed view";

    fn new() -> Self {
        let repository = repository_fixture();
        enable_repository(repository.path()).expect("enable repository");
        let view = repository.path().join(Self::VIEW);
        let spaced_view = repository.path().join("no\u{a0}break");
        for (destination, branch) in [(&view, "feature/managed"), (&spaced_view, "feature/spaced")]
        {
            crate::add_worktree(crate::AddWorktreeRequest {
                repository: repository.path().to_path_buf(),
                destination: destination.clone(),
                revision: os("HEAD"),
                mode: crate::WorktreeMode::NewBranch(os(branch)),
                state_dir: None,
                sparse_directories: Vec::new(),
            })
            .expect("create managed worktree");
        }
        Self {
            repository,
            view,
            spaced_view,
        }
    }

    fn path(&self) -> &Path {
        self.repository.path()
    }

    fn plan(&self, arguments: &[OsString]) -> Result<GitProxyPlan, ActivationError> {
        plan_git_command(self.path(), arguments)
    }

    /// One of the ways a command line can name the managed worktree.
    fn view_spelling(&self, tc: &TestCase) -> OsString {
        match tc.draw(gs::integers::<u8>().max_value(3)) {
            0 => self.view.clone().into_os_string(),
            1 => os(Self::VIEW),
            2 => os(&format!("./{}", Self::VIEW)),
            _ => os(&format!("{}/", Self::VIEW)),
        }
    }

    fn is_view(&self, path: &Path) -> bool {
        fs::canonicalize(path).ok() == fs::canonicalize(&self.view).ok()
    }
}

fn is_refusal(plan: &Result<GitProxyPlan, ActivationError>) -> bool {
    matches!(plan, Err(ActivationError::UnsupportedWorktreeCommand(_)))
}

/// Options in front of the command, with whether they keep the optimized
/// plans available. None of them changes which repository Git uses.
fn global_prefix(tc: &TestCase) -> (Vec<OsString>, bool) {
    let (words, optimizable): (&[&str], bool) = match tc.draw(gs::integers::<u8>().max_value(9)) {
        0 => (&["--no-optional-locks"], false),
        1 | 2 => (&["-c", "core.quotePath=false"], false),
        3 | 4 => (&["--attr-source=HEAD"], false),
        5 => (&["--no-pager"], true),
        6 => (&["-C", "."], true),
        _ => (&[], true),
    };
    (words.iter().map(|word| os(word)).collect(), optimizable)
}

/// The ways Git can be told to force-remove a worktree: spelled out, behind
/// invocation or stored aliases, with quoting and escapes in the alias body.
/// Alias names avoid Git's own commands: Git never expands an alias such as
/// `rm` over its builtin.
fn force_remove_command(tc: &TestCase, selector: OsString) -> Vec<OsString> {
    let alias =
        |body: &str, name: &str| vec![os("-c"), os(&format!("alias.{name}={body}")), os(name)];
    let mut arguments = match tc.draw(gs::integers::<u8>().max_value(6)) {
        0 => vec![os("worktree"), os("remove"), os("--force")],
        1 => vec![os("worktree"), os("remove"), os("-f"), os("--")],
        2 => alias("worktree remove --force", "drop"),
        3 => alias("worktree 'remove' \"--force\"", "DROP"),
        4 => alias(r"w\orktree remove -f", "drop"),
        5 => vec![os("stored-force-remove")],
        _ => vec![
            os("-c"),
            os("alias.outer=inner --force"),
            os("-c"),
            os("alias.inner=worktree remove"),
            os("outer"),
        ],
    };
    arguments.push(selector);
    arguments
}

#[test]
fn a_managed_worktree_is_never_removed_or_moved_outside_the_journal() {
    let fixture = ManagedFixture::new();
    git(
        fixture.path(),
        &[
            "config",
            "alias.stored-force-remove",
            "worktree remove --force",
        ],
    );
    Hegel::new(|tc: TestCase| {
        let (mut arguments, _) = global_prefix(&tc);
        let selector = fixture.view_spelling(&tc);
        if tc.draw(gs::booleans()) {
            arguments.extend(force_remove_command(&tc, selector));
        } else {
            arguments.extend([os("worktree"), os("move")]);
            arguments.extend(tc.draw(gs::booleans()).then(|| os("--force")));
            arguments.extend(tc.draw(gs::booleans()).then(|| os("--")));
            arguments.extend([selector, os("moved")]);
        }
        let plan = fixture.plan(&arguments);
        assert!(
            !matches!(plan, Ok(GitProxyPlan::Passthrough)),
            "{arguments:?} reached Git unguarded"
        );
        assert!(
            fixture.view.join(".git").is_file(),
            "planning changed the view"
        );
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

/// `git worktree remove` is the journaled removal exactly when it names one
/// managed worktree and nothing the journaled removal cannot honor. Anything
/// else that names the managed worktree is refused, and a path Riftri does
/// not manage is Git's.
#[test]
fn removals_are_planned_from_gits_argument_syntax() {
    let fixture = ManagedFixture::new();
    Hegel::new(|tc: TestCase| {
        let (mut arguments, optimizable) = global_prefix(&tc);
        arguments.extend([os("worktree"), os("remove")]);
        let flags = tc
            .draw(gs::vecs(gs::sampled_from(vec!["--force", "-f", "-x", "--forced"])).max_size(2));
        let force = flags.iter().any(|flag| matches!(*flag, "--force" | "-f"));
        let unknown = flags.iter().any(|flag| matches!(*flag, "-x" | "--forced"));
        arguments.extend(flags.into_iter().map(os));
        arguments.extend(tc.draw(gs::booleans()).then(|| os("--")));
        let managed = tc.draw(gs::booleans());
        let positionals = tc.draw(gs::integers::<usize>().max_value(2));
        for index in 0..positionals {
            arguments.push(if managed && index == 0 {
                fixture.view_spelling(&tc)
            } else {
                os("unmanaged")
            });
        }

        let plan = fixture.plan(&arguments);
        let names_the_view = managed && positionals > 0;
        if !names_the_view {
            assert!(
                matches!(plan, Ok(GitProxyPlan::Passthrough)),
                "{arguments:?}: {plan:?}"
            );
        } else if !optimizable || unknown || positionals != 1 {
            assert!(is_refusal(&plan), "{arguments:?}: {plan:?}");
        } else {
            let request = match plan {
                Ok(GitProxyPlan::OptimizedForceRemove(request)) if force => request,
                Ok(GitProxyPlan::OptimizedRemove(request)) if !force => request,
                other => panic!("{arguments:?} was planned as {other:?}"),
            };
            assert_eq!(request.repository, fixture.path());
            assert!(fixture.is_view(&request.destination), "{request:?}");
            assert!(request.state_dir.is_some_and(|state| state.is_dir()));
        }
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

#[test]
fn moves_are_planned_from_gits_argument_syntax() {
    let fixture = ManagedFixture::new();
    Hegel::new(|tc: TestCase| {
        let (mut arguments, optimizable) = global_prefix(&tc);
        arguments.extend([os("worktree"), os("move")]);
        let forced = tc.draw(gs::integers::<u8>().max_value(4)) == 0;
        arguments.extend(forced.then(|| os("--force")));
        let separator = tc.draw(gs::booleans());
        arguments.extend(separator.then(|| os("--")));
        let managed = tc.draw(gs::booleans());
        let positionals = tc.draw(gs::integers::<usize>().max_value(3));
        let destination = tc.draw(gs::sampled_from(vec!["moved", "./moved", "sub/moved"]));
        for index in 0..positionals {
            arguments.push(match index {
                0 if managed => fixture.view_spelling(&tc),
                0 => os("unmanaged"),
                _ => os(destination),
            });
        }

        let plan = fixture.plan(&arguments);
        let names_the_view = managed && positionals > 0;
        if !names_the_view {
            assert!(
                matches!(plan, Ok(GitProxyPlan::Passthrough)),
                "{arguments:?}: {plan:?}"
            );
        } else if !optimizable || forced || positionals != 2 {
            assert!(is_refusal(&plan), "{arguments:?}: {plan:?}");
        } else {
            let Ok(GitProxyPlan::OptimizedMove(request)) = plan else {
                panic!("{arguments:?} was planned as {plan:?}");
            };
            assert_eq!(request.repository, fixture.path());
            assert!(fixture.is_view(&request.source), "{request:?}");
            assert_eq!(request.destination, fixture.path().join(destination));
            assert!(request.state_dir.is_some_and(|state| state.is_dir()));
        }
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

/// With managed state present, `git worktree prune` reaches Git only when it
/// cannot prune: help, or a dry run made of documented options. A bare or
/// verbose prune is the journaled prune, and everything else is refused.
#[test]
fn prunes_are_journaled_unless_they_only_report() {
    let fixture = ManagedFixture::new();
    Hegel::new(|tc: TestCase| {
        let (mut arguments, _) = global_prefix(&tc);
        let significant = arguments
            .first()
            .is_some_and(|option| option == "-c" || option == "--attr-source=HEAD");
        arguments.extend([os("worktree"), os("prune")]);
        // A third of the cases are a plain or verbose prune, the only form
        // that is journaled.
        let choices = if tc.draw(gs::integers::<u8>().max_value(2)) == 0 {
            vec!["-v", "--verbose"]
        } else {
            vec![
                "-n",
                "--dry-run",
                "-v",
                "--verbose",
                "--expire",
                "now",
                "--expire=now",
                "-h",
                "--help",
                "--unknown",
            ]
        };
        let options = tc.draw(gs::vecs(gs::sampled_from(choices)).max_size(4));
        arguments.extend(options.iter().copied().map(os));

        let help = options
            .iter()
            .any(|option| matches!(*option, "-h" | "--help"));
        let dry_run = options
            .iter()
            .any(|option| matches!(*option, "-n" | "--dry-run"));
        let verbose_only = options
            .iter()
            .all(|option| matches!(*option, "-v" | "--verbose"));
        // `--expire` takes the next word as its value, whatever that word is.
        let mut expire_value_pending = false;
        let documented = options.iter().all(|option| {
            if std::mem::take(&mut expire_value_pending) {
                return true;
            }
            expire_value_pending = *option == "--expire";
            !matches!(*option, "now" | "--unknown")
        }) && !expire_value_pending;

        let plan = fixture.plan(&arguments);
        if help || (dry_run && documented) {
            assert!(
                matches!(plan, Ok(GitProxyPlan::Passthrough)),
                "{arguments:?}: {plan:?}"
            );
        } else if verbose_only && !significant {
            let Ok(GitProxyPlan::OptimizedPrune(request)) = plan else {
                panic!("{arguments:?} was planned as {plan:?}");
            };
            assert_eq!(request.repository, fixture.path());
            assert!(request.state_dir.is_some_and(|state| state.is_dir()));
        } else {
            assert!(is_refusal(&plan), "{arguments:?}: {plan:?}");
        }
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

fn assert_invalid_binding(error: &ActivationError) {
    assert!(
        matches!(error, ActivationError::WorktreeBinding(_)),
        "not a binding refusal: {error:?}"
    );
    assert_eq!(
        crate::policy_refusal_code(error),
        Some("invalid-worktree-binding")
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    MainRoot,
    LinkedRoot,
    SymlinkToRoot,
    Subdirectory,
    File,
    Missing,
    Empty,
    PlainDirectory,
    BareRepository,
}

/// `riftri exec --worktree` binds only to the exact root of a live worktree,
/// however that root is spelled. Every other path is refused as a binding
/// problem, which callers report as a policy refusal, not an operational one.
#[test]
fn worktree_bindings_accept_exactly_live_worktree_roots() {
    let fixture = ManagedFixture::new();
    let elsewhere = tempdir().expect("unrelated directories");
    let plain = elsewhere.path().join("plain");
    let bare = elsewhere.path().join("bare.git");
    let link = elsewhere.path().join("link");
    fs::create_dir(&plain).expect("plain directory");
    fs::create_dir(&bare).expect("bare directory");
    git(&bare, &["init", "--quiet", "--bare"]);
    symlink(fixture.path(), &link).expect("symlink to the repository");
    fs::create_dir(fixture.path().join("nested")).expect("subdirectory");

    Hegel::new(|tc: TestCase| {
        let binding = tc.draw(gs::sampled_from(vec![
            Binding::MainRoot,
            Binding::LinkedRoot,
            Binding::SymlinkToRoot,
            Binding::Subdirectory,
            Binding::File,
            Binding::Missing,
            Binding::Empty,
            Binding::PlainDirectory,
            Binding::BareRepository,
        ]));
        let mut requested = match binding {
            Binding::MainRoot => fixture.path().to_path_buf(),
            Binding::LinkedRoot => fixture.view.clone(),
            Binding::SymlinkToRoot => link.clone(),
            Binding::Subdirectory => fixture.path().join("nested"),
            Binding::File => fixture.path().join("tracked.txt"),
            Binding::Missing => fixture.path().join("absent"),
            Binding::Empty => PathBuf::new(),
            Binding::PlainDirectory => plain.clone(),
            Binding::BareRepository => bare.clone(),
        };
        // Spellings that resolve to the same directory.
        if binding != Binding::Empty && binding != Binding::File && binding != Binding::Missing {
            for _ in 0..tc.draw(gs::integers::<u8>().max_value(2)) {
                requested.push(".");
            }
        }

        let bound = resolve_worktree_binding(&requested);
        let is_root = matches!(
            binding,
            Binding::MainRoot | Binding::LinkedRoot | Binding::SymlinkToRoot
        );
        match bound {
            Ok(root) => {
                assert!(is_root, "{binding:?} was bound to {}", root.display());
                assert_eq!(root, fs::canonicalize(&requested).expect("canonical root"));
            }
            Err(error) => {
                assert!(!is_root, "{binding:?} was refused: {error}");
                assert_invalid_binding(&error);
            }
        }
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

/// A directory inside a repository's Git directory is not a worktree root
/// either, so naming one is the same caller mistake as naming a subdirectory.
#[test]
fn worktree_bindings_refuse_git_directories_as_bindings() {
    let fixture = ManagedFixture::new();
    Hegel::new(|tc: TestCase| {
        let inside = tc.draw(gs::sampled_from(vec![
            ".git",
            ".git/objects",
            ".git/refs/heads",
            ".git/worktrees",
        ]));
        let error = resolve_worktree_binding(&fixture.path().join(inside))
            .expect_err("a Git directory was bound as a worktree");
        assert_invalid_binding(&error);
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}

/// Ways to force-remove the managed worktree that real Git executes (see the
/// oracle properties in `activation_hegel_tests.rs`) and the proxy once
/// misread.
fn misread_force_remove(tc: &TestCase, fixture: &ManagedFixture) -> Vec<OsString> {
    let view = fixture.view.clone().into_os_string();
    let alias = [
        os("-c"),
        os("alias.drop=worktree remove --force"),
        os("drop"),
    ];
    let mut arguments: Vec<OsString> = match tc.draw(gs::integers::<u8>().max_value(4)) {
        // The value of a global option the proxy has no rule for.
        0 => [os("--attr-source"), os("HEAD")]
            .into_iter()
            .chain(alias)
            .collect(),
        1 => [os("--config-env"), os("core.quotePath=HOME")]
            .into_iter()
            .chain(alias)
            .collect(),
        2 => ["--attr-source", "worktree", "worktree", "remove", "--force"]
            .map(os)
            .to_vec(),
        // An alias chain longer than the proxy follows.
        3 => {
            let mut arguments = Vec::new();
            for hop in 0..25 {
                let body = if hop == 24 {
                    "worktree remove --force".to_owned()
                } else {
                    format!("hop{}", hop + 1)
                };
                arguments.extend([os("-c"), os(&format!("alias.hop{hop}={body}"))]);
            }
            arguments.push(os("hop0"));
            arguments
        }
        // A path Git reads as one word because U+00A0 is not Git whitespace.
        _ => {
            let spaced = fixture.spaced_view.to_str().expect("UTF-8 fixture path");
            return vec![
                os("-c"),
                os(&format!("alias.drop=worktree remove --force {spaced}")),
                os("drop"),
            ];
        }
    };
    arguments.push(view);
    arguments
}

#[test]
fn a_managed_worktree_is_guarded_however_git_is_asked_to_remove_it() {
    let fixture = ManagedFixture::new();
    Hegel::new(|tc: TestCase| {
        let arguments = misread_force_remove(&tc, &fixture);
        let plan = fixture.plan(&arguments);
        assert!(
            !matches!(plan, Ok(GitProxyPlan::Passthrough)),
            "{arguments:?} reached Git unguarded"
        );
    })
    .settings(Settings::new().test_cases(PLAN_CASES))
    .run();
}
