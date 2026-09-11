use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::fs::OpenOptions;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use fs2::FileExt;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use riftri_git::WorktreeHead;
use riftri_git::{Git, GitAttribute, GitError, ObjectId};
#[cfg(target_os = "macos")]
use riftri_storage::ApfsCloner as NativeCowCloner;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use riftri_storage::ApfsCloner as NativeCowCloner;
#[cfg(target_os = "linux")]
use riftri_storage::ReflinkCloner as NativeCowCloner;
#[cfg(target_os = "windows")]
use riftri_storage::RefsBlockCloner as NativeCowCloner;
use riftri_storage::{BackendKind, StorageError};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use riftri_storage::{CapabilityStatus, DestinationVolume, probe_backends};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::journal::{
    CollectionJournalPaths, CollectionJournalRecord, JournalPaths, JournalRecord, MoveJournalPaths,
    MoveJournalRecord, PruneJournalRecord, RemovalJournalPaths, ensure_real_state_directory,
    require_real_state_directory,
};
use crate::journal::{
    CollectionJournalStore, DecodedCollectionJournal, DecodedJournal, DecodedMoveJournal,
    DecodedPruneJournal, DecodedRemovalJournal, JournalError, JournalStore, MoveJournalStore,
    PruneJournalStore, RemovalJournalRecord, RemovalJournalStore,
};
use crate::{
    AddWorktreePhase, GarbageCollectionPhase, JournalTransitionError, MoveJournalTransitionError,
    MoveWorktreePhase, PruneJournalTransitionError, PruneWorktreesPhase,
    RemoveJournalTransitionError, RemoveWorktreePhase, RepositoryCompatibilityBlocker,
    RepositoryCompatibilityBlockerKind, RepositoryCompatibilityReport,
};

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
static OPERATION_NONCE: AtomicU64 = AtomicU64::new(0);

const STATE_DIRECTORY_CONFIG_KEY: &str = "riftri.stateDirectory";

