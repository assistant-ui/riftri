//! Add journals in states built directly on disk: the window where a view's
//! `.git` pointer sits in the scratch clone, a second journal naming a
//! destination another add already owns, and a journal recorded for a backend
//! this platform cannot run. Riftri never deletes a view it cannot prove is
//! its own unfinished work.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, RemoveWorktreeRequest, WorktreeError,
    WorktreeMode, add_worktree_inner, compact_worktree_inner, force_remove_worktree_inner,
    inspect_managed_worktrees, managed_worktree_state_directory, move_worktree_inner,
    recover_incomplete_operations, remove_worktree_inner, storage_accounting,
};
use super::recovery::{Scene, injected};
use super::{edit_journal, journal_path_value, registered, snapshot, status};
use crate::journal::{JournalStore, RemovalJournalStore};
use crate::{AddWorktreePhase, RemoveWorktreePhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerWindow {
    /// The pointer moved into the scratch clone; the destination is empty.
    PointerInScratch,
    /// The emptied destination was removed as well.
    DestinationRemoved,
    /// Rollback had parked the pointer beside the temporary index.
    PointerStaged,
    /// The pointer exists in both places, as a no-replace move leaves it.
    PointerStagedAndInPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerInterference {
    None,
    UserWritesDestination,
    UserWritesScratch,
    BranchDeleted,
    BranchMoved,
}

fn pointer_window(tc: TestCase, window: PointerWindow) {
    let interference = tc.draw(gs::sampled_from(vec![
        PointerInterference::None,
        PointerInterference::UserWritesDestination,
        PointerInterference::UserWritesScratch,
        PointerInterference::BranchDeleted,
        PointerInterference::BranchMoved,
    ]));
    let existing_branch = tc.draw(gs::booleans());
    let mut scene = Scene::new();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    assert!(scene.git(&repository, &["branch", "topic", &scene.commits[0]]));
    let mut request = scene.fixture.add_request(&view, &scene.commits[0]);
    if existing_branch {
        request.revision = OsString::from("topic");
        request.mode = WorktreeMode::ExistingBranch(OsString::from("topic"));
    }
    let stopped = add_worktree_inner(request, Some(AddWorktreePhase::ViewCreated), false)
        .expect_err("stop the add");
    assert!(injected(&stopped), "{stopped}");
    let journal = JournalStore::open(&scene.fixture.state)
        .load_all()
        .expect("add journal")
        .remove(0);
    let staged = journal.temporary_index.with_extension("git-pointer");
    let pointer = view.join(".git");
    match window {
        PointerWindow::PointerInScratch => {
            fs::rename(&pointer, journal.scratch.join(".git")).unwrap();
        }
        PointerWindow::DestinationRemoved => {
            fs::rename(&pointer, journal.scratch.join(".git")).unwrap();
            fs::remove_dir(&view).unwrap();
        }
        PointerWindow::PointerStaged => fs::rename(&pointer, &staged).unwrap(),
        PointerWindow::PointerStagedAndInPlace => {
            fs::copy(&pointer, &staged).unwrap();
        }
    }
    match interference {
        PointerInterference::None => {}
        PointerInterference::UserWritesDestination => {
            scene.user_writes(&view.clone(), &[&view]);
        }
        // With the destination gone the scratch clone is the only copy of the
        // view, and recovery puts it back in place before judging it.
        PointerInterference::UserWritesScratch if window == PointerWindow::DestinationRemoved => {
            let scratch = journal.scratch.clone();
            scene.user_writes(&scratch, &[&scratch, &view]);
        }
        PointerInterference::BranchDeleted if existing_branch => {
            scene.ref_moved = true;
            scene.git(&repository, &["update-ref", "-d", "refs/heads/topic"]);
        }
        PointerInterference::BranchMoved if existing_branch => {
            scene.ref_moved = true;
            scene.git(
                &repository,
                &["update-ref", "refs/heads/topic", &scene.commits[1]],
            );
        }
        _ => {}
    }
    tc.note(&format!(
        "{window:?}, {interference:?}, existing branch {existing_branch}"
    ));

    let report = scene.settle(&tc, &[&view]);
    if interference == PointerInterference::None {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }
    if report.errors.is_empty() {
        assert!(!registered(&repository, &view), "the add was not undone");
        assert!(
            !journal.scratch.exists() && !staged.exists(),
            "a rolled-back add left its scratch view or staged pointer"
        );
        assert!(
            !view.join("tracked.txt").exists(),
            "a rolled-back add left its checkout"
        );
    }
}

macro_rules! per_variant {
    ($scenario:ident, $cases:expr; $($name:ident => $variant:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = $cases,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                $scenario(tc, $variant);
            }
        )+
    };
}

per_variant!(pointer_window, 8;
    an_add_killed_with_its_pointer_in_the_scratch_clone_is_rolled_back => PointerWindow::PointerInScratch,
    an_add_killed_after_removing_its_destination_is_rolled_back => PointerWindow::DestinationRemoved,
    an_add_killed_with_its_pointer_staged_is_rolled_back => PointerWindow::PointerStaged,
    an_add_killed_with_its_pointer_in_both_places_is_rolled_back => PointerWindow::PointerStagedAndInPlace,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leftover {
    Scratch,
    BaseStaging,
    TemporaryIndex,
    StagedPointer,
}

/// A second add journal for a destination an active journal already owns, as
/// two racing adds left before the metadata lock covered the claim check.
fn lost_destination(tc: TestCase, phase: AddWorktreePhase) {
    let leftovers = tc.draw(gs::vecs(gs::sampled_from(vec![
        Leftover::Scratch,
        Leftover::BaseStaging,
        Leftover::TemporaryIndex,
        Leftover::StagedPointer,
    ])));
    let winner_registered = tc.draw(gs::booleans());
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    let store = JournalStore::open(&state);
    let winner = store.load_all().expect("winner journal").remove(0);

    // The losing journal: a real intent for another path, renamed onto the
    // winner's destination.
    let elsewhere = scene.fixture.path("elsewhere");
    let stopped = add_worktree_inner(
        scene.fixture.add_request(&elsewhere, &scene.commits[1]),
        Some(AddWorktreePhase::IntentRecorded),
        false,
    )
    .expect_err("stop the losing add");
    assert!(injected(&stopped), "{stopped}");
    let loser = store
        .load_all()
        .expect("journals")
        .into_iter()
        .find(|journal| journal.operation_id != winner.operation_id)
        .expect("losing journal");
    edit_journal(&loser.journal_path, |value| {
        value["destination"] = journal_path_value(&view);
        value["phase"] = serde_json::to_value(phase).unwrap();
        if !matches!(phase, AddWorktreePhase::RollbackPending) {
            value["last_forward_phase"] = serde_json::to_value(phase).unwrap();
        }
    });
    let staged = loser.temporary_index.with_extension("git-pointer");
    for leftover in &leftovers {
        match leftover {
            Leftover::Scratch => {
                fs::create_dir_all(&loser.scratch).unwrap();
                fs::write(loser.scratch.join("tracked.txt"), b"second\n").unwrap();
            }
            Leftover::BaseStaging => {
                fs::create_dir_all(loser.base_staging.join("nested")).unwrap();
            }
            Leftover::TemporaryIndex => {
                fs::write(&loser.temporary_index, b"index").unwrap();
            }
            Leftover::StagedPointer => fs::write(&staged, b"gitdir: elsewhere\n").unwrap(),
        }
    }
    if !winner_registered {
        scene.unregister(&view);
    }
    scene
        .kept
        .push((vec![view.join("tracked.txt")], b"first\n".to_vec()));
    tc.note(&format!(
        "{phase:?}, {leftovers:?}, winner registered {winner_registered}"
    ));
    let before = snapshot(&view);

    // Status answers and says so when two live journals claim one path.
    let accounting = storage_accounting(&state).expect("status with a contested destination");
    if phase == AddWorktreePhase::Active && winner_registered {
        assert!(
            accounting
                .diagnostic_issues
                .iter()
                .any(|issue| issue.reason.contains(&loser.operation_id)),
            "status hid a doubly claimed worktree: {:?}",
            accounting.diagnostic_issues
        );
        // Neither journal proves ownership, so no verb may act on the view.
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: view.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("remove a doubly claimed view");
        managed_worktree_state_directory(&repository, &view)
            .expect_err("resolve a doubly claimed view");
    }

    let report = recover_incomplete_operations(&state).expect("repair");
    tc.note(&format!("repair: {report:?}"));
    recover_incomplete_operations(&state).expect("repair again");
    assert_eq!(snapshot(&view), before, "repair changed the winner's view");
    assert_eq!(registered(&repository, &view), winner_registered);
    if winner_registered {
        assert_eq!(status(&view), "");
    }
    let loser_now = store
        .load_operation(&loser.operation_id)
        .expect("losing journal");
    let holds_pointer = leftovers.contains(&Leftover::StagedPointer);
    if phase != AddWorktreePhase::Active && winner_registered && !holds_pointer {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(loser_now.phase, AddWorktreePhase::RolledBack);
        assert!(
            !loser.scratch.exists()
                && !loser.base_staging.exists()
                && !loser.temporary_index.exists(),
            "a retired journal left its own artifacts"
        );
    }
    if holds_pointer && phase != AddWorktreePhase::Active {
        // A journal still holding a pointer is never retired on trust.
        assert!(staged.exists(), "repair deleted a staged Git pointer");
    }
    let winner_now = store
        .load_operation(&winner.operation_id)
        .expect("winner journal");
    assert_eq!(winner_now.phase, AddWorktreePhase::Active);
}

per_variant!(lost_destination, 5;
    a_losing_intent_never_costs_the_winner_its_view => AddWorktreePhase::IntentRecorded,
    a_losing_registration_never_costs_the_winner_its_view => AddWorktreePhase::GitMetadataCreated,
    a_losing_base_build_never_costs_the_winner_its_view => AddWorktreePhase::BaseReady,
    a_losing_view_never_costs_the_winner_its_view => AddWorktreePhase::ViewCreated,
    a_losing_rollback_never_costs_the_winner_its_view => AddWorktreePhase::RollbackPending,
    a_second_active_journal_never_costs_the_winner_its_view => AddWorktreePhase::Active,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutRoot {
    Absent,
    Directory,
    File,
    /// Recorded outside this operation's own directory.
    Misplaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Status,
    Inspect,
    Compact,
    Move,
    Remove,
    ForceRemove,
    Repair,
}

fn typed(error: &WorktreeError) -> bool {
    matches!(
        error,
        WorktreeError::Unsupported(_)
            | WorktreeError::InvalidRequest(_)
            | WorktreeError::RecoveryPending { .. }
    )
}

/// A state directory carried over from Linux holds OverlayFS journals. On a
/// platform without OverlayFS every verb reports or refuses them.
fn overlayfs_journal(tc: TestCase, verb: Verb) {
    let phase = tc.draw(gs::sampled_from(vec![
        AddWorktreePhase::Active,
        AddWorktreePhase::Active,
        AddWorktreePhase::ViewCreated,
        AddWorktreePhase::CleanVerified,
    ]));
    let layout = tc.draw(gs::sampled_from(vec![
        LayoutRoot::Absent,
        LayoutRoot::Directory,
        LayoutRoot::File,
        LayoutRoot::Misplaced,
    ]));
    let boot_id = tc.draw(gs::sampled_from(vec!["", "boot"]));
    let namespace = tc.draw(gs::integers::<u64>().max_value(2));
    let mount_id = tc.draw(gs::integers::<u64>().max_value(2));
    let context = tc.draw(gs::booleans());
    let identity = tc.draw(gs::booleans());
    let stray_layers = tc.draw(gs::booleans());
    let scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    let journal = JournalStore::open(&state)
        .load_all()
        .expect("add journal")
        .remove(0);
    let overlays = state.join("overlays/v1");
    let layout_root = match layout {
        LayoutRoot::Misplaced => state.join("overlays/v1/another-operation"),
        _ => overlays.join(&journal.operation_id),
    };
    match layout {
        LayoutRoot::Directory => fs::create_dir_all(layout_root.join("upper")).unwrap(),
        LayoutRoot::File => {
            fs::create_dir_all(&overlays).unwrap();
            fs::write(&layout_root, b"").unwrap();
        }
        LayoutRoot::Absent | LayoutRoot::Misplaced => {}
    }
    if stray_layers {
        fs::create_dir_all(overlays.join("unclaimed/upper")).unwrap();
        fs::create_dir_all(state.join("overlays/v2")).unwrap();
    }
    edit_journal(&journal.journal_path, |value| {
        let mount = serde_json::json!({
            "boot_id": boot_id,
            "mount_namespace_device": 1,
            "mount_namespace_inode": namespace,
        });
        let mut overlayfs = serde_json::json!({
            "layout_root": journal_path_value(&layout_root),
            "recovery_token": "ab".repeat(32),
        });
        if context {
            overlayfs["mount_context"] = mount.clone();
        }
        if identity {
            let mut mount = mount;
            mount["mount_id"] = serde_json::json!(mount_id);
            overlayfs["mount_identity"] = mount;
        }
        value["backend"] = serde_json::json!("overlay-fs");
        value["overlayfs"] = overlayfs;
        value["phase"] = serde_json::to_value(phase).unwrap();
        value["last_forward_phase"] = serde_json::to_value(phase).unwrap();
    });
    tc.note(&format!(
        "{phase:?}, layout {layout:?}, boot {boot_id:?}, namespace {namespace}, mount {mount_id}, context {context}, identity {identity}, stray {stray_layers}, {verb:?}"
    ));
    let before = snapshot(&view);
    let overlay_paths = |root: &Path| -> Vec<PathBuf> {
        let mut paths = vec![root.to_path_buf()];
        let mut index = 0;
        while index < paths.len() {
            if paths[index].is_dir() {
                paths.extend(
                    fs::read_dir(&paths[index])
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
            index += 1;
        }
        paths.sort();
        paths
    };
    let layers_before = overlay_paths(&state.join("overlays"));

    let removal = RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: view.clone(),
        state_dir: Some(state.clone()),
    };
    let refused = match verb {
        Verb::Status => {
            let accounting = storage_accounting(&state).expect("status with an OverlayFS journal");
            if stray_layers {
                for stray in [overlays.join("unclaimed"), state.join("overlays/v2")] {
                    assert!(
                        accounting
                            .diagnostic_issues
                            .iter()
                            .any(|issue| issue.path == stray),
                        "status hid {}: {:?}",
                        stray.display(),
                        accounting.diagnostic_issues
                    );
                }
            }
            None
        }
        Verb::Inspect => inspect_managed_worktrees(&repository, std::slice::from_ref(&view))
            .map(|inspections| {
                for inspection in inspections {
                    assert!(
                        inspection.backend.is_none() || inspection.mount_status.is_some(),
                        "an OverlayFS view was reported without a mount status"
                    );
                }
            })
            .err(),
        Verb::Compact => compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: view.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .map(|_| ())
        .err(),
        Verb::Move => move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: view.clone(),
                destination: scene.fixture.path("moved"),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .map(|_| ())
        .err(),
        Verb::Remove => remove_worktree_inner(removal, None).map(|_| ()).err(),
        Verb::ForceRemove => force_remove_worktree_inner(removal, None).map(|_| ()).err(),
        Verb::Repair => {
            let report = recover_incomplete_operations(&state).expect("repair");
            if phase != AddWorktreePhase::Active {
                assert!(
                    !report.errors.is_empty(),
                    "repair rolled back an OverlayFS add without OverlayFS"
                );
            }
            None
        }
    };
    if let Some(error) = &refused {
        assert!(typed(error), "{error:?}");
    }
    if matches!(verb, Verb::Compact | Verb::Move | Verb::ForceRemove) {
        assert!(refused.is_some(), "{verb:?} ran on an OverlayFS journal");
    }
    if view.exists() {
        assert_eq!(snapshot(&view), before, "{verb:?} changed the view");
        assert!(registered(&repository, &view));
    } else {
        assert_eq!(verb, Verb::Remove, "{verb:?} deleted the view");
        assert!(!registered(&repository, &view));
    }
    assert!(!scene.fixture.path("moved").exists());
    assert_eq!(
        overlay_paths(&state.join("overlays")),
        layers_before,
        "{verb:?} changed OverlayFS private layers"
    );
}

per_variant!(overlayfs_journal, 6;
    status_reports_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::Status,
    inspection_reports_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::Inspect,
    compaction_refuses_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::Compact,
    a_move_refuses_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::Move,
    removal_never_half_removes_an_overlayfs_view_where_overlayfs_cannot_run => Verb::Remove,
    forced_removal_refuses_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::ForceRemove,
    repair_reports_an_overlayfs_journal_where_overlayfs_cannot_run => Verb::Repair,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Remains {
    /// The user deleted the view and pruned it.
    Nothing,
    /// Deleted and pruned, with the directory recreated empty.
    EmptyDirectory,
    /// Deleted by hand without telling Git.
    StaleRegistration,
    DanglingSymlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Neighbour {
    None,
    /// An unrelated journal file that cannot be parsed.
    Garbage,
    /// An unrelated journal cut short, as a write in progress looks.
    Truncated,
    /// An unrelated journal from a newer format.
    FutureVersion,
}

/// Adding to a path an older journal still names. The add either proves the
/// old journal dead and takes the path, or refuses; one path never ends up
/// with two live journals.
#[hegel::test(
    test_cases = 24,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn a_destination_is_never_claimed_by_two_live_journals(tc: TestCase) {
    let claimant = tc.draw(gs::sampled_from(vec![
        AddWorktreePhase::Active,
        AddWorktreePhase::Active,
        AddWorktreePhase::CleanVerified,
        AddWorktreePhase::RolledBack,
    ]));
    let remains = tc.draw(gs::sampled_from(vec![
        Remains::Nothing,
        Remains::EmptyDirectory,
        Remains::StaleRegistration,
        Remains::DanglingSymlink,
    ]));
    let neighbour = tc.draw(gs::sampled_from(vec![
        Neighbour::None,
        Neighbour::None,
        Neighbour::Garbage,
        Neighbour::Truncated,
        Neighbour::FutureVersion,
    ]));
    let scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    let store = JournalStore::open(&state);
    let old = store.load_all().expect("add journal").remove(0);

    fs::remove_dir_all(&view).unwrap();
    if remains != Remains::StaleRegistration {
        assert!(scene.git(&repository, &["worktree", "prune"]));
    }
    match remains {
        Remains::EmptyDirectory => fs::create_dir(&view).unwrap(),
        Remains::DanglingSymlink => {
            std::os::unix::fs::symlink(scene.fixture.path("nowhere"), &view).unwrap();
        }
        Remains::Nothing | Remains::StaleRegistration => {}
    }
    edit_journal(&old.journal_path, |value| {
        value["phase"] = serde_json::to_value(claimant).unwrap();
        if claimant == AddWorktreePhase::CleanVerified {
            value["last_forward_phase"] = serde_json::to_value(claimant).unwrap();
        }
    });
    let neighbour_path = state.join("operations/18aa000000000000-1-0.json");
    match neighbour {
        Neighbour::None => {}
        Neighbour::Garbage => fs::write(&neighbour_path, b"not json").unwrap(),
        Neighbour::Truncated => {
            let whole = fs::read(&old.journal_path).unwrap();
            fs::write(&neighbour_path, &whole[..whole.len() / 2]).unwrap();
        }
        Neighbour::FutureVersion => {
            fs::copy(&old.journal_path, &neighbour_path).unwrap();
            edit_journal(&neighbour_path, |value| {
                value["operation_id"] = serde_json::json!("18aa000000000000-1-0");
                value["format_version"] = serde_json::json!(999);
            });
        }
    }
    tc.note(&format!("{claimant:?}, {remains:?}, {neighbour:?}"));

    let added = add_worktree_inner(
        scene.fixture.add_request(&view, &scene.commits[1]),
        None,
        true,
    );
    tc.note(&format!("add: {:?}", added.as_ref().map(|_| ())));
    let live = store
        .load_all_reconciling()
        .expect("journals")
        .journals
        .into_iter()
        .filter(|journal| journal.destination == view)
        .filter(|journal| journal.phase != AddWorktreePhase::RolledBack)
        .collect::<Vec<_>>();
    let retired = RemovalJournalStore::open(&state)
        .load_all()
        .map(|removals| {
            removals
                .into_iter()
                .filter(|removal| removal.phase == RemoveWorktreePhase::Complete)
                .map(|removal| removal.source_add_operation_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let claims = live
        .iter()
        .filter(|journal| !retired.contains(&journal.operation_id))
        .count();
    assert!(claims <= 1, "two live journals claim one path: {live:?}");
    match &added {
        Ok(_) => {
            assert_eq!(
                neighbour,
                Neighbour::None,
                "an add ignored a journal it could not read"
            );
            assert!(registered(&repository, &view));
            assert_eq!(fs::read(view.join("tracked.txt")).unwrap(), b"second\n");
            assert_eq!(status(&view), "");
        }
        Err(error) => {
            assert!(
                matches!(
                    error,
                    WorktreeError::InvalidRequest(_)
                        | WorktreeError::DestinationExists(_)
                        | WorktreeError::RecoveryPending { .. }
                ),
                "{error:?}"
            );
            assert!(
                !view.join("tracked.txt").exists(),
                "a refused add left a checkout"
            );
            if remains == Remains::DanglingSymlink {
                assert!(view.is_symlink(), "a refused add removed the user's link");
            }
        }
    }
    if neighbour != Neighbour::None {
        assert!(
            neighbour_path.is_file(),
            "an unreadable journal was deleted"
        );
    }
}
