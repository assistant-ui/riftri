//! Removal journals in every phase against every place the view can be, and
//! removals interrupted at the exact points where a write can slip in. A
//! removal that was not forced never costs a file the user wrote, a forced
//! one deletes only the content it recorded, and repair ends with the view
//! either whole and registered or gone.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::{
    RemoveWorktreeRequest, force_remove_worktree_inner, remove_worktree_inner, storage_accounting,
};
use super::recovery::{Scene, injected};
use super::{edit_journal, registered};
use crate::RemoveWorktreePhase;
use crate::journal::JournalStore;
use crate::test_hooks::{self, FilesystemRacePoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    InPlace,
    Quarantined,
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForceSnapshot {
    Recorded,
    /// A well-formed digest of some other content.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extra {
    None,
    UserWrote,
    QuarantineIsFile,
    /// The add journal claims a backend this platform cannot remove.
    OverlayFsJournal,
}

fn removal_state(tc: TestCase, phase: RemoveWorktreePhase) {
    let force = tc.draw(gs::booleans());
    let place = tc.draw(gs::sampled_from(vec![
        Where::InPlace,
        Where::InPlace,
        Where::Quarantined,
        Where::Gone,
    ]));
    let still_registered = tc.draw(gs::booleans());
    let force_snapshot = tc.draw(gs::sampled_from(vec![
        ForceSnapshot::Recorded,
        ForceSnapshot::Stale,
    ]));
    let extra = tc.draw(gs::sampled_from(vec![
        Extra::None,
        Extra::None,
        Extra::UserWrote,
        Extra::QuarantineIsFile,
        Extra::OverlayFsJournal,
    ]));
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let state = scene.fixture.state.clone();
    let request = RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: view.clone(),
        state_dir: Some(state.clone()),
    };
    let stopped = if force {
        force_remove_worktree_inner(request, Some(RemoveWorktreePhase::IntentRecorded))
    } else {
        remove_worktree_inner(request, Some(RemoveWorktreePhase::IntentRecorded))
    }
    .expect_err("stop the removal");
    assert!(injected(&stopped), "{stopped}");
    let journal = scene.only_journal("removals");
    let operation = journal.file_stem().unwrap().to_str().unwrap().to_owned();
    let quarantine = scene.fixture.path(format!(".riftri-remove-{operation}"));

    match place {
        Where::InPlace => {
            if !still_registered {
                scene.unregister(&view);
            }
        }
        Where::Quarantined | Where::Gone => {
            fs::rename(&view, &quarantine).unwrap();
            if !still_registered {
                assert!(scene.git(&repository, &["worktree", "prune"]));
            }
            if place == Where::Gone {
                fs::remove_dir_all(&quarantine).unwrap();
            }
        }
    }
    let home = match place {
        Where::InPlace => Some(view.clone()),
        Where::Quarantined => Some(quarantine.clone()),
        Where::Gone => None,
    };
    match (extra, &home) {
        // Once Git has dropped a quarantined view, only this removal's own
        // name still points at it; a write there has no other owner.
        (Extra::UserWrote, Some(home)) if place == Where::InPlace || still_registered => {
            scene.user_writes(home, &[&view, &quarantine]);
        }
        (Extra::QuarantineIsFile, _) if place != Where::Quarantined => {
            fs::write(&quarantine, b"a file where the quarantine goes").unwrap();
            scene.kept.push((
                vec![quarantine.clone()],
                b"a file where the quarantine goes".to_vec(),
            ));
        }
        (Extra::OverlayFsJournal, _) => {
            let add = JournalStore::open(&state)
                .load_all()
                .expect("add journal")
                .remove(0);
            edit_journal(&add.journal_path, |value| {
                value["backend"] = serde_json::json!("overlay-fs");
                value["overlayfs"] = serde_json::json!({
                    "layout_root": super::journal_path_value(
                        &state.join("overlays/v1").join(&add.operation_id)
                    ),
                    "recovery_token": "ab".repeat(32),
                });
            });
        }
        _ => {}
    }
    if !still_registered || extra == Extra::OverlayFsJournal {
        // An unregistered view in place may be anyone's directory by now, and
        // nothing on this platform can vouch for an OverlayFS view.
        if let Some(home) = home
            .as_ref()
            .filter(|_| place == Where::InPlace || still_registered)
        {
            scene.kept.push((
                vec![view.join("tracked.txt"), home.join("tracked.txt")],
                b"first\n".to_vec(),
            ));
        }
        // Git no longer answers for the view.
        scene.ref_moved = true;
    }
    edit_journal(&journal, |value| {
        value["phase"] = serde_json::to_value(phase).unwrap();
        if force && force_snapshot == ForceSnapshot::Stale {
            value["force_snapshot"] = serde_json::json!("0".repeat(64));
        }
    });
    tc.note(&format!(
        "{phase:?}, force {force}, {place:?}, registered {still_registered}, {force_snapshot:?}, {extra:?}"
    ));

    // A pending removal is counted while it waits.
    let accounting = storage_accounting(&state).expect("status with a pending removal");
    assert!(
        accounting.pending_removals == 1 || !accounting.diagnostic_issues.is_empty(),
        "{accounting:?}"
    );

    let report = scene.settle(&tc, &[&view]);
    if force
        && force_snapshot != ForceSnapshot::Recorded
        && place == Where::InPlace
        && still_registered
        && phase <= RemoveWorktreePhase::CleanVerified
    {
        // Nothing vouches for what a forced removal may delete.
        assert!(
            view.join("tracked.txt").is_file(),
            "an unverified view was deleted"
        );
    }
    if report.errors.is_empty() {
        assert_eq!(view.is_dir(), registered(&repository, &view));
        // The quarantine exists only between the clean check and the phase
        // after it; a later phase has no business with that name.
        if phase <= RemoveWorktreePhase::CleanVerified {
            assert!(
                fs::symlink_metadata(&quarantine).is_err(),
                "a settled removal left its quarantine"
            );
        }
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
                removal_state(tc, $phase);
            }
        )+
    };
}

