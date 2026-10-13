//! Property tests that put a scripted stand-in between Riftri and Git: one
//! invocation of a read-only operation is replaced with generated output or
//! an abnormal end, and the operation must refuse it instead of answering.
#![cfg(unix)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use riftri_git::{Git, GitError, ObjectId};

#[allow(dead_code)]
mod support;

use support::{End, Fixture, ScriptedGit};

struct Scene {
    fixture: Fixture,
    scripted: ScriptedGit,
    tree: ObjectId,
    blob: ObjectId,
    invocations: Vec<usize>,
}

impl Scene {
    fn new() -> Self {
        let fixture = Fixture::committed();
        let repository = fixture.repository();
        let blob = fixture.blob(b"content");
        let tree = fixture.tree(&[(0o100644, "blob", &blob, b"file")]);
        for arguments in [
            &["config", "probe.flag", "true"][..],
            &["config", "probe.path", "/somewhere"],
            &["config", "includeIf.gitdir:/elsewhere/.path", "included"],
            &["config", "--file", ".git/included", "core.eol", "lf"],
            &["sparse-checkout", "set", "--cone", "directory"],
            // A gitlink in the index, as a submodule would leave.
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},module", "1".repeat(40)),
            ],
        ] {
            support::git(&repository, arguments);
        }
        let mut scene = Self {
            fixture,
            scripted: ScriptedGit::new(),
            tree,
            blob,
            invocations: Vec::new(),
        };
        scene.invocations = invocation_counts(&scene);
        scene
    }
}

thread_local! {
    /// Every probe only reads, so the cases of one test share a repository
    /// instead of paying for `git init` each. The test thread drops it.
    static SCENE: Scene = Scene::new();
}

type Probe = fn(&Git, &Path, &Scene) -> Result<String, GitError>;

fn shown<T: std::fmt::Debug>(result: Result<T, GitError>) -> Result<String, GitError> {
    result.map(|value| format!("{value:?}"))
}

