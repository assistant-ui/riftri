//! Property tests for how journals change over an operation's life: phase
//! transitions, the add journal's crash and recovery interleavings, the
//! in-place retargeting an active add journal allows, and the state paths a
//! store refuses to treat as its directory.

use std::fmt::Debug;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use tempfile::tempdir;

use super::hegel_tests::{
    AddParts, FILESYSTEM_CASES, Kind, entry_names, native_path, operation_id, sha256_digest,
};
use super::{
    CollectionJournalPaths, CollectionJournalRecord, CompactJournalPaths, CompactJournalRecord,
    DecodedJournal, JournalError, JournalRecord, JournalStore, MoveJournalPaths, MoveJournalRecord,
    PruneJournalRecord, RemovalJournalPaths, RemovalJournalRecord, journal_error_is_in_flight,
};
use crate::test_hooks::{self, FilesystemRacePoint};
use crate::{
    AddWorktreePhase, CompactWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase,
    PruneWorktreesPhase, RemoveWorktreePhase,
};

const ADD_PHASES: [AddWorktreePhase; 10] = [
    AddWorktreePhase::IntentRecorded,
    AddWorktreePhase::GitMetadataCreated,
    AddWorktreePhase::BaseReady,
    AddWorktreePhase::ViewCreated,
    AddWorktreePhase::GitPointerRestored,
    AddWorktreePhase::IndexSynchronized,
    AddWorktreePhase::CleanVerified,
    AddWorktreePhase::Active,
    AddWorktreePhase::RollbackPending,
    AddWorktreePhase::RolledBack,
];

fn is_rollback(phase: AddWorktreePhase) -> bool {
    matches!(
        phase,
        AddWorktreePhase::RollbackPending | AddWorktreePhase::RolledBack
    )
}

/// A phase to request next: usually the one declared after `current`, so
/// walks get deep into the graph, otherwise any phase at all.
fn next_phase<P: Copy + PartialEq + Debug + Send + Sync + 'static>(
    tc: &TestCase,
    all: &[P],
    current: P,
) -> P {
    if tc.draw(gs::booleans()) {
        let index = all.iter().position(|phase| *phase == current).unwrap_or(0);
        all[(index + 1) % all.len()]
    } else {
        tc.draw(gs::sampled_from(all.to_vec()))
    }
}

/// One record type's phase machine, so every family is walked the same way.
trait Phased {
    type Phase: Copy + PartialEq + Debug + Send + Sync + 'static;

    fn phase(&self) -> Self::Phase;
    /// `Err` carries the `(current, requested)` pair the refusal reported.
    fn step(&mut self, next: Self::Phase) -> Result<(), (Self::Phase, Self::Phase)>;
}

macro_rules! phased {
    ($record:ty, $phase:ty) => {
        impl Phased for $record {
            type Phase = $phase;

            fn phase(&self) -> $phase {
                self.phase
            }

            fn step(&mut self, next: $phase) -> Result<(), ($phase, $phase)> {
                self.transition(next)
                    .map_err(|error| (error.current, error.requested))
            }
        }
    };
}

phased!(JournalRecord, AddWorktreePhase);
phased!(RemovalJournalRecord, RemoveWorktreePhase);
phased!(MoveJournalRecord, MoveWorktreePhase);
phased!(CompactJournalRecord, CompactWorktreePhase);
phased!(PruneJournalRecord, PruneWorktreesPhase);

impl Phased for CollectionJournalRecord {
    type Phase = GarbageCollectionPhase;

    fn phase(&self) -> GarbageCollectionPhase {
        self.phase
    }

    fn step(
        &mut self,
        next: GarbageCollectionPhase,
    ) -> Result<(), (GarbageCollectionPhase, GarbageCollectionPhase)> {
        self.transition(next).map_err(|error| match error {
            JournalError::InvalidCollectionTransition { current, requested } => {
                (current, requested)
            }
            other => panic!("unexpected collection transition failure: {other}"),
        })
    }
}

