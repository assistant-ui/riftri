//! Lifecycle verbs on views they cannot safely change. A verb either does
//! all of its work or refuses and leaves the target, Git's registration, and
//! the journals exactly as they were.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::tests::git;
use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, PruneWorktreesRequest, RemoveWorktreeRequest,
    WorktreeError, add_worktree_inner, compact_worktree_inner, force_remove_worktree_inner,
    move_worktree_inner, prune_worktrees_inner, recover_incomplete_operations,
    remove_worktree_inner, storage_accounting,
};
use super::{Fixture, TreeFile, registered, snapshot, status};
use crate::{CompactWorktreePhase, MoveWorktreePhase, PruneWorktreesPhase, RemoveWorktreePhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Managed,
    Dirty,
    PlainWorktree,
    Missing,
    MainWorktree,
    Unregistered,
    Locked(bool),
    HeadCorrupt,
    PendingRemoval,
    PendingMove,
    PendingCompaction,
    NonConeSparse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Remove,
    ForceRemove,
    Move,
    Compact,
}

fn git_ok(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run git")
        .status
        .success()
}

fn pending(state: &Path) -> (usize, usize, usize, usize) {
    let accounting = storage_accounting(state).expect("status");
    (
        accounting.pending_adds,
        accounting.pending_removals,
        accounting.pending_moves,
        accounting.pending_compactions,
    )
}