/// Read-only operations that succeed against the fixture. `recovers` marks
/// the ones documented to answer despite a failed invocation: a listing is
/// retried, an unhealthy Git LFS is reported as absent, and a probe whose
/// only job is to classify an earlier failure answers "unknown".
const PROBES: &[(&str, bool, Probe)] = &[
    ("detect", false, |git, _, _| shown(git.detect())),
    ("inspect_repository", false, |git, path, _| {
        shown(git.inspect_repository(path))
    }),
    ("inspect_repository_for_report", false, |git, path, _| {
        shown(git.inspect_repository_for_report(path))
    }),
    ("inspect_repository_for_add", true, |git, path, _| {
        shown(git.inspect_repository_for_add(path, OsStr::new("main")))
    }),
    ("resolve_revision", false, |git, path, _| {
        shown(git.resolve_revision(path, OsStr::new("HEAD")))
    }),
    ("resolve_revision by search", false, |git, path, _| {
        shown(git.resolve_revision(path, OsStr::new(":/initial")))
    }),
    ("resolve_requested_revision", false, |git, path, _| {
        shown(git.resolve_requested_revision(path, OsStr::new("main")))
    }),
    ("list_worktrees", true, |git, path, _| {
        shown(git.list_worktrees(path))
    }),
    ("resolve_worktree_path", true, |git, path, _| {
        shown(git.resolve_worktree_path(path, path, OsStr::new("repository")))
    }),
    ("worktree_root_from_git_dir", false, |git, path, _| {
        shown(git.worktree_root_from_git_dir(&path.join(".git")))
    }),
    ("list_tree", false, |git, path, scene| {
        shown(git.list_tree(path, &scene.tree))
    }),
    ("blob_size", false, |git, path, scene| {
        shown(git.blob_size(path, &scene.blob))
    }),
    ("read_blob", false, |git, path, scene| {
        shown(git.read_blob(path, &scene.blob))
    }),
    ("blob_sizes", false, |git, path, scene| {
        shown(git.blob_sizes(path, std::slice::from_ref(&scene.blob)))
    }),
    ("read_small_blobs", false, |git, path, scene| {
        shown(git.read_small_blobs(path, std::slice::from_ref(&scene.blob), 64))
    }),
    ("lfs_version", true, |git, path, _| {
        shown(git.lfs_version(path))
    }),
    ("has_config_matching", false, |git, path, _| {
        shown(git.has_config_matching(path, "^probe\\."))
    }),
    ("conditional_config_has_only", true, |git, path, _| {
        shown(git.conditional_config_has_only(path, &["core.eol"]))
    }),
    ("config_value", false, |git, path, _| {
        shown(git.config_value(path, "probe.flag"))
    }),
    ("config_values", false, |git, path, _| {
        shown(git.config_values(path, &["probe.flag", "probe.path"]))
    }),
    ("config_value_origins", false, |git, path, _| {
        shown(git.config_value_origins(path, "probe.flag"))
    }),
    ("local_config_value", false, |git, path, _| {
        shown(git.local_config_value(path, "probe.flag"))
    }),
    ("local_config_paths", false, |git, path, _| {
        shown(git.local_config_paths(path, "probe.path"))
    }),
    ("local_config_bool", false, |git, path, _| {
        shown(git.local_config_bool(path, "probe.flag"))
    }),
    ("expand_config_path", false, |git, _, _| {
        shown(git.expand_config_path(OsStr::new("/somewhere")))
    }),
    ("info_attributes_path", false, |git, path, _| {
        shown(git.info_attributes_path(path))
    }),
    ("effective_attributes_for_paths", false, |git, path, _| {
        shown(git.effective_attributes_for_paths(path, &[PathBuf::from("file")]))
    }),
    ("in_tree_attributes_for_paths", false, |git, path, scene| {
        shown(git.in_tree_attributes_for_paths(path, &scene.tree, &[PathBuf::from("file")]))
    }),
    ("worktree_removal_state", false, |git, path, _| {
        shown(git.worktree_removal_state(path))
    }),
    ("worktree_index_has_changes", false, |git, path, _| {
        shown(git.worktree_index_has_changes(path))
    }),
    ("sparse_checkout_state", false, |git, path, _| {
        shown(git.sparse_checkout_state(path))
    }),
    ("symbolic_head_branch", false, |git, path, _| {
        shown(git.symbolic_head_branch(path))
    }),
    ("has_any_reference", false, |git, path, _| {
        shown(git.has_any_reference(path))
    }),
    ("worktree_is_clean", false, |git, path, _| {
        shown(git.worktree_is_clean(path))
    }),
    ("worktree_is_pristine", false, |git, path, _| {
        shown(git.worktree_is_pristine(path))
    }),
    ("worktree_has_submodules", false, |git, path, _| {
        shown(git.worktree_has_submodules(path))
    }),
    ("local_branch_target", false, |git, path, _| {
        shown(git.local_branch_target(path, OsStr::new("main")))
    }),
];

/// Output assembled from the pieces Git's formats are made of, so a draw
/// often lands one field away from something a parser accepts.
fn output() -> impl PrintableGenerator<Vec<u8>> {
    let id = "1".repeat(40);
    let pieces: Vec<Vec<u8>> = [
        "true\n",
        "false\n",
        "\n",
        "\0",
        "\t",
        " ",
        "160000 ",
        "100644 blob ",
        "worktree /somewhere\0",
        "HEAD ",
        "bare\0",
        "detached\0",
        "branch refs/heads/main\0",
        "local\0",
        "file:.git/config\0",
        "probe.flag\n",
        "includeif.x.path\n",
        "refs/heads/main",
        " blob 7\n",
        "sha1\n",
        "/somewhere\n",
        "git version 2.50.0\n",
        id.as_str(),
    ]
    .iter()
    .map(|piece| piece.as_bytes().to_vec())
    .collect();
    gs::vecs(hegel::one_of!(
        gs::sampled_from(pieces),
        gs::binary().max_size(4)
    ))
    .max_size(6)
    .map(|pieces| pieces.concat())
}

