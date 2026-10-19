//! Property tests for the public policy model: lifecycle phases, checkout
//! profiles, compatibility blocker names, and failure classification.

use std::error::Error;
use std::fmt::Debug;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_core::{
    ActivationError, AddWorktreeJournal, AddWorktreePhase, CheckoutProfile, CheckoutProfileInput,
    CompactWorktreePhase, GitError, MoveWorktreePhase, PruneWorktreesPhase, RemoveWorktreePhase,
    RepositoryCompatibilityBlockerKind, WorktreeError, is_absent_repository, is_storage_full,
    policy_refusal_code,
};

const ADD: [AddWorktreePhase; 10] = [
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
const REMOVE: [RemoveWorktreePhase; 6] = [
    RemoveWorktreePhase::IntentRecorded,
    RemoveWorktreePhase::CleanVerified,
    RemoveWorktreePhase::WorktreeRemoved,
    RemoveWorktreePhase::BaseReleased,
    RemoveWorktreePhase::Complete,
    RemoveWorktreePhase::Cancelled,
];
const MOVE: [MoveWorktreePhase; 5] = [
    MoveWorktreePhase::IntentRecorded,
    MoveWorktreePhase::WorktreeMoved,
    MoveWorktreePhase::AddJournalUpdated,
    MoveWorktreePhase::Complete,
    MoveWorktreePhase::Cancelled,
];
const PRUNE: [PruneWorktreesPhase; 3] = [
    PruneWorktreesPhase::IntentRecorded,
    PruneWorktreesPhase::GitMetadataPruned,
    PruneWorktreesPhase::Complete,
];
const COMPACT: [CompactWorktreePhase; 7] = [
    CompactWorktreePhase::IntentRecorded,
    CompactWorktreePhase::ReplacementReady,
    CompactWorktreePhase::ReplacementActivated,
    CompactWorktreePhase::AddJournalUpdated,
    CompactWorktreePhase::RestoringOriginal,
    CompactWorktreePhase::Complete,
    CompactWorktreePhase::Cancelled,
];

/// Follow a generated sequence of requests through one phase graph and check
/// what every graph promises: it starts at the first phase, never returns to
/// a phase it left (so every operation terminates), and never leaves a
/// finished phase.
fn walk<P: Copy + PartialEq + Debug + Send + Sync + 'static>(
    tc: &TestCase,
    all: &[P],
    finished: &[P],
    allowed: fn(P, P) -> bool,
) {
    let mut visited = vec![all[0]];
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(24)) {
        let current = *visited.last().expect("a walk has a start");
        let next = tc.draw(gs::sampled_from(all.to_vec()));
        if !allowed(current, next) {
            continue;
        }
        assert!(
            !visited.contains(&next),
            "{next:?} was reached again after {visited:?}"
        );
        assert!(
            !finished.contains(&current),
            "{current:?} is finished but can move to {next:?}"
        );
        visited.push(next);
    }
}

#[hegel::test(test_cases = 300)]
fn every_lifecycle_only_moves_forward_and_stops_when_finished(tc: TestCase) {
    walk(
        &tc,
        &ADD,
        &[AddWorktreePhase::Active, AddWorktreePhase::RolledBack],
        AddWorktreePhase::can_transition_to,
    );
    // `is_finished` is what recovery consults, so it must name exactly the
    // phases nothing follows.
    let finished_removals = REMOVE
        .into_iter()
        .filter(|phase| phase.is_finished())
        .collect::<Vec<_>>();
    assert_eq!(
        finished_removals,
        [
            RemoveWorktreePhase::Complete,
            RemoveWorktreePhase::Cancelled
        ]
    );
    walk(
        &tc,
        &REMOVE,
        &finished_removals,
        RemoveWorktreePhase::can_transition_to,
    );
    let finished_moves = MOVE
        .into_iter()
        .filter(|phase| phase.is_finished())
        .collect::<Vec<_>>();
    assert_eq!(
        finished_moves,
        [MoveWorktreePhase::Complete, MoveWorktreePhase::Cancelled]
    );
    walk(
        &tc,
        &MOVE,
        &finished_moves,
        MoveWorktreePhase::can_transition_to,
    );
    walk(
        &tc,
        &PRUNE,
        &[PruneWorktreesPhase::Complete],
        PruneWorktreesPhase::can_transition_to,
    );
    walk(
        &tc,
        &COMPACT,
        &[
            CompactWorktreePhase::Complete,
            CompactWorktreePhase::Cancelled,
        ],
        CompactWorktreePhase::can_transition_to,
    );
}