struct CompatibilityAnalysis {
    report: RepositoryCompatibilityReport,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    checkout_profile: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMode {
    NewBranch(OsString),
    Detached,
}

#[derive(Debug, Clone)]
pub struct AddWorktreeRequest {
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub revision: OsString,
    pub mode: WorktreeMode,
    /// Defaults to `<common-git-dir>/riftri`. A custom directory must be on the
    /// same filesystem volume as the destination.
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AddWorktreeResult {
    pub destination: PathBuf,
    pub commit: ObjectId,
    pub tree: ObjectId,
    pub base_path: PathBuf,
    pub journal_path: PathBuf,
    pub reused_base: bool,
    pub backend: BackendKind,
}

#[derive(Debug, Clone)]
pub struct RemoveWorktreeRequest {
    pub repository: PathBuf,
    pub destination: PathBuf,
    /// Defaults to `<common-git-dir>/riftri`.
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct RemoveWorktreeResult {
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub journal_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MoveWorktreeRequest {
    pub repository: PathBuf,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Defaults to `<common-git-dir>/riftri`.
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct MoveWorktreeResult {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub journal_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PruneWorktreesRequest {
    pub repository: PathBuf,
    /// Defaults to `<common-git-dir>/riftri`.
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PruneWorktreesResult {
    pub journal_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseStorageAccounting {
    pub path: PathBuf,
    pub reference_count: usize,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStorageAccounting {
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub backend: BackendKind,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDiagnosticIssue {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default)]
struct StatePathDiagnosis {
    issues: Vec<StateDiagnosticIssue>,
    coordination_locks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GarbageCollectionCandidate {
    pub base_path: PathBuf,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GarbageCollectionReport {
    pub applied: bool,
    pub candidates: Vec<GarbageCollectionCandidate>,
    pub collected: Vec<PathBuf>,
    pub skipped_in_use: Vec<PathBuf>,
    pub resumed_collections: usize,
    pub removed_logical_bytes: u64,
    pub removed_allocated_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageAccountingReport {
    pub active_views: usize,
    pub pending_adds: usize,
    pub completed_removals: usize,
    pub pending_removals: usize,
    pub completed_moves: usize,
    pub pending_moves: usize,
    pub completed_prunes: usize,
    pub pending_prunes: usize,
    pub completed_collections: usize,
    pub cancelled_collections: usize,
    pub pending_collections: usize,
    pub coordination_locks: usize,
    pub bases: Vec<BaseStorageAccounting>,
    pub views: Vec<ViewStorageAccounting>,
    pub diagnostic_issues: Vec<StateDiagnosticIssue>,
    pub total_logical_bytes: u64,
    pub total_allocated_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    pub scanned: usize,
    pub recovered: usize,
    pub active: usize,
    pub completed_removals: usize,
    pub recovered_removals: usize,
    pub completed_moves: usize,
    pub recovered_moves: usize,
    pub completed_prunes: usize,
    pub recovered_prunes: usize,
    pub completed_collections: usize,
    pub recovered_collections: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Journal(#[from] JournalError),

    #[error(transparent)]
    JournalTransition(#[from] JournalTransitionError),

    #[error(transparent)]
    RemoveJournalTransition(#[from] RemoveJournalTransitionError),

    #[error(transparent)]
    MoveJournalTransition(#[from] MoveJournalTransitionError),

    #[error(transparent)]
    PruneJournalTransition(#[from] PruneJournalTransitionError),

    #[error("unsupported optimized checkout: {0}")]
    Unsupported(String),

    #[error("invalid worktree request: {0}")]
    InvalidRequest(String),

    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("creation failed: {operation}; rollback also needs attention: {rollback}")]
    OperationAndRollback { operation: String, rollback: String },

    #[error("injected failure after {0:?}")]
    InjectedFailure(AddWorktreePhase),

    #[error("injected removal failure after {0:?}")]
    InjectedRemovalFailure(RemoveWorktreePhase),

    #[error("injected move failure after {0:?}")]
    InjectedMoveFailure(MoveWorktreePhase),

    #[error("injected prune failure after {0:?}")]
    InjectedPruneFailure(PruneWorktreesPhase),

    #[error("injected garbage-collection failure after {0:?}")]
    InjectedCollectionFailure(GarbageCollectionPhase),
}

pub fn add_worktree(request: AddWorktreeRequest) -> Result<AddWorktreeResult, WorktreeError> {
    add_worktree_inner(request, None, true)
}

pub fn remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_inner(request, None)
}

pub fn move_worktree(request: MoveWorktreeRequest) -> Result<MoveWorktreeResult, WorktreeError> {
    move_worktree_inner(request, None)
}

pub fn prune_worktrees(
    request: PruneWorktreesRequest,
) -> Result<PruneWorktreesResult, WorktreeError> {
    prune_worktrees_inner(request, None)
}

/// Plan or apply collection of immutable bases with no journaled references.
pub fn garbage_collect(
    state_directory: &Path,
    apply: bool,
) -> Result<GarbageCollectionReport, WorktreeError> {
    garbage_collect_inner(state_directory, apply, None)
}

/// Return whether `destination` is an active Riftri-managed worktree in any
/// state directory registered by the repository.
pub fn is_managed_worktree(repository: &Path, destination: &Path) -> Result<bool, WorktreeError> {
    Ok(managed_worktree_state_directory(repository, destination)?.is_some())
}

pub(crate) fn managed_worktree_state_directory(
    repository: &Path,
    destination: &Path,
) -> Result<Option<PathBuf>, WorktreeError> {
    let git = Git::default();
    let repository_info = git.inspect_repository(repository)?;
    let destinations = managed_destination_candidates(destination)?;
    let destination_set = destinations.iter().cloned().collect::<HashSet<_>>();
    let mut matches = Vec::new();
    for state_directory in repository_state_directories_with_git(&git, &repository_info)? {
        let mut active_add = false;
        for destination in &destinations {
            if find_managed_add_journal(&state_directory, destination)?.is_some() {
                active_add = true;
                break;
            }
        }
        let pending_move = MoveJournalStore::open(&state_directory)
            .load_all()?
            .into_iter()
            .any(|journal| {
                journal.phase != MoveWorktreePhase::Complete
                    && (destination_set.contains(&journal.source)
                        || destination_set.contains(&journal.destination))
            });
        if active_add || pending_move {
            matches.push(state_directory);
        }
    }
    if matches.len() > 1 {
        return Err(WorktreeError::InvalidRequest(format!(
            "multiple registered Riftri state directories manage {}",
            destination.display()
        )));
    }
    Ok(matches.pop())
}

pub(crate) fn repository_state_directories(
    repository: &Path,
) -> Result<Vec<PathBuf>, WorktreeError> {
    let git = Git::default();
    let repository_info = git.inspect_repository(repository)?;
    repository_state_directories_with_git(&git, &repository_info)
}

fn repository_state_directories_with_git(
    git: &Git,
    repository: &riftri_git::RepositoryInfo,
) -> Result<Vec<PathBuf>, WorktreeError> {
    let repository_root = repository.root.as_deref().ok_or_else(|| {
        WorktreeError::InvalidRequest("bare repositories have no Riftri state locations".to_owned())
    })?;
    let default = repository.identity.common_git_dir.join("riftri");
    let mut directories = Vec::new();
    if default.exists() {
        directories.push(
            fs::canonicalize(&default)
                .map_err(|source| io("resolve default state directory", &default, source))?,
        );
    }
    for configured in git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)? {
        if !configured.is_absolute() {
            return Err(WorktreeError::InvalidRequest(format!(
                "registered Riftri state directory is not absolute: {}",
                configured.display()
            )));
        }
        let canonical = fs::canonicalize(&configured).map_err(|source| {
            io(
                "resolve registered Riftri state directory",
                &configured,
                source,
            )
        })?;
        let metadata = fs::symlink_metadata(&canonical).map_err(|source| {
            io(
                "inspect registered Riftri state directory",
                &canonical,
                source,
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::InvalidRequest(format!(
                "registered Riftri state path is not a real directory: {}",
                canonical.display()
            )));
        }
        directories.push(canonical);
    }
    directories.sort_unstable();
    directories.dedup();
    Ok(directories)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn register_state_directory(
    git: &Git,
    repository: &riftri_git::RepositoryInfo,
    state_directory: &Path,
) -> Result<(), WorktreeError> {
    let repository_root = repository.root.as_deref().ok_or_else(|| {
        WorktreeError::InvalidRequest("bare repositories have no Riftri state locations".to_owned())
    })?;
    let default = repository.identity.common_git_dir.join("riftri");
    if fs::canonicalize(&default).is_ok_and(|path| path == state_directory) {
        return Ok(());
    }
    let lock_path = repository
        .identity
        .common_git_dir
        .join("riftri-state-directory.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| io("open state-directory locator lock", &lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io("lock state-directory locators", &lock_path, source))?;
    let registered = git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)?;
    if registered.into_iter().any(|path| {
        path == state_directory
            || fs::canonicalize(path).is_ok_and(|canonical| canonical == state_directory)
    }) {
        return Ok(());
    }
    git.add_local_config_path(repository_root, STATE_DIRECTORY_CONFIG_KEY, state_directory)?;
    Ok(())
}

fn managed_destination_candidates(destination: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
    let absolute = absolute_path(destination)?;
    let mut candidates = vec![absolute.clone()];

    match fs::canonicalize(&absolute) {
        Ok(canonical) => candidates.push(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io("resolve worktree destination", destination, source)),
    }

    if let (Some(parent), Some(file_name)) = (absolute.parent(), absolute.file_name()) {
        match fs::canonicalize(parent) {
            Ok(parent) => candidates.push(parent.join(file_name)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io("resolve worktree parent", parent, source)),
        }
    }

    candidates.sort_unstable();
    candidates.dedup();
    Ok(candidates)
}

/// Inventory retained immutable bases and active views from durable journals.
pub fn storage_accounting(
    state_directory: &Path,
) -> Result<StorageAccountingReport, WorktreeError> {
    let state_directory = absolute_path(state_directory)?;
    let state_directory = if state_directory.exists() {
        fs::canonicalize(&state_directory)
            .map_err(|source| io("resolve state directory", &state_directory, source))?
    } else {
        state_directory
    };
    let add_journals = JournalStore::open(&state_directory).load_all()?;
    let removal_journals = RemovalJournalStore::open(&state_directory).load_all()?;
    let move_journals = MoveJournalStore::open(&state_directory).load_all()?;
    let prune_journals = PruneJournalStore::open(&state_directory).load_all()?;
    let collection_journals = CollectionJournalStore::open(&state_directory).load_all()?;
    let completed = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.clone())
        .collect::<HashSet<_>>();

    let mut references = BTreeMap::<PathBuf, usize>::new();
    let mut views = Vec::new();
    for journal in add_journals.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active && !completed.contains(&journal.operation_id)
    }) {
        *references.entry(journal.base_path.clone()).or_default() += 1;
        if journal.destination.is_dir() {
            let (logical_bytes, allocated_bytes) = tree_usage(&journal.destination)?;
            views.push(ViewStorageAccounting {
                destination: journal.destination.clone(),
                base_path: journal.base_path.clone(),
                backend: journal.backend,
                logical_bytes,
                allocated_bytes,
            });
        }
    }
    views.sort_unstable_by(|left, right| left.destination.cmp(&right.destination));

    for base_path in retained_base_paths(&state_directory)? {
        references.entry(base_path).or_default();
    }
    let mut bases = Vec::with_capacity(references.len());
    for (path, reference_count) in references {
        let (logical_bytes, allocated_bytes) = if path.is_dir() {
            tree_usage(&path)?
        } else {
            (0, 0)
        };
        bases.push(BaseStorageAccounting {
            path,
            reference_count,
            logical_bytes,
            allocated_bytes,
        });
    }

    let total_logical_bytes = bases
        .iter()
        .map(|base| base.logical_bytes)
        .chain(views.iter().map(|view| view.logical_bytes))
        .fold(0_u64, u64::saturating_add);
    let total_allocated_bytes = bases
        .iter()
        .map(|base| base.allocated_bytes)
        .chain(views.iter().map(|view| view.allocated_bytes))
        .fold(0_u64, u64::saturating_add);
    let state_diagnosis = diagnose_state_paths(
        &state_directory,
        &add_journals,
        &removal_journals,
        &move_journals,
        &prune_journals,
        &collection_journals,
    )?;

    Ok(StorageAccountingReport {
        active_views: views.len(),
        pending_adds: add_journals
            .iter()
            .filter(|journal| {
                !matches!(
                    journal.phase,
                    AddWorktreePhase::Active | AddWorktreePhase::RolledBack
                )
            })
            .count(),
        completed_removals: removal_journals
            .iter()
            .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
            .count(),
        pending_removals: removal_journals
            .iter()
            .filter(|journal| journal.phase != RemoveWorktreePhase::Complete)
            .count(),
        completed_moves: move_journals
            .iter()
            .filter(|journal| journal.phase == MoveWorktreePhase::Complete)
            .count(),
        pending_moves: move_journals
            .iter()
            .filter(|journal| journal.phase != MoveWorktreePhase::Complete)
            .count(),
        completed_prunes: prune_journals
            .iter()
            .filter(|journal| journal.phase == PruneWorktreesPhase::Complete)
            .count(),
        pending_prunes: prune_journals
            .iter()
            .filter(|journal| journal.phase != PruneWorktreesPhase::Complete)
            .count(),
        completed_collections: collection_journals
            .iter()
            .filter(|journal| journal.phase == GarbageCollectionPhase::Complete)
            .count(),
        cancelled_collections: collection_journals
            .iter()
            .filter(|journal| journal.phase == GarbageCollectionPhase::Cancelled)
            .count(),
        pending_collections: collection_journals
            .iter()
            .filter(|journal| {
                !matches!(
                    journal.phase,
                    GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
                )
            })
            .count(),
        coordination_locks: state_diagnosis.coordination_locks,
        bases,
        views,
        diagnostic_issues: state_diagnosis.issues,
        total_logical_bytes,
        total_allocated_bytes,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn garbage_collect_inner(
    _state_directory: &Path,
    _apply: bool,
    _fail_after: Option<GarbageCollectionPhase>,
) -> Result<GarbageCollectionReport, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "immutable-base garbage collection currently requires macOS".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn garbage_collect_inner(
    state_directory: &Path,
    apply: bool,
    fail_after: Option<GarbageCollectionPhase>,
) -> Result<GarbageCollectionReport, WorktreeError> {
    let state_directory = absolute_path(state_directory)?;
    if !state_directory.exists() {
        return Ok(GarbageCollectionReport {
            applied: apply,
            ..GarbageCollectionReport::default()
        });
    }
    let state_directory = fs::canonicalize(&state_directory)
        .map_err(|source| io("resolve state directory", &state_directory, source))?;

    let resumed_collections = if apply {
        recover_collection_journals(&state_directory)?
    } else {
        0
    };
    let candidates = garbage_collection_candidates(&state_directory)?;
    let mut report = GarbageCollectionReport {
        applied: apply,
        candidates: candidates.clone(),
        resumed_collections,
        ..GarbageCollectionReport::default()
    };
    if !apply {
        return Ok(report);
    }

    let store = CollectionJournalStore::create(&state_directory)?;
    for candidate in candidates {
        let operation_id = format!("gc-{}", next_operation_id(current_timestamp()?));
        let parent = candidate.base_path.expect_parent()?.to_path_buf();
        let quarantine_path = parent.join(format!(".riftri-gc-{operation_id}"));
        let marker_path = candidate.base_path.with_extension("complete");
        let mut journal = CollectionJournalRecord::new(
            operation_id,
            CollectionJournalPaths {
                base_path: &candidate.base_path,
                quarantine_path: &quarantine_path,
                marker_path: &marker_path,
            },
        );
        let journal_path = store.persist(&journal)?;
        fail_collection_if_requested(journal.phase, fail_after)?;
        let decoded = journal.clone().decode(journal_path)?;
        if resume_collection(&state_directory, &store, &mut journal, &decoded, fail_after)? {
            report.removed_logical_bytes = report
                .removed_logical_bytes
                .saturating_add(candidate.logical_bytes);
            report.removed_allocated_bytes = report
                .removed_allocated_bytes
                .saturating_add(candidate.allocated_bytes);
            report.collected.push(candidate.base_path);
        } else {
            report.skipped_in_use.push(candidate.base_path);
        }
    }
    Ok(report)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn current_timestamp() -> Result<u128, WorktreeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|error| WorktreeError::InvalidRequest(format!("system clock error: {error}")))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn garbage_collection_candidates(
    state_directory: &Path,
) -> Result<Vec<GarbageCollectionCandidate>, WorktreeError> {
    let protected = protected_base_paths(state_directory)?;
    let pending = CollectionJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
            )
        })
        .map(|journal| journal.base_path)
        .collect::<HashSet<_>>();
    let mut candidates = Vec::new();
    for base_path in retained_base_paths(state_directory)? {
        if protected.contains(&base_path) || pending.contains(&base_path) {
            continue;
        }
        let (logical_bytes, allocated_bytes) = if base_path.is_dir() {
            tree_usage(&base_path)?
        } else {
            (0, 0)
        };
        candidates.push(GarbageCollectionCandidate {
            base_path,
            logical_bytes,
            allocated_bytes,
        });
    }
    Ok(candidates)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn protected_base_paths(state_directory: &Path) -> Result<HashSet<PathBuf>, WorktreeError> {
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed = removals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    Ok(JournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            journal.phase != AddWorktreePhase::RolledBack
                && !completed.contains(journal.operation_id.as_str())
        })
        .map(|journal| journal.base_path)
        .collect())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn recover_collection_journals(state_directory: &Path) -> Result<usize, WorktreeError> {
    let store = CollectionJournalStore::open(state_directory);
    let journals = store.load_all()?;
    let mut recovered = 0;
    for journal in journals.into_iter().filter(|journal| {
        !matches!(
            journal.phase,
            GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
        )
    }) {
        resume_decoded_collection(state_directory, &store, &journal, None)?;
        recovered += 1;
    }
    Ok(recovered)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn resume_decoded_collection(
    state_directory: &Path,
    store: &CollectionJournalStore,
    journal: &DecodedCollectionJournal,
    fail_after: Option<GarbageCollectionPhase>,
) -> Result<bool, WorktreeError> {
    let file = File::open(&journal.journal_path)
        .map_err(|source| io("open collection journal", &journal.journal_path, source))?;
    let mut record: CollectionJournalRecord =
        serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
            path: journal.journal_path.clone(),
            source,
        })?;
    resume_collection(state_directory, store, &mut record, journal, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn resume_collection(
    state_directory: &Path,
    store: &CollectionJournalStore,
    record: &mut CollectionJournalRecord,
    journal: &DecodedCollectionJournal,
    fail_after: Option<GarbageCollectionPhase>,
) -> Result<bool, WorktreeError> {
    validate_collection_paths(state_directory, journal)?;
    let lock_path = journal.base_path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| io("open immutable-base lock", &lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io("lock immutable base", &lock_path, source))?;

    if record.phase == GarbageCollectionPhase::IntentRecorded {
        if journal.quarantine_path.exists() {
            if !journal.base_path.exists() {
                validate_collection_marker(journal)?;
                remove_file_if_present(&journal.marker_path)?;
                sync_parent(&journal.marker_path)?;
            }
            advance_collection(
                store,
                record,
                GarbageCollectionPhase::MarkerRemoved,
                fail_after,
            )?;
        } else if protected_base_paths(state_directory)?.contains(&journal.base_path) {
            advance_collection(store, record, GarbageCollectionPhase::Cancelled, fail_after)?;
            return Ok(false);
        } else {
            validate_collectible_base(journal)?;
            remove_file_if_present(&journal.marker_path)?;
            sync_parent(&journal.marker_path)?;
            advance_collection(
                store,
                record,
                GarbageCollectionPhase::MarkerRemoved,
                fail_after,
            )?;
        }
    }

    if record.phase == GarbageCollectionPhase::MarkerRemoved {
        if journal.quarantine_path.exists() {
            advance_collection(
                store,
                record,
                GarbageCollectionPhase::BaseQuarantined,
                fail_after,
            )?;
        } else if protected_base_paths(state_directory)?.contains(&journal.base_path)
            || journal.marker_path.exists()
        {
            advance_collection(store, record, GarbageCollectionPhase::Cancelled, fail_after)?;
            return Ok(false);
        } else {
            validate_collectible_base(journal)?;
            if journal.base_path.exists() {
                make_directory_owner_writable(&journal.base_path)?;
                fs::rename(&journal.base_path, &journal.quarantine_path).map_err(|source| {
                    io(
                        "quarantine immutable base",
                        &journal.quarantine_path,
                        source,
                    )
                })?;
                sync_parent(&journal.base_path)?;
            }
            advance_collection(
                store,
                record,
                GarbageCollectionPhase::BaseQuarantined,
                fail_after,
            )?;
        }
    }

    if record.phase == GarbageCollectionPhase::BaseQuarantined {
        remove_tree_if_present(&journal.quarantine_path)?;
        sync_parent(&journal.quarantine_path)?;
        advance_collection(store, record, GarbageCollectionPhase::Complete, fail_after)?;
    }
    Ok(record.phase == GarbageCollectionPhase::Complete)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_collection_paths(
    state_directory: &Path,
    journal: &DecodedCollectionJournal,
) -> Result<(), WorktreeError> {
    let base_root = state_directory.join("bases/v1");
    let Some(repository_directory) = journal.base_path.parent() else {
        return Err(WorktreeError::InvalidRequest(format!(
            "collection journal {} has no base parent",
            journal.journal_path.display()
        )));
    };
    if !journal.base_path.is_absolute()
        || repository_directory.parent() != Some(base_root.as_path())
        || journal.marker_path != journal.base_path.with_extension("complete")
        || journal.quarantine_path.parent() != Some(repository_directory)
        || journal.quarantine_path.file_name()
            != Some(OsStr::new(&format!(".riftri-gc-{}", journal.operation_id)))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "collection journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    for directory in [&base_root, repository_directory] {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|source| io("inspect collection parent", directory, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::InvalidRequest(format!(
                "collection journal {} has a non-directory or symlinked parent {}",
                journal.journal_path.display(),
                directory.display()
            )));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_collectible_base(journal: &DecodedCollectionJournal) -> Result<(), WorktreeError> {
    if journal.base_path.exists() {
        let metadata = fs::symlink_metadata(&journal.base_path)
            .map_err(|source| io("inspect collectible base", &journal.base_path, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::InvalidRequest(format!(
                "collectible base {} is not a real directory",
                journal.base_path.display()
            )));
        }
    }
    validate_collection_marker(journal)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_collection_marker(journal: &DecodedCollectionJournal) -> Result<(), WorktreeError> {
    if journal.marker_path.exists() {
        let metadata = fs::symlink_metadata(&journal.marker_path).map_err(|source| {
            io(
                "inspect collectible base marker",
                &journal.marker_path,
                source,
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::InvalidRequest(format!(
                "collectible base marker {} is not a real file",
                journal.marker_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_directory_owner_writable(path: &Path) -> Result<(), WorktreeError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect collectible base directory", path, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(WorktreeError::InvalidRequest(format!(
            "collectible base {} is not a real directory",
            path.display()
        )));
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
    )
    .map_err(|source| io("prepare collectible base directory", path, source))
}

#[cfg(target_os = "windows")]
#[allow(clippy::permissions_set_readonly_false)]
fn make_directory_owner_writable(path: &Path) -> Result<(), WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect collectible base directory", path, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(WorktreeError::InvalidRequest(format!(
            "collectible base {} is not a real directory",
            path.display()
        )));
    }
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
        .map_err(|source| io("prepare collectible base directory", path, source))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn advance_collection(
    store: &CollectionJournalStore,
    journal: &mut CollectionJournalRecord,
    phase: GarbageCollectionPhase,
    fail_after: Option<GarbageCollectionPhase>,
) -> Result<(), WorktreeError> {
    journal.transition(phase)?;
    store.persist(journal)?;
    fail_collection_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_collection_if_requested(
    phase: GarbageCollectionPhase,
    fail_after: Option<GarbageCollectionPhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        Err(WorktreeError::InjectedCollectionFailure(phase))
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn remove_worktree_inner(
    _request: RemoveWorktreeRequest,
    _fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "journaled Riftri removal currently requires macOS".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn remove_worktree_inner(
    request: RemoveWorktreeRequest,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories do not have removable linked worktree views".to_owned(),
        ));
    }
    let repository_root = repository.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let destination = normalize_existing_destination(&request.destination)?;
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = fs::canonicalize(absolute_path(&requested_state)?)
        .map_err(|source| io("resolve state directory", &requested_state, source))?;
    let managed = find_managed_add_journal(&state_directory, &destination)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "{} is not an active Riftri-managed worktree in {}",
            destination.display(),
            state_directory.display()
        ))
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    let metadata_lock = acquire_git_worktree_metadata_lock(&repository.identity.common_git_dir)?;
    if !git
        .list_worktrees(&repository_root)?
        .into_iter()
        .any(|worktree| paths_match(&worktree.path, &destination))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} is not registered as a Git linked worktree",
            destination.display()
        )));
    }
    if !git.worktree_is_clean(&destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} has changes; commit, stash, or remove them before retrying",
            destination.display()
        )));
    }

    let store = RemovalJournalStore::create(&state_directory)?;
    let operation_id = allocate_removal_operation_id(&store)?;
    let mut journal = RemovalJournalRecord::new(
        operation_id,
        RemovalJournalPaths {
            repository: &repository_root,
            destination: &destination,
            base_path: &managed.base_path,
        },
        managed.operation_id,
    );
    let journal_path = store.persist(&journal)?;
    fail_removal_if_requested(journal.phase, fail_after)?;
    advance_removal(
        &store,
        &mut journal,
        RemoveWorktreePhase::CleanVerified,
        fail_after,
    )?;

    git.remove_worktree(&repository_root, &destination)?;
    advance_removal(
        &store,
        &mut journal,
        RemoveWorktreePhase::WorktreeRemoved,
        fail_after,
    )?;
    drop(metadata_lock);
    advance_removal(
        &store,
        &mut journal,
        RemoveWorktreePhase::BaseReleased,
        fail_after,
    )?;
    advance_removal(
        &store,
        &mut journal,
        RemoveWorktreePhase::Complete,
        fail_after,
    )?;