#[hegel::test(
    test_cases = 12,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn a_verb_finishes_or_refuses_without_touching_its_target(tc: TestCase) {
    let target = tc.draw(gs::sampled_from(vec![
        Target::Managed,
        Target::Dirty,
        Target::PlainWorktree,
        Target::Missing,
        Target::MainWorktree,
        Target::Unregistered,
        Target::Locked(false),
        Target::Locked(true),
        Target::HeadCorrupt,
        Target::PendingRemoval,
        Target::PendingMove,
        Target::PendingCompaction,
        Target::NonConeSparse,
    ]));
    let verb = tc.draw(gs::sampled_from(vec![
        Verb::Remove,
        Verb::ForceRemove,
        Verb::Move,
        Verb::Compact,
    ]));
    let fixture = Fixture::new();
    let commit = fixture.commit(
        "main",
        &[
            TreeFile::regular("tracked.txt", b"first\n"),
            TreeFile::regular("nested/deep.txt", b"deep\n"),
        ],
    );
    let state = fixture.state.clone();
    let repository = fixture.repository.clone();
    let view = fixture.path("view");
    let view_text = view.to_str().unwrap().to_owned();
    let removal = |destination: &Path| RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: destination.to_path_buf(),
        state_dir: Some(state.clone()),
    };
    let mut path = view.clone();
    if !matches!(target, Target::PlainWorktree | Target::MainWorktree) {
        add_worktree_inner(fixture.add_request(&view, &commit), None, true).expect("managed view");
    }
    match target {
        Target::Managed => {}
        Target::Dirty => fs::write(view.join("untracked"), b"user\n").unwrap(),
        Target::PlainWorktree => {
            git(
                &repository,
                &["worktree", "add", "--quiet", "--detach", &view_text],
            );
            // A state directory exists, but it does not manage this worktree.
            add_worktree_inner(
                fixture.add_request(&fixture.path("other"), &commit),
                None,
                true,
            )
            .expect("another managed view");
        }
        Target::Missing => path = fixture.path("never-created"),
        Target::MainWorktree => {
            add_worktree_inner(fixture.add_request(&view, &commit), None, true).expect("view");
            path = repository.clone();
        }
        Target::Unregistered => {
            let aside = fixture.path("aside");
            fs::rename(&view, &aside).unwrap();
            git(&repository, &["worktree", "prune"]);
            fs::rename(&aside, &view).unwrap();
        }
        Target::Locked(with_reason) => {
            let mut arguments = vec!["worktree", "lock"];
            if with_reason {
                arguments.extend(["--reason", "in use"]);
            }
            arguments.push(&view_text);
            git(&repository, &arguments);
        }
        Target::HeadCorrupt => {
            fs::write(repository.join(".git/worktrees/view/HEAD"), b"garbage\n").unwrap();
        }
        Target::PendingRemoval => {
            remove_worktree_inner(removal(&view), Some(RemoveWorktreePhase::IntentRecorded))
                .expect_err("stop a removal");
        }
        Target::PendingMove => {
            move_worktree_inner(
                MoveWorktreeRequest {
                    repository: repository.clone(),
                    source: view.clone(),
                    destination: fixture.path("half-moved"),
                    state_dir: Some(state.clone()),
                },
                Some(MoveWorktreePhase::IntentRecorded),
            )
            .expect_err("stop a move");
        }
        Target::PendingCompaction => {
            compact_worktree_inner(
                CompactWorktreeRequest {
                    repository: repository.clone(),
                    destination: view.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(CompactWorktreePhase::IntentRecorded),
            )
            .expect_err("stop a compaction");
        }
        Target::NonConeSparse => {
            git(&view, &["sparse-checkout", "set", "--no-cone", "/*"]);
        }
    }
    let content = path.is_dir().then(|| snapshot(&path));
    let was_registered = registered(&repository, &path);
    let pending_before = pending(&state);
    let moved = fixture.path("moved");

    let result = match verb {
        Verb::Remove => remove_worktree_inner(removal(&path), None).map(|_| ()),
        Verb::ForceRemove => force_remove_worktree_inner(removal(&path), None).map(|_| ()),
        Verb::Move => move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: path.clone(),
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .map(|_| ()),
        Verb::Compact => compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: path.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .map(|_| ()),
    };
    tc.note(&format!("{verb:?} on {target:?}: {result:?}"));

    let must_refuse = match (target, verb) {
        (Target::Managed, _) => false,
        (Target::Dirty, Verb::ForceRemove | Verb::Move) => false,
        (Target::NonConeSparse, Verb::Compact) => true,
        (Target::NonConeSparse, _) => false,
        // Git itself moves and removes nothing that is locked, and compaction
        // swaps the directory a lock protects.
        (Target::Locked(_), Verb::Compact) => return,
        _ => true,
    };
    match result {
        Ok(()) => {
            assert!(!must_refuse, "{verb:?} went ahead on {target:?}");
            match verb {
                Verb::Remove | Verb::ForceRemove => {
                    assert!(!path.exists() && !registered(&repository, &path));
                }
                Verb::Move => {
                    assert!(!path.exists() && registered(&repository, &moved));
                    assert_eq!(Some(snapshot(&moved)), content, "the move changed the view");
                }
                Verb::Compact => assert_eq!(status(&path), ""),
            }
        }
        Err(error) => {
            assert!(
                !matches!(error, WorktreeError::Io { .. }) || target == Target::HeadCorrupt,
                "a refusal surfaced as an I/O failure: {error:?}"
            );
            assert_eq!(
                path.is_dir().then(|| snapshot(&path)),
                content,
                "a refused {verb:?} changed its target"
            );
            assert_eq!(registered(&repository, &path), was_registered);
            assert!(!moved.exists(), "a refused move created its destination");
            assert_eq!(
                pending(&state),
                pending_before,
                "a refused {verb:?} left a journal pending"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedState {
    InPlace,
    HandDeleted,
    MovedWithGit,
    Unregistered,
    RemovalPending,
}

/// `prune` clears stale registrations of ordinary worktrees, and only while
/// every managed view is where its journal says. It never unregisters a
/// managed view, whether it runs to the end or is stopped and repaired.
#[hegel::test(
    test_cases = 10,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn prune_runs_only_while_managed_views_are_in_place(tc: TestCase) {
    let managed = tc.draw(gs::sampled_from(vec![
        ManagedState::InPlace,
        ManagedState::InPlace,
        ManagedState::HandDeleted,
        ManagedState::MovedWithGit,
        ManagedState::Unregistered,
        ManagedState::RemovalPending,
    ]));
    let fail_after = tc.draw(gs::optional(gs::sampled_from(vec![
        PruneWorktreesPhase::IntentRecorded,
        PruneWorktreesPhase::GitMetadataPruned,
    ])));
    let later = tc.draw(gs::sampled_from(vec![
        ManagedState::InPlace,
        ManagedState::HandDeleted,
        ManagedState::MovedWithGit,
    ]));
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("tracked.txt", b"first\n")]);
    let repository = fixture.repository.clone();
    let view = fixture.path("view");
    let stale = fixture.path("stale");
    add_worktree_inner(fixture.add_request(&view, &commit), None, true).expect("managed view");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            stale.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&stale).unwrap();
    let relocated = fixture.path("relocated");
    let disturb = |state: ManagedState| -> PathBuf {
        match state {
            ManagedState::InPlace => {}
            ManagedState::HandDeleted => fs::remove_dir_all(&view).unwrap(),
            ManagedState::MovedWithGit => {
                git(
                    &repository,
                    &[
                        "worktree",
                        "move",
                        view.to_str().unwrap(),
                        relocated.to_str().unwrap(),
                    ],
                );
                return relocated.clone();
            }
            ManagedState::Unregistered => {
                let aside = fixture.path("aside");
                fs::rename(&view, &aside).unwrap();
                // Unregister only the managed view; the stale one must stay
                // for the prune under test to find.
                assert!(git_ok(&repository, &["worktree", "prune"]));
                git(
                    &repository,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "--detach",
                        stale.to_str().unwrap(),
                    ],
                );
                fs::remove_dir_all(&stale).unwrap();
                fs::rename(&aside, &view).unwrap();
            }
            ManagedState::RemovalPending => {
                remove_worktree_inner(
                    RemoveWorktreeRequest {
                        repository: repository.clone(),
                        destination: view.clone(),
                        state_dir: Some(fixture.state.clone()),
                    },
                    Some(RemoveWorktreePhase::CleanVerified),
                )
                .expect_err("stop a removal");
            }
        }
        view.clone()
    };
    let mut home = disturb(managed);
    let content = home.is_dir().then(|| snapshot(&home));

    let result = prune_worktrees_inner(
        PruneWorktreesRequest {
            repository: repository.clone(),
            state_dir: Some(fixture.state.clone()),
        },
        fail_after,
    );
    tc.note(&format!(
        "{managed:?}, stop {fail_after:?}, then {later:?}: {result:?}"
    ));
    assert_eq!(
        home.is_dir().then(|| snapshot(&home)),
        content,
        "prune changed a managed view"
    );
    match &result {
        Ok(_) => {
            assert_eq!(
                managed,
                ManagedState::InPlace,
                "prune ran past a displaced view"
            );
            assert!(fail_after.is_none());
            assert!(
                !registered(&repository, &stale),
                "the stale worktree is still listed"
            );
            assert!(
                registered(&repository, &view),
                "prune unregistered a managed view"
            );
            return;
        }
        Err(error) if managed != ManagedState::InPlace => {
            assert!(!error.to_string().contains("injected"), "{error}");
            assert!(
                registered(&repository, &stale),
                "a refused prune still pruned"
            );
            assert_eq!(
                storage_accounting(&fixture.state)
                    .expect("status")
                    .pending_prunes,
                0,
                "a refused prune left its journal pending"
            );
            return;
        }
        Err(error) => assert!(error.to_string().contains("injected"), "{error}"),
    }

    // Stopped mid-way with the managed view in place; something may move
    // before repair resumes the prune.
    if later != ManagedState::InPlace {
        home = disturb(later);
    }
    let content = home.is_dir().then(|| snapshot(&home));
    let report = recover_incomplete_operations(&fixture.state).expect("repair");
    tc.note(&format!("repair: {report:?}"));
    assert_eq!(
        home.is_dir().then(|| snapshot(&home)),
        content,
        "repair changed a managed view"
    );
    if later == ManagedState::InPlace {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!registered(&repository, &stale));
        assert!(registered(&repository, &view));
        assert_eq!(
            storage_accounting(&fixture.state)
                .expect("status")
                .pending_prunes,
            0
        );
    }
    if later == ManagedState::MovedWithGit {
        assert!(
            registered(&repository, &relocated),
            "repair unregistered a moved view"
        );
    }
}