/// Request a generated sequence of phases and return the accepted ones, in
/// order, starting with the record's initial phase.
///
/// Whatever the sequence, a refusal names the exact pair it refused and leaves
/// the record where it was, a phase is never entered twice (so the graph has
/// no cycles and no self-loops), and nothing follows a `terminal` phase.
fn walk<R: Phased>(tc: &TestCase, record: &mut R, all: &[R::Phase], terminal: &[R::Phase]) {
    let mut entered = vec![record.phase()];
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(16)) {
        let current = record.phase();
        let next = next_phase(tc, all, current);
        match record.step(next) {
            Ok(()) => {
                assert_eq!(record.phase(), next);
                assert!(
                    !terminal.contains(&current),
                    "{current:?} is finished but moved to {next:?}"
                );
                assert!(
                    !entered.contains(&next),
                    "{next:?} was entered twice: {entered:?}"
                );
                entered.push(next);
            }
            Err(reported) => {
                assert_eq!(reported, (current, next));
                assert_eq!(record.phase(), current);
            }
        }
    }
    tc.note(&format!("accepted walk: {entered:?}"));
    // A cancellation is only reachable before the step that cannot be undone.
    let position = |name: &str| {
        entered
            .iter()
            .position(|phase| format!("{phase:?}") == name)
    };
    if let Some(cancelled) = position("Cancelled") {
        for committed in [
            "WorktreeRemoved",
            "WorktreeMoved",
            "BaseQuarantined",
            "Complete",
        ] {
            assert!(
                position(committed).is_none_or(|index| index > cancelled),
                "cancelled after {committed}: {entered:?}"
            );
        }
        // A compaction that already swapped the views is only cancelled by
        // swapping them back first.
        if position("ReplacementActivated").is_some() {
            assert_eq!(position("RestoringOriginal"), Some(cancelled - 1));
        }
    }
}

#[hegel::test]
fn journal_records_only_move_forward_through_their_phases(tc: TestCase) {
    let paths = (0..8).map(|_| native_path(&tc)).collect::<Vec<_>>();
    let id = || "operation".to_owned();

    let mut add = AddParts::draw(&tc).record("operation");
    let mut last_forward = add.last_forward_phase;
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(16)) {
        let next = next_phase(&tc, &ADD_PHASES, add.phase);
        if add.transition(next).is_ok() && !is_rollback(next) {
            assert!(
                next > last_forward,
                "{last_forward:?} moved back to {next:?}"
            );
            last_forward = next;
        }
        // Rollback keeps the phase the add had reached, and an add that became
        // active is never rolled back.
        assert_eq!(add.last_forward_phase, last_forward);
        assert!(!(is_rollback(add.phase) && last_forward == AddWorktreePhase::Active));
    }
    walk(
        &tc,
        &mut AddParts::draw(&tc).record("operation"),
        &ADD_PHASES,
        &[AddWorktreePhase::Active, AddWorktreePhase::RolledBack],
    );

    walk(
        &tc,
        &mut RemovalJournalRecord::new(
            id(),
            RemovalJournalPaths {
                repository: &paths[0],
                destination: &paths[1],
                base_path: &paths[2],
            },
            id(),
        ),
        &[
            RemoveWorktreePhase::IntentRecorded,
            RemoveWorktreePhase::CleanVerified,
            RemoveWorktreePhase::WorktreeRemoved,
            RemoveWorktreePhase::BaseReleased,
            RemoveWorktreePhase::Complete,
            RemoveWorktreePhase::Cancelled,
        ],
        &[
            RemoveWorktreePhase::Complete,
            RemoveWorktreePhase::Cancelled,
        ],
    );
    walk(
        &tc,
        &mut MoveJournalRecord::new(
            id(),
            MoveJournalPaths {
                repository: &paths[0],
                source: &paths[1],
                destination: &paths[2],
            },
            id(),
        ),
        &[
            MoveWorktreePhase::IntentRecorded,
            MoveWorktreePhase::WorktreeMoved,
            MoveWorktreePhase::AddJournalUpdated,
            MoveWorktreePhase::Complete,
            MoveWorktreePhase::Cancelled,
        ],
        &[MoveWorktreePhase::Complete, MoveWorktreePhase::Cancelled],
    );
    walk(
        &tc,
        &mut CompactJournalRecord::new(
            id(),
            CompactJournalPaths {
                repository: &paths[0],
                destination: &paths[1],
                replacement: &paths[2],
                quarantine: &paths[3],
                base_staging: &paths[4],
                base_path: &paths[5],
                temporary_index: &paths[6],
                old_base_path: &paths[7],
            },
            id(),
            id(),
            sha256_digest(&tc),
            riftri_storage::BackendKind::ApfsClone,
        ),
        &[
            CompactWorktreePhase::IntentRecorded,
            CompactWorktreePhase::ReplacementReady,
            CompactWorktreePhase::ReplacementActivated,
            CompactWorktreePhase::AddJournalUpdated,
            CompactWorktreePhase::RestoringOriginal,
            CompactWorktreePhase::Cancelled,
            CompactWorktreePhase::Complete,
        ],
        &[
            CompactWorktreePhase::Complete,
            CompactWorktreePhase::Cancelled,
        ],
    );
    walk(
        &tc,
        &mut PruneJournalRecord::new(id(), &paths[0]),
        &[
            PruneWorktreesPhase::IntentRecorded,
            PruneWorktreesPhase::GitMetadataPruned,
            PruneWorktreesPhase::Complete,
        ],
        &[PruneWorktreesPhase::Complete],
    );
    walk(
        &tc,
        &mut CollectionJournalRecord::new(
            id(),
            CollectionJournalPaths {
                base_path: &paths[0],
                quarantine_path: &paths[1],
                marker_path: &paths[2],
            },
        ),
        &[
            GarbageCollectionPhase::IntentRecorded,
            GarbageCollectionPhase::MarkerRemoved,
            GarbageCollectionPhase::BaseQuarantined,
            GarbageCollectionPhase::Complete,
            GarbageCollectionPhase::Cancelled,
        ],
        &[
            GarbageCollectionPhase::Complete,
            GarbageCollectionPhase::Cancelled,
        ],
    );
}