    Ok(RemoveWorktreeResult {
        destination,
        base_path: managed.base_path,
        journal_path,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn move_worktree_inner(
    _request: MoveWorktreeRequest,
    _fail_after: Option<MoveWorktreePhase>,
) -> Result<MoveWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "journaled Riftri moves currently require macOS".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn move_worktree_inner(
    request: MoveWorktreeRequest,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<MoveWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories do not have movable linked worktree views".to_owned(),
        ));
    }
    let repository_root = repository.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let source = normalize_existing_destination(&request.source)?;
    let destination = normalize_new_destination(&request.destination)?;
    let source_volume = inspected_native_cow_volume(&source)?;
    let destination_volume = supported_native_cow_volume(&destination)?;
    if source_volume.identity != destination_volume.identity {
        return Err(WorktreeError::Unsupported(
            "moving an optimized worktree across filesystem volumes is not supported".to_owned(),
        ));
    }

    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = fs::canonicalize(absolute_path(&requested_state)?)
        .map_err(|source| io("resolve state directory", &requested_state, source))?;
    let managed = find_managed_add_journal(&state_directory, &source)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "{} is not an active Riftri-managed worktree in {}",
            source.display(),
            state_directory.display()
        ))
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    let inventory = git.list_worktrees(&repository_root)?;
    if !inventory
        .iter()
        .any(|worktree| paths_match(&worktree.path, &source))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} is not registered as a Git linked worktree",
            source.display()
        )));
    }
    if MoveJournalStore::open(&state_directory)
        .load_all()?
        .iter()
        .any(|journal| {
            journal.source_add_operation_id == managed.operation_id
                && journal.phase != MoveWorktreePhase::Complete
        })
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a move of {} is already pending; run `riftri repair --state-dir {}`",
            source.display(),
            state_directory.display()
        )));
    }

    let store = MoveJournalStore::create(&state_directory)?;
    let operation_id = allocate_move_operation_id(&store)?;
    let journal = MoveJournalRecord::new(
        operation_id,
        MoveJournalPaths {
            repository: &repository_root,
            source: &source,
            destination: &destination,
        },
        managed.operation_id,
    );
    let journal_path = store.persist(&journal)?;
    fail_move_if_requested(journal.phase, fail_after)?;
    resume_move(
        &git,
        &store,
        journal.decode(journal_path.clone())?,
        fail_after,
    )?;

    Ok(MoveWorktreeResult {
        source,
        destination,
        base_path: managed.base_path,
        journal_path,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn prune_worktrees_inner(
    _request: PruneWorktreesRequest,
    _fail_after: Option<PruneWorktreesPhase>,
) -> Result<PruneWorktreesResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "journaled Riftri pruning currently requires macOS".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn prune_worktrees_inner(
    request: PruneWorktreesRequest,
    fail_after: Option<PruneWorktreesPhase>,
) -> Result<PruneWorktreesResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories do not have linked worktree views to prune".to_owned(),
        ));
    }
    let repository_root = repository.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = fs::canonicalize(absolute_path(&requested_state)?)
        .map_err(|source| io("resolve state directory", &requested_state, source))?;
    verify_repository_prune_safe(&git, &state_directory, &repository_root, None)?;

    let store = PruneJournalStore::create(&state_directory)?;
    let operation_id = allocate_prune_operation_id(&store)?;
    let journal = PruneJournalRecord::new(operation_id, &repository_root);
    let journal_path = store.persist(&journal)?;
    fail_prune_if_requested(journal.phase, fail_after)?;
    resume_prune(
        &git,
        &store,
        journal.decode(journal_path.clone())?,
        fail_after,
    )?;
    Ok(PruneWorktreesResult { journal_path })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn add_worktree_inner(
    _request: AddWorktreeRequest,
    _fail_after: Option<AddWorktreePhase>,
    _rollback_on_error: bool,
) -> Result<AddWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "this build has no supported native copy-on-write worktree backend".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn add_worktree_inner(
    request: AddWorktreeRequest,
    fail_after: Option<AddWorktreePhase>,
    rollback_on_error: bool,
) -> Result<AddWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories are not supported by optimized checkout".to_owned(),
        ));
    }
    let repository_root = repository.root.clone().ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let destination = normalize_new_destination(&request.destination)?;
    let checkout_profile = validate_compatibility(&git, &repository_root, &request.revision)?;
    let resolved = git.resolve_revision(&repository_root, &request.revision)?;
    let destination_volume = supported_native_cow_volume(&destination)?;

    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = absolute_path(&requested_state)?;
    let state_volume = inspected_native_cow_volume(&state_directory)?;
    if destination_volume.identity != state_volume.identity {
        return Err(WorktreeError::Unsupported(format!(
            "state directory {} and destination {} are on different volumes; pass --state-dir on the destination filesystem volume",
            state_directory.display(),
            destination.display()
        )));
    }

    create_state_layout(&state_directory)?;
    let state_directory = fs::canonicalize(&state_directory)
        .map_err(|source| io("resolve state directory", &state_directory, source))?;
    register_state_directory(&git, &repository, &state_directory)?;
    let store = JournalStore::create(&state_directory)?;
    let base_directory = state_directory.join("bases/v1").join(repository_cache_id(
        &repository.identity.common_git_dir,
        &checkout_profile,
    ));
    fs::create_dir_all(&base_directory)
        .map_err(|source| io("create repository base directory", &base_directory, source))?;
    sync_parent(&base_directory)?;
    let operation_id =
        allocate_operation_id(&store, &state_directory, &base_directory, &destination)?;
    let base_path = base_directory.join(resolved.tree.as_str());
    let base_staging = base_directory.join(format!(".riftri-build-{operation_id}"));
    let temporary_index = state_directory
        .join("tmp")
        .join(format!("index-{operation_id}"));
    let scratch = destination
        .parent()
        .expect("normalized destination has a parent")
        .join(format!(".riftri-view-{operation_id}"));
    let branch = match &request.mode {
        WorktreeMode::NewBranch(branch) => Some(branch.as_os_str()),
        WorktreeMode::Detached => None,
    };
    let journal_paths = JournalPaths {
        repository: &repository_root,
        destination: &destination,
        scratch: &scratch,
        base_staging: &base_staging,
        base_path: &base_path,
        temporary_index: &temporary_index,
        branch,
    };
    let backend = native_backend_kind();
    let mut journal = if backend == BackendKind::OverlayFs {
        let layout_root = state_directory.join("overlays/v1").join(&operation_id);
        JournalRecord::new_overlayfs(
            operation_id,
            journal_paths,
            resolved.commit.as_str().to_owned(),
            &layout_root,
            overlayfs_recovery_token(&layout_root),
        )?
    } else {
        JournalRecord::new(
            operation_id,
            journal_paths,
            resolved.commit.as_str().to_owned(),
            backend,
        )
    };
    let journal_path = store.persist(&journal)?;

    let operation = fail_add_if_requested(journal.phase, fail_after).and_then(|()| {
        perform_add(
            &git,
            &store,
            &mut journal,
            &repository_root,
            &destination,
            &scratch,
            &base_path,
            &base_staging,
            &temporary_index,
            &request.revision,
            &request.mode,
            &resolved.tree,
            &repository.identity.common_git_dir,
            fail_after,
        )
    });

    match operation {
        Ok(reused_base) => Ok(AddWorktreeResult {
            destination,
            commit: resolved.commit,
            tree: resolved.tree,
            base_path,
            journal_path,
            reused_base,
            backend: native_backend_kind(),
        }),
        Err(operation_error) => {
            if !rollback_on_error {
                return Err(operation_error);
            }
            let decoded = journal.clone().decode(journal_path.clone());
            let rollback_transition = journal.transition(AddWorktreePhase::RollbackPending);
            let rollback_journal =
                rollback_transition
                    .map_err(WorktreeError::from)
                    .and_then(|()| {
                        store
                            .persist(&journal)
                            .map(|_| ())
                            .map_err(WorktreeError::from)
                    });
            let rollback = rollback_journal
                .and_then(|()| decoded.map_err(WorktreeError::from))
                .and_then(|decoded| rollback_decoded(&git, &decoded))
                .and_then(|()| {
                    journal.transition(AddWorktreePhase::RolledBack)?;
                    store.persist(&journal)?;
                    Ok(())
                });

            match rollback {
                Ok(()) => Err(operation_error),
                Err(rollback_error) => Err(WorktreeError::OperationAndRollback {
                    operation: operation_error.to_string(),
                    rollback: rollback_error.to_string(),
                }),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn perform_add(
    git: &Git,
    store: &JournalStore,
    journal: &mut JournalRecord,
    repository: &Path,
    destination: &Path,
    scratch: &Path,
    base_path: &Path,
    base_staging: &Path,
    temporary_index: &Path,
    revision: &OsStr,
    mode: &WorktreeMode,
    tree: &ObjectId,
    common_git_dir: &Path,
    fail_after: Option<AddWorktreePhase>,
) -> Result<bool, WorktreeError> {
    let head = match mode {
        WorktreeMode::NewBranch(branch) => WorktreeHead::NewBranch(branch),
        WorktreeMode::Detached => WorktreeHead::Detached,
    };
    let metadata_lock = acquire_git_worktree_metadata_lock(common_git_dir)?;
    git.add_worktree_no_checkout(repository, destination, revision, head)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitMetadataCreated,
        fail_after,
    )?;
    drop(metadata_lock);

    let reused_base = prepare_base(
        git,
        repository,
        tree,
        base_path,
        base_staging,
        temporary_index,
    )?;
    advance(store, journal, AddWorktreePhase::BaseReady, fail_after)?;

    NativeCowCloner::clone_tree(base_path, scratch)?;
    NativeCowCloner::make_tree_owner_writable(scratch)?;
    advance(store, journal, AddWorktreePhase::ViewCreated, fail_after)?;

    let git_pointer = destination.join(".git");
    if !contains_only_git_pointer(destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "Git created unexpected files in {}; refusing to replace them",
            destination.display()
        )));
    }
    fs::rename(&git_pointer, scratch.join(".git"))
        .map_err(|source| io("move linked-worktree pointer", &git_pointer, source))?;
    fs::remove_dir(destination)
        .map_err(|source| io("remove empty checkout directory", destination, source))?;
    fs::rename(scratch, destination)
        .map_err(|source| io("activate native COW worktree view", destination, source))?;
    sync_parent(destination)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitPointerRestored,
        fail_after,
    )?;

    git.synchronize_worktree_index(destination)?;
    advance(
        store,
        journal,
        AddWorktreePhase::IndexSynchronized,
        fail_after,
    )?;
    if !git.worktree_is_clean(destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "new worktree {} is not clean; it was not activated",
            destination.display()
        )));
    }
    advance(store, journal, AddWorktreePhase::CleanVerified, fail_after)?;
    advance(store, journal, AddWorktreePhase::Active, fail_after)?;
    Ok(reused_base)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn advance(
    store: &JournalStore,
    journal: &mut JournalRecord,
    phase: AddWorktreePhase,
    fail_after: Option<AddWorktreePhase>,
) -> Result<(), WorktreeError> {
    journal.transition(phase)?;
    store.persist(journal)?;
    fail_add_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_add_if_requested(
    phase: AddWorktreePhase,
    fail_after: Option<AddWorktreePhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        return Err(WorktreeError::InjectedFailure(phase));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn prepare_base(
    git: &Git,
    repository: &Path,
    tree: &ObjectId,
    base_path: &Path,
    base_staging: &Path,
    temporary_index: &Path,
) -> Result<bool, WorktreeError> {
    let base_parent = base_path.expect_parent()?;
    let lock_path = base_parent.join(format!("{}.lock", tree.as_str()));
    let complete_path = base_parent.join(format!("{}.complete", tree.as_str()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| io("open immutable-base lock", &lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io("lock immutable base", &lock_path, source))?;

    let base_exists = base_path
        .try_exists()
        .map_err(|source| io("inspect immutable base", base_path, source))?;
    let complete_exists = match fs::symlink_metadata(&complete_path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(WorktreeError::InvalidRequest(format!(
                    "immutable-base completion marker {} is not a real file",
                    complete_path.display()
                )));
            }
            true
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(io(
                "inspect immutable-base completion marker",
                &complete_path,
                source,
            ));
        }
    };
    if base_exists && complete_exists {
        let metadata = fs::symlink_metadata(base_path)
            .map_err(|source| io("inspect immutable base", base_path, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorktreeError::InvalidRequest(format!(
                "immutable base path {} is not a real directory",
                base_path.display()
            )));
        }
        return Ok(true);
    }
    if base_exists {
        remove_tree_if_present(base_path)?;
    }
    if complete_exists {
        remove_file_if_present(&complete_path)?;
    }

    fs::create_dir(base_staging).map_err(|source| {
        io(
            "create immutable-base staging directory",
            base_staging,
            source,
        )
    })?;
    git.materialize_tree(repository, tree, base_staging, temporary_index)?;
    remove_file_if_present(temporary_index)?;
    fs::rename(base_staging, base_path)
        .map_err(|source| io("activate immutable base", base_path, source))?;
    NativeCowCloner::make_tree_read_only(base_path)?;
    let marker = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&complete_path)
        .map_err(|source| io("create immutable-base marker", &complete_path, source))?;
    marker
        .sync_all()
        .map_err(|source| io("sync immutable-base marker", &complete_path, source))?;
    sync_parent(base_path)?;
    Ok(false)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_compatibility(
    git: &Git,
    repository: &Path,
    revision: &OsStr,
) -> Result<Vec<u8>, WorktreeError> {
    let analysis = analyze_repository_compatibility(git, repository, revision)?;
    if let Some(blocker) = analysis.report.blockers.first() {
        return Err(WorktreeError::Unsupported(blocker.explanation.clone()));
    }
    Ok(analysis.checkout_profile)
}

pub(crate) fn inspect_repository_compatibility(
    git: &Git,
    repository: &Path,
    revision: &OsStr,
) -> Result<RepositoryCompatibilityReport, WorktreeError> {
    Ok(analyze_repository_compatibility(git, repository, revision)?.report)
}

fn analyze_repository_compatibility(
    git: &Git,
    repository: &Path,
    revision: &OsStr,
) -> Result<CompatibilityAnalysis, WorktreeError> {
    let resolved = git.resolve_revision(repository, revision)?;
    let entries = git.list_tree(repository, &resolved.tree)?;
    let paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    let mut blockers = Vec::new();
    let mut has_submodules = false;
    for entry in &entries {
        if entry.path == Path::new(".gitmodules") || entry.object_kind == b"commit" {
            has_submodules = true;
        }
    }
    if has_submodules {
        blockers.push(RepositoryCompatibilityBlocker {
            kind: RepositoryCompatibilityBlockerKind::Submodules,
            explanation: "tree contains submodule metadata or gitlink entries; submodules are not supported yet"
                .to_owned(),
        });
    }

    let info_attributes_path = git.info_attributes_path(repository)?;
    let info_attributes_safe = match fs::read(&info_attributes_path) {
        Ok(contents) if contents.is_empty() => true,
        Ok(_) => {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::EffectiveAttributes,
                explanation: format!(
                    "repository attributes file {} is not empty; external attributes are not part of the immutable tree and are not supported yet",
                    info_attributes_path.display()
                ),
            });
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::EffectiveAttributes,
                explanation: format!(
                    "repository attributes file {} could not be inspected safely: {error}",
                    info_attributes_path.display()
                ),
            });
            false
        }
    };
    if info_attributes_safe {
        let mut in_tree = git.in_tree_attributes_for_paths(repository, &resolved.tree, &paths)?;
        if let Some(attribute) = in_tree
            .iter()
            .find(|attribute| !is_supported_in_tree_attribute(attribute))
        {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::InTreeAttributes,
                explanation: format!(
                    "tree attribute {}={} for {} is outside Riftri's deterministic checkout allowlist; Git LFS, filters, encodings, ident substitution, legacy, and unknown attributes are not supported yet",
                    String::from_utf8_lossy(&attribute.name),
                    String::from_utf8_lossy(&attribute.value),
                    attribute.path.display(),
                ),
            });
        }

        let mut effective =
            git.effective_attributes_for_tree_paths(repository, &resolved.tree, &paths)?;
        in_tree.sort_unstable();
        effective.sort_unstable();
        if effective != in_tree {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::EffectiveAttributes,
                explanation: "global or system attributes change at least one tracked path; external attributes are not part of the immutable tree and are not supported yet"
                    .to_owned(),
            });
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let mut profile = {
        let mut profile = Sha256::new();
        profile.update(b"riftri-checkout-profile-v1\0");
        let git_version = git.detect()?.version;
        hash_profile_input(&mut profile, b"git.version", Some(git_version.as_bytes()));
        profile
    };

    for (key, accepted, kind) in [
        (
            "core.attributesfile",
            &[][..],
            RepositoryCompatibilityBlockerKind::EffectiveAttributes,
        ),
        (
            "core.sparsecheckout",
            &[b"false".as_slice()][..],
            RepositoryCompatibilityBlockerKind::SparseCheckout,
        ),
        (
            "core.sparsecheckoutcone",
            &[b"false".as_slice()][..],
            RepositoryCompatibilityBlockerKind::SparseCheckout,
        ),
        (
            "core.autocrlf",
            &[b"false".as_slice()][..],
            RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
        ),
        (
            "core.eol",
            &[b"native".as_slice(), b"lf".as_slice()][..],
            RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
        ),
        (
            "core.symlinks",
            &[b"true".as_slice()][..],
            RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
        ),
    ] {
        let value = git.config_value(repository, key)?;
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
        if let Some(value) = value
            && !accepted
                .iter()
                .any(|accepted| value.eq_ignore_ascii_case(accepted))
        {
            blockers.push(RepositoryCompatibilityBlocker {
                kind,
                explanation: format!(
                    "Git configuration {key}={} can change checkout bytes and is not supported yet",
                    String::from_utf8_lossy(&value)
                ),
            });
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        for key in [
            "core.filemode",
            "core.ignorecase",
            "core.precomposeunicode",
            "core.protecthfs",
            "core.protectntfs",
        ] {
            let value = git.config_value(repository, key)?;
            hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
        }
    }
    Ok(CompatibilityAnalysis {
        report: RepositoryCompatibilityReport {
            commit: resolved.commit,
            tree: resolved.tree,
            compatible: blockers.is_empty(),
            blockers,
        },
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        checkout_profile: profile.finalize().to_vec(),
    })
}

