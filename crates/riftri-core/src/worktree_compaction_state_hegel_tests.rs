//! Compaction journals in every phase against every arrangement of the three
//! trees a compaction owns. A kill, an older Riftri, or a user tidying up can
//! leave any phase beside any arrangement, so the pair is generated outright
//! instead of being reached by stopping a run. Whatever the pair, status still
//! answers, lifecycle verbs keep out while the journal is pending, and repair
//! either finishes with one clean registered view or reports and keeps every
//! tree it did not verify.

use std::fs;
use std::path::{Path, PathBuf};

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::tests::git_stdout;
use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, RemoveWorktreeRequest, compact_worktree_inner,
    garbage_collect, move_worktree_inner, recover_incomplete_operations, remove_worktree_inner,
    storage_accounting, try_lock_add_operation,
};
use super::recovery::{Scene, injected};
use super::{edit_journal, inode, registered, status};
use crate::CompactWorktreePhase;
use crate::journal::{CompactJournalStore, JournalStore};

/// Where one of the compaction's two trees is put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Destination,
    Replacement,
    Quarantine,
    /// The name cleanup gives a verified old view just before deleting it.
    Dropped,
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Twist {
    None,
    /// A file the user wrote into their original view.
    UserWroteOriginal,
    /// A file the user wrote into the compacted view once it was in place.
    UserWroteCompacted,
    /// A Git command moved HEAD from inside the original view.
    HeadMovedInOriginal,
    Unregistered,
    /// A journal from before compactions recorded the commit they replaced.
    NoOriginalCommit,
    /// Another Riftri command holds the view while repair runs.
    Busy,
}

/// What is tried on the view while its compaction is still pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeforeRepair {
    Collect,
    Compact,
    Remove,
    Move,
}

fn slots(journal: &crate::journal::DecodedCompactJournal) -> [PathBuf; 4] {
    [
        journal.destination.clone(),
        journal.replacement.clone(),
        journal.quarantine.clone(),
        journal
            .quarantine
            .with_file_name(format!(".riftri-compact-drop-{}", journal.operation_id)),
    ]
}

fn slot(paths: &[PathBuf; 4], place: Place) -> Option<&Path> {
    match place {
        Place::Destination => Some(&paths[0]),
        Place::Replacement => Some(&paths[1]),
        Place::Quarantine => Some(&paths[2]),
        Place::Dropped => Some(&paths[3]),
        Place::Gone => None,
    }
}