/// The add journals of one state directory under any interleaving of new
/// operations, phase updates, crashes in the middle of a write, restarts,
/// recovery with an outdated snapshot, relocation, and retirement.
struct AddJournalLifecycle {
    _fixture: tempfile::TempDir,
    state: PathBuf,
    store: JournalStore,
    /// What recovery must find: each live operation's last persisted journal.
    durable: Vec<DecodedJournal>,
    /// Snapshots a later write replaced, as a slow recovery might still hold.
    superseded: Vec<DecodedJournal>,
    begun: usize,
}

impl AddJournalLifecycle {
    fn pick(&self, tc: &TestCase) -> usize {
        tc.assume(!self.durable.is_empty());
        tc.draw(gs::integers::<usize>().max_value(self.durable.len() - 1))
    }

    fn replace(&mut self, index: usize, updated: DecodedJournal) {
        let previous = std::mem::replace(&mut self.durable[index], updated);
        if previous != self.durable[index] {
            self.superseded.push(previous);
        }
    }
}

#[hegel::state_machine]
impl AddJournalLifecycle {
    #[rule]
    fn begin(&mut self, tc: TestCase) {
        let id = format!("{}-{}", self.begun, operation_id(&tc));
        self.begun += 1;
        let path = self
            .store
            .persist(&AddParts::draw(&tc).record(&id))
            .expect("persist intent");
        let journal = self.store.load_operation(&id).expect("load intent");
        assert_eq!(journal.journal_path, path);
        assert_eq!(journal.phase, AddWorktreePhase::IntentRecorded);
        self.durable.push(journal);
    }

    #[rule]
    fn advance(&mut self, tc: TestCase) {
        let index = self.pick(&tc);
        let current = self.durable[index].clone();
        let next = next_phase(&tc, &ADD_PHASES, current.phase);
        let mut record = self.store.reload(&current).expect("reload journal");
        if record.transition(next).is_err() {
            // A refused transition persists nothing; the invariant checks it.
            return;
        }
        self.store.persist(&record).expect("persist phase");
        let last_forward_phase = if is_rollback(next) {
            current.last_forward_phase
        } else {
            next
        };
        self.replace(
            index,
            DecodedJournal {
                phase: next,
                last_forward_phase,
                ..current
            },
        );
    }

    /// A crash before the atomic rename leaves only a temporary file, however
    /// much of the next version reached it. The journal is still the old one.
    #[rule]
    fn crash_while_persisting(&mut self, tc: TestCase) {
        let index = self.pick(&tc);
        let current = &self.durable[index];
        let mut record = self.store.reload(current).expect("reload journal");
        record.phase = tc.draw(gs::sampled_from(ADD_PHASES.to_vec()));
        let bytes = serde_json::to_vec_pretty(&record).expect("serialize");
        let written = tc.draw(gs::integers::<usize>().max_value(bytes.len()));
        let nonce = tc.draw(gs::integers::<u32>());
        let temporary = current
            .journal_path
            .with_file_name(format!(".{}.{nonce}.tmp", current.operation_id));
        fs::write(temporary, &bytes[..written]).expect("leave a torn temporary");
    }