fn is_supported_in_tree_attribute(attribute: &GitAttribute) -> bool {
    match attribute.name.as_slice() {
        b"text" => matches!(attribute.value.as_slice(), b"set" | b"unset" | b"auto"),
        b"eol" => matches!(attribute.value.as_slice(), b"lf" | b"crlf"),
        b"binary" => attribute.value == b"set",
        b"diff" | b"merge" => attribute.value == b"unset",
        _ => false,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn hash_profile_input(hasher: &mut Sha256, key: &[u8], value: Option<&[u8]>) {
    hasher.update((key.len() as u64).to_le_bytes());
    hasher.update(key);
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        None => hasher.update([0]),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn supported_native_cow_volume(path: &Path) -> Result<DestinationVolume, WorktreeError> {
    #[cfg(target_os = "macos")]
    let capability = probe_backends(path)
        .into_iter()
        .find(|capability| capability.kind == BackendKind::ApfsClone)
        .ok_or_else(|| {
            WorktreeError::Unsupported(
                "this build does not provide the APFS clone backend".to_owned(),
            )
        })?;
    #[cfg(target_os = "linux")]
    let capability = riftri_storage::ReflinkCloner::probe(path);
    #[cfg(target_os = "windows")]
    let capability = riftri_storage::RefsBlockCloner::probe(path);
    if capability.status != CapabilityStatus::Supported {
        return Err(WorktreeError::Unsupported(capability.explanation));
    }
    capability.volume.ok_or_else(|| {
        WorktreeError::Unsupported(
            "native COW capability did not include a volume identity".to_owned(),
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn inspected_native_cow_volume(path: &Path) -> Result<DestinationVolume, WorktreeError> {
    let backend = native_backend_kind();
    let capability = probe_backends(path)
        .into_iter()
        .find(|capability| capability.kind == backend)
        .ok_or_else(|| {
            WorktreeError::Unsupported(format!(
                "this build does not provide the {} backend",
                backend.display_name()
            ))
        })?;
    let volume = capability.volume.ok_or_else(|| {
        WorktreeError::Unsupported(format!(
            "{} capability did not include a volume identity",
            backend.display_name()
        ))
    })?;
    #[cfg(target_os = "linux")]
    if capability.status == CapabilityStatus::Unavailable
        && volume.identity.filesystem == "xfs"
        && !volume.read_only
    {
        return Ok(volume);
    }
    #[cfg(target_os = "windows")]
    if capability.status == CapabilityStatus::Unavailable
        && volume.identity.filesystem.eq_ignore_ascii_case("ReFS")
        && !volume.read_only
    {
        return Ok(volume);
    }
    if capability.status != CapabilityStatus::Supported {
        return Err(WorktreeError::Unsupported(capability.explanation));
    }
    Ok(volume)
}

#[cfg(target_os = "macos")]
const fn native_backend_kind() -> BackendKind {
    BackendKind::ApfsClone
}

#[cfg(target_os = "linux")]
const fn native_backend_kind() -> BackendKind {
    BackendKind::Reflink
}

#[cfg(target_os = "windows")]
const fn native_backend_kind() -> BackendKind {
    BackendKind::RefsBlockClone
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn normalize_new_destination(destination: &Path) -> Result<PathBuf, WorktreeError> {
    if destination.as_os_str().is_empty() {
        return Err(WorktreeError::InvalidRequest(
            "destination cannot be empty".to_owned(),
        ));
    }
    if destination
        .try_exists()
        .map_err(|source| io("inspect worktree destination", destination, source))?
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination already exists: {}",
            destination.display()
        )));
    }
    let absolute = absolute_path(destination)?;
    let file_name = absolute.file_name().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "destination must name a new directory: {}",
            destination.display()
        ))
    })?;
    let parent = absolute.expect_parent()?;
    let parent =
        fs::canonicalize(parent).map_err(|source| io("resolve worktree parent", parent, source))?;
    let normalized = parent.join(file_name);
    if normalized
        .try_exists()
        .map_err(|source| io("inspect normalized destination", &normalized, source))?
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination already exists: {}",
            normalized.display()
        )));
    }
    Ok(normalized)
}

fn absolute_path(path: &Path) -> Result<PathBuf, WorktreeError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|source| io("resolve current directory", path, source))
    }
}

#[cfg(target_os = "windows")]
fn paths_match(left: &Path, right: &Path) -> bool {
    windows_path_key(left) == windows_path_key(right)
}