fn compaction_state(tc: TestCase, phase: CompactWorktreePhase) {
    let places = vec![
        Place::Destination,
        Place::Replacement,
        Place::Quarantine,
        Place::Dropped,
        Place::Gone,
    ];
    let original_at = tc.draw(gs::sampled_from(places.clone()));
    let compacted_at = tc.draw(gs::sampled_from(
        places
            .into_iter()
            .filter(|place| *place == Place::Gone || *place != original_at)
            .collect::<Vec<_>>(),
    ));
    let add_names_new_base = tc.draw(gs::booleans());
    let twist = tc.draw(gs::sampled_from(vec![
        Twist::None,
        Twist::UserWroteOriginal,
        Twist::UserWroteCompacted,
        Twist::HeadMovedInOriginal,
        Twist::Unregistered,
        Twist::NoOriginalCommit,
        Twist::Busy,
    ]));

    let before_repair = tc.draw(gs::sampled_from(vec![
        BeforeRepair::Collect,
        BeforeRepair::Compact,
        BeforeRepair::Remove,
        BeforeRepair::Move,
    ]));
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    assert!(scene.git(
        &view,
        &["checkout", "--quiet", "--detach", &scene.commits[1]]
    ));
    let request = CompactWorktreeRequest {
        repository: repository.clone(),
        destination: view.clone(),
        state_dir: Some(state.clone()),
    };
    let stopped = compact_worktree_inner(
        request.clone(),
        Some(CompactWorktreePhase::ReplacementReady),
    )
    .expect_err("stop the compaction");
    assert!(injected(&stopped), "{stopped}");
    let journal = CompactJournalStore::open(&state)
        .load_all()
        .expect("compaction journal")
        .remove(0);
    let paths = slots(&journal);

    // Both trees exist now: the user's original view and the pristine
    // replacement. Set them aside, then put each where this case wants it.
    let held = [
        scene.fixture.path("held-original"),
        scene.fixture.path("held-compacted"),
    ];
    fs::rename(&journal.destination, &held[0]).unwrap();
    fs::rename(&journal.replacement, &held[1]).unwrap();
    if twist == Twist::Unregistered {
        assert!(scene.git(&repository, &["worktree", "prune"]));
        // Git no longer answers for the view, so it cannot be asked for a
        // clean status afterwards.
        scene.ref_moved = true;
    }
    for (tree, place) in held.iter().zip([original_at, compacted_at]) {
        match slot(&paths, place) {
            Some(path) => fs::rename(tree, path).unwrap(),
            None => fs::remove_dir_all(tree).unwrap(),
        }
    }
    // Only the destination and the quarantine ever hold a view the user
    // worked in; the other two names are Riftri's own scratch space.
    let original = matches!(original_at, Place::Destination | Place::Quarantine)
        .then(|| slot(&paths, original_at).map(Path::to_path_buf))
        .flatten();

    if add_names_new_base {
        let add_store = JournalStore::open(&state);
        let add = add_store.load_all().expect("add journal").remove(0);
        add_store
            .update_active_base(
                &add.journal_path,
                &journal.destination,
                &journal.old_base_path,
                &journal.base_path,
                &journal.expected_commit,
            )
            .expect("retarget the add journal");
    }
    edit_journal(&journal.journal_path, |value| {
        value["phase"] = serde_json::to_value(phase).unwrap();
        if twist == Twist::NoOriginalCommit {
            value.as_object_mut().unwrap().remove("old_expected_commit");
        }
    });
    if twist == Twist::UserWroteCompacted && compacted_at == Place::Destination {
        let homes = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        scene.user_writes(&paths[0].clone(), &homes);
    }
    match (twist, &original) {
        (Twist::UserWroteOriginal, Some(original)) => {
            let homes = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
            scene.user_writes(original, &homes);
        }
        (Twist::HeadMovedInOriginal, Some(original))
            if scene.git(
                original,
                &["checkout", "--quiet", "--detach", &scene.commits[0]],
            ) =>
        {
            scene.kept.push((
                paths.iter().map(|path| path.join("tracked.txt")).collect(),
                b"first\n".to_vec(),
            ));
        }
        _ => {}
    }
    tc.note(&format!(
        "{phase:?}: original {original_at:?}, compacted {compacted_at:?}, add names new base {add_names_new_base}, {twist:?}, then {before_repair:?}"
    ));
    let located = |paths: &[PathBuf; 4]| paths.iter().map(|path| inode(path)).collect::<Vec<_>>();
    let before = located(&paths);
    let any_tree = before.iter().any(Option::is_some);
    let finished = matches!(
        phase,
        CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
    );

    // Status describes the state directory whatever shape it is in.
    let accounting = storage_accounting(&state).expect("status with a compaction journal");
    if !finished && accounting.pending_compactions == 0 {
        // A journal that contradicts its add journal is reported, not trusted.
        assert!(
            accounting
                .diagnostic_issues
                .iter()
                .any(|issue| issue.path == journal.journal_path),
            "status hid a pending compaction: {:?}",
            accounting.diagnostic_issues
        );
    } else {
        assert_eq!(accounting.pending_compactions, usize::from(!finished));
    }

    // A pending compaction holds both of its bases against collection.
    if before_repair == BeforeRepair::Collect && !finished && accounting.pending_compactions == 1 {
        let bases = [&journal.old_base_path, &journal.base_path].map(|base| base.is_dir());
        let collected = garbage_collect(&state, true).expect("collect beside a compaction");
        tc.note(&format!("collected {:?}", collected.collected));
        assert_eq!(
            [&journal.old_base_path, &journal.base_path].map(|base| base.is_dir()),
            bases,
            "a base of a pending compaction was collected"
        );
        assert_eq!(located(&paths), before, "collection moved a tree");
    }

    // No other lifecycle verb may start on a view with a pending compaction.
    if !finished {
        match before_repair {
            BeforeRepair::Compact => {
                compact_worktree_inner(request.clone(), None)
                    .expect_err("compact a compacting view");
            }
            BeforeRepair::Remove => {
                remove_worktree_inner(
                    RemoveWorktreeRequest {
                        repository: repository.clone(),
                        destination: view.clone(),
                        state_dir: Some(state.clone()),
                    },
                    None,
                )
                .expect_err("remove a compacting view");
            }
            BeforeRepair::Move => {
                move_worktree_inner(
                    MoveWorktreeRequest {
                        repository: repository.clone(),
                        source: view.clone(),
                        destination: scene.fixture.path("moved"),
                        state_dir: Some(state.clone()),
                    },
                    None,
                )
                .expect_err("move a compacting view");
            }
            BeforeRepair::Collect => {}
        }
        assert_eq!(located(&paths), before, "a refused verb moved a tree");
        assert!(!scene.fixture.path("moved").exists());
    }

    if twist == Twist::Busy && !finished {
        let add = JournalStore::open(&state)
            .load_all()
            .expect("add journal")
            .remove(0);
        let lock = try_lock_add_operation(&add.journal_path)
            .expect("lock the view")
            .expect("nothing else holds it");
        let report = recover_incomplete_operations(&state).expect("repair a busy view");
        assert!(report.busy_adds > 0, "{report:?}");
        assert_eq!(located(&paths), before, "repair acted on a busy view");
        drop(lock);
    }

    // With the view at neither name Git knows, repair retires the add journal
    // before it looks at the compaction. `a_compaction_killed_between_its_
    // renames_stays_managed` asserts what should happen instead; here only
    // the trees themselves are checked.
    let mid_swap = !finished && before[0].is_none() && before[2].is_some();
    if mid_swap {
        for _ in 0..2 {
            recover_incomplete_operations(&state).expect("repair a compaction caught mid-swap");
        }
        for (homes, data) in &scene.kept {
            assert!(
                homes
                    .iter()
                    .any(|home| fs::read(home).is_ok_and(|found| &found == data)),
                "repair lost the user's {}",
                homes[0].display()
            );
        }
        assert!(
            paths.iter().any(|path| path.join("tracked.txt").is_file()),
            "repair deleted every copy of a view caught mid-swap"
        );
        return;
    }

    let report = scene.settle(&tc, &[&view]);
    let after = located(&paths);
    if finished {
        assert_eq!(after, before, "repair acted on a finished compaction");
        return;
    }
    if report.errors.is_empty() {
        assert!(
            after[1..3].iter().all(Option::is_none),
            "a settled compaction left a tree behind: {after:?}"
        );
        // Only the last cleanup step creates the drop name, so no other phase
        // may treat a directory found there as its own.
        if phase == CompactWorktreePhase::AddJournalUpdated {
            assert_eq!(after[3], None, "a settled compaction left its old view");
        } else {
            assert_eq!(
                after[3], before[3],
                "repair deleted a path it never creates"
            );
        }
        if any_tree && twist != Twist::Unregistered {
            assert!(registered(&repository, &view) && view.join(".git").exists());
            if scene.kept.is_empty() && !scene.ref_moved {
                assert_eq!(status(&view), "", "the settled view is dirty");
            }
            assert!(
                scene
                    .commits
                    .contains(&git_stdout(&view, &["rev-parse", "HEAD"])),
                "the settled view is on an unknown commit"
            );
        }
    } else if any_tree {
        assert!(
            paths.iter().any(|path| path.join("tracked.txt").is_file()),
            "a compaction repair reported an error after deleting every copy of the view"
        );
    }
}

