//! High-level, non-destructive orchestration and product policy.

use std::path::{Path, PathBuf};

use riftri_git::{Git, GitInfo, ObjectId, RepositoryIdentity, RepositoryInfo};
use riftri_storage::{BackendCapability, VolumeIdentity, probe_backends};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod activation;
mod journal;
mod worktree;

pub use activation::{
    ActivationError, BYPASS_ENV, CACHE_DIR_ENV, GitProxyOutcome, GitProxyPlan,
    RepositoryActivation, SHIM_ACTIVE_ENV, disable_repository, enable_repository,
    execute_scoped_command, execute_scoped_command_in_worktree, plan_git_command,
    prepare_posix_shell_hook, proxy_git_command, repository_activation,
};
pub use riftri_git::REAL_GIT_ENV;
pub use worktree::{
    AddWorktreeRequest, AddWorktreeResult, BaseStorageAccounting, GarbageCollectionCandidate,
    GarbageCollectionReport, RecoveryReport, RemoveWorktreeRequest, RemoveWorktreeResult,
    StateDiagnosticIssue, StorageAccountingReport, ViewStorageAccounting, WorktreeError,
    WorktreeMode, add_worktree, garbage_collect, is_managed_worktree,
    recover_incomplete_operations, remove_worktree, storage_accounting,
};

/// A diagnostic check and its optional failure explanation.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic<T> {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> Diagnostic<T> {
    fn success(value: T) -> Self {
        Self {
            available: true,
            value: Some(value),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            available: false,
            value: None,
            error: Some(error.into()),
        }
    }
}

/// Read-only snapshot produced by `riftri doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub project_stage: &'static str,
    pub operating_system: &'static str,
    pub architecture: &'static str,
    pub cow_backend_active: bool,
    pub repository_enabled: Option<bool>,
    pub git_shim_active: bool,
    pub git: Diagnostic<GitInfo>,
    pub repository: Diagnostic<RepositoryInfo>,
    pub storage_capabilities: Vec<BackendCapability>,
}

/// One canonical input to checkout-byte semantics.
///
/// Inputs are byte strings rather than Rust `String`s because Git config,
/// attributes paths, sparse patterns, and filter definitions are not
/// universally UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckoutProfileInput {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// Versioned, canonical model of everything outside the Git tree that can
/// affect checked-out bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckoutProfile {
    pub format_version: u16,
    pub inputs: Vec<CheckoutProfileInput>,
}

impl CheckoutProfile {
    pub const CURRENT_FORMAT_VERSION: u16 = 1;

    /// Build a deterministic profile independent of discovery order.
    pub fn new(mut inputs: Vec<CheckoutProfileInput>) -> Self {
        inputs.sort_unstable_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then_with(|| left.value.cmp(&right.value))
        });
        inputs.dedup();
        Self {
            format_version: Self::CURRENT_FORMAT_VERSION,
            inputs,
        }
    }
}

/// Complete immutable-base cache identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BaseKey {
    pub repository: RepositoryIdentity,
    pub tree: ObjectId,
    pub checkout_profile: CheckoutProfile,
    pub volume: VolumeIdentity,
}

/// Durable phases of an explicit worktree-add transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum AddWorktreePhase {
    IntentRecorded,
    GitMetadataCreated,
    BaseReady,
    ViewCreated,
    GitPointerRestored,
    IndexSynchronized,
    CleanVerified,
    Active,
    RollbackPending,
    RolledBack,
}

/// Durable phases of an explicit worktree-removal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum RemoveWorktreePhase {
    IntentRecorded,
    CleanVerified,
    WorktreeRemoved,
    BaseReleased,
    Complete,
}

/// Durable phases of an immutable-base garbage-collection transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum GarbageCollectionPhase {
    IntentRecorded,
    MarkerRemoved,
    BaseQuarantined,
    Complete,
    Cancelled,
}

impl RemoveWorktreePhase {
    /// Return whether a removal journal may atomically advance to `next`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use RemoveWorktreePhase::{
            BaseReleased, CleanVerified, Complete, IntentRecorded, WorktreeRemoved,
        };

        matches!(
            (self, next),
            (IntentRecorded, CleanVerified)
                | (CleanVerified, WorktreeRemoved)
                | (WorktreeRemoved, BaseReleased)
                | (BaseReleased, Complete)
        )
    }
}

impl AddWorktreePhase {
    /// Return whether a journal may atomically advance to `next`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use AddWorktreePhase::{
            Active, BaseReady, CleanVerified, GitMetadataCreated, GitPointerRestored,
            IndexSynchronized, IntentRecorded, RollbackPending, RolledBack, ViewCreated,
        };

        matches!(
            (self, next),
            (IntentRecorded, GitMetadataCreated)
                | (GitMetadataCreated, BaseReady)
                | (BaseReady, ViewCreated)
                | (ViewCreated, GitPointerRestored)
                | (GitPointerRestored, IndexSynchronized)
                | (IndexSynchronized, CleanVerified)
                | (CleanVerified, Active)
                | (RollbackPending, RolledBack)
        ) || (!matches!(self, Active | RolledBack | RollbackPending) && next == RollbackPending)
    }
}

/// In-memory representation required of a future on-disk operation journal.
///
/// Persistence is intentionally not implemented before Milestone 2 has a real
/// mutation to recover. `PathBuf` keeps platform-native path representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddWorktreeJournal {
    pub format_version: u16,
    pub operation_id: String,
    pub destination: PathBuf,
    pub phase: AddWorktreePhase,
}

impl AddWorktreeJournal {
    pub const CURRENT_FORMAT_VERSION: u16 = 1;

