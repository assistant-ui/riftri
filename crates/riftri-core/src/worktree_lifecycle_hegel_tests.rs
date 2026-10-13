//! The managed lifecycle as a state machine: every verb can be stopped after
//! any journal phase, a user can change or delete a view between steps, and
//! `repair` runs at arbitrary points. Whatever the order, nothing a user wrote
//! is lost without a forced removal, and immutable bases never change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, PruneWorktreesRequest, RemoveWorktreeRequest,
    WorktreeError, add_worktree_inner, compact_worktree_inner, force_remove_worktree_inner,
    garbage_collect_inner, move_worktree_inner, prune_worktrees_inner,
    recover_incomplete_operations, remove_worktree_inner, storage_accounting,
};
use super::{Entry, Fixture, TreeFile, registered, snapshot, status};
use crate::{
    AddWorktreePhase, CompactWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase,
    PruneWorktreesPhase, RemoveWorktreePhase,
};

const SLOTS: usize = 2;

const ADD_PHASES: [AddWorktreePhase; 7] = [
    AddWorktreePhase::IntentRecorded,
    AddWorktreePhase::GitMetadataCreated,
    AddWorktreePhase::BaseReady,
    AddWorktreePhase::ViewCreated,
    AddWorktreePhase::GitPointerRestored,
    AddWorktreePhase::IndexSynchronized,
    AddWorktreePhase::CleanVerified,
];
const REMOVE_PHASES: [RemoveWorktreePhase; 4] = [
    RemoveWorktreePhase::IntentRecorded,
    RemoveWorktreePhase::CleanVerified,
    RemoveWorktreePhase::WorktreeRemoved,
    RemoveWorktreePhase::BaseReleased,
];
const MOVE_PHASES: [MoveWorktreePhase; 3] = [
    MoveWorktreePhase::IntentRecorded,
    MoveWorktreePhase::WorktreeMoved,
    MoveWorktreePhase::AddJournalUpdated,
];
const COMPACT_PHASES: [CompactWorktreePhase; 4] = [
    CompactWorktreePhase::IntentRecorded,
    CompactWorktreePhase::ReplacementReady,
    CompactWorktreePhase::ReplacementActivated,
    CompactWorktreePhase::AddJournalUpdated,
];
const PRUNE_PHASES: [PruneWorktreesPhase; 2] = [
    PruneWorktreesPhase::IntentRecorded,
    PruneWorktreesPhase::GitMetadataPruned,
];
const COLLECTION_PHASES: [GarbageCollectionPhase; 3] = [
    GarbageCollectionPhase::IntentRecorded,
    GarbageCollectionPhase::MarkerRemoved,
    GarbageCollectionPhase::BaseQuarantined,
];

struct Lifecycle {
    fixture: Fixture,
    commits: [String; 2],
    /// Per slot, the files a user wrote that no later forced removal or
    /// deletion by that user was entitled to discard.
    edits: Vec<BTreeMap<String, Vec<u8>>>,
    /// Content of every completed immutable base when it was first seen.
    bases: BTreeMap<PathBuf, BTreeMap<Vec<u8>, Entry>>,
    writes: usize,
}

impl Lifecycle {
    fn new() -> Self {
        let fixture = Fixture::new();
        let first = fixture.commit(
            "main",
            &[
                TreeFile::regular("tracked.txt", b"first\n"),
                TreeFile::regular("nested/deep.txt", b"deep\n"),
            ],
        );
        let second = fixture.commit(
            "other",
            &[
                TreeFile::regular("tracked.txt", b"second\n"),
                TreeFile::regular("nested/deep.txt", b"deep\n"),
                TreeFile::regular("added.txt", b"added\n"),
            ],
        );
        let machine = Self {
            fixture,
            commits: [first, second],
            edits: vec![BTreeMap::new(); SLOTS],
            bases: BTreeMap::new(),
            writes: 0,
        };
        // Start with one managed view so the first rules have a target.
        let [view, _] = machine.candidates(0);
        add_worktree_inner(
            machine.fixture.add_request(&view, &machine.commits[0]),
            None,
            true,
        )
        .expect("initial view");
        machine
    }

    /// A slot's view lives at one of two names; a move swaps between them.
    fn candidates(&self, slot: usize) -> [PathBuf; 2] {
        [
            self.fixture.path(format!("view-{slot}")),
            self.fixture.path(format!("view-{slot}-moved")),
        ]
    }

    fn located(&self, slot: usize) -> Option<PathBuf> {
        self.candidates(slot).into_iter().find(|path| path.is_dir())
    }

    fn slot(tc: &TestCase) -> usize {
        tc.draw(gs::integers::<usize>().max_value(SLOTS - 1))
    }

    fn completed_bases(&self) -> Vec<PathBuf> {
        let mut bases = Vec::new();
        let Ok(repositories) = fs::read_dir(self.fixture.state.join("bases/v1")) else {
            return bases;
        };
        for repository in repositories.flatten() {
            let Ok(entries) = fs::read_dir(repository.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.with_extension("complete").is_file() {
                    bases.push(path);
                }
            }
        }
        bases
    }
}