#[cfg(not(target_os = "windows"))]
fn paths_match(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(target_os = "windows")]
fn windows_path_key(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];

    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut normalized = if wide.starts_with(VERBATIM_UNC) {
        let mut unc = vec![b'\\' as u16, b'\\' as u16];
        unc.extend_from_slice(&wide[VERBATIM_UNC.len()..]);
        unc
    } else if wide.starts_with(VERBATIM) {
        wide[VERBATIM.len()..].to_vec()
    } else {
        wide
    };
    for unit in &mut normalized {
        if *unit == b'/' as u16 {
            *unit = b'\\' as u16;
        } else if (b'a' as u16..=b'z' as u16).contains(unit) {
            *unit -= u16::from(b'a' - b'A');
        }
    }
    normalized
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn create_state_layout(state_directory: &Path) -> Result<(), WorktreeError> {
    fs::create_dir_all(state_directory)
        .map_err(|source| io("create Riftri state directory", state_directory, source))?;
    require_real_state_directory(state_directory)?;
    for directory in [
        state_directory.join("bases"),
        state_directory.join("bases/v1"),
        state_directory.join("overlays"),
        state_directory.join("overlays/v1"),
        state_directory.join("operations"),
        state_directory.join("removals"),
        state_directory.join("moves"),
        state_directory.join("prunes"),
        state_directory.join("collections"),
        state_directory.join("tmp"),
    ] {
        ensure_real_state_directory(&directory, "create Riftri state directory")?;
    }
    sync_parent(state_directory)?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn acquire_git_worktree_metadata_lock(common_git_dir: &Path) -> Result<File, WorktreeError> {
    let lock_path = common_git_dir.join("riftri-worktree-metadata.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| io("open Git worktree metadata lock", &lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io("lock Git worktree metadata", &lock_path, source))?;
    Ok(lock)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn acquire_git_worktree_metadata_lock_for_repository(
    git: &Git,
    repository: &Path,
) -> Result<File, WorktreeError> {
    let repository = git.inspect_repository(repository)?;
    acquire_git_worktree_metadata_lock(&repository.identity.common_git_dir)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn allocate_operation_id(
    store: &JournalStore,
    state_directory: &Path,
    base_directory: &Path,
    destination: &Path,
) -> Result<String, WorktreeError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorktreeError::InvalidRequest(format!("system clock error: {error}")))?
        .as_nanos();
    for _ in 0..1000_u16 {
        let operation_id = next_operation_id(timestamp);
        let scratch = destination
            .parent()
            .expect("normalized destination has a parent")
            .join(format!(".riftri-view-{operation_id}"));
        let staging = base_directory.join(format!(".riftri-build-{operation_id}"));
        let index = state_directory
            .join("tmp")
            .join(format!("index-{operation_id}"));
        if !store.path_for(&operation_id).exists()
            && !scratch.exists()
            && !staging.exists()
            && !index.exists()
        {
            return Ok(operation_id);
        }
    }
    Err(WorktreeError::InvalidRequest(
        "could not allocate a unique operation ID".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn repository_cache_id(common_git_directory: &Path, checkout_profile: &[u8]) -> String {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(common_git_directory.as_os_str().as_bytes());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in common_git_directory.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }
    hasher.update([0]);
    hasher.update(checkout_profile);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(2 + digest.len() * 2);
    encoded.push_str("r-");
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn overlayfs_recovery_token(layout_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"riftri-overlayfs-recovery-v1\0");
    hasher.update(
        layout_root
            .file_name()
            .expect("an OverlayFS layout root always has an operation ID")
            .to_string_lossy()
            .as_bytes(),
    );
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn normalize_existing_destination(destination: &Path) -> Result<PathBuf, WorktreeError> {
    if destination.as_os_str().is_empty() {
        return Err(WorktreeError::InvalidRequest(
            "destination cannot be empty".to_owned(),
        ));
    }
    fs::canonicalize(destination)
        .map_err(|source| io("resolve worktree destination", destination, source))
}

fn find_managed_add_journal(
    state_directory: &Path,
    destination: &Path,
) -> Result<Option<DecodedJournal>, WorktreeError> {
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed = removals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let pending = removals
        .iter()
        .filter(|journal| journal.phase != RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let mut matches = JournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            journal.phase == AddWorktreePhase::Active
                && journal.destination == destination
                && !completed.contains(journal.operation_id.as_str())
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(WorktreeError::InvalidRequest(format!(
            "multiple active Riftri journals reference {}",
            destination.display()
        )));
    }
    let managed = matches.pop();
    if managed
        .as_ref()
        .is_some_and(|journal| pending.contains(journal.operation_id.as_str()))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a removal of {} is already pending; run `riftri recover --state-dir {}`",
            destination.display(),
            state_directory.display()
        )));
    }
    Ok(managed)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn allocate_removal_operation_id(store: &RemovalJournalStore) -> Result<String, WorktreeError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorktreeError::InvalidRequest(format!("system clock error: {error}")))?
        .as_nanos();
    for _ in 0..1000_u16 {
        let operation_id = format!("remove-{}", next_operation_id(timestamp));
        if !store.path_for(&operation_id).exists() {
            return Ok(operation_id);
        }
    }
    Err(WorktreeError::InvalidRequest(
        "could not allocate a unique removal operation ID".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn allocate_move_operation_id(store: &MoveJournalStore) -> Result<String, WorktreeError> {
    allocate_lifecycle_operation_id("move", |operation_id| store.path_for(operation_id).exists())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn allocate_prune_operation_id(store: &PruneJournalStore) -> Result<String, WorktreeError> {
    allocate_lifecycle_operation_id("prune", |operation_id| {
        store.path_for(operation_id).exists()
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn allocate_lifecycle_operation_id(
    prefix: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<String, WorktreeError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorktreeError::InvalidRequest(format!("system clock error: {error}")))?
        .as_nanos();
    for _ in 0..1000_u16 {
        let operation_id = format!("{prefix}-{}", next_operation_id(timestamp));
        if !exists(&operation_id) {
            return Ok(operation_id);
        }
    }
    Err(WorktreeError::InvalidRequest(format!(
        "could not allocate a unique {prefix} operation ID"
    )))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn next_operation_id(timestamp: u128) -> String {
    let nonce = OPERATION_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:x}-{:x}-{nonce:x}", std::process::id())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn advance_removal(
    store: &RemovalJournalStore,
    journal: &mut RemovalJournalRecord,
    phase: RemoveWorktreePhase,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    journal.transition(phase)?;
    store.persist(journal)?;
    fail_removal_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_removal_if_requested(
    phase: RemoveWorktreePhase,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        return Err(WorktreeError::InjectedRemovalFailure(phase));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn diagnose_state_paths(
    state_directory: &Path,
    add_journals: &[DecodedJournal],
    removal_journals: &[DecodedRemovalJournal],
    move_journals: &[DecodedMoveJournal],
    prune_journals: &[DecodedPruneJournal],
    collection_journals: &[DecodedCollectionJournal],
) -> Result<StatePathDiagnosis, WorktreeError> {
    if !state_directory.exists() {
        return Ok(StatePathDiagnosis::default());
    }

    let mut issues = Vec::new();
    let expected_roots = [
        "bases",
        "overlays",
        "operations",
        "removals",
        "moves",
        "prunes",
        "collections",
        "tmp",
    ];
    for path in child_paths(state_directory, "read Riftri state directory")? {
        let expected = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| expected_roots.contains(&name));
        if !expected {
            add_state_issue(
                &mut issues,
                path,
                "not part of the versioned Riftri state layout",
            );
        } else if !is_real_directory(&path)? {
            add_state_issue(
                &mut issues,
                path,
                "expected a real Riftri state directory, not a file or symlink",
            );
        }
    }

    diagnose_journal_directory(
        &state_directory.join("operations"),
        add_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("removals"),
        removal_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("moves"),
        move_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("prunes"),
        prune_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("collections"),
        collection_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &mut issues,
    )?;

    let pending_adds = add_journals
        .iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                AddWorktreePhase::Active | AddWorktreePhase::RolledBack
            )
        })
        .collect::<Vec<_>>();
    let pending_base_builds = pending_adds
        .iter()
        .copied()
        .filter(|journal| journal.last_forward_phase < AddWorktreePhase::BaseReady)
        .collect::<Vec<_>>();
    diagnose_temporary_directory(
        &state_directory.join("tmp"),
        pending_base_builds
            .iter()
            .map(|journal| journal.temporary_index.clone())
            .collect(),
        &mut issues,
    )?;
    let mut coordination_locks = 0;
    diagnose_base_directories(
        state_directory,
        collection_journals,
        &pending_base_builds,
        &mut issues,
        &mut coordination_locks,
    )?;
    diagnose_overlay_directories(state_directory, add_journals, removal_journals, &mut issues)?;

    let completed_removals = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    for journal in add_journals.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active
            && !completed_removals.contains(journal.operation_id.as_str())
    }) {
        if !is_real_directory_if_present(&journal.destination)? {
            add_state_issue(
                &mut issues,
                journal.destination.clone(),
                "an active add journal references a missing worktree",
            );
        }
        if !is_real_directory_if_present(&journal.base_path)? {
            add_state_issue(
                &mut issues,
                journal.base_path.clone(),
                "an active add journal references a missing or unsafe immutable base",
            );
        } else if !is_regular_file_if_present(&journal.base_path.with_extension("complete"))? {
            add_state_issue(
                &mut issues,
                journal.base_path.with_extension("complete"),
                "an active add journal's immutable base has no safe completion marker",
            );
        }
    }

    issues.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    issues.dedup_by(|left, right| left.path == right.path && left.reason == right.reason);
    Ok(StatePathDiagnosis {
        issues,
        coordination_locks,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn diagnose_state_paths(
    _state_directory: &Path,
    _add_journals: &[DecodedJournal],
    _removal_journals: &[DecodedRemovalJournal],
    _move_journals: &[DecodedMoveJournal],
    _prune_journals: &[DecodedPruneJournal],
    _collection_journals: &[DecodedCollectionJournal],
) -> Result<StatePathDiagnosis, WorktreeError> {
    Ok(StatePathDiagnosis::default())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn diagnose_journal_directory(
    directory: &Path,
    expected: HashSet<PathBuf>,
    issues: &mut Vec<StateDiagnosticIssue>,
) -> Result<(), WorktreeError> {
    if !is_real_directory_if_present(directory)? {
        return Ok(());
    }
    for path in child_paths(directory, "read Riftri journal directory")? {
        if !expected.contains(&path) {
            add_state_issue(
                issues,
                path,
                "not a recognized durable operation journal; Riftri will preserve it",
            );
        } else if !is_regular_file(&path)? {
            add_state_issue(
                issues,
                path,
                "a durable operation journal must be a regular file",
            );
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn diagnose_temporary_directory(
    directory: &Path,
    expected: HashSet<PathBuf>,
    issues: &mut Vec<StateDiagnosticIssue>,
) -> Result<(), WorktreeError> {
    if !is_real_directory_if_present(directory)? {
        return Ok(());
    }
    for path in child_paths(directory, "read Riftri temporary directory")? {
        if !expected.contains(&path) {
            add_state_issue(
                issues,
                path,
                "temporary artifact is not referenced by a pending add journal",
            );
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn diagnose_base_directories(
    state_directory: &Path,
    collection_journals: &[DecodedCollectionJournal],
    pending_base_builds: &[&DecodedJournal],
    issues: &mut Vec<StateDiagnosticIssue>,
    coordination_locks: &mut usize,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases");
    if is_real_directory_if_present(&bases)? {
        for path in child_paths(&bases, "read immutable-base layout")? {
            if path != bases.join("v1") {
                add_state_issue(
                    issues,
                    path,
                    "not part of the supported immutable-base layout version",
                );
            }
        }
    }

    let root = bases.join("v1");
    if !is_real_directory_if_present(&root)? {
        return Ok(());
    }
    let pending_staging = pending_base_builds
        .iter()
        .map(|journal| journal.base_staging.clone())
        .collect::<HashSet<_>>();
    let pending_quarantines = collection_journals
        .iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
            )
        })
        .map(|journal| journal.quarantine_path.clone())
        .collect::<HashSet<_>>();

    for repository_path in child_paths(&root, "read immutable-base root")? {
        if !is_real_directory(&repository_path)? {
            add_state_issue(
                issues,
                repository_path,
                "immutable-base repository bucket must be a real directory",
            );
            continue;
        }
        let entries = child_paths(&repository_path, "read immutable-base repository bucket")?;
        if entries.is_empty() {
            add_state_issue(
                issues,
                repository_path,
                "empty immutable-base repository bucket has no journaled owner",
            );
            continue;
        }
        for path in entries {
            let expected_base = is_regular_file_if_present(&path.with_extension("complete"))?
                || collection_journals.iter().any(|journal| {
                    journal.phase == GarbageCollectionPhase::MarkerRemoved
                        && journal.base_path == path
                });
            let expected_staging = pending_staging.contains(&path);
            let expected_quarantine = pending_quarantines.contains(&path);
            let is_complete_marker = path.extension() == Some(OsStr::new("complete"))
                && is_regular_file(&path)?
                && is_real_directory_if_present(&path.with_extension(""))?;
            let is_lock = path.extension() == Some(OsStr::new("lock"))
                && is_regular_file(&path)?
                && path
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .is_some_and(looks_like_object_id);

            if is_complete_marker || is_lock {
                if is_lock {
                    *coordination_locks = (*coordination_locks).saturating_add(1);
                }
                continue;
            }
            if expected_base || expected_staging || expected_quarantine {
                if !is_real_directory(&path)? {
                    add_state_issue(
                        issues,
                        path,
                        "journaled immutable-base artifact must be a real directory",
                    );
                }
            } else {
                add_state_issue(
                    issues,
                    path,
                    "immutable-base artifact is not explained by a completion marker or journal",
                );
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn diagnose_overlay_directories(
    state_directory: &Path,
    add_journals: &[DecodedJournal],
    removal_journals: &[DecodedRemovalJournal],
    issues: &mut Vec<StateDiagnosticIssue>,
) -> Result<(), WorktreeError> {
    let overlays = state_directory.join("overlays");
    if is_real_directory_if_present(&overlays)? {
        for path in child_paths(&overlays, "read OverlayFS state layout")? {
            if path != overlays.join("v1") {
                add_state_issue(
                    issues,
                    path,
                    "not part of the supported OverlayFS state layout version",
                );
            }
        }
    }

    let root = overlays.join("v1");
    if !is_real_directory_if_present(&root)? {
        return Ok(());
    }
    let completed_removals = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let expected = add_journals
        .iter()
        .filter(|journal| {
            journal.backend == BackendKind::OverlayFs
                && journal.phase != AddWorktreePhase::RolledBack
                && journal.last_forward_phase >= AddWorktreePhase::ViewCreated
                && !completed_removals.contains(journal.operation_id.as_str())
        })
        .filter_map(|journal| {
            journal
                .overlayfs
                .as_ref()
                .map(|overlayfs| overlayfs.layout_root.clone())
        })
        .collect::<HashSet<_>>();

    for path in &expected {
        if !is_real_directory_if_present(path)? {
            add_state_issue(
                issues,
                path.clone(),
                "a live OverlayFS journal references missing or unsafe private layers",
            );
        }
    }

    for path in child_paths(&root, "read OverlayFS view roots")? {
        if !expected.contains(&path) {
            add_state_issue(
                issues,
                path,
                "OverlayFS private layers are not referenced by a live add journal",
            );
        } else if !is_real_directory(&path)? {
            add_state_issue(
                issues,
                path,
                "journaled OverlayFS private layers must use a real directory",
            );
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn child_paths(directory: &Path, operation: &'static str) -> Result<Vec<PathBuf>, WorktreeError> {
    let mut paths = fs::read_dir(directory)
        .map_err(|source| io(operation, directory, source))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| io(operation, directory, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort_unstable();
    Ok(paths)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn add_state_issue(
    issues: &mut Vec<StateDiagnosticIssue>,
    path: PathBuf,
    reason: impl Into<String>,
) {
    issues.push(StateDiagnosticIssue {
        path,
        reason: reason.into(),
    });
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn is_real_directory(path: &Path) -> Result<bool, WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect Riftri state path", path, source))?;
    Ok(metadata.is_dir() && !metadata.file_type().is_symlink())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn is_real_directory_if_present(path: &Path) -> Result<bool, WorktreeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io("inspect Riftri state path", path, source)),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn is_regular_file(path: &Path) -> Result<bool, WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect Riftri state path", path, source))?;
    Ok(metadata.is_file() && !metadata.file_type().is_symlink())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn is_regular_file_if_present(path: &Path) -> Result<bool, WorktreeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io("inspect Riftri state path", path, source)),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn looks_like_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn retained_base_paths(state_directory: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
    let root = state_directory.join("bases/v1");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut bases = Vec::new();
    for repository_entry in
        fs::read_dir(&root).map_err(|source| io("read immutable-base root", &root, source))?
    {
        let repository_entry =
            repository_entry.map_err(|source| io("read immutable-base entry", &root, source))?;
        let repository_path = repository_entry.path();
        let metadata = fs::symlink_metadata(&repository_path).map_err(|source| {
            io(
                "inspect immutable-base repository",
                &repository_path,
                source,
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        for entry in fs::read_dir(&repository_path)
            .map_err(|source| io("read immutable-base repository", &repository_path, source))?
        {
            let entry = entry
                .map_err(|source| io("read immutable-base marker", &repository_path, source))?;
            let marker = entry.path();
            if marker.extension() == Some(OsStr::new("complete")) && marker.is_file() {
                bases.push(marker.with_extension(""));
            }
        }
    }
    bases.sort_unstable();
    bases.dedup();
    Ok(bases)
}

fn tree_usage(path: &Path) -> Result<(u64, u64), WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect storage accounting path", path, source))?;
    let mut logical_bytes = if metadata.is_dir() { 0 } else { metadata.len() };
    let mut allocated_bytes = allocated_bytes(path, &metadata)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path)
            .map_err(|source| io("read storage accounting directory", path, source))?
        {
            let entry =
                entry.map_err(|source| io("read storage accounting entry", path, source))?;
            let (entry_logical, entry_allocated) = tree_usage(&entry.path())?;
            logical_bytes = logical_bytes.saturating_add(entry_logical);
            allocated_bytes = allocated_bytes.saturating_add(entry_allocated);
        }
    }
    Ok((logical_bytes, allocated_bytes))
}

#[cfg(unix)]
fn allocated_bytes(_path: &Path, metadata: &fs::Metadata) -> Result<u64, WorktreeError> {
    use std::os::unix::fs::MetadataExt;

    Ok(metadata.blocks().saturating_mul(512))
}

#[cfg(target_os = "windows")]
fn allocated_bytes(path: &Path, metadata: &fs::Metadata) -> Result<u64, WorktreeError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GetLastError, SetLastError};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};

    if !metadata.is_file() {
        return Ok(0);
    }
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let mut high = 0_u32;
    // SAFETY: the path is NUL-terminated, `high` is writable, and clearing the
    // thread-local last error disambiguates a valid low word of `u32::MAX`.
    let low = unsafe {
        SetLastError(ERROR_SUCCESS);
        GetCompressedFileSizeW(wide.as_ptr(), &mut high)
    };
    if low == INVALID_FILE_SIZE {
        // SAFETY: this reads the calling thread's error value immediately after
        // `GetCompressedFileSizeW`.
        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            return Err(io(
                "measure filesystem allocation",
                path,
                std::io::Error::from_raw_os_error(error as i32),
            ));
        }
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn allocated_bytes(_path: &Path, metadata: &fs::Metadata) -> Result<u64, WorktreeError> {
    Ok(metadata.len())
}

pub fn recover_incomplete_operations(
    state_directory: &Path,
) -> Result<RecoveryReport, WorktreeError> {
    let state_directory = absolute_path(state_directory)?;
    let state_directory = if state_directory.exists() {
        fs::canonicalize(&state_directory)
            .map_err(|source| io("resolve state directory", &state_directory, source))?
    } else {
        state_directory
    };
    let store = JournalStore::open(&state_directory);
    let journals = store.load_all()?;
    let removal_store = RemovalJournalStore::open(&state_directory);
    let removal_journals = removal_store.load_all()?;
    let move_store = MoveJournalStore::open(&state_directory);
    let move_journals = move_store.load_all()?;
    let prune_store = PruneJournalStore::open(&state_directory);
    let prune_journals = prune_store.load_all()?;
    let collection_store = CollectionJournalStore::open(&state_directory);
    let collection_journals = collection_store.load_all()?;
    let completed_adds = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let mut report = RecoveryReport {
        scanned: journals
            .len()
            .saturating_add(removal_journals.len())
            .saturating_add(move_journals.len())
            .saturating_add(prune_journals.len())
            .saturating_add(collection_journals.len()),
        ..RecoveryReport::default()
    };
    let git = Git::default();

    for journal in journals {
        match journal.phase {
            AddWorktreePhase::Active => {
                if !completed_adds.contains(journal.operation_id.as_str()) {
                    report.active += 1;
                }
            }
            AddWorktreePhase::RolledBack => {}
            _ => {
                if let Err(error) =
                    validate_recovery_paths(&state_directory, &journal).and_then(|()| {
                        store.update_phase(
                            &journal.journal_path,
                            AddWorktreePhase::RollbackPending,
                        )?;
                        rollback_decoded(&git, &journal)?;
                        store.update_phase(&journal.journal_path, AddWorktreePhase::RolledBack)?;
                        Ok(())
                    })
                {
                    report
                        .errors
                        .push(format!("operation {}: {error}", journal.operation_id));
                } else {
                    report.recovered += 1;
                }
            }
        }
    }

    for journal in removal_journals {
        if journal.phase == RemoveWorktreePhase::Complete {
            report.completed_removals += 1;
            continue;
        }
        if let Err(error) = resume_removal(&git, &removal_store, journal.clone()) {
            report.errors.push(format!(
                "removal operation {}: {error}",
                journal.operation_id
            ));
        } else {
            report.recovered_removals += 1;
            report.completed_removals += 1;
            report.active = report.active.saturating_sub(1);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    for journal in move_journals {
        if journal.phase == MoveWorktreePhase::Complete {
            report.completed_moves += 1;
            continue;
        }
        if let Err(error) = resume_move(&git, &move_store, journal.clone(), None) {
            report
                .errors
                .push(format!("move operation {}: {error}", journal.operation_id));
        } else {
            report.recovered_moves += 1;
            report.completed_moves += 1;
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    for journal in prune_journals {
        if journal.phase == PruneWorktreesPhase::Complete {
            report.completed_prunes += 1;
            continue;
        }
        if let Err(error) = resume_prune(&git, &prune_store, journal.clone(), None) {
            report
                .errors
                .push(format!("prune operation {}: {error}", journal.operation_id));
        } else {
            report.recovered_prunes += 1;
            report.completed_prunes += 1;
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    for journal in collection_journals {
        match journal.phase {
            GarbageCollectionPhase::Complete => report.completed_collections += 1,
            GarbageCollectionPhase::Cancelled => {}
            _ => {
                match resume_decoded_collection(&state_directory, &collection_store, &journal, None)
                {
                    Ok(true) => {
                        report.recovered_collections += 1;
                        report.completed_collections += 1;
                    }
                    Ok(false) => {}
                    Err(error) => report.errors.push(format!(
                        "garbage-collection operation {}: {error}",
                        journal.operation_id
                    )),
                }
            }
        }
    }
    Ok(report)
}

fn validate_removal_paths(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases/v1");
    if !journal.repository.is_absolute()
        || !journal.destination.is_absolute()
        || !journal.base_path.is_absolute()
        || journal.base_path.parent().and_then(Path::parent) != Some(bases.as_path())
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "removal journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    let source = JournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .find(|candidate| candidate.operation_id == journal.source_add_operation_id)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "removal journal {} does not reference a known add operation",
                journal.journal_path.display()
            ))
        })?;
    if source.phase != AddWorktreePhase::Active
        || source.repository != journal.repository
        || source.destination != journal.destination
        || source.base_path != journal.base_path
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "removal journal {} does not match its active add operation",
            journal.journal_path.display()
        )));
    }
    Ok(())
}

fn resume_removal(
    git: &Git,
    store: &RemovalJournalStore,
    journal: DecodedRemovalJournal,
) -> Result<(), WorktreeError> {
    let state_directory = store
        .path_for(&journal.operation_id)
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "removal journal has no state directory: {}",
                journal.journal_path.display()
            ))
        })?
        .to_path_buf();
    validate_removal_paths(&state_directory, &journal)?;
    let file = File::open(&journal.journal_path)
        .map_err(|source| io("open removal journal", &journal.journal_path, source))?;
    let mut record: RemovalJournalRecord = serde_json::from_reader(file).map_err(|source| {
        WorktreeError::InvalidRequest(format!(
            "read removal journal {}: {source}",
            journal.journal_path.display()
        ))
    })?;

    let metadata_lock = if record.phase <= RemoveWorktreePhase::CleanVerified {
        Some(acquire_git_worktree_metadata_lock_for_repository(
            git,
            &journal.repository,
        )?)
    } else {
        None
    };

    if record.phase == RemoveWorktreePhase::IntentRecorded {
        verify_recoverable_removal(git, &journal)?;
        record.transition(RemoveWorktreePhase::CleanVerified)?;
        store.persist(&record)?;
    }

    if record.phase == RemoveWorktreePhase::CleanVerified {
        let (registered, destination_exists) = removal_presence(git, &journal)?;
        if registered {
            if destination_exists && !git.worktree_is_clean(&journal.destination)? {
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree {} has changes; recovery preserved it",
                    journal.destination.display()
                )));
            }
            git.remove_worktree(&journal.repository, &journal.destination)?;
        } else if destination_exists {
            return Err(WorktreeError::InvalidRequest(format!(
                "destination {} exists but is not registered by Git; recovery preserved it",
                journal.destination.display()
            )));
        }
        record.transition(RemoveWorktreePhase::WorktreeRemoved)?;
        store.persist(&record)?;
    }
    drop(metadata_lock);

    if record.phase >= RemoveWorktreePhase::WorktreeRemoved {
        let (registered, destination_exists) = removal_presence(git, &journal)?;
        if registered || destination_exists {
            return Err(WorktreeError::InvalidRequest(format!(
                "removal journal {} says the worktree was removed, but {} is still present or registered",
                journal.journal_path.display(),
                journal.destination.display()
            )));
        }
    }

    if record.phase == RemoveWorktreePhase::WorktreeRemoved {
        record.transition(RemoveWorktreePhase::BaseReleased)?;
        store.persist(&record)?;
    }
    if record.phase == RemoveWorktreePhase::BaseReleased {
        record.transition(RemoveWorktreePhase::Complete)?;
        store.persist(&record)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn resume_move(
    git: &Git,
    store: &MoveJournalStore,
    journal: DecodedMoveJournal,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "move")?;
    if store.path_for(&journal.operation_id) != journal.journal_path {
        return Err(WorktreeError::InvalidRequest(format!(
            "move journal {} has an operation ID that does not match its filename",
            journal.journal_path.display()
        )));
    }
    validate_move_paths(&state_directory, &journal)?;
    let file = File::open(&journal.journal_path)
        .map_err(|source| io("open move journal", &journal.journal_path, source))?;
    let mut record: MoveJournalRecord = serde_json::from_reader(file).map_err(|source| {
        WorktreeError::InvalidRequest(format!(
            "read move journal {}: {source}",
            journal.journal_path.display()
        ))
    })?;

    if record.phase == MoveWorktreePhase::IntentRecorded {
        let metadata_lock =
            acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
        let (source_registered, destination_registered) = move_registration(git, &journal)?;
        let source_exists = journal.source.exists();
        let destination_exists = journal.destination.exists();
        if source_registered && source_exists && !destination_registered && !destination_exists {
            git.move_worktree(&journal.repository, &journal.source, &journal.destination)?;
        } else if !source_registered
            && !source_exists
            && destination_registered
            && destination_exists
        {
            // Git completed the move before the phase update reached disk.
        } else {
            return Err(WorktreeError::InvalidRequest(format!(
                "move journal {} does not match a safe source/destination state; both paths were preserved",
                journal.journal_path.display()
            )));
        }
        advance_move(
            store,
            &mut record,
            MoveWorktreePhase::WorktreeMoved,
            fail_after,
        )?;
        drop(metadata_lock);
    }

    if record.phase >= MoveWorktreePhase::WorktreeMoved {
        let (source_registered, destination_registered) = move_registration(git, &journal)?;
        if source_registered
            || journal.source.exists()
            || !destination_registered
            || !journal.destination.is_dir()
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "move journal {} says Git moved the worktree, but its paths or registration disagree; both paths were preserved",
                journal.journal_path.display()
            )));
        }
    }

    if record.phase == MoveWorktreePhase::WorktreeMoved {
        let add_journal = JournalStore::open(&state_directory)
            .load_all()?
            .into_iter()
            .find(|candidate| candidate.operation_id == journal.source_add_operation_id)
            .ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "move journal {} does not reference a known add operation",
                    journal.journal_path.display()
                ))
            })?;
        JournalStore::open(&state_directory).update_active_destination(
            &add_journal.journal_path,
            &journal.source,
            &journal.destination,
        )?;
        advance_move(
            store,
            &mut record,
            MoveWorktreePhase::AddJournalUpdated,
            fail_after,
        )?;
    }
    if record.phase == MoveWorktreePhase::AddJournalUpdated {
        validate_move_paths(&state_directory, &journal)?;
        advance_move(store, &mut record, MoveWorktreePhase::Complete, fail_after)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_move_paths(
    state_directory: &Path,
    journal: &DecodedMoveJournal,
) -> Result<(), WorktreeError> {
    if !journal.repository.is_absolute()
        || !journal.source.is_absolute()
        || !journal.destination.is_absolute()
        || journal.source == journal.destination
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "move journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    let source = JournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .find(|candidate| candidate.operation_id == journal.source_add_operation_id)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "move journal {} does not reference a known add operation",
                journal.journal_path.display()
            ))
        })?;
    if source.phase != AddWorktreePhase::Active
        || source.repository != journal.repository
        || (source.destination != journal.source && source.destination != journal.destination)
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "move journal {} does not match its active add operation",
            journal.journal_path.display()
        )));
    }
    validate_recovery_paths(state_directory, &source)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn move_registration(
    git: &Git,
    journal: &DecodedMoveJournal,
) -> Result<(bool, bool), WorktreeError> {
    let inventory = git.list_worktrees(&journal.repository)?;
    Ok((
        inventory
            .iter()
            .any(|worktree| paths_match(&worktree.path, &journal.source)),
        inventory
            .iter()
            .any(|worktree| paths_match(&worktree.path, &journal.destination)),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn resume_prune(
    git: &Git,
    store: &PruneJournalStore,
    journal: DecodedPruneJournal,
    fail_after: Option<PruneWorktreesPhase>,
) -> Result<(), WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "prune")?;
    if store.path_for(&journal.operation_id) != journal.journal_path {
        return Err(WorktreeError::InvalidRequest(format!(
            "prune journal {} has an operation ID that does not match its filename",
            journal.journal_path.display()
        )));
    }
    if !journal.repository.is_absolute() {
        return Err(WorktreeError::InvalidRequest(format!(
            "prune journal {} contains a relative repository path",
            journal.journal_path.display()
        )));
    }
    let file = File::open(&journal.journal_path)
        .map_err(|source| io("open prune journal", &journal.journal_path, source))?;
    let mut record: PruneJournalRecord = serde_json::from_reader(file).map_err(|source| {
        WorktreeError::InvalidRequest(format!(
            "read prune journal {}: {source}",
            journal.journal_path.display()
        ))
    })?;
    if record.phase == PruneWorktreesPhase::IntentRecorded {
        let metadata_lock =
            acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
        verify_repository_prune_safe(
            git,
            &state_directory,
            &journal.repository,
            Some(&journal.operation_id),
        )?;
        git.prune_worktrees(&journal.repository)?;
        advance_prune(
            store,
            &mut record,
            PruneWorktreesPhase::GitMetadataPruned,
            fail_after,
        )?;
        drop(metadata_lock);
    }
    if record.phase == PruneWorktreesPhase::GitMetadataPruned {
        verify_repository_prune_safe(
            git,
            &state_directory,
            &journal.repository,
            Some(&journal.operation_id),
        )?;
        advance_prune(
            store,
            &mut record,
            PruneWorktreesPhase::Complete,
            fail_after,
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_repository_prune_safe(
    git: &Git,
    current_state_directory: &Path,
    repository: &Path,
    current_prune: Option<&str>,
) -> Result<(), WorktreeError> {
    let repository_info = git.inspect_repository(repository)?;
    let mut state_directories = repository_state_directories_with_git(git, &repository_info)?;
    if !state_directories
        .iter()
        .any(|state_directory| state_directory == current_state_directory)
    {
        state_directories.push(current_state_directory.to_path_buf());
    }
    for state_directory in state_directories {
        verify_prune_safe(
            git,
            &state_directory,
            repository,
            if state_directory == current_state_directory {
                current_prune
            } else {
                None
            },
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_prune_safe(
    git: &Git,
    state_directory: &Path,
    repository: &Path,
    current_prune: Option<&str>,
) -> Result<(), WorktreeError> {
    let repository_info = git.inspect_repository(repository)?;
    let repository_root = repository_info.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("prune journal references a bare repository".to_owned())
    })?;
    if repository_root != repository {
        return Err(WorktreeError::InvalidRequest(format!(
            "prune journal repository {} does not match Git's worktree root {}",
            repository.display(),
            repository_root.display()
        )));
    }
    let adds = JournalStore::open(state_directory).load_all()?;
    if adds.iter().any(|journal| {
        !matches!(
            journal.phase,
            AddWorktreePhase::Active | AddWorktreePhase::RolledBack
        )
    }) || RemovalJournalStore::open(state_directory)
        .load_all()?
        .iter()
        .any(|journal| journal.phase != RemoveWorktreePhase::Complete)
        || MoveJournalStore::open(state_directory)
            .load_all()?
            .iter()
            .any(|journal| journal.phase != MoveWorktreePhase::Complete)
        || PruneJournalStore::open(state_directory)
            .load_all()?
            .iter()
            .any(|journal| {
                journal.phase != PruneWorktreesPhase::Complete
                    && Some(journal.operation_id.as_str()) != current_prune
            })
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "another Riftri lifecycle operation is pending; run `riftri repair --state-dir {}` first",
            state_directory.display()
        )));
    }
    let completed_removals = RemovalJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id)
        .collect::<HashSet<_>>();
    let inventory = git.list_worktrees(repository)?;
    for journal in adds.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active
            && !completed_removals.contains(&journal.operation_id)
    }) {
        if !journal.destination.is_dir()
            || !inventory
                .iter()
                .any(|worktree| paths_match(&worktree.path, &journal.destination))
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "managed worktree {} is missing or not registered; prune was not run",
                journal.destination.display()
            )));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn lifecycle_state_directory(
    journal_path: &Path,
    operation: &str,
) -> Result<PathBuf, WorktreeError> {
    journal_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "{operation} journal has no state directory: {}",
                journal_path.display()
            ))
        })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn advance_move(
    store: &MoveJournalStore,
    record: &mut MoveJournalRecord,
    phase: MoveWorktreePhase,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    record.transition(phase)?;
    store.persist(record)?;
    fail_move_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_move_if_requested(
    phase: MoveWorktreePhase,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        Err(WorktreeError::InjectedMoveFailure(phase))
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn advance_prune(
    store: &PruneJournalStore,
    record: &mut PruneJournalRecord,
    phase: PruneWorktreesPhase,
    fail_after: Option<PruneWorktreesPhase>,
) -> Result<(), WorktreeError> {
    record.transition(phase)?;
    store.persist(record)?;
    fail_prune_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_prune_if_requested(
    phase: PruneWorktreesPhase,
    fail_after: Option<PruneWorktreesPhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        Err(WorktreeError::InjectedPruneFailure(phase))
    } else {
        Ok(())
    }
}

fn verify_recoverable_removal(
    git: &Git,
    journal: &DecodedRemovalJournal,
) -> Result<(), WorktreeError> {
    let (registered, destination_exists) = removal_presence(git, journal)?;
    if registered && destination_exists && !git.worktree_is_clean(&journal.destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} has changes; recovery preserved it",
            journal.destination.display()
        )));
    }
    if !registered && destination_exists {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} exists but is not registered by Git; recovery preserved it",
            journal.destination.display()
        )));
    }
    Ok(())
}