/// The in-memory add journal accepts exactly the transitions its phase
/// allows; a refused one reports both phases and changes nothing.
#[hegel::test]
fn the_add_journal_refuses_transitions_without_changing(tc: TestCase) {
    let destination = PathBuf::from(tc.draw(gs::text().max_size(8)));
    let operation = tc.draw(gs::text().max_size(8));
    let mut journal = AddWorktreeJournal::new(operation.clone(), destination.clone());
    assert_eq!(journal.phase, AddWorktreePhase::IntentRecorded);
    assert_eq!(
        journal.format_version,
        AddWorktreeJournal::CURRENT_FORMAT_VERSION
    );
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(16)) {
        let before = journal.clone();
        // Usually the next phase in order, so walks reach an active journal.
        let position = ADD.iter().position(|phase| *phase == journal.phase);
        let next = if tc.draw(gs::booleans()) {
            ADD[(position.expect("known phase") + 1) % ADD.len()]
        } else {
            tc.draw(gs::sampled_from(ADD.to_vec()))
        };
        match journal.transition(next) {
            Ok(()) => {
                assert!(before.phase.can_transition_to(next));
                assert_eq!(
                    journal,
                    AddWorktreeJournal {
                        phase: next,
                        ..before
                    }
                );
            }
            Err(error) => {
                assert!(!before.phase.can_transition_to(next));
                assert_eq!((error.current, error.requested), (before.phase, next));
                let message = error.to_string();
                assert!(
                    message.contains(&format!("{:?}", before.phase)),
                    "{message}"
                );
                assert!(message.contains(&format!("{next:?}")), "{message}");
                assert_eq!(journal, before);
            }
        }
    }
    assert_eq!(
        (journal.operation_id, journal.destination),
        (operation, destination)
    );
}

fn profile_input(tc: &TestCase) -> CheckoutProfileInput {
    CheckoutProfileInput {
        key: tc.draw(gs::binary().max_size(3)),
        value: tc.draw(gs::binary().max_size(3)),
    }
}

/// A checkout profile is part of a base's identity, so it must be the same
/// value however its inputs were discovered: in any order, with repeats.
#[hegel::test]
fn checkout_profiles_are_canonical(tc: TestCase) {
    let inputs = tc.draw(gs::integers::<usize>().max_value(6));
    let inputs = (0..inputs).map(|_| profile_input(&tc)).collect::<Vec<_>>();
    let profile = CheckoutProfile::new(inputs.clone());
    assert_eq!(
        profile.format_version,
        CheckoutProfile::CURRENT_FORMAT_VERSION
    );

    let mut reordered = tc.draw(gs::permutations(inputs.clone()));
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(3)) {
        if let Some(last) = inputs.len().checked_sub(1) {
            let repeated = tc.draw(gs::integers::<usize>().max_value(last));
            reordered.push(inputs[repeated].clone());
        }
    }
    assert_eq!(CheckoutProfile::new(reordered), profile);
    assert_eq!(CheckoutProfile::new(profile.inputs.clone()), profile);

    assert!(
        profile
            .inputs
            .windows(2)
            .all(|pair| { (&pair[0].key, &pair[0].value) < (&pair[1].key, &pair[1].value) })
    );
    assert!(inputs.iter().all(|input| profile.inputs.contains(input)));
    assert!(profile.inputs.iter().all(|input| inputs.contains(input)));

    // Any other set of inputs is a different profile.
    let extra = profile_input(&tc);
    tc.assume(!inputs.contains(&extra));
    let mut widened = inputs;
    widened.push(extra);
    assert_ne!(CheckoutProfile::new(widened), profile);
}

/// The machine name of a blocker kind is the name it serializes under, so a
/// receipt's `kind` and a readiness blocker's `kind` never disagree.
#[hegel::test]
fn blocker_kind_names_match_their_serialized_form(tc: TestCase) {
    let kind = tc.draw(gs::sampled_from(vec![
        RepositoryCompatibilityBlockerKind::InTreeAttributes,
        RepositoryCompatibilityBlockerKind::EffectiveAttributes,
        RepositoryCompatibilityBlockerKind::GitLfs,
        RepositoryCompatibilityBlockerKind::Submodules,
        RepositoryCompatibilityBlockerKind::SparseCheckout,
        RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
    ]));
    assert_eq!(
        serde_json::to_value(kind).expect("serialize kind"),
        serde_json::Value::from(kind.as_str())
    );
}

#[derive(Debug, Clone, PartialEq)]
enum Cause {
    RepositoryAbsent,
    GitFailed { stderr: String },
    Io { code: i32 },
}