fn injected(error: &WorktreeError) -> bool {
    matches!(
        error,
        WorktreeError::InjectedFailure(_)
            | WorktreeError::InjectedRemovalFailure(_)
            | WorktreeError::InjectedMoveFailure(_)
            | WorktreeError::InjectedPruneFailure(_)
            | WorktreeError::InjectedCollectionFailure(_)
            | WorktreeError::InjectedCompactionFailure(_)
    )
}

fn git_succeeds(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run git")
        .status
        .success()
}

#[hegel::state_machine]
impl Lifecycle {
    #[rule(weight = 3.0)]
    fn add(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let commit = tc.draw(gs::integers::<usize>().max_value(1));
        let fail_after = tc.draw(gs::optional(gs::sampled_from(ADD_PHASES.to_vec())));
        let rollback = tc.draw(gs::booleans());
        let [destination, moved] = self.candidates(slot);
        // One slot models one view, so never add beside its moved copy.
        if moved.exists() {
            return;
        }
        let occupied = destination.exists();
        let result = add_worktree_inner(
            self.fixture
                .add_request(&destination, &self.commits[commit]),
            fail_after,
            rollback,
        );
        match result {
            Ok(added) => {
                assert!(fail_after.is_none(), "an injected failure must surface");
                assert!(!occupied || self.edits[slot].is_empty());
                assert_eq!(added.commit.as_str(), self.commits[commit]);
                assert_eq!(status(&added.destination), "", "a new view is clean");
            }
            Err(error) if injected(&error) && rollback && !occupied => {
                assert!(
                    !destination.exists(),
                    "a rolled-back add leaves no destination"
                );
            }
            Err(_) => {}
        }
    }

    #[rule(weight = 3.0)]
    fn remove(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let force = tc.draw(gs::booleans());
        let fail_after = tc.draw(gs::optional(gs::sampled_from(REMOVE_PHASES.to_vec())));
        let Some(destination) = self.located(slot) else {
            return;
        };
        let request = RemoveWorktreeRequest {
            repository: self.fixture.repository.clone(),
            destination: destination.clone(),
            state_dir: Some(self.fixture.state.clone()),
        };
        let result = if force {
            force_remove_worktree_inner(request, fail_after)
        } else {
            remove_worktree_inner(request, fail_after)
        };
        match result {
            Ok(_) => {
                assert!(fail_after.is_none(), "an injected failure must surface");
                assert!(!destination.exists(), "a removed view is gone");
                assert!(!registered(&self.fixture.repository, &destination));
                assert!(
                    force || self.edits[slot].is_empty(),
                    "an unforced removal deleted a changed view"
                );
                self.edits[slot].clear();
            }
            // The forced intent recorded the view as it was, so repair may
            // finish discarding exactly that content.
            Err(error) if force && injected(&error) => self.edits[slot].clear(),
            Err(_) => {}
        }
    }

    #[rule]
    fn relocate(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let fail_after = tc.draw(gs::optional(gs::sampled_from(MOVE_PHASES.to_vec())));
        let Some(source) = self.located(slot) else {
            return;
        };
        let destination = self
            .candidates(slot)
            .into_iter()
            .find(|path| *path != source)
            .expect("the other name");
        let result = move_worktree_inner(
            MoveWorktreeRequest {
                repository: self.fixture.repository.clone(),
                source: source.clone(),
                destination: destination.clone(),
                state_dir: Some(self.fixture.state.clone()),
            },
            fail_after,
        );
        if result.is_ok() {
            assert!(fail_after.is_none(), "an injected failure must surface");
            assert!(!source.exists(), "a moved view left its source");
            assert!(registered(&self.fixture.repository, &destination));
        }
    }

    #[rule]
    fn compact(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let fail_after = tc.draw(gs::optional(gs::sampled_from(COMPACT_PHASES.to_vec())));
        let Some(destination) = self.located(slot) else {
            return;
        };
        let result = compact_worktree_inner(
            CompactWorktreeRequest {
                repository: self.fixture.repository.clone(),
                destination: destination.clone(),
                state_dir: Some(self.fixture.state.clone()),
            },
            fail_after,
        );
        if let Ok(compacted) = result {
            assert!(fail_after.is_none(), "an injected failure must surface");
            assert!(
                self.edits[slot].is_empty(),
                "compaction replaced a changed view"
            );
            assert_eq!(status(&compacted.destination), "");
        }
    }

    #[rule]
    fn prune(&mut self, tc: TestCase) {
        let fail_after = tc.draw(gs::optional(gs::sampled_from(PRUNE_PHASES.to_vec())));
        let result = prune_worktrees_inner(
            PruneWorktreesRequest {
                repository: self.fixture.repository.clone(),
                state_dir: Some(self.fixture.state.clone()),
            },
            fail_after,
        );
        assert!(result.is_err() || fail_after.is_none());
    }