    #[rule]
    fn restart(&mut self, _: TestCase) {
        self.store = JournalStore::open(&self.state);
        for journal in &self.durable {
            assert_eq!(
                &self
                    .store
                    .load_operation(&journal.operation_id)
                    .expect("load after restart"),
                journal
            );
        }
    }

    /// Recovery validated its snapshot before another writer moved the
    /// journal on. Acting on that snapshot must fail and change nothing.
    #[rule]
    fn recover_with_a_superseded_snapshot(&mut self, tc: TestCase) {
        tc.assume(!self.superseded.is_empty());
        let stale =
            &self.superseded[tc.draw(gs::integers::<usize>().max_value(self.superseded.len() - 1))];
        tc.assume(!self.durable.contains(stale));
        let phase = tc.draw(gs::sampled_from(ADD_PHASES.to_vec()));
        let outcome = self.store.update_phase(stale, phase);
        assert!(
            outcome.is_err(),
            "a superseded snapshot was applied: {outcome:?}"
        );
    }

    #[rule]
    fn roll_back_during_recovery(&mut self, tc: TestCase) {
        let index = self.pick(&tc);
        let current = self.durable[index].clone();
        let next = if current.phase == AddWorktreePhase::RollbackPending {
            AddWorktreePhase::RolledBack
        } else {
            AddWorktreePhase::RollbackPending
        };
        tc.assume(current.phase.can_transition_to(next));
        let updated = self
            .store
            .update_phase(&current, next)
            .expect("persist rollback phase");
        assert_eq!(
            updated,
            DecodedJournal {
                phase: next,
                ..current
            }
        );
        self.replace(index, updated);
    }

    #[rule]
    fn relocate(&mut self, tc: TestCase) {
        let index = self.pick(&tc);
        let current = self.durable[index].clone();
        let destination = native_path(&tc);
        let outcome = self.store.update_active_destination(
            &current.journal_path,
            &current.destination,
            &destination,
        );
        if current.phase != AddWorktreePhase::Active {
            assert!(matches!(outcome, Err(JournalError::InvalidRecord { .. })));
            return;
        }
        outcome.expect("relocate an active journal");
        self.replace(
            index,
            DecodedJournal {
                destination,
                ..current
            },
        );
    }

    /// `gc --apply` deletes a finished journal; nothing else may notice.
    #[rule]
    fn retire(&mut self, tc: TestCase) {
        let index = self.pick(&tc);
        let retired = self.durable.remove(index);
        fs::remove_file(&retired.journal_path).expect("retire journal");
        self.superseded.push(retired);
    }

    #[invariant(always_run)]
    fn every_reader_finds_exactly_the_durable_journals(&self, _: TestCase) {
        let mut expected = self.durable.clone();
        expected.sort_by(|left, right| left.journal_path.cmp(&right.journal_path));
        assert_eq!(self.store.load_all().expect("load all"), expected);
        let status = self.store.load_all_for_status().expect("status load");
        assert!(status.issues.is_empty(), "{:?}", status.issues);
        assert_eq!(status.journals, expected);
        let reconciling = self.store.load_all_reconciling().expect("reconciling load");
        assert!(reconciling.unreadable.is_empty() && reconciling.issues.is_empty());
        assert_eq!(reconciling.journals, expected);
        for journal in &expected {
            let (phase, forward) = (journal.phase, journal.last_forward_phase);
            assert!(!is_rollback(forward), "{journal:?}");
            if is_rollback(phase) {
                assert_ne!(forward, AddWorktreePhase::Active, "{journal:?}");
            } else {
                assert_eq!(phase, forward, "{journal:?}");
            }
        }
    }
}

#[hegel::test(test_cases = 40)]
fn add_journals_stay_recoverable_under_any_interleaving(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let state = fixture.path().to_path_buf();
    let store = JournalStore::create(&state).expect("create store");
    let machine = AddJournalLifecycle {
        _fixture: fixture,
        state,
        store,
        durable: Vec::new(),
        superseded: Vec::new(),
        begun: 0,
    };
    hegel::stateful::machine(machine).steps(24).run(tc);
}