fn cause(tc: &TestCase) -> Cause {
    match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => Cause::RepositoryAbsent,
        1 => Cause::GitFailed {
            stderr: tc
                .draw(gs::sampled_from(vec![
                    "fatal: write error: No space left on device",
                    "fatal: not a git repository",
                    "",
                ]))
                .to_owned(),
        },
        _ => Cause::Io {
            code: tc.draw(gs::sampled_from(vec![STORAGE_FULL, 1, 2, 13, 17])),
        },
    }
}

/// The operating system's "no space left on device" error code.
#[cfg(unix)]
const STORAGE_FULL: i32 = 28;
#[cfg(not(unix))]
const STORAGE_FULL: i32 = 112;

impl Cause {
    fn is_storage_full(&self) -> bool {
        match self {
            Self::RepositoryAbsent => false,
            Self::GitFailed { stderr } => stderr.contains("No space left on device"),
            Self::Io { code } => *code == STORAGE_FULL,
        }
    }

    fn as_worktree_error(&self) -> WorktreeError {
        match self {
            Self::RepositoryAbsent => WorktreeError::Git(GitError::RepositoryAbsent {
                path: PathBuf::from("/somewhere"),
            }),
            Self::GitFailed { stderr } => WorktreeError::Git(GitError::CommandFailed {
                arguments: "status".to_owned(),
                stderr: stderr.clone(),
                disposition: "exit code 128".to_owned(),
            }),
            Self::Io { code } => WorktreeError::Io {
                operation: "write file",
                path: PathBuf::from("/somewhere"),
                source: std::io::Error::from_raw_os_error(*code),
            },
        }
    }

    /// The same failure as each entry point reports it. The classifiers are
    /// applied to every error in a caller's chain, so the innermost form is
    /// included for wrappers that expose their source.
    fn as_reported(&self, tc: &TestCase) -> Box<dyn Error + 'static> {
        let worktree = self.as_worktree_error();
        match tc.draw(gs::integers::<u8>().max_value(2)) {
            0 => match worktree {
                WorktreeError::Git(git) => Box::new(git),
                WorktreeError::Io { source, .. } => Box::new(source),
                other => Box::new(other),
            },
            1 => Box::new(worktree),
            _ => match worktree {
                WorktreeError::Git(git) => Box::new(ActivationError::Git(git)),
                other => Box::new(ActivationError::Worktree(other)),
            },
        }
    }
}

/// How a failure is classified does not depend on which entry point wrapped
/// it: the same cause is storage exhaustion, or an absent repository, or
/// neither, through `GitError`, `WorktreeError`, and `ActivationError` alike.
#[hegel::test]
fn failures_classify_the_same_through_every_wrapper(tc: TestCase) {
    let cause = cause(&tc);
    let reported = cause.as_reported(&tc);
    assert_eq!(
        is_absent_repository(reported.as_ref()),
        cause == Cause::RepositoryAbsent,
        "{reported:?}"
    );
    assert_eq!(
        is_storage_full(reported.as_ref()),
        cause.is_storage_full(),
        "{reported:?}"
    );
    assert_eq!(policy_refusal_code(reported.as_ref()), None, "{reported:?}");

    // A failed add whose rollback also failed is storage exhaustion when
    // either half was.
    let other = self::cause(&tc);
    let both = WorktreeError::OperationAndRollback {
        operation: Box::new(cause.as_worktree_error()),
        rollback: Box::new(other.as_worktree_error()),
    };
    let expected = cause.is_storage_full() || other.is_storage_full();
    assert_eq!(is_storage_full(&both), expected);
    assert_eq!(is_storage_full(&ActivationError::Worktree(both)), expected);
}

#[hegel::test]
fn only_policy_refusals_have_a_refusal_code(tc: TestCase) {
    let message = tc.draw(gs::text().max_size(12));
    let (error, code) = match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => (ActivationError::BareRepository, Some("bare-repository")),
        4 => (
            ActivationError::UnsupportedGitVersion {
                git: message.clone().into(),
                version: message,
                minimum: "2.36",
            },
            Some("unsupported-git-version"),
        ),
        1 => (
            ActivationError::WorktreeBinding(message),
            Some("invalid-worktree-binding"),
        ),
        2 => (ActivationError::UnsupportedWorktreeCommand(message), None),
        3 => (ActivationError::Process(message), None),
        _ => (ActivationError::Shell(message), None),
    };
    assert_eq!(policy_refusal_code(&error), code, "{error:?}");
    assert!(!is_absent_repository(&error) && !is_storage_full(&error));
}