macro_rules! per_phase {
    ($($name:ident => $phase:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = 12,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                compaction_state(tc, $phase);
            }
        )+
    };
}

per_phase!(
    an_intent_beside_any_arrangement_settles_or_is_reported => CompactWorktreePhase::IntentRecorded,
    a_ready_replacement_beside_any_arrangement_settles_or_is_reported => CompactWorktreePhase::ReplacementReady,
    an_activated_replacement_beside_any_arrangement_settles_or_is_reported => CompactWorktreePhase::ReplacementActivated,
    an_updated_add_journal_beside_any_arrangement_settles_or_is_reported => CompactWorktreePhase::AddJournalUpdated,
    a_restore_beside_any_arrangement_settles_or_is_reported => CompactWorktreePhase::RestoringOriginal,
    a_completed_compaction_is_left_alone => CompactWorktreePhase::Complete,
    a_cancelled_compaction_is_left_alone => CompactWorktreePhase::Cancelled,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swap {
    /// `resume_compaction` renamed the view to its quarantine and was killed
    /// before renaming the replacement into place.
    Activating,
    /// `restore_original_after_compaction` set the compacted view aside and
    /// was killed before renaming the original back.
    Restoring,
}

/// Both swaps are two renames, and between them nothing exists at the path
/// Git registers. Repair must finish or undo the swap and keep managing the
/// view it puts back.
#[hegel::test(
    test_cases = 8,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
#[ignore = "bug: repair retires the add journal of a view whose compaction was killed between its two renames"]
fn a_compaction_killed_between_its_renames_stays_managed(tc: TestCase) {
    let swap = tc.draw(gs::sampled_from(vec![Swap::Activating, Swap::Restoring]));
    let status_first = tc.draw(gs::booleans());
    let scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    assert!(scene.git(
        &view,
        &["checkout", "--quiet", "--detach", &scene.commits[1]]
    ));
    let stop = match swap {
        Swap::Activating => CompactWorktreePhase::ReplacementReady,
        Swap::Restoring => CompactWorktreePhase::AddJournalUpdated,
    };
    let stopped = compact_worktree_inner(
        CompactWorktreeRequest {
            repository: repository.clone(),
            destination: view.clone(),
            state_dir: Some(state.clone()),
        },
        Some(stop),
    )
    .expect_err("stop the compaction");
    assert!(injected(&stopped), "{stopped}");
    let journal = CompactJournalStore::open(&state)
        .load_all()
        .expect("compaction journal")
        .remove(0);
    let expected_head = match swap {
        Swap::Activating => {
            fs::rename(&journal.destination, &journal.quarantine).unwrap();
            scene.commits[1].clone()
        }
        Swap::Restoring => {
            // A Git command in the old view is what makes repair restore it.
            assert!(scene.git(
                &journal.quarantine,
                &["checkout", "--quiet", "--detach", &scene.commits[0]]
            ));
            edit_journal(&journal.journal_path, |value| {
                value["phase"] =
                    serde_json::to_value(CompactWorktreePhase::RestoringOriginal).unwrap();
            });
            fs::rename(&journal.destination, &journal.replacement).unwrap();
            scene.commits[0].clone()
        }
    };
    if status_first {
        storage_accounting(&state).expect("status mid-swap");
    }

    for pass in 0..2 {
        let report = recover_incomplete_operations(&state).expect("repair");
        tc.note(&format!("repair {pass}: {report:?}"));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.retired_adds, 0, "repair retired a live view");
    }
    assert!(registered(&repository, &view));
    assert_eq!(git_stdout(&view, &["rev-parse", "HEAD"]), expected_head);
    assert_eq!(status(&view), "");
    let accounting = storage_accounting(&state).expect("status after repair");
    assert_eq!(accounting.active_views, 1, "{accounting:?}");
    assert_eq!(accounting.pending_compactions, 0);
    assert!(
        accounting.diagnostic_issues.is_empty(),
        "{:?}",
        accounting.diagnostic_issues
    );
    for leftover in [&journal.replacement, &journal.quarantine] {
        assert!(!leftover.exists(), "repair left {}", leftover.display());
    }
    // The view is still Riftri's to remove.
    remove_worktree_inner(
        RemoveWorktreeRequest {
            repository,
            destination: view.clone(),
            state_dir: Some(state),
        },
        None,
    )
    .expect("remove the repaired view");
    assert!(!view.exists());
}
