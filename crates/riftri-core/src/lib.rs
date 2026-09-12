//! High-level, non-destructive orchestration and product policy.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use riftri_git::{Git, GitInfo, ObjectId, RepositoryIdentity, RepositoryInfo};
use riftri_storage::{BackendCapability, BackendKind, VolumeIdentity, probe_backends};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod activation;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod base_integrity;
mod journal;
#[cfg(all(
    test,
    any(
        target_os = "macos",
        all(
            feature = "native-cow-integration",
            any(target_os = "linux", target_os = "windows")
        )
    )
))]
#[path = "../tests/support/mod.rs"]
mod test_support;
mod worktree;

pub use activation::{
    ActivationError, BYPASS_ENV, CACHE_DIR_ENV, GitProxyOutcome, GitProxyPlan,
    RepositoryActivation, SHIM_ACTIVE_ENV, ShellActivationStatus, disable_repository,
    enable_repository, execute_scoped_command, execute_scoped_command_in_worktree,
    install_overlayfs_helper, plan_git_command, prepare_posix_shell_deactivation,
    prepare_posix_shell_hook, proxy_git_command, repository_activation, shell_activation_status,
};
pub use riftri_git::REAL_GIT_ENV;
pub use worktree::{
    AddWorktreeRequest, AddWorktreeResult, BaseStorageAccounting, GarbageCollectionCandidate,
    GarbageCollectionReport, MoveWorktreeRequest, MoveWorktreeResult, PruneWorktreesRequest,
    PruneWorktreesResult, RecoveryReport, RemoveWorktreeRequest, RemoveWorktreeResult,
    StateDiagnosticIssue, StorageAccountingReport, ViewStorageAccounting, WorktreeError,
    WorktreeMode, add_worktree, forget_missing_state_directory, garbage_collect,
    is_managed_worktree, move_worktree, prune_worktrees, recover_incomplete_operations,
    remove_worktree, storage_accounting,
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
    pub repository_compatibility: Diagnostic<RepositoryCompatibilityReport>,
    pub destination_readiness: DestinationReadiness,
    pub storage_capabilities: Vec<BackendCapability>,
}

/// Overall result of checking whether an optimized add can target one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DestinationReadinessStatus {
    Ready,
    NeedsActivation,
    Blocked,
}

/// Whether Linux OverlayFS needs and can use Riftri's narrow mount helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayFsHelperReadiness {
    NotApplicable,
    NotRequired,
    Ready,
    Unavailable,
}

/// One actionable reason a destination is not ready for transparent adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DestinationReadinessBlocker {
    pub kind: &'static str,
    pub explanation: String,
    pub remedy: String,
}

/// Destination-specific preflight for the preferred `riftri exec` workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DestinationReadiness {
    pub destination: PathBuf,
    pub status: DestinationReadinessStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<riftri_storage::BackendKind>,
    pub copy_on_write: bool,
    pub overlayfs_helper: OverlayFsHelperReadiness,
    pub blockers: Vec<DestinationReadinessBlocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
}

/// One reason an exact Git tree cannot use the current optimized checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryCompatibilityBlockerKind {
    InTreeAttributes,
    EffectiveAttributes,
    Submodules,
    SparseCheckout,
    CheckoutConfiguration,
}

impl RepositoryCompatibilityBlockerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InTreeAttributes => "in-tree-attributes",
            Self::EffectiveAttributes => "effective-attributes",
            Self::Submodules => "submodules",
            Self::SparseCheckout => "sparse-checkout",
            Self::CheckoutConfiguration => "checkout-configuration",
        }
    }
}

/// A specific checkout input that the current compatibility envelope rejects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryCompatibilityBlocker {
    pub kind: RepositoryCompatibilityBlockerKind,
    pub explanation: String,
}

/// Read-only compatibility result for one exact revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryCompatibilityReport {
    pub commit: ObjectId,
    pub tree: ObjectId,
    pub compatible: bool,
    pub blockers: Vec<RepositoryCompatibilityBlocker>,
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

/// Durable phases of a managed worktree-move transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum MoveWorktreePhase {
    IntentRecorded,
    WorktreeMoved,
    AddJournalUpdated,
    Complete,
}

/// Durable phases of a Git worktree-prune transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum PruneWorktreesPhase {
    IntentRecorded,
    GitMetadataPruned,
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

impl MoveWorktreePhase {
    /// Return whether a move journal may atomically advance to `next`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use MoveWorktreePhase::{AddJournalUpdated, Complete, IntentRecorded, WorktreeMoved};

        matches!(
            (self, next),
            (IntentRecorded, WorktreeMoved)
                | (WorktreeMoved, AddJournalUpdated)
                | (AddJournalUpdated, Complete)
        )
    }
}