fn removal_presence(
    git: &Git,
    journal: &DecodedRemovalJournal,
) -> Result<(bool, bool), WorktreeError> {
    let registered = git
        .list_worktrees(&journal.repository)?
        .into_iter()
        .any(|worktree| paths_match(&worktree.path, &journal.destination));
    Ok((registered, journal.destination.exists()))
}

fn validate_recovery_paths(
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases/v1");
    let temporary = state_directory.join("tmp");
    let base_repository = journal.base_path.parent();
    let overlay_paths_valid = match (&journal.backend, &journal.overlayfs) {
        (BackendKind::OverlayFs, Some(overlayfs)) => {
            let overlay_root = state_directory.join("overlays/v1");
            let identity_valid = overlayfs.mount_identity.as_ref().is_none_or(|identity| {
                !identity.boot_id.is_empty()
                    && identity.mount_namespace_inode != 0
                    && identity.mount_id != 0
            });
            overlayfs.layout_root.parent() == Some(overlay_root.as_path())
                && overlayfs.layout_root.file_name() == Some(OsStr::new(&journal.operation_id))
                && overlayfs.recovery_token.len() == 64
                && identity_valid
        }
        (BackendKind::OverlayFs, None) => false,
        (_, None) => true,
        (_, Some(_)) => false,
    };
    if base_repository != journal.base_staging.parent()
        || base_repository.and_then(Path::parent) != Some(bases.as_path())
        || !journal
            .base_staging
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".riftri-build-"))
        || journal.temporary_index.parent() != Some(temporary.as_path())
        || (journal.phase != AddWorktreePhase::Active
            && journal.scratch.parent() != journal.destination.parent())
        || !journal
            .scratch
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".riftri-view-"))
        || !journal.repository.is_absolute()
        || !journal.destination.is_absolute()
        || !overlay_paths_valid
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    Ok(())
}