    #[rule]
    fn collect(&mut self, tc: TestCase) {
        let apply = tc.draw(gs::booleans());
        let fail_after = tc.draw(gs::optional(gs::sampled_from(COLLECTION_PHASES.to_vec())));
        let before = self.completed_bases();
        let Ok(report) = garbage_collect_inner(&self.fixture.state, apply, fail_after) else {
            return;
        };
        if !apply {
            assert!(report.collected.is_empty());
            assert_eq!(self.completed_bases(), before, "a plan deleted a base");
        }
        // A base an active view was cloned from must outlive collection.
        let accounting = storage_accounting(&self.fixture.state).expect("account after gc");
        for view in &accounting.views {
            assert!(
                !report.collected.contains(&view.base_path),
                "collected the base of active view {}",
                view.destination.display()
            );
        }
    }

    /// A Git command that moves the view's HEAD, as a user switching commits.
    #[rule]
    fn switch(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let commit = tc.draw(gs::integers::<usize>().max_value(1));
        let Some(view) = self.located(slot) else {
            return;
        };
        git_succeeds(
            &view,
            &["checkout", "--quiet", "--detach", &self.commits[commit]],
        );
    }

    #[rule(weight = 3.0)]
    fn write(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let tracked = tc.draw(gs::booleans());
        let Some(view) = self.located(slot) else {
            return;
        };
        self.writes += 1;
        let name = if tracked {
            "tracked.txt".to_owned()
        } else {
            format!("user-{}.txt", self.writes)
        };
        let data = format!("user write {}\n", self.writes).into_bytes();
        if fs::write(view.join(&name), &data).is_ok() {
            self.edits[slot].insert(name, data);
        }
    }

    /// The user deletes a view by hand, or with plain Git, behind Riftri.
    #[rule]
    fn discard(&mut self, tc: TestCase) {
        let slot = Self::slot(&tc);
        let with_git = tc.draw(gs::booleans());
        let Some(view) = self.located(slot) else {
            return;
        };
        let gone = if with_git {
            let path = view.to_str().expect("fixture paths are UTF-8");
            git_succeeds(
                &self.fixture.repository,
                &["worktree", "remove", "--force", path],
            )
        } else {
            fs::remove_dir_all(&view).is_ok()
        };
        if gone {
            self.edits[slot].clear();
        }
    }

    #[rule(weight = 2.0)]
    fn repair(&mut self, _: TestCase) {
        let mut last = recover_incomplete_operations(&self.fixture.state).expect("repair");
        for _ in 0..2 {
            last = recover_incomplete_operations(&self.fixture.state).expect("repair again");
        }
        assert_eq!(
            (
                last.recovered,
                last.recovered_removals,
                last.recovered_moves,
                last.recovered_compactions,
                last.recovered_prunes,
                last.recovered_collections,
                last.retired_adds,
            ),
            (0, 0, 0, 0, 0, 0, 0),
            "repair keeps finding work on an untouched state directory: {last:?}"
        );
        if !last.errors.is_empty() {
            return;
        }
        let accounting = storage_accounting(&self.fixture.state).expect("account after repair");
        assert_eq!(
            (
                accounting.pending_adds,
                accounting.pending_removals,
                accounting.pending_moves,
                accounting.pending_compactions,
                accounting.pending_prunes,
                accounting.pending_collections,
            ),
            (0, 0, 0, 0, 0, 0),
            "repair reported no error but left work pending: {accounting:?}"
        );
        for view in &accounting.views {
            assert!(
                view.destination.join(".git").exists(),
                "an active view is missing: {}",
                view.destination.display()
            );
            assert!(view.base_path.is_dir(), "an active view lost its base");
        }
        for slot in 0..SLOTS {
            let Some(view) = self.located(slot) else {
                continue;
            };
            if self.edits[slot].is_empty() && registered(&self.fixture.repository, &view) {
                assert_eq!(status(&view), "", "an untouched view is dirty after repair");
            }
        }
    }

    #[invariant(always_run)]
    fn user_writes_survive(&self, _: TestCase) {
        for slot in 0..SLOTS {
            for (name, data) in &self.edits[slot] {
                let kept = self
                    .candidates(slot)
                    .iter()
                    .any(|view| fs::read(view.join(name)).is_ok_and(|found| &found == data));
                assert!(kept, "slot {slot} lost the user's {name}");
            }
        }
    }

    #[invariant(always_run)]
    fn bases_are_immutable(&mut self, _: TestCase) {
        for base in self.completed_bases() {
            let content = snapshot(&base);
            let first = self
                .bases
                .entry(base.clone())
                .or_insert_with(|| content.clone());
            assert_eq!(*first, content, "base {} changed", base.display());
        }
    }
}

#[hegel::test(
    test_cases = 5,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn interrupted_lifecycles_never_lose_user_writes(tc: TestCase) {
    hegel::stateful::machine(Lifecycle::new()).steps(10).run(tc);
}