impl PruneWorktreesPhase {
    /// Return whether a prune journal may atomically advance to `next`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use PruneWorktreesPhase::{Complete, GitMetadataPruned, IntentRecorded};

        matches!(
            (self, next),
            (IntentRecorded, GitMetadataPruned) | (GitMetadataPruned, Complete)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid move-worktree journal transition from {current:?} to {requested:?}")]
pub struct MoveJournalTransitionError {
    pub current: MoveWorktreePhase,
    pub requested: MoveWorktreePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid prune-worktrees journal transition from {current:?} to {requested:?}")]
pub struct PruneJournalTransitionError {
    pub current: PruneWorktreesPhase,
    pub requested: PruneWorktreesPhase,
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
    let repository_compatibility = match repository_check.value.as_ref() {
        Some(repository) if repository.is_bare => {
            Diagnostic::failure("bare repositories are not supported by optimized checkout")
        }
        Some(repository) => match repository.root.as_deref() {
            Some(root) if repository.head_commit.is_some() => {
                match worktree::inspect_repository_compatibility(&git, root, OsStr::new("HEAD")) {
                    Ok(report) => Diagnostic::success(report),
                    Err(error) => Diagnostic::failure(error.to_string()),
                }
            }
            Some(_) => Diagnostic::failure("repository HEAD is unborn; commit a tree first"),
            None => Diagnostic::failure("Git did not report a working-tree root"),
        },
        None => Diagnostic::failure("repository inspection did not succeed"),
    };
    let destination_readiness = destination_readiness(
        destination,
        repository_enabled,
        &git_check,
        &repository_check,
        &repository_compatibility,
    );
    let cow_backend_active = destination_readiness.copy_on_write;
    let storage_capabilities = probe_backends(destination);

    DoctorReport {
        project_stage: "native-cow-with-repository-activation",
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        cow_backend_active,
        repository_enabled,
        git_shim_active: std::env::var_os(SHIM_ACTIVE_ENV).is_some(),
        git: git_check,
        repository: repository_check,
        repository_compatibility,
        destination_readiness,
        storage_capabilities,
    }
}

fn destination_readiness(
    destination: &Path,
    repository_enabled: Option<bool>,
    git: &Diagnostic<GitInfo>,
    repository: &Diagnostic<RepositoryInfo>,
    compatibility: &Diagnostic<RepositoryCompatibilityReport>,
) -> DestinationReadiness {
    let backend = worktree::destination_backend_readiness(destination);
    let selected_backend = backend.as_ref().ok().map(|selection| selection.kind);
    let mut blockers = Vec::new();

    if let Some(error) = &git.error {
        blockers.push(DestinationReadinessBlocker {
            kind: "git",
            explanation: error.clone(),
            remedy: "Install Git and ensure the real `git` executable is available on PATH."
                .to_owned(),
        });
    }

    if let Some(error) = &repository.error {
        blockers.push(DestinationReadinessBlocker {
            kind: "repository",
            explanation: error.clone(),
            remedy: "Run `riftri doctor` from an existing non-bare Git worktree.".to_owned(),
        });
    }

    match repository_enabled {
        Some(false) => blockers.push(DestinationReadinessBlocker {
            kind: "repository-activation",
            explanation: "transparent Git interception is disabled for this repository".to_owned(),
            remedy: "Run `riftri enable` from this repository; activation remains opt-in one repository at a time."
                .to_owned(),
        }),
        None if repository.error.is_none() => blockers.push(DestinationReadinessBlocker {
            kind: "repository-activation",
            explanation: "repository-local activation could not be determined".to_owned(),
            remedy: "Run `riftri enable` from an existing non-bare Git worktree.".to_owned(),
        }),
        Some(true) | None => {}
    }

    match &compatibility.value {
        Some(report) => {
            blockers.extend(report.blockers.iter().map(|blocker| DestinationReadinessBlocker {
                kind: blocker.kind.as_str(),
                explanation: blocker.explanation.clone(),
                remedy: compatibility_remedy(blocker.kind).to_owned(),
            }));
        }
        None => blockers.push(DestinationReadinessBlocker {
            kind: "checkout-compatibility",
            explanation: compatibility
                .error
                .clone()
                .unwrap_or_else(|| "checkout compatibility could not be established".to_owned()),
            remedy: "Resolve the repository diagnostic, then rerun `riftri doctor --destination <path>`."
                .to_owned(),
        }),
    }

    if let Err(error) = &backend {
        blockers.push(DestinationReadinessBlocker {
            kind: "storage-backend",
            explanation: error.to_string(),
            remedy: storage_remedy(&error.to_string()).to_owned(),
        });
    }

    let only_activation_blocks = !blockers.is_empty()
        && blockers
            .iter()
            .all(|blocker| blocker.kind == "repository-activation");
    let status = if blockers.is_empty() {
        DestinationReadinessStatus::Ready
    } else if only_activation_blocks {
        DestinationReadinessStatus::NeedsActivation
    } else {
        DestinationReadinessStatus::Blocked
    };
    let overlayfs_helper = match backend.as_ref() {
        Ok(selection) if selection.kind == BackendKind::OverlayFs => {
            if selection.overlayfs_helper_required {
                OverlayFsHelperReadiness::Ready
            } else {
                OverlayFsHelperReadiness::NotRequired
            }
        }
        Err(error) if error.to_string().contains("helper") => OverlayFsHelperReadiness::Unavailable,
        Ok(_) | Err(_) => OverlayFsHelperReadiness::NotApplicable,
    };
    let next_command = match status {
        DestinationReadinessStatus::Ready => Some(format!(
            "riftri worktree add {} --detach HEAD",
            destination.display()
        )),
        DestinationReadinessStatus::NeedsActivation => Some("riftri enable".to_owned()),
        DestinationReadinessStatus::Blocked
            if overlayfs_helper == OverlayFsHelperReadiness::Unavailable =>
        {
            Some("sudo riftri overlayfs install-helper".to_owned())
        }
        DestinationReadinessStatus::Blocked => None,
    };

    DestinationReadiness {
        destination: destination.to_path_buf(),
        status,
        backend: selected_backend,
        copy_on_write: selected_backend.is_some(),
        overlayfs_helper,
        blockers,
        next_command,
    }
}

fn compatibility_remedy(kind: RepositoryCompatibilityBlockerKind) -> &'static str {
    match kind {
        RepositoryCompatibilityBlockerKind::InTreeAttributes => {
            "Use only Riftri's documented deterministic `text`, `eol`, and `binary` attributes, or use ordinary Git for this worktree."
        }
        RepositoryCompatibilityBlockerKind::EffectiveAttributes => {
            "Remove the external attributes or custom filters affecting tracked paths, or use ordinary Git for this worktree."
        }
        RepositoryCompatibilityBlockerKind::Submodules => {
            "Use ordinary Git for this worktree until Riftri supports submodules."
        }
        RepositoryCompatibilityBlockerKind::SparseCheckout => {
            "Disable sparse checkout for this repository, or use ordinary Git for this worktree."
        }
        RepositoryCompatibilityBlockerKind::CheckoutConfiguration => {
            "Restore a supported checkout configuration shown by the diagnostic, or use ordinary Git for this worktree."
        }
    }
}

fn storage_remedy(error: &str) -> &'static str {
    #[cfg(target_os = "linux")]
    if error.contains("helper") {
        return "Install and validate the narrow helper with `sudo riftri overlayfs install-helper`, then rerun doctor.";
    }
    #[cfg(target_os = "macos")]
    {
        let _ = error;
        "Choose a writable APFS destination and rerun `riftri doctor --destination <path>`."
    }
    #[cfg(target_os = "linux")]
    {
        "Choose writable Btrfs, reflink-enabled XFS, or a usable OverlayFS destination and rerun doctor."
    }
    #[cfg(target_os = "windows")]
    {
        let _ = error;
        "Choose a writable ReFS destination and rerun `riftri doctor --destination <path>`."
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = error;
        "No optimized storage backend is available on this platform."
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
        MoveWorktreePhase, PruneWorktreesPhase, RemoveWorktreePhase,
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

    #[test]
    fn move_and_prune_journals_accept_only_forward_transactions() {
        assert!(
            MoveWorktreePhase::IntentRecorded.can_transition_to(MoveWorktreePhase::WorktreeMoved)
        );
        assert!(
            MoveWorktreePhase::WorktreeMoved
                .can_transition_to(MoveWorktreePhase::AddJournalUpdated)
        );
        assert!(
            MoveWorktreePhase::AddJournalUpdated.can_transition_to(MoveWorktreePhase::Complete)
        );
        assert!(!MoveWorktreePhase::Complete.can_transition_to(MoveWorktreePhase::IntentRecorded));
        assert!(
            PruneWorktreesPhase::IntentRecorded
                .can_transition_to(PruneWorktreesPhase::GitMetadataPruned)
        );
        assert!(
            PruneWorktreesPhase::GitMetadataPruned.can_transition_to(PruneWorktreesPhase::Complete)
        );
        assert!(
            !PruneWorktreesPhase::Complete.can_transition_to(PruneWorktreesPhase::IntentRecorded)
        );
    }
}