fn rollback_decoded(git: &Git, journal: &DecodedJournal) -> Result<(), WorktreeError> {
    let metadata_lock =
        acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
    let expected_branch_target =
        if journal.last_forward_phase >= AddWorktreePhase::GitMetadataCreated {
            journal
                .branch
                .as_ref()
                .map(|branch| {
                    let expected = ObjectId::parse(journal.expected_commit.clone())?;
                    let current = git.local_branch_target(&journal.repository, branch)?;
                    if current.as_ref().is_some_and(|current| current != &expected) {
                        return Err(WorktreeError::InvalidRequest(format!(
                            "branch {} moved after creation; recovery preserved its worktree",
                            branch.to_string_lossy()
                        )));
                    }
                    Ok((branch, current))
                })
                .transpose()?
        } else {
            None
        };

    let registered = git
        .list_worktrees(&journal.repository)?
        .into_iter()
        .any(|worktree| paths_match(&worktree.path, &journal.destination));

    if registered {
        restore_pointer_for_rollback(journal)?;
        if journal.destination.exists() {
            let safe_to_remove = contains_only_git_pointer(&journal.destination)?
                || git.worktree_is_clean(&journal.destination)?
                || view_matches_base(&journal.base_path, &journal.destination)?;
            if safe_to_remove {
                git.remove_worktree_force(&journal.repository, &journal.destination)?;
            } else {
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree {} has changes; recovery preserved it",
                    journal.destination.display()
                )));
            }
        } else {
            git.remove_worktree_force(&journal.repository, &journal.destination)?;
        }
        remove_empty_directory_if_present(&journal.destination)?;
    } else if journal.destination.exists() {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} exists but is not registered by Git; recovery preserved it",
            journal.destination.display()
        )));
    }

    remove_tree_if_present(&journal.scratch)?;
    remove_tree_if_present(&journal.base_staging)?;
    remove_file_if_present(&journal.temporary_index)?;

    if let Some((branch, Some(_))) = expected_branch_target {
        git.delete_branch_force(&journal.repository, branch)?;
    }
    drop(metadata_lock);
    Ok(())
}

fn restore_pointer_for_rollback(journal: &DecodedJournal) -> Result<(), WorktreeError> {
    if journal.destination.join(".git").is_file() {
        return Ok(());
    }
    let scratch_pointer = journal.scratch.join(".git");
    if !scratch_pointer.is_file() {
        return Ok(());
    }
    if !journal.destination.exists() {
        fs::rename(&journal.scratch, &journal.destination).map_err(|source| {
            io(
                "restore interrupted worktree view",
                &journal.destination,
                source,
            )
        })?;
        return Ok(());
    }
    if fs::read_dir(&journal.destination)
        .map_err(|source| {
            io(
                "inspect interrupted destination",
                &journal.destination,
                source,
            )
        })?
        .next()
        .is_some()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} is not empty; recovery preserved it",
            journal.destination.display()
        )));
    }
    fs::rename(&scratch_pointer, journal.destination.join(".git")).map_err(|source| {
        io(
            "restore linked-worktree pointer",
            &journal.destination,
            source,
        )
    })?;
    Ok(())
}

fn contains_only_git_pointer(path: &Path) -> Result<bool, WorktreeError> {
    let mut entries = fs::read_dir(path)
        .map_err(|source| io("inspect linked worktree", path, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io("read linked-worktree entry", path, source))?;
    Ok(entries.len() == 1
        && entries
            .pop()
            .is_some_and(|entry| entry.file_name() == OsStr::new(".git") && entry.path().is_file()))
}

fn view_matches_base(base: &Path, view: &Path) -> Result<bool, WorktreeError> {
    if !base.is_dir() || !view.is_dir() {
        return Ok(false);
    }
    compare_directories(base, view, true)
}

fn compare_directories(base: &Path, view: &Path, root: bool) -> Result<bool, WorktreeError> {
    let mut base_entries = directory_entries(base, false)?;
    let mut view_entries = directory_entries(view, root)?;
    base_entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    view_entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if base_entries.len() != view_entries.len() {
        return Ok(false);
    }

    for ((base_name, base_path), (view_name, view_path)) in
        base_entries.into_iter().zip(view_entries)
    {
        if base_name != view_name {
            return Ok(false);
        }
        let base_metadata = fs::symlink_metadata(&base_path)
            .map_err(|source| io("inspect immutable-base entry", &base_path, source))?;
        let view_metadata = fs::symlink_metadata(&view_path)
            .map_err(|source| io("inspect worktree entry", &view_path, source))?;
        let base_type = base_metadata.file_type();
        let view_type = view_metadata.file_type();
        if base_type.is_dir() != view_type.is_dir()
            || base_type.is_file() != view_type.is_file()
            || base_type.is_symlink() != view_type.is_symlink()
        {
            return Ok(false);
        }
        if base_type.is_dir() {
            if !compare_directories(&base_path, &view_path, false)? {
                return Ok(false);
            }
        } else if base_type.is_file() {
            if base_metadata.len() != view_metadata.len()
                || !same_executable_mode(&base_metadata, &view_metadata)
                || !files_equal(&base_path, &view_path)?
            {
                return Ok(false);
            }
        } else if base_type.is_symlink() {
            let base_target = fs::read_link(&base_path)
                .map_err(|source| io("read immutable-base symlink", &base_path, source))?;
            let view_target = fs::read_link(&view_path)
                .map_err(|source| io("read worktree symlink", &view_path, source))?;
            if base_target != view_target {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn directory_entries(
    path: &Path,
    ignore_root_git_pointer: bool,
) -> Result<Vec<(OsString, PathBuf)>, WorktreeError> {
    fs::read_dir(path)
        .map_err(|source| io("read directory for rollback comparison", path, source))?
        .filter_map(|entry| match entry {
            Ok(entry) if ignore_root_git_pointer && entry.file_name() == OsStr::new(".git") => None,
            Ok(entry) => Some(Ok((entry.file_name(), entry.path()))),
            Err(source) => Some(Err(io("read rollback comparison entry", path, source))),
        })
        .collect()
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, WorktreeError> {
    use std::io::{BufReader, Read};

    let mut left = BufReader::new(
        File::open(left).map_err(|source| io("open immutable-base file", left, source))?,
    );
    let mut right = BufReader::new(
        File::open(right).map_err(|source| io("open worktree file", right, source))?,
    );
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_length = left
            .read(&mut left_buffer)
            .map_err(|source| WorktreeError::InvalidRequest(format!("read base file: {source}")))?;
        let right_length = right.read(&mut right_buffer).map_err(|source| {
            WorktreeError::InvalidRequest(format!("read worktree file: {source}"))
        })?;
        if left_length != right_length || left_buffer[..left_length] != right_buffer[..right_length]
        {
            return Ok(false);
        }
        if left_length == 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn same_executable_mode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    left.permissions().mode() & 0o111 == right.permissions().mode() & 0o111
}

#[cfg(not(unix))]
fn same_executable_mode(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

fn remove_tree_if_present(path: &Path) -> Result<(), WorktreeError> {
    let Some(metadata) = fs::symlink_metadata(path)
        .map(Some)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|source| io("inspect rollback directory", path, source))?
    else {
        return Ok(());
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(WorktreeError::InvalidRequest(format!(
            "rollback path {} is not a real directory",
            path.display()
        )));
    }
    NativeCowCloner::make_tree_owner_writable(path)?;
    fs::remove_dir_all(path).map_err(|source| io("remove rollback directory", path, source))
}

fn remove_file_if_present(path: &Path) -> Result<(), WorktreeError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io("remove temporary file", path, source)),
    }
}

fn remove_empty_directory_if_present(path: &Path) -> Result<(), WorktreeError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io("remove empty linked-worktree directory", path, source)),
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), WorktreeError> {
    let parent = path.expect_parent()?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io("sync parent directory", parent, source))
}