per_phase!(
    a_removal_intent_beside_any_arrangement_settles_or_is_reported => RemoveWorktreePhase::IntentRecorded,
    a_verified_removal_beside_any_arrangement_settles_or_is_reported => RemoveWorktreePhase::CleanVerified,
    a_finished_deletion_beside_any_arrangement_settles_or_is_reported => RemoveWorktreePhase::WorktreeRemoved,
    a_released_base_beside_any_arrangement_settles_or_is_reported => RemoveWorktreePhase::BaseReleased,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intrusion {
    /// A file lands in the view after the last check that could see it.
    WriteIntoView,
    /// Something new appears at the path the view just left.
    RecreateDestination,
    /// The path the view just left becomes a file.
    DestinationBecomesFile,
    Nothing,
}

/// The two hook points sit between a removal's last look at the view and the
/// step that makes the view unreachable. Whatever lands there is kept.
#[hegel::test(
    test_cases = 20,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn a_write_that_races_a_removal_is_never_deleted(tc: TestCase) {
    let force = tc.draw(gs::booleans());
    let point = tc.draw(gs::sampled_from(vec![
        FilesystemRacePoint::RemovalQuarantined,
        FilesystemRacePoint::RemovalQuarantined,
        FilesystemRacePoint::ForceRemovalRevalidation,
    ]));
    let intrusion = tc.draw(gs::sampled_from(vec![
        Intrusion::WriteIntoView,
        Intrusion::RecreateDestination,
        Intrusion::DestinationBecomesFile,
        Intrusion::Nothing,
    ]));
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let request = RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: view.clone(),
        state_dir: Some(scene.fixture.state.clone()),
    };
    // Where the view was when the hook ran, which is where a write went.
    let seen = Rc::new(RefCell::new(None::<PathBuf>));
    let guard = {
        let seen = Rc::clone(&seen);
        let view = view.clone();
        test_hooks::install(point, move |path| {
            *seen.borrow_mut() = Some(path.to_path_buf());
            match intrusion {
                Intrusion::WriteIntoView => {
                    fs::write(path.join("user-file.txt"), b"written by the user\n").unwrap();
                }
                Intrusion::RecreateDestination if path != view => {
                    fs::create_dir(&view).unwrap();
                    fs::write(view.join("new-work.txt"), b"new work\n").unwrap();
                }
                Intrusion::DestinationBecomesFile if path != view => {
                    fs::write(&view, b"new work\n").unwrap();
                }
                _ => {}
            }
        })
    };
    let removed = if force {
        force_remove_worktree_inner(request, None)
    } else {
        remove_worktree_inner(request, None)
    };
    drop(guard);
    let seen = seen.borrow().clone();
    tc.note(&format!(
        "force {force}, {point:?}, {intrusion:?}, hook ran at {seen:?}: {:?}",
        removed.as_ref().map(|_| ())
    ));
    let fired = seen.is_some();
    assert_eq!(
        fired,
        force || point == FilesystemRacePoint::RemovalQuarantined,
        "the hook point did not behave as the test assumes"
    );
    if let Some(seen) = &seen {
        let moved = seen != &view;
        match intrusion {
            Intrusion::WriteIntoView => {
                removed.expect_err("a removal deleted a view that changed under it");
                scene.kept.push((
                    vec![view.join("user-file.txt"), seen.join("user-file.txt")],
                    b"written by the user\n".to_vec(),
                ));
            }
            Intrusion::RecreateDestination if moved => {
                scene
                    .kept
                    .push((vec![view.join("new-work.txt")], b"new work\n".to_vec()));
                scene.ref_moved = true;
            }
            Intrusion::DestinationBecomesFile if moved => {
                scene
                    .kept
                    .push((vec![view.clone()], b"new work\n".to_vec()));
                scene.ref_moved = true;
            }
            _ => {
                removed.expect("an undisturbed removal");
                assert!(!view.exists() && !registered(&repository, &view));
            }
        }
    }
    scene.settle(&tc, &[&view]);
}