/// One persisted add journal, active unless the case asks for an earlier
/// phase, with the store and decoded form the retargeting calls start from.
struct ActiveAdd {
    _fixture: tempfile::TempDir,
    store: JournalStore,
    journal: DecodedJournal,
}

impl ActiveAdd {
    fn draw(tc: &TestCase) -> Self {
        Self::with(tc, &AddParts::draw(tc))
    }

    fn with(tc: &TestCase, parts: &AddParts) -> Self {
        let fixture = tempdir().expect("journal fixture");
        let store = JournalStore::create(fixture.path()).expect("create store");
        let mut record = parts.record(&operation_id(tc));
        let reached = if tc.draw(gs::weighted_booleans(0.8)) {
            7
        } else {
            tc.draw(gs::integers::<usize>().max_value(7))
        };
        for phase in &ADD_PHASES[1..=reached] {
            record.transition(*phase).expect("forward transition");
        }
        let path = store.persist(&record).expect("persist journal");
        let journal = record.decode(path).expect("decode journal");
        Self {
            _fixture: fixture,
            store,
            journal,
        }
    }

    fn is_active(&self) -> bool {
        self.journal.phase == AddWorktreePhase::Active
    }

    fn reloaded(&self) -> DecodedJournal {
        self.store
            .load_operation(&self.journal.operation_id)
            .expect("reload journal")
    }
}

/// Either the path the journal already records or an unrelated one, so both
/// the matching and the mismatching branch of each guard are common.
fn same_or_other(tc: &TestCase, recorded: &Path) -> PathBuf {
    if tc.draw(gs::booleans()) {
        recorded.to_path_buf()
    } else {
        native_path(tc)
    }
}

/// The recorded path most of the time, for a guard that every later check
/// sits behind.
fn usually_same(tc: &TestCase, recorded: &Path) -> PathBuf {
    if tc.draw(gs::weighted_booleans(0.8)) {
        recorded.to_path_buf()
    } else {
        native_path(tc)
    }
}

/// A path with no parent or no final component, which a bucket and a staging
/// name cannot be taken from.
fn rootless_path(tc: &TestCase) -> PathBuf {
    PathBuf::from(tc.draw(gs::sampled_from(vec!["", "/", "..", "a/.."])))
}

/// A move may only rewrite the destination of an active journal that still
/// records the expected source, and repeating a completed rewrite is a no-op.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn relocating_an_add_journal_requires_the_expected_source(tc: TestCase) {
    let add = ActiveAdd::draw(&tc);
    let before = add.journal.clone();
    let expected_source = same_or_other(&tc, &before.destination);
    let destination = same_or_other(&tc, &before.destination);

    let outcome =
        add.store
            .update_active_destination(&before.journal_path, &expected_source, &destination);
    let allowed = add.is_active()
        && (before.destination == destination || before.destination == expected_source);
    assert_eq!(outcome.is_ok(), allowed, "{outcome:?}");
    let expected = if allowed {
        DecodedJournal {
            destination: destination.clone(),
            ..before.clone()
        }
    } else {
        before.clone()
    };
    assert_eq!(add.reloaded(), expected);
    if allowed {
        add.store
            .update_active_destination(&before.journal_path, &expected_source, &destination)
            .expect("repeating a finished relocation");
        assert_eq!(add.reloaded(), expected);
    }
}

/// Repair may only re-home the expected active journal, from the repository
/// location it expects.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn re_homing_an_add_journal_requires_the_expected_repository(tc: TestCase) {
    let add = ActiveAdd::draw(&tc);
    let before = add.journal.clone();
    let expected_destination = usually_same(&tc, &before.destination);
    let expected_repository = same_or_other(&tc, &before.repository);
    let repository = same_or_other(&tc, &before.repository);

    let outcome = add.store.update_active_repository(
        &before.journal_path,
        &expected_destination,
        &expected_repository,
        &repository,
    );
    let allowed = add.is_active()
        && before.destination == expected_destination
        && (before.repository == repository || before.repository == expected_repository);
    assert_eq!(outcome.is_ok(), allowed, "{outcome:?}");
    let expected = if allowed {
        DecodedJournal {
            repository,
            ..before.clone()
        }
    } else {
        before
    };
    assert_eq!(add.reloaded(), expected);
}