#[cfg(target_os = "windows")]
fn sync_parent(path: &Path) -> Result<(), WorktreeError> {
    path.expect_parent().map(|_| ())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
trait PathExt {
    fn expect_parent(&self) -> Result<&Path, WorktreeError>;
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl PathExt for Path {
    fn expect_parent(&self) -> Result<&Path, WorktreeError> {
        self.parent().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!("path has no parent: {}", self.display()))
        })
    }
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> WorktreeError {
    WorktreeError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

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
mod tests {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{
        AddWorktreeRequest, MoveWorktreeRequest, PruneWorktreesRequest, RemoveWorktreeRequest,
        WorktreeMode, add_worktree_inner, garbage_collect_inner, move_worktree_inner,
        next_operation_id, prune_worktrees_inner, recover_incomplete_operations,
        remove_worktree_inner, storage_accounting,
    };
    use crate::test_support::writable_tempdir as tempdir;
    use crate::{
        AddWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase, PruneWorktreesPhase,
        RemoveWorktreePhase,
    };

    fn git(path: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn matches_git_and_verbatim_windows_worktree_paths() {
        assert!(super::paths_match(
            Path::new(r"R:\repo\view"),
            Path::new(r"\\?\r:/repo/view")
        ));
        assert!(super::paths_match(
            Path::new(r"\\server\share\view"),
            Path::new(r"\\?\UNC\SERVER\SHARE\VIEW")
        ));
    }

    #[test]
    fn concurrent_operation_ids_are_unique_at_the_same_timestamp() {
        const WORKERS: usize = 64;
        let start = Arc::new(Barrier::new(WORKERS + 1));
        let handles = (0..WORKERS)
            .map(|_| {
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    next_operation_id(0)
                })
            })
            .collect::<Vec<_>>();

        start.wait();
        let ids = handles
            .into_iter()
            .map(|handle| handle.join().expect("operation ID thread"))
            .collect::<HashSet<_>>();

        assert_eq!(ids.len(), WORKERS);
    }

    #[test]
    fn status_reports_unexplained_state_without_removing_it() {
        let fixture = tempdir().expect("fixture");
        let state = fixture.path().join("state");
        let empty_bucket = state.join("bases/v1/empty-bucket");
        let stray_journal = state.join("operations/stray.tmp");
        let stray_temporary = state.join("tmp/orphan-index");
        let stray_overlay = state.join("overlays/v1/orphan-view");
        let unknown_root = state.join("unknown-root");
        fs::create_dir_all(&empty_bucket).expect("create empty base bucket");
        fs::create_dir_all(state.join("operations")).expect("create journal directory");
        fs::create_dir_all(state.join("tmp")).expect("create temporary directory");
        fs::create_dir_all(&stray_overlay).expect("create stray OverlayFS layers");
        fs::write(&stray_journal, "unfinished\n").expect("write stray journal");
        fs::write(&stray_temporary, "temporary\n").expect("write stray temporary file");
        fs::create_dir(&unknown_root).expect("create unknown state directory");
        let empty_bucket = empty_bucket.canonicalize().expect("resolve empty bucket");
        let stray_journal = stray_journal.canonicalize().expect("resolve stray journal");
        let stray_temporary = stray_temporary
            .canonicalize()
            .expect("resolve stray temporary file");
        let stray_overlay = stray_overlay
            .canonicalize()
            .expect("resolve stray OverlayFS layers");
        let unknown_root = unknown_root.canonicalize().expect("resolve unknown root");

        let report = storage_accounting(&state).expect("diagnose state");
        let paths = report
            .diagnostic_issues
            .iter()
            .map(|issue| issue.path.as_path())
            .collect::<HashSet<_>>();

        assert_eq!(report.diagnostic_issues.len(), 5);
        assert!(paths.contains(empty_bucket.as_path()));
        assert!(paths.contains(stray_journal.as_path()));
        assert!(paths.contains(stray_temporary.as_path()));
        assert!(paths.contains(stray_overlay.as_path()));
        assert!(paths.contains(unknown_root.as_path()));
        assert!(empty_bucket.is_dir());
        assert!(stray_journal.is_file());
        assert!(stray_temporary.is_file());
        assert!(stray_overlay.is_dir());
        assert!(unknown_root.is_dir());
    }

    #[test]
    fn rolls_back_an_interruption_after_view_activation() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/interrupted")),
                state_dir: Some(state),
            },
            Some(AddWorktreePhase::GitPointerRestored),
            true,
        )
        .expect_err("failure should be injected");

        assert!(
            error.to_string().contains("injected failure"),
            "unexpected error: {error}"
        );
        assert!(!destination.exists(), "destination remains after: {error}");
        let branch = Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/feature/interrupted",
            ])
            .current_dir(&repository)
            .status()
            .expect("check branch");
        assert!(!branch.success());
    }

    #[test]
    fn recovery_rolls_back_a_durable_interrupted_operation() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/recover")),
                state_dir: Some(state.clone()),
            },
            Some(AddWorktreePhase::GitPointerRestored),
            false,
        )
        .expect_err("simulate process termination");
        assert!(error.to_string().contains("injected failure"));
        assert!(destination.exists());

        let report = recover_incomplete_operations(&state).expect("recover operation");

        assert_eq!(report.recovered, 1);
        assert!(report.errors.is_empty());
        assert!(!destination.exists());
        let branch = Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/feature/recover",
            ])
            .current_dir(&repository)
            .status()
            .expect("check branch");
        assert!(!branch.success());
    }

    #[test]
    fn recovery_is_idempotent_after_every_add_transition() {
        let phases = [
            AddWorktreePhase::IntentRecorded,
            AddWorktreePhase::GitMetadataCreated,
            AddWorktreePhase::BaseReady,
            AddWorktreePhase::ViewCreated,
            AddWorktreePhase::GitPointerRestored,
            AddWorktreePhase::IndexSynchronized,
            AddWorktreePhase::CleanVerified,
        ];

        for (index, phase) in phases.into_iter().enumerate() {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let state = fixture.path().join("state");
            let branch_name = format!("feature/recover-phase-{index}");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);

            let error = add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(&branch_name)),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
                false,
            )
            .expect_err("simulate process termination");
            assert!(
                error.to_string().contains("injected failure"),
                "unexpected failure after {phase:?}: {error}"
            );

            let recovered = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("recover after {phase:?}: {error}"));
            assert_eq!(recovered.recovered, 1, "phase {phase:?}");
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert!(!destination.exists(), "phase {phase:?}");

            let branch = Command::new("git")
                .args(["show-ref", "--verify", "--quiet"])
                .arg(format!("refs/heads/{branch_name}"))
                .current_dir(&repository)
                .status()
                .expect("check branch");
            assert!(!branch.success(), "phase {phase:?}");

            let repeated = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("repeat recovery after {phase:?}: {error}"));
            assert_eq!(repeated.recovered, 0, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");
        }
    }

    #[test]
    fn recovery_preserves_a_changed_interrupted_view() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/preserve")),
                state_dir: Some(state.clone()),
            },
            Some(AddWorktreePhase::GitPointerRestored),
            false,
        )
        .expect_err("simulate process termination");
        fs::write(destination.join("tracked.txt"), "user change\n").expect("change view");

        let report = recover_incomplete_operations(&state).expect("attempt recovery");

        assert_eq!(report.recovered, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(destination.exists());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved file"),
            "user change\n"
        );
    }

    #[test]
    fn recovery_preserves_an_interrupted_view_whose_branch_moved() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/committed")),
                state_dir: Some(state.clone()),
            },
            Some(AddWorktreePhase::IndexSynchronized),
            false,
        )
        .expect_err("simulate process termination");
        fs::write(destination.join("tracked.txt"), "committed change\n").expect("change view");
        git(&destination, &["add", "--", "tracked.txt"]);
        git(&destination, &["commit", "--quiet", "-m", "preserve me"]);

        let report = recover_incomplete_operations(&state).expect("attempt recovery");

        assert_eq!(report.recovered, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("branch"));
        assert!(destination.exists());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved file"),
            "committed change\n"
        );
    }

    #[test]
    fn recovery_completes_an_interrupted_clean_removal_idempotently() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/remove-recover")),
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create worktree");

        let error = remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("simulate interruption after removal intent");
        assert!(error.to_string().contains("injected removal failure"));
        assert!(destination.is_dir());

        let retry = remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("a pending removal must be recovered instead of duplicated");
        assert!(retry.to_string().contains("already pending"));

        let recovered = recover_incomplete_operations(&state).expect("recover removal");
        assert_eq!(recovered.recovered_removals, 1);
        assert_eq!(recovered.completed_removals, 1);
        assert!(recovered.errors.is_empty());
        assert!(!destination.exists());
        let accounting = storage_accounting(&state).expect("account after recovery");
        assert_eq!(accounting.active_views, 0);
        assert_eq!(accounting.bases[0].reference_count, 0);

        let repeated = recover_incomplete_operations(&state).expect("repeat recovery");
        assert_eq!(repeated.recovered_removals, 0);
        assert_eq!(repeated.completed_removals, 1);
        assert!(repeated.errors.is_empty());
    }

    #[test]
    fn recovery_is_safe_and_idempotent_after_every_removal_transition() {
        let phases = [
            RemoveWorktreePhase::IntentRecorded,
            RemoveWorktreePhase::CleanVerified,
            RemoveWorktreePhase::WorktreeRemoved,
            RemoveWorktreePhase::BaseReleased,
            RemoveWorktreePhase::Complete,
        ];

        for (index, phase) in phases.into_iter().enumerate() {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!(
                        "feature/remove-phase-{index}"
                    ))),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create worktree");

            let error = remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("simulate interruption after removal transition");
            assert!(
                error.to_string().contains("injected removal failure"),
                "unexpected {phase:?} error: {error}"
            );
            assert_eq!(
                destination.exists(),
                phase <= RemoveWorktreePhase::CleanVerified,
                "unexpected destination state after {phase:?}"
            );

            let recovered = recover_incomplete_operations(&state).expect("recover removal");
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert_eq!(
                recovered.recovered_removals,
                usize::from(phase != RemoveWorktreePhase::Complete),
                "phase {phase:?}"
            );
            assert_eq!(recovered.completed_removals, 1, "phase {phase:?}");
            assert!(!destination.exists(), "phase {phase:?}");

            let accounting = storage_accounting(&state).expect("account after recovery");
            assert_eq!(accounting.active_views, 0, "phase {phase:?}");
            assert_eq!(accounting.pending_removals, 0, "phase {phase:?}");
            assert_eq!(accounting.completed_removals, 1, "phase {phase:?}");
            assert_eq!(accounting.bases.len(), 1, "phase {phase:?}");
            assert_eq!(accounting.bases[0].reference_count, 0, "phase {phase:?}");

            let repeated = recover_incomplete_operations(&state).expect("repeat recovery");
            assert_eq!(repeated.recovered_removals, 0, "phase {phase:?}");
            assert_eq!(repeated.completed_removals, 1, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");
        }
    }

    #[test]
    fn recovery_preserves_a_worktree_changed_after_removal_intent() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/remove-preserve")),
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("simulate interruption after removal intent");
        fs::write(destination.join("tracked.txt"), "changed after intent\n")
            .expect("change worktree");

        let report = recover_incomplete_operations(&state).expect("attempt removal recovery");
        assert_eq!(report.recovered_removals, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("changes"));
        assert!(destination.is_dir());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved change"),
            "changed after intent\n"
        );
    }

    #[test]
    fn recovery_is_idempotent_after_every_collection_transition() {
        let phases = [
            GarbageCollectionPhase::IntentRecorded,
            GarbageCollectionPhase::BaseQuarantined,
            GarbageCollectionPhase::MarkerRemoved,
            GarbageCollectionPhase::Complete,
        ];

        for (index, phase) in phases.into_iter().enumerate() {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            let added = add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!(
                        "feature/gc-phase-{index}"
                    ))),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create worktree");
            remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository,
                    destination,
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .expect("remove worktree");

            let error = garbage_collect_inner(&state, true, Some(phase))
                .expect_err("simulate collection interruption");
            assert!(
                error
                    .to_string()
                    .contains("injected garbage-collection failure"),
                "unexpected {phase:?} error: {error}"
            );

            let recovered = recover_incomplete_operations(&state).expect("recover collection");
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert_eq!(
                recovered.recovered_collections,
                usize::from(phase != GarbageCollectionPhase::Complete),
                "phase {phase:?}"
            );
            assert_eq!(recovered.completed_collections, 1, "phase {phase:?}");
            assert!(!added.base_path.exists(), "phase {phase:?}");
            assert!(
                !added.base_path.with_extension("complete").exists(),
                "phase {phase:?}"
            );

            let accounting = storage_accounting(&state).expect("account collection");
            assert!(accounting.bases.is_empty(), "phase {phase:?}");
            assert_eq!(accounting.pending_collections, 0, "phase {phase:?}");
            assert_eq!(accounting.completed_collections, 1, "phase {phase:?}");

            let repeated = recover_incomplete_operations(&state).expect("repeat recovery");
            assert_eq!(repeated.recovered_collections, 0, "phase {phase:?}");
            assert_eq!(repeated.completed_collections, 1, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");
        }
    }

    #[test]
    fn garbage_collection_preserves_a_base_referenced_by_an_incomplete_add() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        add_worktree_inner(
            AddWorktreeRequest {
                repository,
                destination,
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/gc-incomplete")),
                state_dir: Some(state.clone()),
            },
            Some(AddWorktreePhase::BaseReady),
            false,
        )
        .expect_err("leave an incomplete add after base creation");

        let plan = garbage_collect_inner(&state, false, None).expect("plan collection");
        assert!(plan.candidates.is_empty());
        let accounting = storage_accounting(&state).expect("account incomplete add");
        assert_eq!(accounting.bases.len(), 1);

        let recovery = recover_incomplete_operations(&state).expect("recover incomplete add");
        assert_eq!(recovery.recovered, 1);
        assert!(recovery.errors.is_empty());
        let after = garbage_collect_inner(&state, false, None).expect("plan recovered collection");
        assert_eq!(after.candidates.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn garbage_collection_rejects_a_symlinked_completion_marker() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        let protected_file = fixture.path().join("do-not-delete");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        fs::write(&protected_file, "preserve\n").expect("write protected file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        let added = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/gc-marker")),
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove worktree");
        let marker = added.base_path.with_extension("complete");
        fs::remove_file(&marker).expect("remove real marker");
        symlink(&protected_file, &marker).expect("replace marker with symlink");

        let error = garbage_collect_inner(&state, true, None)
            .expect_err("symlinked marker must stop collection");
        assert!(error.to_string().contains("not a real file"));
        assert!(added.base_path.is_dir());
        assert_eq!(
            fs::read_to_string(&protected_file).expect("read protected file"),
            "preserve\n"
        );
    }

    #[test]
    fn recovery_is_idempotent_after_every_move_transition() {
        let phases = [
            MoveWorktreePhase::IntentRecorded,
            MoveWorktreePhase::WorktreeMoved,
            MoveWorktreePhase::AddJournalUpdated,
            MoveWorktreePhase::Complete,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let source = fixture.path().join("source");
            let destination = fixture.path().join("destination");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: source.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!(
                        "feature/move-phase-{index}"
                    ))),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create worktree");
            fs::write(source.join("private.txt"), "preserve\n").expect("write private file");

            let error = move_worktree_inner(
                MoveWorktreeRequest {
                    repository: repository.clone(),
                    source: source.clone(),
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("simulate move interruption");
            assert!(error.to_string().contains("injected move failure"));

            let recovered = recover_incomplete_operations(&state).expect("recover move");
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert_eq!(
                recovered.recovered_moves,
                usize::from(phase != MoveWorktreePhase::Complete),
                "phase {phase:?}"
            );
            assert!(!source.exists(), "phase {phase:?}");
            assert_eq!(
                fs::read_to_string(destination.join("private.txt")).expect("read private file"),
                "preserve\n",
                "phase {phase:?}"
            );
            let accounting = storage_accounting(&state).expect("account move");
            assert_eq!(accounting.completed_moves, 1, "phase {phase:?}");
            assert_eq!(accounting.pending_moves, 0, "phase {phase:?}");
            assert_eq!(
                accounting.views[0].destination,
                destination.canonicalize().expect("resolve moved worktree"),
                "phase {phase:?}"
            );

            let repeated = recover_incomplete_operations(&state).expect("repeat recovery");
            assert_eq!(repeated.recovered_moves, 0, "phase {phase:?}");
            assert_eq!(repeated.completed_moves, 1, "phase {phase:?}");
        }
    }

    #[test]
    fn recovery_is_idempotent_after_every_prune_transition() {
        let phases = [
            PruneWorktreesPhase::IntentRecorded,
            PruneWorktreesPhase::GitMetadataPruned,
            PruneWorktreesPhase::Complete,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let managed = fixture.path().join("managed");
            let stale = fixture.path().join("stale");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: managed.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!(
                        "feature/prune-managed-{index}"
                    ))),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create managed worktree");
            let stale_text = stale.to_string_lossy().into_owned();
            git(
                &repository,
                &["worktree", "add", "--detach", stale_text.as_str(), "HEAD"],
            );
            fs::remove_dir_all(&stale).expect("remove unmanaged worktree directory");

            let error = prune_worktrees_inner(
                PruneWorktreesRequest {
                    repository: repository.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("simulate prune interruption");
            assert!(error.to_string().contains("injected prune failure"));

            let recovered = recover_incomplete_operations(&state).expect("recover prune");
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert_eq!(
                recovered.recovered_prunes,
                usize::from(phase != PruneWorktreesPhase::Complete),
                "phase {phase:?}"
            );
            assert!(managed.is_dir(), "phase {phase:?}");
            let inventory = riftri_git::Git::default()
                .list_worktrees(&repository)
                .expect("list worktrees");
            assert!(
                !inventory.iter().any(|worktree| worktree.path == stale),
                "phase {phase:?}"
            );
            let accounting = storage_accounting(&state).expect("account prune");
            assert_eq!(accounting.completed_prunes, 1, "phase {phase:?}");
            assert_eq!(accounting.pending_prunes, 0, "phase {phase:?}");

            let repeated = recover_incomplete_operations(&state).expect("repeat recovery");
            assert_eq!(repeated.recovered_prunes, 0, "phase {phase:?}");
            assert_eq!(repeated.completed_prunes, 1, "phase {phase:?}");
        }
    }
}