    pub fn new(operation_id: impl Into<String>, destination: PathBuf) -> Self {
        Self {
            format_version: Self::CURRENT_FORMAT_VERSION,
            operation_id: operation_id.into(),
            destination,
            phase: AddWorktreePhase::IntentRecorded,
        }
    }

    pub fn transition(&mut self, next: AddWorktreePhase) -> Result<(), JournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(JournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid add-worktree journal transition from {current:?} to {requested:?}")]
pub struct JournalTransitionError {
    pub current: AddWorktreePhase,
    pub requested: AddWorktreePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid remove-worktree journal transition from {current:?} to {requested:?}")]
pub struct RemoveJournalTransitionError {
    pub current: RemoveWorktreePhase,
    pub requested: RemoveWorktreePhase,
}

/// Inspect a repository and the volume containing `path` without changing
/// Git or filesystem state.
pub fn doctor(path: &Path) -> DoctorReport {
    doctor_for_destination(path, path)
}

/// Inspect a repository and a possibly different proposed destination.
pub fn doctor_for_destination(repository_path: &Path, destination: &Path) -> DoctorReport {
    let git = Git::default();
    let git_check = match git.detect() {
        Ok(info) => Diagnostic::success(info),
        Err(error) => Diagnostic::failure(error.to_string()),
    };
    let repository_check = match git.inspect_repository(repository_path) {
        Ok(info) => Diagnostic::success(info),
        Err(error) => Diagnostic::failure(error.to_string()),
    };
    let repository_enabled = repository_check
        .value
        .as_ref()
        .and_then(|repository| repository.root.as_deref())
        .map(|root| {
            git.local_config_bool(root, activation::ENABLED_CONFIG_KEY)
                .ok()
                .flatten()
                .unwrap_or(false)
        });

    DoctorReport {
        project_stage: "apfs-prototype-with-repository-activation",
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        cow_backend_active: false,
        repository_enabled,
        git_shim_active: std::env::var_os(SHIM_ACTIVE_ENV).is_some(),
        git: git_check,
        repository: repository_check,
        storage_capabilities: probe_backends(destination),
    }
}

/// Probe storage backends for a concrete destination path.
pub fn backends(destination: &Path) -> Vec<BackendCapability> {
    probe_backends(destination)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use riftri_git::{ObjectId, RepositoryIdentity};
    use riftri_storage::VolumeIdentity;

    use super::{
        AddWorktreeJournal, AddWorktreePhase, BaseKey, CheckoutProfile, CheckoutProfileInput,
        RemoveWorktreePhase,
    };

    #[test]
    fn checkout_profiles_are_canonical() {
        let first = CheckoutProfile::new(vec![
            CheckoutProfileInput {
                key: b"core.autocrlf".to_vec(),
                value: b"false".to_vec(),
            },
            CheckoutProfileInput {
                key: b"filter.lfs.smudge".to_vec(),
                value: b"git-lfs smudge -- %f".to_vec(),
            },
        ]);
        let second = CheckoutProfile::new(first.inputs.iter().cloned().rev().collect());

        assert_eq!(first, second);
    }

    #[test]
    fn base_keys_are_volume_specific() {
        let template = BaseKey {
            repository: RepositoryIdentity {
                common_git_dir: PathBuf::from("/repo/.git"),
            },
            tree: ObjectId::parse("0123456789abcdef0123456789abcdef01234567").expect("object ID"),
            checkout_profile: CheckoutProfile::new(Vec::new()),
            volume: VolumeIdentity {
                device_id: 1,
                filesystem: "apfs".to_owned(),
            },
        };
        let mut other_volume = template.clone();
        other_volume.volume.device_id = 2;

        assert_ne!(template, other_volume);
    }

    #[test]
    fn journal_accepts_the_happy_path() {
        let mut journal = AddWorktreeJournal::new("operation-1", PathBuf::from("/worktree"));
        for phase in [
            AddWorktreePhase::GitMetadataCreated,
            AddWorktreePhase::BaseReady,
            AddWorktreePhase::ViewCreated,
            AddWorktreePhase::GitPointerRestored,
            AddWorktreePhase::IndexSynchronized,
            AddWorktreePhase::CleanVerified,
            AddWorktreePhase::Active,
        ] {
            journal.transition(phase).expect("valid transition");
        }

        assert_eq!(journal.phase, AddWorktreePhase::Active);
    }

    #[test]
    fn journal_can_roll_back_every_incomplete_phase() {
        for phase in [
            AddWorktreePhase::IntentRecorded,
            AddWorktreePhase::GitMetadataCreated,
            AddWorktreePhase::BaseReady,
            AddWorktreePhase::ViewCreated,
            AddWorktreePhase::GitPointerRestored,
            AddWorktreePhase::IndexSynchronized,
            AddWorktreePhase::CleanVerified,
        ] {
            assert!(phase.can_transition_to(AddWorktreePhase::RollbackPending));
        }
        assert!(!AddWorktreePhase::Active.can_transition_to(AddWorktreePhase::RollbackPending));
    }

    #[test]
    fn removal_journal_accepts_only_the_forward_transaction() {
        assert!(
            RemoveWorktreePhase::IntentRecorded
                .can_transition_to(RemoveWorktreePhase::CleanVerified)
        );
        assert!(
            RemoveWorktreePhase::CleanVerified
                .can_transition_to(RemoveWorktreePhase::WorktreeRemoved)
        );
        assert!(
            RemoveWorktreePhase::WorktreeRemoved
                .can_transition_to(RemoveWorktreePhase::BaseReleased)
        );
        assert!(RemoveWorktreePhase::BaseReleased.can_transition_to(RemoveWorktreePhase::Complete));
        assert!(
            !RemoveWorktreePhase::Complete.can_transition_to(RemoveWorktreePhase::IntentRecorded)
        );
    }
}