/// Compaction retargets an active journal at a new base. Afterwards the
/// staging record sits in the new base's bucket under its old name, because
/// recovery validation requires base and staging to share a parent.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn retargeting_an_add_journal_moves_base_and_staging_together(tc: TestCase) {
    let mut parts = AddParts::draw(&tc);
    if tc.draw(gs::weighted_booleans(0.2)) {
        parts.base_staging = rootless_path(&tc);
    }
    let add = ActiveAdd::with(&tc, &parts);
    let before = add.journal.clone();
    let expected_destination = usually_same(&tc, &before.destination);
    let expected_old_base = same_or_other(&tc, &before.base_path);
    let base_path = if tc.draw(gs::weighted_booleans(0.2)) {
        rootless_path(&tc)
    } else {
        same_or_other(&tc, &before.base_path)
    };
    let commit = if tc.draw(gs::booleans()) {
        before.expected_commit.clone()
    } else {
        tc.draw(gs::text().max_size(40))
    };

    let outcome = add.store.update_active_base(
        &before.journal_path,
        &expected_destination,
        &expected_old_base,
        &base_path,
        &commit,
    );
    let after = add.reloaded();
    let Ok(()) = outcome else {
        assert_eq!(after, before, "a refused retarget changed the journal");
        let retargetable = add.is_active()
            && before.destination == expected_destination
            && base_path.parent().is_some()
            && before.base_staging.file_name().is_some()
            && (before.base_path == expected_old_base
                || (before.base_path == base_path && before.expected_commit == commit));
        assert!(!retargetable, "refused: {outcome:?}");
        return;
    };
    assert!(add.is_active() && before.destination == expected_destination);
    assert_eq!(after.base_path, base_path);
    assert_eq!(after.expected_commit, commit);
    assert_eq!(after.base_staging.parent(), base_path.parent());
    assert_eq!(
        after.base_staging.file_name(),
        before.base_staging.file_name()
    );
    assert_eq!(
        after,
        DecodedJournal {
            base_path: after.base_path.clone(),
            base_staging: after.base_staging.clone(),
            expected_commit: after.expected_commit.clone(),
            ..before.clone()
        },
        "retargeting changed an unrelated field"
    );
    add.store
        .update_active_base(
            &before.journal_path,
            &expected_destination,
            &expected_old_base,
            &base_path,
            &commit,
        )
        .expect("repeating a finished retarget");
    assert_eq!(add.reloaded(), after);
}

/// A journal damaged after recovery validated it is never retargeted or
/// advanced: every in-place update re-reads it and fails on the damage.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn in_place_updates_refuse_a_journal_damaged_since_its_snapshot(tc: TestCase) {
    let add = ActiveAdd::draw(&tc);
    let journal = &add.journal;
    let bytes = fs::read(&journal.journal_path).expect("read journal");
    let torn = &bytes[..tc.draw(gs::integers::<usize>().max_value(bytes.len() - 2))];
    fs::write(&journal.journal_path, torn).expect("tear journal");
    let other = native_path(&tc);

    let outcomes = [
        add.store
            .update_phase(journal, AddWorktreePhase::RollbackPending)
            .map(drop),
        add.store
            .update_active_destination(&journal.journal_path, &journal.destination, &other),
        add.store.update_active_repository(
            &journal.journal_path,
            &journal.destination,
            &journal.repository,
            &other,
        ),
        add.store.update_active_base(
            &journal.journal_path,
            &journal.destination,
            &journal.base_path,
            &other,
            "commit",
        ),
    ];
    for outcome in outcomes {
        assert!(matches!(outcome, Err(JournalError::Deserialize { .. })));
    }
    assert_eq!(fs::read(&journal.journal_path).expect("read journal"), torn);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Obstacle {
    RegularFile,
    SymlinkToDirectory,
    DanglingSymlink,
    StateIsAFile,
    StateIsMissing,
}

