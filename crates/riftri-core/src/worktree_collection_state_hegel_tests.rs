//! Garbage-collection journals in every phase against every arrangement of
//! the base, its completion marker, and its quarantine, including journals
//! whose paths were tampered with. Resuming one deletes a base only through
//! its own quarantine name, never a base a live journal references, and
//! never anything a tampered journal points at.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use hegel::TestCase;
use hegel::generators as gs;

use super::super::{
    WorktreeError, garbage_collect, garbage_collect_inner, recover_incomplete_operations,
    storage_accounting, verify_existing_base,
};
use super::state::{BaseShape, MarkerShape, SyntheticState};
use super::{edit_journal, journal_path_value, snapshot};
use crate::GarbageCollectionPhase;
use crate::journal::CollectionJournalStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quarantine {
    Absent,
    /// The base was renamed here, as the step after `marker-removed` does.
    HoldsBase,
    EmptyDirectory,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseNow {
    AsLeft,
    Gone,
    File,
    /// Another add rebuilt the base after it was quarantined.
    Rebuilt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerNow {
    AsLeft,
    Gone,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tamper {
    None,
    QuarantineRenamed,
    QuarantineOutside,
    MarkerElsewhere,
    BaseOutside,
    BaseRelative,
    LockIsDirectory,
    LockIsSymlink,
}

fn collection_state(tc: TestCase, phase: GarbageCollectionPhase) {
    let fixture = SyntheticState::new();
    fixture.plant(0, BaseShape::ReadOnlyDirectory, MarkerShape::File, &tc);
    let base = fixture.base(0);
    let marker = base.with_extension("complete");
    let stopped = garbage_collect_inner(
        &fixture.state,
        true,
        Some(GarbageCollectionPhase::IntentRecorded),
    );
    assert!(matches!(
        stopped,
        Err(WorktreeError::InjectedCollectionFailure(_))
    ));
    let store = CollectionJournalStore::open(&fixture.state);
    let journal = store.load_all().expect("collection journal").remove(0);

    let quarantine = tc.draw(gs::sampled_from(vec![
        Quarantine::Absent,
        Quarantine::Absent,
        Quarantine::HoldsBase,
        Quarantine::HoldsBase,
        Quarantine::EmptyDirectory,
        Quarantine::File,
    ]));
    let base_now = tc.draw(gs::sampled_from(vec![
        BaseNow::AsLeft,
        BaseNow::AsLeft,
        BaseNow::Gone,
        BaseNow::File,
        BaseNow::Rebuilt,
    ]));
    let marker_now = tc.draw(gs::sampled_from(vec![
        MarkerNow::AsLeft,
        MarkerNow::AsLeft,
        MarkerNow::Gone,
        MarkerNow::Gone,
        MarkerNow::Directory,
        MarkerNow::Symlink,
    ]));
    let tamper = tc.draw(gs::sampled_from(vec![
        Tamper::None,
        Tamper::None,
        Tamper::None,
        Tamper::None,
        Tamper::QuarantineRenamed,
        Tamper::QuarantineOutside,
        Tamper::MarkerElsewhere,
        Tamper::BaseOutside,
        Tamper::BaseRelative,
        Tamper::LockIsDirectory,
        Tamper::LockIsSymlink,
    ]));
    let referenced = tc.draw(gs::booleans());

    let unseal = |path: &std::path::Path| {
        for directory in [path.to_path_buf(), path.join("nested")] {
            let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o755));
        }
    };
    match quarantine {
        Quarantine::Absent => {}
        Quarantine::HoldsBase => fs::rename(&base, &journal.quarantine_path).unwrap(),
        Quarantine::EmptyDirectory => fs::create_dir(&journal.quarantine_path).unwrap(),
        Quarantine::File => fs::write(&journal.quarantine_path, b"").unwrap(),
    }
    match base_now {
        BaseNow::AsLeft => {}
        BaseNow::Gone | BaseNow::File | BaseNow::Rebuilt => {
            if base.exists() {
                unseal(&base);
                fs::remove_dir_all(&base).unwrap();
            }
            match base_now {
                BaseNow::File => fs::write(&base, b"a file where the base was").unwrap(),
                BaseNow::Rebuilt => {
                    fs::remove_file(&marker).unwrap();
                    fixture.plant(0, BaseShape::ReadOnlyDirectory, MarkerShape::File, &tc);
                }
                _ => {}
            }
        }
    }
    match marker_now {
        MarkerNow::AsLeft => {}
        MarkerNow::Gone | MarkerNow::Directory | MarkerNow::Symlink => {
            fs::remove_file(&marker).unwrap();
            match marker_now {
                MarkerNow::Directory => fs::create_dir(&marker).unwrap(),
                MarkerNow::Symlink => symlink(fixture.outside.join("kept/data"), &marker).unwrap(),
                _ => {}
            }
        }
    }
    let lock = base.with_extension("lock");
    edit_journal(&journal.journal_path, |value| {
        value["phase"] = serde_json::to_value(phase).unwrap();
        match tamper {
            Tamper::QuarantineRenamed => {
                value["quarantine_path"] = journal_path_value(&fixture.bucket().join("kept-aside"));
            }
            Tamper::QuarantineOutside => {
                value["quarantine_path"] = journal_path_value(
                    &fixture
                        .outside
                        .join(journal.quarantine_path.file_name().unwrap()),
                );
            }
            Tamper::MarkerElsewhere => {
                value["marker_path"] = journal_path_value(&fixture.outside.join("kept/data"));
            }
            Tamper::BaseOutside => {
                value["base_path"] = journal_path_value(&fixture.outside.join("kept"));
            }
            Tamper::BaseRelative => {
                value["base_path"] = journal_path_value(std::path::Path::new("kept"));
            }
            _ => {}
        }
    });
    match tamper {
        Tamper::LockIsDirectory => {
            let _ = fs::remove_file(&lock);
            fs::create_dir(&lock).unwrap();
        }
        Tamper::LockIsSymlink => {
            let _ = fs::remove_file(&lock);
            symlink(fixture.outside.join("kept/data"), &lock).unwrap();
        }
        _ => {}
    }
    if referenced {
        fixture.reference(&base);
    }
    tc.note(&format!(
        "{phase:?}, quarantine {quarantine:?}, base {base_now:?}, marker {marker_now:?}, {tamper:?}, referenced {referenced}"
    ));

    let finished = matches!(
        phase,
        GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
    );
    let outside = snapshot(&fixture.outside);
    let before = fixture.everything();
    let base_before = base.is_dir().then(|| snapshot(&base));

    // Status and a plan describe the state directory without changing it.
    let accounting = storage_accounting(&fixture.state).expect("status");
    assert_eq!(
        accounting.pending_collections,
        usize::from(!finished),
        "{accounting:?}"
    );
    let _ = garbage_collect(&fixture.state, false);
    assert_eq!(
        fixture.everything(),
        before,
        "a plan or status wrote to disk"
    );

    // The synthetic reference names no real repository, so resume the way a
    // collection run does: without planning anything new.
    let errors = if referenced {
        super::super::recover_collection_journals(&fixture.state)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default()
    } else {
        recover_incomplete_operations(&fixture.state)
            .expect("repair")
            .errors
    };
    tc.note(&format!("errors: {errors:?}"));
    let after = store
        .load_all()
        .ok()
        .and_then(|mut journals| journals.pop())
        .map(|journal| journal.phase);

    assert_eq!(
        snapshot(&fixture.outside),
        outside,
        "a path outside changed"
    );
    if finished {
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            fixture.everything(),
            before,
            "a finished journal was resumed"
        );
        return;
    }
    if tamper != Tamper::None {
        assert!(!errors.is_empty(), "a tampered collection was resumed");
        // Only a lock file may appear; nothing else moves.
        for (path, entry) in &before {
            if path.ends_with(b".lock") {
                continue;
            }
            assert_eq!(
                fixture.everything().get(path),
                Some(entry),
                "a tampered collection changed {}",
                String::from_utf8_lossy(path)
            );
        }
        return;
    }
    if referenced && quarantine == Quarantine::Absent {
        // The reference wins while the base is still in place.
        if let Some(content) = &base_before {
            assert_eq!(
                base.is_dir().then(|| snapshot(&base)).as_ref(),
                Some(content),
                "a referenced base changed"
            );
        }
    }
    if errors.is_empty() {
        assert!(
            matches!(
                after,
                Some(GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled)
            ),
            "an error-free resume left the journal at {after:?}"
        );
        if after == Some(GarbageCollectionPhase::Complete) {
            assert!(
                fs::symlink_metadata(&journal.quarantine_path).is_err(),
                "a completed collection left its quarantine"
            );
        }
        if after == Some(GarbageCollectionPhase::Cancelled)
            && phase == GarbageCollectionPhase::MarkerRemoved
            && base.is_dir()
        {
            assert!(
                verify_existing_base(&base, &marker).expect("verify the kept base"),
                "a cancelled collection left a base no marker vouches for"
            );
        }
    }
}

macro_rules! per_phase {
    ($($name:ident => $phase:expr),+ $(,)?) => {
        $(
            #[hegel::test(test_cases = 70, derandomize = true)]
            fn $name(tc: TestCase) {
                collection_state(tc, $phase);
            }
        )+
    };
}

per_phase!(
    a_collection_intent_deletes_only_its_own_quarantine => GarbageCollectionPhase::IntentRecorded,
    a_collection_past_its_marker_deletes_only_its_own_quarantine => GarbageCollectionPhase::MarkerRemoved,
    a_collection_past_its_quarantine_deletes_only_its_own_quarantine => GarbageCollectionPhase::BaseQuarantined,
    a_completed_collection_is_left_alone => GarbageCollectionPhase::Complete,
    a_cancelled_collection_is_left_alone => GarbageCollectionPhase::Cancelled,
);