/// How many times each probe invokes Git when nothing is scripted, which
/// also checks that every probe answers through the stand-in untouched.
fn invocation_counts(scene: &Scene) -> Vec<usize> {
    PROBES
        .iter()
        .map(|(name, _, probe)| {
            scene.scripted.reset();
            let untouched = probe(&scene.scripted.git(), &scene.fixture.repository(), scene);
            assert!(untouched.is_ok(), "{name}: {untouched:?}");
            scene.scripted.invocations()
        })
        .collect()
}

/// Replace invocations of each probe with a reply drawn by `reply`, and
/// hand the outcome to `check`: each invocation in turn when `exhaustive`,
/// otherwise one drawn invocation per probe.
fn for_every_probe(
    tc: &TestCase,
    exhaustive: bool,
    reply: impl Fn(&TestCase) -> End,
    check: impl Fn(&str, End, Result<String, GitError>),
) {
    SCENE.with(|scene| {
        for ((name, recovers, probe), invocations) in PROBES.iter().zip(&scene.invocations) {
            // Only the exhaustive properties demand a refusal, which a
            // recovering probe is documented not to give.
            if exhaustive && *recovers {
                continue;
            }
            let replaced = if exhaustive {
                0..*invocations
            } else {
                let one = tc.draw(gs::integers::<usize>().max_value(invocations - 1));
                one..one + 1
            };
            for replaced in replaced {
                let end = reply(tc);
                scene.scripted.reset();
                scene.scripted.script(replaced, &tc.draw(output()), end);
                let result = probe(&scene.scripted.git(), &scene.fixture.repository(), scene);
                check(&format!("{name}, invocation {replaced}"), end, result);
            }
        }
    });
}

/// The operation must fail. A streaming reader may notice the malformed
/// reply before the exit status; every other failure names how Git ended.
fn assert_refused(context: &str, result: Result<String, GitError>, ending: &str) {
    match result {
        Ok(answer) => panic!("{context}: answered {answer}"),
        Err(GitError::InvalidOutput { .. }) => {}
        Err(error) => assert!(error.to_string().contains(ending), "{context}: {error}"),
    }
}

#[hegel::test(test_cases = 2)]
fn a_git_killed_by_a_signal_never_yields_an_answer(tc: TestCase) {
    for_every_probe(
        &tc,
        true,
        |tc| {
            End::Signal(tc.draw(gs::sampled_from(vec![
                libc::SIGKILL,
                libc::SIGTERM,
                libc::SIGHUP,
            ])))
        },
        |context, end, result| {
            let End::Signal(signal) = end else {
                unreachable!("only signals are drawn");
            };
            assert_refused(context, result, &format!("killed by signal {signal}"));
        },
    );
}

#[hegel::test(test_cases = 2)]
fn an_exit_code_git_does_not_document_never_yields_an_answer(tc: TestCase) {
    for_every_probe(
        &tc,
        true,
        // 1 is "no such value" for several queries and 5 is "nothing to
        // unset"; no query gives any other code a meaning.
        |tc| End::Exit(tc.draw(gs::integers::<u8>().min_value(2).filter(|code| *code != 5))),
        |context, end, result| {
            let End::Exit(code) = end else {
                unreachable!("only exit codes are drawn");
            };
            assert_refused(context, result, &format!("exit code {code}"));
        },
    );
}

#[hegel::test(test_cases = 6)]
fn generated_output_is_parsed_or_refused_without_panicking(tc: TestCase) {
    for_every_probe(
        &tc,
        false,
        |tc| End::Exit(tc.draw(gs::sampled_from(vec![0, 0, 0, 1]))),
        |context, _, result| {
            // Bad output is Git misbehaving, never Git failing to start.
            if let Err(error) = result {
                assert!(
                    !matches!(error, GitError::Start { .. } | GitError::Wait { .. }),
                    "{context}: {error:?}"
                );
            }
        },
    );
}