/// A store only ever uses a real directory. Whatever sits at its path
/// instead, creating the store fails, nothing is read through it, and a
/// journal directory reached through a symlink is never written to.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn stores_refuse_a_state_path_that_is_not_a_real_directory(tc: TestCase) {
    let kind = Kind::draw(&tc);
    let obstacle = tc.draw(gs::sampled_from(vec![
        Obstacle::RegularFile,
        Obstacle::SymlinkToDirectory,
        Obstacle::DanglingSymlink,
        Obstacle::StateIsAFile,
        Obstacle::StateIsMissing,
    ]));
    let fixture = tempdir().expect("journal fixture");
    // A real store elsewhere, holding one journal a symlink could expose.
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).expect("outside state");
    kind.persist_sample(&tc, &outside, &operation_id(&tc))
        .expect("persist outside journal");
    let outside_journals = outside.join(kind.directory());
    let outside_before = entry_names(&outside_journals);

    let state = fixture.path().join("state");
    let directory = state.join(kind.directory());
    match obstacle {
        Obstacle::RegularFile => {
            fs::create_dir(&state).expect("state directory");
            fs::write(&directory, tc.draw(gs::binary().max_size(16))).expect("obstacle file");
        }
        Obstacle::SymlinkToDirectory => {
            fs::create_dir(&state).expect("state directory");
            symlink(&outside_journals, &directory).expect("obstacle symlink");
        }
        Obstacle::DanglingSymlink => {
            fs::create_dir(&state).expect("state directory");
            symlink(fixture.path().join("absent"), &directory).expect("dangling symlink");
        }
        Obstacle::StateIsAFile => fs::write(&state, b"not a directory").expect("state file"),
        Obstacle::StateIsMissing => {}
    }

    let is_the_obstacle = matches!(
        obstacle,
        Obstacle::RegularFile | Obstacle::SymlinkToDirectory | Obstacle::DanglingSymlink
    );
    let refused_as_expected = |error: &JournalError| {
        assert_eq!(
            matches!(error, JournalError::InvalidStateDirectory { .. }),
            is_the_obstacle,
            "{obstacle:?}: {error}"
        );
        assert_eq!(
            crate::policy_refusal_code(error),
            is_the_obstacle.then_some("invalid-state-directory")
        );
    };
    refused_as_expected(
        &kind
            .create_store(&state)
            .expect_err("a store was created over the obstacle"),
    );
    let listed = kind.load_all(&state);
    if let Err(error) = &listed {
        refused_as_expected(error);
    }
    // Only a state directory that does not exist yet is simply empty.
    match listed {
        Ok(journals) => {
            assert_eq!(obstacle, Obstacle::StateIsMissing);
            assert!(journals.is_empty());
        }
        Err(_) => assert_ne!(obstacle, Obstacle::StateIsMissing),
    }
    if kind == Kind::Add {
        let loaded = JournalStore::open(&state).load_operation("operation");
        match obstacle {
            Obstacle::StateIsMissing => assert!(matches!(loaded, Err(JournalError::Io { .. }))),
            _ => assert!(matches!(
                loaded,
                Err(JournalError::InvalidStateDirectory { .. })
            )),
        }
    }
    assert_eq!(entry_names(&outside_journals), outside_before);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swap {
    Directory,
    Symlink,
    NewVersion,
    Removed,
}

/// Whatever takes a journal's path between opening it and rechecking the
/// path, the bytes already read are discarded. Only a replacement by another
/// real file, or a removal, is reported as a write in flight; a symlink or a
/// directory at the path is an invalid record.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn a_journal_swapped_while_it_is_read_is_not_trusted(tc: TestCase) {
    let add = ActiveAdd::draw(&tc);
    let swap = tc.draw(gs::sampled_from(vec![
        Swap::Directory,
        Swap::Symlink,
        Swap::NewVersion,
        Swap::Removed,
    ]));
    let copy = add.journal.journal_path.with_extension("copy");
    fs::copy(&add.journal.journal_path, &copy).expect("copy journal");

    let guard = test_hooks::install(FilesystemRacePoint::JournalOpened, move |path| match swap {
        Swap::Directory => {
            fs::remove_file(path).expect("remove journal");
            fs::create_dir(path).expect("directory at the journal path");
        }
        Swap::Symlink => {
            fs::remove_file(path).expect("remove journal");
            symlink(&copy, path).expect("symlink at the journal path");
        }
        Swap::NewVersion => fs::rename(&copy, path).expect("replace journal"),
        Swap::Removed => fs::remove_file(path).expect("remove journal"),
    });
    let outcome = add.store.load_operation(&add.journal.operation_id);
    drop(guard);

    let error = outcome.expect_err("a swapped journal was trusted");
    assert_eq!(
        journal_error_is_in_flight(&error),
        matches!(swap, Swap::NewVersion | Swap::Removed),
        "{swap:?}: {error}"
    );
    if matches!(swap, Swap::Directory | Swap::Symlink) {
        assert!(matches!(error, JournalError::InvalidRecord { .. }));
    }
}
