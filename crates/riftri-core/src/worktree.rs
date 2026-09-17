use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "windows")]
use std::os::windows::fs::OpenOptionsExt;

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
use riftri_git::{Git, GitAttribute, GitError, ObjectId, ResolvedRevision};
#[cfg(target_os = "macos")]
use riftri_storage::ApfsCloner as NativeCowCloner;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use riftri_storage::ApfsCloner as NativeCowCloner;
#[cfg(target_os = "linux")]
use riftri_storage::OverlayFsMounter;
#[cfg(target_os = "linux")]
use riftri_storage::ReflinkCloner as NativeCowCloner;
#[cfg(target_os = "windows")]
use riftri_storage::RefsBlockCloner as NativeCowCloner;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use riftri_storage::probe_backends;
use riftri_storage::{BackendKind, StorageError};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use riftri_storage::{CapabilityStatus, DestinationVolume};
#[cfg(target_os = "linux")]
use riftri_storage::{OverlayFsMountProfile, OverlayFsMountState, OverlayFsRecoveryState};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use sha2::{Digest, Sha256};
use thiserror::Error;
#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::journal::{
    CollectionJournalPaths, CollectionJournalRecord, CompactJournalPaths, CompactJournalRecord,
    JournalPaths, JournalRecord, MoveJournalPaths, MoveJournalRecord, PruneJournalRecord,
    RemovalJournalPaths, ensure_real_state_directory, require_real_state_directory,
};
use crate::journal::{
    CollectionJournalStore, CompactJournalStore, DecodedCollectionJournal, DecodedCompactJournal,
    DecodedJournal, DecodedMoveJournal, DecodedPruneJournal, DecodedRemovalJournal, JournalError,
    JournalStore, MoveJournalStore, PruneJournalStore, RemovalJournalRecord, RemovalJournalStore,
};
use crate::progress::{self, ProgressEvent};
use crate::{
    AddWorktreePhase, CompactJournalTransitionError, CompactWorktreePhase, GarbageCollectionPhase,
    JournalTransitionError, MoveJournalTransitionError, MoveWorktreePhase,
    PruneJournalTransitionError, PruneWorktreesPhase, RemoveJournalTransitionError,
    RemoveWorktreePhase, RepositoryCompatibilityBlocker, RepositoryCompatibilityBlockerKind,
    RepositoryCompatibilityReport,
};

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
static OPERATION_NONCE: AtomicU64 = AtomicU64::new(0);

const STATE_DIRECTORY_CONFIG_KEY: &str = "riftri.stateDirectory";

struct CompatibilityAnalysis {
    report: RepositoryCompatibilityReport,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    checkout_profile: Vec<u8>,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    checkout_paths: Vec<PathBuf>,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    checkout_config: Vec<(String, Vec<u8>)>,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    lfs_objects: Vec<GitLfsObject>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitLfsPointer {
    oid: String,
    size: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitLfsObject {
    checkout_path: PathBuf,
    source_path: PathBuf,
    pointer: GitLfsPointer,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct SelectedBackend {
    kind: BackendKind,
    volume: DestinationVolume,
    #[cfg(target_os = "linux")]
    overlayfs_profile: Option<OverlayFsMountProfile>,
}

pub(crate) struct DestinationBackendReadiness {
    pub kind: BackendKind,
    pub overlayfs_helper_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMode {
    NewBranch(OsString),
    ExistingBranch(OsString),
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
pub struct CompactWorktreeRequest {
    pub repository: PathBuf,
    pub destination: PathBuf,
    /// Defaults to `<common-git-dir>/riftri`.
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CompactWorktreeResult {
    pub destination: PathBuf,
    pub commit: ObjectId,
    pub tree: ObjectId,
    pub old_base_path: PathBuf,
    pub base_path: PathBuf,
    pub journal_path: PathBuf,
    pub reused_base: bool,
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
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub backend: BackendKind,
    pub head: ObjectId,
    /// Full refname as raw Git bytes so non-UTF-8 refs remain representable.
    pub branch: Option<Vec<u8>>,
    pub detached: bool,
    pub locked_reason: Option<Vec<u8>>,
    pub prunable_reason: Option<Vec<u8>>,
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
    pub completed_compactions: usize,
    pub cancelled_compactions: usize,
    pub pending_compactions: usize,
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
    pub busy_adds: usize,
    pub recovered: usize,
    pub active: usize,
    pub recovered_mounts: usize,
    pub completed_removals: usize,
    pub recovered_removals: usize,
    pub completed_moves: usize,
    pub recovered_moves: usize,
    pub completed_compactions: usize,
    pub recovered_compactions: usize,
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

    #[error(transparent)]
    CompactJournalTransition(#[from] CompactJournalTransitionError),

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

    #[error("injected compaction failure after {0:?}")]
    InjectedCompactionFailure(CompactWorktreePhase),
}

pub fn add_worktree(request: AddWorktreeRequest) -> Result<AddWorktreeResult, WorktreeError> {
    add_worktree_inner(request, None, true)
}

pub fn remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_inner(request, None)
}

/// Remove one explicitly selected managed worktree even when it has changes.
/// The durable operation records and revalidates an exact content snapshot so
/// recovery cannot discard changes made after this request.
pub fn force_remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    force_remove_worktree_inner(request, None)
}

pub fn move_worktree(request: MoveWorktreeRequest) -> Result<MoveWorktreeResult, WorktreeError> {
    move_worktree_inner(request, None)
}

pub fn compact_worktree(
    request: CompactWorktreeRequest,
) -> Result<CompactWorktreeResult, WorktreeError> {
    compact_worktree_inner(request, None)
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

/// Forget one explicitly selected state-directory registration after the
/// directory has been removed. Existing paths are never unregistered here.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn forget_missing_state_directory(
    repository: &Path,
    state_directory: &Path,
) -> Result<PathBuf, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(repository)?;
    let repository_root = repository.root.as_deref().ok_or_else(|| {
        WorktreeError::InvalidRequest("bare repositories have no Riftri state locations".to_owned())
    })?;
    let state_directory = absolute_path(state_directory)?;
    match fs::symlink_metadata(&state_directory) {
        Ok(_) => {
            return Err(WorktreeError::InvalidRequest(format!(
                "registered Riftri state path still exists; refusing to unregister it: {}",
                state_directory.display()
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io(
                "inspect registered Riftri state directory",
                &state_directory,
                source,
            ));
        }
    }

    let candidates = managed_destination_candidates(&state_directory)?;
    let _lock = acquire_state_directory_locator_lock(&repository.identity.common_git_dir)?;
    let registered = git
        .local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)?
        .into_iter()
        .find(|registered| {
            candidates
                .iter()
                .any(|candidate| paths_match(registered, candidate))
        })
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "Riftri state path is not registered in this repository: {}",
                state_directory.display()
            ))
        })?;
    git.unset_local_config_value(
        repository_root,
        STATE_DIRECTORY_CONFIG_KEY,
        registered.as_os_str(),
    )?;
    Ok(state_directory)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn forget_missing_state_directory(
    _repository: &Path,
    _state_directory: &Path,
) -> Result<PathBuf, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "state-directory registration recovery requires macOS, Linux, or Windows".to_owned(),
    ))
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
    if let Some(default) = resolve_real_state_directory_if_present(&default)? {
        directories.push(default);
    }
    for configured in git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)? {
        if !configured.is_absolute() {
            return Err(WorktreeError::InvalidRequest(format!(
                "registered Riftri state directory is not absolute: {}",
                configured.display()
            )));
        }
        directories.push(resolve_real_state_directory(&configured)?);
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
    if resolve_real_state_directory_if_present(&default)?.as_deref() == Some(state_directory) {
        return Ok(());
    }
    let _lock = acquire_state_directory_locator_lock(&repository.identity.common_git_dir)?;
    let registered = git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)?;
    for registered in registered {
        if registered == state_directory
            || resolve_real_state_directory(&registered)? == state_directory
        {
            return Ok(());
        }
    }
    git.add_local_config_path(repository_root, STATE_DIRECTORY_CONFIG_KEY, state_directory)?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn acquire_state_directory_locator_lock(common_git_dir: &Path) -> Result<File, WorktreeError> {
    let lock_path = common_git_dir.join("riftri-state-directory.lock");
    acquire_coordination_lock(
        &lock_path,
        "open state-directory locator lock",
        "lock state-directory locators",
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn acquire_coordination_lock(
    lock_path: &Path,
    open_operation: &'static str,
    lock_operation: &'static str,
) -> Result<File, WorktreeError> {
    let lock = open_coordination_lock(lock_path, open_operation)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            progress::emit(ProgressEvent::LockContended {
                operation: lock_operation,
            });
            lock.lock_exclusive()
                .map_err(|source| io(lock_operation, lock_path, source))?;
            progress::emit(ProgressEvent::LockAcquired {
                operation: lock_operation,
            });
        }
        Err(source) => return Err(io(lock_operation, lock_path, source)),
    }
    validate_coordination_lock(&lock, lock_path)?;
    Ok(lock)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn open_coordination_lock(
    lock_path: &Path,
    operation: &'static str,
) -> Result<File, WorktreeError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(target_os = "windows")]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);

    options
        .open(lock_path)
        .map_err(|source| io(operation, lock_path, source))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_coordination_lock(lock: &File, lock_path: &Path) -> Result<(), WorktreeError> {
    let opened_metadata = lock
        .metadata()
        .map_err(|source| io("inspect opened coordination lock", lock_path, source))?;
    let path_metadata = fs::symlink_metadata(lock_path)
        .map_err(|source| io("inspect coordination lock path", lock_path, source))?;
    if !opened_metadata.is_file()
        || !path_metadata.is_file()
        || path_metadata.file_type().is_symlink()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "coordination lock {} is not a real file",
            lock_path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if opened_metadata.dev() != path_metadata.dev()
            || opened_metadata.ino() != path_metadata.ino()
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "coordination lock {} changed while it was opened",
                lock_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct AddOperationLock(File);

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Drop for AddOperationLock {
    fn drop(&mut self) {
        // Explicitly release ownership even if a concurrent fork temporarily
        // inherited this open-file description before its CLOEXEC took effect.
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn try_lock_add_operation(journal_path: &Path) -> Result<Option<AddOperationLock>, WorktreeError> {
    require_real_state_directory(journal_path.expect_parent()?)?;
    let path = journal_path.with_extension("lock");
    let lock = open_coordination_lock(&path, "open add-operation lock")?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            return Ok(None);
        }
        Err(error) => return Err(io("lock add operation", &path, error)),
    }
    let lock = AddOperationLock(lock);
    validate_coordination_lock(&lock.0, &path)?;
    // Never unlink a coordination lock: another opener may already hold the
    // same inode. Process exit releases ownership without removing the path.
    Ok(Some(lock))
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
    let state_directory =
        resolve_real_state_directory_if_present(&state_directory)?.unwrap_or(state_directory);
    let add_load = JournalStore::open(&state_directory).load_all_for_status()?;
    let removal_load = RemovalJournalStore::open(&state_directory).load_all_for_status()?;
    let move_load = MoveJournalStore::open(&state_directory).load_all_for_status()?;
    let compact_load = CompactJournalStore::open(&state_directory).load_all_for_status()?;
    let prune_load = PruneJournalStore::open(&state_directory).load_all_for_status()?;
    let collection_load = CollectionJournalStore::open(&state_directory).load_all_for_status()?;
    let journal_issues = add_load
        .issues
        .into_iter()
        .chain(removal_load.issues)
        .chain(move_load.issues)
        .chain(compact_load.issues)
        .chain(prune_load.issues)
        .chain(collection_load.issues)
        .map(|issue| StateDiagnosticIssue {
            path: issue.path,
            reason: format!(
                "malformed durable operation journal; Riftri preserved it: {}",
                issue.reason
            ),
        })
        .collect::<Vec<_>>();
    let loaded_add_journals = add_load.journals;
    let loaded_removal_journals = removal_load.journals;
    let move_journals = move_load.journals;
    let loaded_compact_journals = compact_load.journals;
    let prune_journals = prune_load.journals;
    let collection_journals = collection_load.journals;
    let mut removal_journals = Vec::with_capacity(loaded_removal_journals.len());
    let mut invalid_removal_journals = Vec::new();
    for journal in loaded_removal_journals {
        match validate_removal_against_add_journals(
            &state_directory,
            &journal,
            &loaded_add_journals,
        ) {
            Ok(()) => removal_journals.push(journal),
            Err(error) => invalid_removal_journals.push(StateDiagnosticIssue {
                path: journal.journal_path.clone(),
                reason: format!(
                    "unsafe durable removal journal; Riftri did not trust its lifecycle claim: {error}"
                ),
            }),
        }
    }
    let completed = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.clone())
        .collect::<HashSet<_>>();
    let git = Git::default();
    let mut add_journals = Vec::with_capacity(loaded_add_journals.len());
    let mut registered_worktrees = BTreeMap::new();
    let mut invalid_add_journals = Vec::new();
    for journal in loaded_add_journals {
        match validate_status_add_journal(
            &git,
            &state_directory,
            &journal,
            completed.contains(&journal.operation_id),
        ) {
            Ok(worktree) => {
                if let Some(worktree) = worktree {
                    registered_worktrees.insert(journal.operation_id.clone(), worktree);
                }
                add_journals.push(journal);
            }
            Err(error) => invalid_add_journals.push(StateDiagnosticIssue {
                path: journal.journal_path.clone(),
                reason: format!(
                    "unsafe durable add journal; Riftri did not inspect its referenced paths: {error}"
                ),
            }),
        }
    }
    let mut compact_journals = Vec::with_capacity(loaded_compact_journals.len());
    let mut invalid_compact_journals = Vec::new();
    for journal in loaded_compact_journals {
        match validate_compaction_paths(&state_directory, &journal) {
            Ok(()) => compact_journals.push(journal),
            Err(error) => invalid_compact_journals.push(StateDiagnosticIssue {
                path: journal.journal_path.clone(),
                reason: format!(
                    "unsafe durable compaction journal; Riftri did not trust its lifecycle claim: {error}"
                ),
            }),
        }
    }
    let mut references = BTreeMap::<PathBuf, usize>::new();
    let mut views = Vec::new();
    for journal in add_journals.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active && !completed.contains(&journal.operation_id)
    }) {
        *references.entry(journal.base_path.clone()).or_default() += 1;
        if journal.destination.is_dir() {
            let worktree = registered_worktrees
                .get(&journal.operation_id)
                .ok_or_else(|| {
                    WorktreeError::InvalidRequest(format!(
                        "active destination {} lost its validated Git inventory entry",
                        journal.destination.display()
                    ))
                })?;
            let head = worktree.head.clone().ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "active destination {} has no Git HEAD commit",
                    journal.destination.display()
                ))
            })?;
            let (logical_bytes, allocated_bytes) = tree_usage(&journal.destination)?;
            #[cfg(target_os = "linux")]
            let allocated_bytes = if journal.backend == BackendKind::OverlayFs
                && let Some(overlayfs) = journal.overlayfs.as_ref()
                && overlayfs.layout_root.exists()
            {
                tree_usage(&overlayfs.layout_root)?.1
            } else {
                allocated_bytes
            };
            views.push(ViewStorageAccounting {
                repository: journal.repository.clone(),
                destination: journal.destination.clone(),
                base_path: journal.base_path.clone(),
                backend: journal.backend,
                head,
                branch: worktree.branch.clone(),
                detached: worktree.detached,
                locked_reason: worktree.locked_reason.clone(),
                prunable_reason: worktree.prunable_reason.clone(),
                logical_bytes,
                allocated_bytes,
            });
        }
    }
    views.sort_unstable_by(|left, right| left.destination.cmp(&right.destination));

    for base_path in retained_base_paths(&state_directory, UnsafeBaseInventory::Ignore)? {
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
    let mut state_diagnosis = diagnose_state_paths(
        &state_directory,
        &add_journals,
        &removal_journals,
        &move_journals,
        &compact_journals,
        &prune_journals,
        &collection_journals,
        &journal_issues,
    )?;
    let invalid_journal_paths = invalid_add_journals
        .iter()
        .chain(&invalid_removal_journals)
        .chain(&invalid_compact_journals)
        .map(|issue| issue.path.as_path())
        .collect::<HashSet<_>>();
    state_diagnosis
        .issues
        .retain(|issue| !invalid_journal_paths.contains(issue.path.as_path()));
    state_diagnosis.issues.extend(invalid_add_journals);
    state_diagnosis.issues.extend(invalid_removal_journals);
    state_diagnosis.issues.extend(invalid_compact_journals);
    state_diagnosis
        .issues
        .sort_unstable_by(|left, right| left.path.cmp(&right.path));

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
        completed_compactions: compact_journals
            .iter()
            .filter(|journal| journal.phase == CompactWorktreePhase::Complete)
            .count(),
        cancelled_compactions: compact_journals
            .iter()
            .filter(|journal| journal.phase == CompactWorktreePhase::Cancelled)
            .count(),
        pending_compactions: compact_journals
            .iter()
            .filter(|journal| {
                !matches!(
                    journal.phase,
                    CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                )
            })
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

fn validate_status_add_journal(
    git: &Git,
    state_directory: &Path,
    journal: &DecodedJournal,
    removal_complete: bool,
) -> Result<Option<riftri_git::WorktreeInfo>, WorktreeError> {
    validate_recovery_paths(state_directory, journal)?;
    if journal.phase != AddWorktreePhase::Active || removal_complete {
        return Ok(None);
    }
    let registered = git
        .list_worktrees(&journal.repository)?
        .into_iter()
        .find(|worktree| paths_match(&worktree.path, &journal.destination))
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "active destination {} is not registered by Git for {}",
                journal.destination.display(),
                journal.repository.display()
            ))
        })?;
    if registered.head.is_none() {
        return Err(WorktreeError::InvalidRequest(format!(
            "active destination {} has no Git HEAD commit",
            journal.destination.display()
        )));
    }
    Ok(Some(registered))
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
    let Some(state_directory) = resolve_real_state_directory_if_present(&state_directory)? else {
        return Ok(GarbageCollectionReport {
            applied: apply,
            ..GarbageCollectionReport::default()
        });
    };

    let resumed_collections = if apply {
        recover_collection_journals(&state_directory)?
    } else {
        0
    };
    let candidates = garbage_collection_candidates(&state_directory)?;
    progress::emit(ProgressEvent::GcPlanned {
        candidates: candidates.len(),
    });
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
        let journal_path = store.path_for(&journal.operation_id);
        let decoded = journal.clone().decode(journal_path.clone())?;
        validate_new_collection_candidate(&state_directory, &decoded)?;
        let journal_path = store.persist(&journal)?;
        progress::emit(ProgressEvent::GcPhase {
            phase: journal.phase,
        });
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
    for base_path in retained_base_paths(state_directory, UnsafeBaseInventory::Reject)? {
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
    let adds = JournalStore::open(state_directory).load_all()?;
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed = validated_completed_removal_ids(state_directory, &adds, &removals)?;
    let mut protected = adds
        .into_iter()
        .filter(|journal| {
            journal.phase != AddWorktreePhase::RolledBack
                && !completed.contains(&journal.operation_id)
        })
        .map(|journal| journal.base_path)
        .collect::<HashSet<_>>();
    for journal in CompactJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
            )
        })
    {
        protected.insert(journal.old_base_path);
        protected.insert(journal.base_path);
    }
    Ok(protected)
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
    let mut record = store.reload(journal)?;
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
    let _lock = acquire_coordination_lock(
        &lock_path,
        "open immutable-base lock",
        "lock immutable base",
    )?;

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
            if let Err(error) = validate_collectible_base(journal) {
                advance_collection(store, record, GarbageCollectionPhase::Cancelled, None)?;
                return Err(error);
            }
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
            if let Err(error) = validate_collectible_base(journal) {
                advance_collection(store, record, GarbageCollectionPhase::Cancelled, None)?;
                return Err(error);
            }
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
    for directory in [
        &state_directory.join("bases"),
        &base_root,
        repository_directory,
    ] {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|source| io("inspect collection parent", directory, source))?;
        if metadata.file_type().is_symlink() {
            return Err(symlinked_base_parent_error(state_directory, directory));
        }
        if !metadata.is_dir() {
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
fn validate_new_collection_candidate(
    state_directory: &Path,
    journal: &DecodedCollectionJournal,
) -> Result<(), WorktreeError> {
    validate_collection_paths(state_directory, journal)?;
    let base_metadata = fs::symlink_metadata(&journal.base_path)
        .map_err(|source| io("inspect collectible base", &journal.base_path, source))?;
    if !base_metadata.is_dir() || base_metadata.file_type().is_symlink() {
        return Err(WorktreeError::InvalidRequest(format!(
            "collectible base {} is not a real directory",
            journal.base_path.display()
        )));
    }
    let marker_metadata = fs::symlink_metadata(&journal.marker_path).map_err(|source| {
        io(
            "inspect collectible base marker",
            &journal.marker_path,
            source,
        )
    })?;
    if !marker_metadata.is_file() || marker_metadata.file_type().is_symlink() {
        return Err(WorktreeError::InvalidRequest(format!(
            "collectible base marker {} is not a real file",
            journal.marker_path.display()
        )));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_collectible_base(journal: &DecodedCollectionJournal) -> Result<(), WorktreeError> {
    match fs::symlink_metadata(&journal.base_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(WorktreeError::InvalidRequest(format!(
                "collectible base {} is not a real directory",
                journal.base_path.display()
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io("inspect collectible base", &journal.base_path, source)),
    }
    validate_collection_marker(journal)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_collection_marker(journal: &DecodedCollectionJournal) -> Result<(), WorktreeError> {
    match fs::symlink_metadata(&journal.marker_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(WorktreeError::InvalidRequest(format!(
                "collectible base marker {} is not a real file",
                journal.marker_path.display()
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io(
                "inspect collectible base marker",
                &journal.marker_path,
                source,
            ));
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
    progress::emit(ProgressEvent::GcPhase { phase });
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn force_remove_worktree_inner(
    request: RemoveWorktreeRequest,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_inner(request, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn remove_worktree_inner(
    request: RemoveWorktreeRequest,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_with_mode(request, fail_after, false)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn force_remove_worktree_inner(
    request: RemoveWorktreeRequest,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_with_mode(request, fail_after, true)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn remove_worktree_with_mode(
    request: RemoveWorktreeRequest,
    fail_after: Option<RemoveWorktreePhase>,
    force: bool,
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
    let state_directory = resolve_real_state_directory(&absolute_path(&requested_state)?)?;
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
    let overlayfs_clean_snapshot = snapshot_overlayfs_private_layer(&managed)?;
    let force_snapshot = if force {
        Some(snapshot_managed_worktree_for_force(&managed)?)
    } else {
        None
    };
    if !force && !git.worktree_is_clean(&destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} has changes; commit, stash, or remove them before retrying",
            destination.display()
        )));
    }

    let store = RemovalJournalStore::create(&state_directory)?;
    let operation_id = allocate_removal_operation_id(&store)?;
    let paths = RemovalJournalPaths {
        repository: &managed.repository,
        destination: &destination,
        base_path: &managed.base_path,
    };
    let mut journal = match force_snapshot {
        Some(snapshot) => RemovalJournalRecord::new_forced(
            operation_id,
            paths,
            managed.operation_id.clone(),
            snapshot,
        ),
        None => RemovalJournalRecord::new(operation_id, paths, managed.operation_id.clone()),
    };
    journal.overlayfs_clean_snapshot = overlayfs_clean_snapshot;
    let journal_path = store.persist(&journal)?;
    fail_removal_if_requested(journal.phase, fail_after)?;
    advance_removal(
        &store,
        &mut journal,
        RemoveWorktreePhase::CleanVerified,
        fail_after,
    )?;

    remove_managed_worktree_files(
        &git,
        &repository_root,
        &destination,
        &managed,
        journal.overlayfs_clean_snapshot.as_deref(),
        journal.force,
        journal.force_snapshot.as_deref(),
    )?;
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
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = resolve_real_state_directory(&absolute_path(&requested_state)?)?;
    let managed = find_managed_add_journal(&state_directory, &source)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "{} is not an active Riftri-managed worktree in {}",
            source.display(),
            state_directory.display()
        ))
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    if managed.backend == BackendKind::OverlayFs {
        return Err(WorktreeError::Unsupported(
            "moving an active OverlayFS worktree is not yet supported; remove and recreate the worktree at its new path"
                .to_owned(),
        ));
    }
    let source_volume = inspected_native_cow_volume(&source, managed.backend)?;
    let destination_volume = supported_worktree_backend(&destination)?.volume;
    if source_volume.identity != destination_volume.identity {
        return Err(WorktreeError::Unsupported(
            "moving an optimized worktree across filesystem volumes is not supported".to_owned(),
        ));
    }
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

    let store = MoveJournalStore::create(&state_directory)?;
    let operation_id = allocate_move_operation_id(&store)?;
    let journal = MoveJournalRecord::new(
        operation_id,
        MoveJournalPaths {
            repository: &managed.repository,
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
fn compact_worktree_inner(
    _request: CompactWorktreeRequest,
    _fail_after: Option<CompactWorktreePhase>,
) -> Result<CompactWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "journaled Riftri compaction requires a supported native COW backend".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn compact_worktree_inner(
    request: CompactWorktreeRequest,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<CompactWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories do not have compactable worktree views".to_owned(),
        ));
    }
    let repository_root = repository.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let destination = normalize_existing_destination(&request.destination)?;
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = resolve_real_state_directory(&absolute_path(&requested_state)?)?;
    let managed = find_managed_add_journal(&state_directory, &destination)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "{} is not an active Riftri-managed worktree in {}",
            destination.display(),
            state_directory.display()
        ))
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    if managed.backend == BackendKind::OverlayFs {
        return Err(WorktreeError::Unsupported(
            "compacting an active OverlayFS worktree is not yet supported; remove and recreate it to reset the private upper layer"
                .to_owned(),
        ));
    }
    if CompactJournalStore::open(&state_directory)
        .load_all()?
        .iter()
        .any(|journal| {
            journal.source_add_operation_id == managed.operation_id
                && !matches!(
                    journal.phase,
                    CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                )
        })
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a compaction of {} is already pending; run `riftri repair --state-dir {}`",
            destination.display(),
            state_directory.display()
        )));
    }

    let _operation_lock = try_lock_add_operation(&managed.journal_path)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "worktree {} is busy with another Riftri operation",
            destination.display()
        ))
    })?;
    let resolved = git.resolve_revision(&destination, OsStr::new("HEAD"))?;
    verify_compaction_source(&git, &repository_root, &destination, &resolved.commit, None)?;
    let compatibility = validate_resolved_compatibility(&git, &destination, &resolved)?;
    validate_destination_path_semantics(&compatibility.checkout_paths, &destination)?;
    verify_compaction_checkout_shape(&destination, &compatibility.checkout_paths)?;
    #[cfg(target_os = "windows")]
    ensure_no_windows_alternate_streams(&destination)?;
    let selected_backend = supported_worktree_backend(&destination)?;
    if selected_backend.kind != managed.backend {
        return Err(WorktreeError::Unsupported(format!(
            "worktree was created with {}, but {} is now selected for its volume",
            managed.backend.display_name(),
            selected_backend.kind.display_name()
        )));
    }
    let state_volume = inspected_native_cow_volume(&state_directory, managed.backend)?;
    if selected_backend.volume.identity != state_volume.identity {
        return Err(WorktreeError::Unsupported(
            "the managed worktree and Riftri state directory are no longer on the same volume"
                .to_owned(),
        ));
    }
    let expected_snapshot = directory_snapshot(&destination)?;
    let base_directory = state_directory.join("bases/v1").join(repository_cache_id(
        &repository.identity.common_git_dir,
        &compatibility.checkout_profile,
    ));
    ensure_real_state_directory(&base_directory, "create repository base directory")?;
    let store = CompactJournalStore::create(&state_directory)?;
    let operation_id = allocate_lifecycle_operation_id("compact", |operation_id| {
        store.path_for(operation_id).exists()
    })?;
    let replacement = destination
        .expect_parent()?
        .join(format!(".riftri-compact-new-{operation_id}"));
    let quarantine = destination
        .expect_parent()?
        .join(format!(".riftri-compact-old-{operation_id}"));
    if replacement.exists() || quarantine.exists() {
        return Err(WorktreeError::InvalidRequest(
            "new compaction paths unexpectedly already exist".to_owned(),
        ));
    }
    let base_path = base_directory.join(resolved.tree.as_str());
    let base_staging = base_directory.join(format!(".riftri-build-{operation_id}"));
    let temporary_index = state_directory
        .join("tmp")
        .join(format!("compact-index-{operation_id}"));
    let mut journal = CompactJournalRecord::new(
        operation_id,
        CompactJournalPaths {
            repository: &managed.repository,
            destination: &destination,
            replacement: &replacement,
            quarantine: &quarantine,
            base_staging: &base_staging,
            base_path: &base_path,
            temporary_index: &temporary_index,
            old_base_path: &managed.base_path,
        },
        managed.operation_id.clone(),
        resolved.commit.as_str().to_owned(),
        expected_snapshot,
        managed.backend,
    );
    let journal_path = store.persist(&journal)?;
    fail_compaction_if_requested(journal.phase, fail_after)?;

    let reused_base = prepare_base(
        &git,
        &destination,
        &resolved.tree,
        &base_path,
        &base_staging,
        &temporary_index,
        &compatibility.checkout_config,
        &compatibility.lfs_objects,
    )?;
    NativeCowCloner::clone_tree(&base_path, &replacement)?;
    NativeCowCloner::make_tree_owner_writable(&replacement)?;
    copy_git_pointer(&destination, &replacement)?;
    verify_snapshot(&replacement, &journal.expected_snapshot)?;
    sync_parent(&replacement)?;
    advance_compaction(
        &store,
        &mut journal,
        CompactWorktreePhase::ReplacementReady,
        fail_after,
    )?;
    resume_compaction(
        &git,
        &store,
        journal.decode(journal_path.clone())?,
        fail_after,
    )?;

    Ok(CompactWorktreeResult {
        destination,
        commit: resolved.commit,
        tree: resolved.tree,
        old_base_path: managed.base_path,
        base_path,
        journal_path,
        reused_base,
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
    let state_directory = resolve_real_state_directory(&absolute_path(&requested_state)?)?;
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
    let resolved = git.resolve_revision(&repository_root, &request.revision)?;
    if let WorktreeMode::ExistingBranch(branch) = &request.mode {
        let target = git
            .local_branch_target(&repository_root, branch)?
            .ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "existing local branch does not exist: {}",
                    branch.to_string_lossy()
                ))
            })?;
        if target != resolved.commit {
            return Err(WorktreeError::InvalidRequest(format!(
                "existing branch moved from {} to {} while the request was being validated",
                resolved.commit.as_str(),
                target.as_str()
            )));
        }
    }
    let compatibility = validate_resolved_compatibility(&git, &repository_root, &resolved)?;
    validate_destination_path_semantics(&compatibility.checkout_paths, &destination)?;
    let selected_backend = supported_worktree_backend(&destination)?;
    let destination_volume = &selected_backend.volume;

    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = absolute_path(&requested_state)?;
    let state_volume = inspected_native_cow_volume(&state_directory, selected_backend.kind)?;
    if destination_volume.identity != state_volume.identity {
        return Err(WorktreeError::Unsupported(format!(
            "state directory {} and destination {} are on different volumes; pass --state-dir on the destination filesystem volume",
            state_directory.display(),
            destination.display()
        )));
    }

    create_state_layout(&state_directory)?;
    let state_directory = resolve_real_state_directory(&state_directory)?;
    register_state_directory(&git, &repository, &state_directory)?;
    let store = JournalStore::create(&state_directory)?;
    let base_directory = state_directory.join("bases/v1").join(repository_cache_id(
        &repository.identity.common_git_dir,
        &compatibility.checkout_profile,
    ));
    ensure_real_state_directory(&base_directory, "create repository base directory")?;
    sync_parent(&base_directory)?;
    let operation_id =
        allocate_operation_id(&store, &state_directory, &base_directory, &destination)?;
    let _operation_lock =
        try_lock_add_operation(&store.path_for(&operation_id))?.ok_or_else(|| {
            WorktreeError::InvalidRequest("new add-operation ID is already locked".to_owned())
        })?;
    let base_path = base_directory.join(resolved.tree.as_str());
    let base_staging = base_directory.join(format!(".riftri-build-{operation_id}"));
    let temporary_index = state_directory
        .join("tmp")
        .join(format!("index-{operation_id}"));
    let scratch = destination
        .parent()
        .expect("normalized destination has a parent")
        .join(format!(".riftri-view-{operation_id}"));
    let (branch, branch_created) = match &request.mode {
        WorktreeMode::NewBranch(branch) => (Some(branch.as_os_str()), true),
        WorktreeMode::ExistingBranch(branch) => (Some(branch.as_os_str()), false),
        WorktreeMode::Detached => (None, false),
    };
    let journal_paths = JournalPaths {
        repository: &repository_root,
        destination: &destination,
        scratch: &scratch,
        base_staging: &base_staging,
        base_path: &base_path,
        temporary_index: &temporary_index,
        branch,
        branch_created,
    };
    let backend = selected_backend.kind;
    let mut journal = if backend == BackendKind::OverlayFs {
        let layout_root = state_directory.join("overlays/v1").join(&operation_id);
        #[cfg(target_os = "linux")]
        let mount_context = Some(OverlayFsMounter::current_mount_context_for(
            selected_backend.overlayfs_profile.ok_or_else(|| {
                WorktreeError::Unsupported(
                    "OverlayFS activation did not select a durable mount profile".to_owned(),
                )
            })?,
        )?);
        #[cfg(not(target_os = "linux"))]
        let mount_context = None;
        JournalRecord::new_overlayfs(
            operation_id,
            journal_paths,
            resolved.commit.as_str().to_owned(),
            &layout_root,
            overlayfs_recovery_token(&layout_root),
            mount_context,
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
    progress::emit(ProgressEvent::AddPhase {
        phase: journal.phase,
    });

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
            &resolved.commit,
            &request.mode,
            &resolved.tree,
            &compatibility.checkout_config,
            &compatibility.lfs_objects,
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
            backend,
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
                .map(|()| {
                    progress::emit(ProgressEvent::AddPhase {
                        phase: AddWorktreePhase::RollbackPending,
                    });
                })
                .and_then(|()| decoded.map_err(WorktreeError::from))
                .and_then(|decoded| rollback_decoded(&git, &decoded))
                .and_then(|()| {
                    journal.transition(AddWorktreePhase::RolledBack)?;
                    store.persist(&journal)?;
                    progress::emit(ProgressEvent::AddPhase {
                        phase: AddWorktreePhase::RolledBack,
                    });
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
    commit: &ObjectId,
    mode: &WorktreeMode,
    tree: &ObjectId,
    checkout_config: &[(String, Vec<u8>)],
    lfs_objects: &[GitLfsObject],
    common_git_dir: &Path,
    fail_after: Option<AddWorktreePhase>,
) -> Result<bool, WorktreeError> {
    let head = match mode {
        WorktreeMode::NewBranch(branch) => WorktreeHead::NewBranch(branch),
        WorktreeMode::ExistingBranch(branch) => WorktreeHead::ExistingBranch(branch),
        WorktreeMode::Detached => WorktreeHead::Detached,
    };
    let metadata_lock = acquire_git_worktree_metadata_lock(common_git_dir)?;
    git.add_worktree_no_checkout(repository, destination, OsStr::new(commit.as_str()), head)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitMetadataCreated,
        fail_after,
    )?;
    if matches!(mode, WorktreeMode::ExistingBranch(_)) {
        let attached = git.resolve_revision(destination, OsStr::new("HEAD"))?;
        if attached.commit != *commit {
            return Err(WorktreeError::InvalidRequest(format!(
                "existing branch moved from {} to {} while its worktree was being created",
                commit.as_str(),
                attached.commit.as_str()
            )));
        }
    }
    drop(metadata_lock);

    let reused_base = prepare_base(
        git,
        repository,
        tree,
        base_path,
        base_staging,
        temporary_index,
        checkout_config,
        lfs_objects,
    )?;
    advance(store, journal, AddWorktreePhase::BaseReady, fail_after)?;

    if journal.backend == BackendKind::OverlayFs {
        #[cfg(target_os = "linux")]
        perform_overlayfs_view(store, journal, destination, scratch, base_path, fail_after)?;
        #[cfg(not(target_os = "linux"))]
        return Err(WorktreeError::Unsupported(
            "OverlayFS worktree execution requires Linux".to_owned(),
        ));
    } else {
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
    }

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

#[cfg(target_os = "linux")]
fn perform_overlayfs_view(
    store: &JournalStore,
    journal: &mut JournalRecord,
    destination: &Path,
    scratch: &Path,
    base_path: &Path,
    fail_after: Option<AddWorktreePhase>,
) -> Result<(), WorktreeError> {
    if !contains_only_git_pointer(destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "Git created unexpected files in {}; refusing to replace them",
            destination.display()
        )));
    }
    fs::create_dir(scratch).map_err(|source| {
        io(
            "create OverlayFS pointer staging directory",
            scratch,
            source,
        )
    })?;
    let git_pointer = destination.join(".git");
    fs::rename(&git_pointer, scratch.join(".git"))
        .map_err(|source| io("stage linked-worktree pointer", &git_pointer, source))?;
    fs::remove_dir(destination)
        .map_err(|source| io("remove empty checkout directory", destination, source))?;
    fs::create_dir(destination)
        .map_err(|source| io("create OverlayFS mountpoint", destination, source))?;
    sync_parent(destination)?;

    let overlayfs = journal.overlayfs_intent()?;
    let layout = OverlayFsMounter::prepare(&overlayfs.layout_root, base_path, destination)?;
    let upper_pointer = layout.upper().join(".git");
    fs::rename(scratch.join(".git"), &upper_pointer).map_err(|source| {
        io(
            "place linked-worktree pointer in OverlayFS upper",
            &upper_pointer,
            source,
        )
    })?;
    fs::remove_dir(scratch).map_err(|source| {
        io(
            "remove OverlayFS pointer staging directory",
            scratch,
            source,
        )
    })?;
    OverlayFsMounter::arm_recovery(&layout, &overlayfs.recovery_token)?;
    advance(store, journal, AddWorktreePhase::ViewCreated, fail_after)?;

    let context = overlayfs.mount_context.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest("OverlayFS mount intent has no activation profile".to_owned())
    })?;
    let identity = OverlayFsMounter::mount_with_profile(&layout, context.profile)?;
    if identity.profile == OverlayFsMountProfile::PrivilegedTrustedXattr {
        OverlayFsMounter::make_view_owner_writable(&layout, &identity)?;
    }
    #[cfg(test)]
    exit_after_overlayfs_mount_for_test();
    journal.record_overlayfs_mount_identity(identity)?;
    store.persist(journal)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitPointerRestored,
        fail_after,
    )?;
    OverlayFsMounter::clear_recovery(&layout, &overlayfs.recovery_token)?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
fn exit_after_overlayfs_mount_for_test() {
    if std::env::var_os("RIFTRI_TEST_EXIT_AFTER_OVERLAYFS_MOUNT").as_deref()
        == Some(OsStr::new("1"))
    {
        std::process::exit(86);
    }
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
    progress::emit(ProgressEvent::AddPhase { phase });
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
fn verify_existing_base(base_path: &Path, complete_path: &Path) -> Result<bool, WorktreeError> {
    let base_exists = base_path
        .try_exists()
        .map_err(|source| io("inspect immutable base", base_path, source))?;
    let complete_exists = match fs::symlink_metadata(complete_path) {
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
                complete_path,
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
        use std::io::Read;
        let mut stored = Vec::new();
        crate::base_integrity::open_regular(complete_path)
            .map_err(|source| {
                io(
                    "open immutable-base integrity marker",
                    complete_path,
                    source,
                )
            })?
            .take(128)
            .read_to_end(&mut stored)
            .map_err(|source| {
                io(
                    "read immutable-base integrity marker",
                    complete_path,
                    source,
                )
            })?;
        if stored
            != crate::base_integrity::marker(base_path)
                .map_err(|source| io("verify immutable-base integrity", base_path, source))?
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "immutable-base integrity check failed for {}; the base was preserved and cannot be reused",
                base_path.display()
            )));
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct BaseReadLock(File);

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux", target_os = "windows")
))]
#[path = "base_read_lock_tests.rs"]
mod base_read_lock_tests;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Drop for BaseReadLock {
    fn drop(&mut self) {
        // A concurrent fork can inherit the descriptor before CLOEXEC. Release
        // ownership explicitly before opening the exclusive slow-path lock.
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn acquire_base_read_lock(lock_path: &Path) -> Result<BaseReadLock, WorktreeError> {
    const LOCK_OPERATION: &str = "read-lock immutable base";
    let lock = open_coordination_lock(lock_path, "open immutable-base lock")?;
    match FileExt::try_lock_shared(&lock) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            progress::emit(ProgressEvent::LockContended {
                operation: LOCK_OPERATION,
            });
            FileExt::lock_shared(&lock).map_err(|source| io(LOCK_OPERATION, lock_path, source))?;
            progress::emit(ProgressEvent::LockAcquired {
                operation: LOCK_OPERATION,
            });
        }
        Err(source) => return Err(io(LOCK_OPERATION, lock_path, source)),
    }
    let lock = BaseReadLock(lock);
    validate_coordination_lock(&lock.0, lock_path)?;
    Ok(lock)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn materialize_git_lfs_objects(
    base_staging: &Path,
    objects: &[GitLfsObject],
) -> Result<(), WorktreeError> {
    use std::io::{Read, Write};

    for object in objects {
        let destination = base_staging.join(&object.checkout_path);
        let mut source_options = OpenOptions::new();
        source_options.read(true);
        #[cfg(unix)]
        source_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        #[cfg(target_os = "windows")]
        source_options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut source = source_options.open(&object.source_path).map_err(|error| {
            WorktreeError::Unsupported(format!(
                "could not open local Git LFS object {} for {}: {error}",
                object.pointer.oid,
                object.checkout_path.display()
            ))
        })?;
        validate_opened_git_lfs_file(&source, &object.source_path, "local Git LFS object")?;
        let source_size = source
            .metadata()
            .map_err(|error| io("inspect opened Git LFS object", &object.source_path, error))?
            .len();
        if source_size != object.pointer.size {
            return Err(WorktreeError::Unsupported(format!(
                "local Git LFS object {} changed size before materialization; expected {}, found {source_size}",
                object.pointer.oid, object.pointer.size
            )));
        }

        let mut destination_options = OpenOptions::new();
        destination_options.write(true).truncate(true);
        #[cfg(unix)]
        destination_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        #[cfg(target_os = "windows")]
        destination_options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut output = destination_options
            .open(&destination)
            .map_err(|error| io("open Git LFS checkout destination", &destination, error))?;
        validate_opened_git_lfs_file(&output, &destination, "Git LFS checkout destination")?;

        let mut digest = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|error| io("read local Git LFS object", &object.source_path, error))?;
            if count == 0 {
                break;
            }
            copied = copied.checked_add(count as u64).ok_or_else(|| {
                WorktreeError::Unsupported("Git LFS object size overflowed u64".to_owned())
            })?;
            digest.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|error| io("write expanded Git LFS object", &destination, error))?;
        }
        if copied != object.pointer.size {
            return Err(WorktreeError::Unsupported(format!(
                "local Git LFS object {} changed while it was read; expected {} bytes, read {copied}",
                object.pointer.oid, object.pointer.size
            )));
        }
        let actual_oid = crate::base_integrity::hex_lower(digest.finalize());
        if actual_oid != object.pointer.oid {
            return Err(WorktreeError::Unsupported(format!(
                "local Git LFS object {} failed SHA-256 verification; found {actual_oid}",
                object.pointer.oid
            )));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_opened_git_lfs_file(
    opened: &File,
    path: &Path,
    role: &str,
) -> Result<(), WorktreeError> {
    let opened_metadata = opened
        .metadata()
        .map_err(|error| io("inspect opened Git LFS file", path, error))?;
    let path_metadata =
        fs::symlink_metadata(path).map_err(|error| io("inspect Git LFS file path", path, error))?;
    if !opened_metadata.is_file()
        || !path_metadata.is_file()
        || path_metadata.file_type().is_symlink()
    {
        return Err(WorktreeError::Unsupported(format!(
            "{role} {} is not a stable regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened_metadata.dev() != path_metadata.dev()
            || opened_metadata.ino() != path_metadata.ino()
        {
            return Err(WorktreeError::Unsupported(format!(
                "{role} {} changed while it was opened",
                path.display()
            )));
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if opened_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || path_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(WorktreeError::Unsupported(format!(
                "{role} {} is a reparse point",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[allow(clippy::too_many_arguments)]
fn prepare_base(
    git: &Git,
    repository: &Path,
    tree: &ObjectId,
    base_path: &Path,
    base_staging: &Path,
    temporary_index: &Path,
    checkout_config: &[(String, Vec<u8>)],
    lfs_objects: &[GitLfsObject],
) -> Result<bool, WorktreeError> {
    let base_parent = base_path.expect_parent()?;
    let lock_path = base_parent.join(format!("{}.lock", tree.as_str()));
    let complete_path = base_parent.join(format!("{}.complete", tree.as_str()));
    let read_lock = acquire_base_read_lock(&lock_path)?;
    if verify_existing_base(base_path, &complete_path)? {
        progress::emit(ProgressEvent::BaseReused);
        return Ok(true);
    }
    // Never upgrade a held shared lock: concurrent cold callers could deadlock.
    // Another builder or collector may run in the gap, so revalidate all state
    // after acquiring the same stable lock file exclusively.
    drop(read_lock);
    let _write_lock = acquire_coordination_lock(
        &lock_path,
        "open immutable-base lock",
        "lock immutable base",
    )?;
    if verify_existing_base(base_path, &complete_path)? {
        progress::emit(ProgressEvent::BaseReused);
        return Ok(true);
    }
    progress::emit(ProgressEvent::BaseMaterializing);
    remove_tree_if_present(base_path)?;
    remove_file_if_present(&complete_path)?;

    fs::create_dir(base_staging).map_err(|source| {
        io(
            "create immutable-base staging directory",
            base_staging,
            source,
        )
    })?;
    git.materialize_tree_with_config(
        repository,
        tree,
        base_staging,
        temporary_index,
        checkout_config,
    )?;
    materialize_git_lfs_objects(base_staging, lfs_objects)?;
    remove_file_if_present(temporary_index)?;
    fs::rename(base_staging, base_path)
        .map_err(|source| io("activate immutable base", base_path, source))?;
    NativeCowCloner::make_tree_read_only(base_path)?;
    let integrity = crate::base_integrity::marker(base_path)
        .map_err(|source| io("record immutable-base integrity", base_path, source))?;
    let mut marker = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&complete_path)
        .map_err(|source| io("create immutable-base marker", &complete_path, source))?;
    use std::io::Write;
    marker.write_all(&integrity).map_err(|source| {
        io(
            "write immutable-base integrity marker",
            &complete_path,
            source,
        )
    })?;
    marker
        .sync_all()
        .map_err(|source| io("sync immutable-base marker", &complete_path, source))?;
    sync_parent(base_path)?;
    Ok(false)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_resolved_compatibility(
    git: &Git,
    repository: &Path,
    resolved: &ResolvedRevision,
) -> Result<CompatibilityAnalysis, WorktreeError> {
    let analysis = analyze_resolved_repository_compatibility(git, repository, resolved)?;
    if let Some(blocker) = analysis.report.blockers.first() {
        return Err(WorktreeError::Unsupported(blocker.explanation.clone()));
    }
    Ok(analysis)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_destination_path_semantics(
    paths: &[PathBuf],
    destination: &Path,
) -> Result<(), WorktreeError> {
    for path in paths {
        let components = path.components().collect::<Vec<_>>();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(WorktreeError::Unsupported(format!(
                "Git tree path {} is not a relative checkout path",
                path.display()
            )));
        }
    }
    if !needs_destination_path_probe(paths) {
        return Ok(());
    }

    let parent = destination.expect_parent()?;
    let probe = tempfile::Builder::new()
        .prefix(".riftri-path-probe-")
        .tempdir_in(parent)
        .map_err(|source| io("create destination path-semantics probe", parent, source))?;
    let probe_path = probe.path().to_path_buf();
    let mut directories = HashSet::new();

    let validation = (|| {
        for path in paths {
            let components = path.components().collect::<Vec<_>>();
            let mut relative_parent = PathBuf::new();
            for component in &components[..components.len() - 1] {
                let Component::Normal(name) = component else {
                    unreachable!("checkout path components were validated above");
                };
                relative_parent.push(name);
                if directories.insert(relative_parent.clone()) {
                    let directory = probe_path.join(&relative_parent);
                    fs::create_dir(&directory).map_err(|source| {
                        WorktreeError::Unsupported(format!(
                            "Git tree path {} cannot coexist on the destination filesystem: {source}",
                            path.display()
                        ))
                    })?;
                }
            }

            let candidate = probe_path.join(path);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .map_err(|source| {
                    WorktreeError::Unsupported(format!(
                        "Git tree path {} cannot coexist on the destination filesystem: {source}",
                        path.display()
                    ))
                })?;
        }
        Ok(())
    })();

    probe.close().map_err(|source| {
        io(
            "remove destination path-semantics probe",
            &probe_path,
            source,
        )
    })?;
    validation
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn needs_destination_path_probe(paths: &[PathBuf]) -> bool {
    // Filesystems disagree about Unicode case folding and normalization. Do
    // not approximate their rules or decode arbitrary native Unix bytes: probe
    // any non-ASCII path set, retaining the no-I/O fast path for ordinary ASCII.
    paths
        .iter()
        .any(|path| !path.as_os_str().as_encoded_bytes().is_ascii())
        || has_ascii_case_alias(paths)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn has_ascii_case_alias(paths: &[PathBuf]) -> bool {
    let mut seen = BTreeMap::new();
    for path in paths {
        let mut prefix = PathBuf::new();
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            prefix.push(name);
            let folded = ascii_lowercase_path(&prefix);
            if let Some(previous) = seen.insert(folded, prefix.clone())
                && previous != prefix
            {
                return true;
            }
        }
    }
    false
}

#[cfg(unix)]
fn ascii_lowercase_path(path: &Path) -> PathBuf {
    PathBuf::from(OsString::from_vec(
        path.as_os_str()
            .as_bytes()
            .iter()
            .map(u8::to_ascii_lowercase)
            .collect(),
    ))
}

#[cfg(target_os = "windows")]
fn ascii_lowercase_path(path: &Path) -> PathBuf {
    PathBuf::from(OsString::from_wide(
        &path
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                    unit + u16::from(b'a' - b'A')
                } else {
                    unit
                }
            })
            .collect::<Vec<_>>(),
    ))
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
    analyze_resolved_repository_compatibility(git, repository, &resolved)
}

fn analyze_resolved_repository_compatibility(
    git: &Git,
    repository: &Path,
    resolved: &ResolvedRevision,
) -> Result<CompatibilityAnalysis, WorktreeError> {
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
    let mut lfs_paths = Vec::new();

    let info_attributes_path = git.info_attributes_path(repository)?;
    // Only an absent or empty regular file is supported. Inspect its metadata
    // without reading contents: FIFOs must not block, symlinks must not escape
    // this path, and a large unsupported file needs no memory allocation.
    let info_metadata = fs::symlink_metadata(
        info_attributes_path.parent().expect("info directory"),
    )
    .and_then(|metadata| {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "attributes parent is not a real directory",
            ));
        }
        fs::symlink_metadata(&info_attributes_path)
    });
    let info_attributes_safe = match info_metadata {
        Ok(metadata)
            if metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == 0 =>
        {
            true
        }
        Ok(_) => {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::EffectiveAttributes,
                explanation: format!(
                    "repository attributes file {} is not an empty regular file; external attributes are not part of the immutable tree and are not supported yet",
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
        match classify_in_tree_attributes(&in_tree) {
            Ok(paths) => lfs_paths = paths,
            Err(explanation) => {
                blockers.push(RepositoryCompatibilityBlocker {
                    kind: RepositoryCompatibilityBlockerKind::InTreeAttributes,
                    explanation,
                });
            }
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
        profile.update(b"riftri-checkout-profile-v3-lfs\0");
        let git_version = git.detect()?.version;
        hash_profile_input(&mut profile, b"git.version", Some(git_version.as_bytes()));
        profile
    };

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let mut checkout_config = Vec::new();
    let checked_config = [
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
    ];
    let mut config_keys = checked_config
        .iter()
        .map(|(key, _, _)| *key)
        .collect::<Vec<_>>();
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let profile_keys = [
        "core.filemode",
        "core.ignorecase",
        "core.precomposeunicode",
        "core.protecthfs",
        "core.protectntfs",
    ];
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    config_keys.extend(profile_keys);
    let lfs_config_keys = [
        "filter.lfs.clean",
        "filter.lfs.smudge",
        "filter.lfs.process",
        "filter.lfs.required",
        "lfs.storage",
    ];
    if !lfs_paths.is_empty() {
        config_keys.extend(lfs_config_keys);
    }
    let config_values = git.config_values(repository, &config_keys)?;
    for (key, accepted, kind) in checked_config {
        let value = config_values.get(key).cloned();
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        if let Some(value) = &value {
            checkout_config.push((key.to_owned(), value.clone()));
        }
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
        for key in profile_keys {
            let value = config_values.get(key).cloned();
            hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
            if let Some(value) = value {
                checkout_config.push((key.to_owned(), value));
            }
        }
    }
    if !lfs_paths.is_empty() {
        let expected_lfs_config = [
            ("filter.lfs.clean", Some(&b"git-lfs clean -- %f"[..]), false),
            (
                "filter.lfs.smudge",
                Some(&b"git-lfs smudge -- %f"[..]),
                false,
            ),
            (
                "filter.lfs.process",
                Some(&b"git-lfs filter-process"[..]),
                true,
            ),
            ("filter.lfs.required", Some(&b"true"[..]), false),
            ("lfs.storage", None, true),
        ];
        for (key, expected, optional) in expected_lfs_config {
            let value = config_values.get(key).map(Vec::as_slice);
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            hash_profile_input(&mut profile, key.as_bytes(), value);
            let matches = match (value, expected) {
                (None, None) => true,
                (None, Some(_)) => optional,
                (Some(actual), Some(expected)) if key == "filter.lfs.required" => {
                    actual.eq_ignore_ascii_case(expected)
                }
                (Some(actual), Some(expected)) => actual == expected,
                (Some(_), None) => false,
            };
            if !matches {
                blockers.push(RepositoryCompatibilityBlocker {
                    kind: RepositoryCompatibilityBlockerKind::GitLfs,
                    explanation: match value {
                        Some(value) => format!(
                            "Git LFS configuration {key}={} is outside Riftri's deterministic local-object profile",
                            String::from_utf8_lossy(value)
                        ),
                        None => format!(
                            "Git LFS configuration {key} is required for clean-worktree verification"
                        ),
                    },
                });
            }
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let mut lfs_objects = Vec::new();
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if !lfs_paths.is_empty() {
        let lfs_version = git.lfs_version(repository)?;
        hash_profile_input(&mut profile, b"git-lfs.version", lfs_version.as_deref());
        if lfs_version.is_none() {
            blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::GitLfs,
                explanation: "the standard `git lfs` command is unavailable; Riftri cannot prove that the expanded worktree will remain clean"
                    .to_owned(),
            });
        }
        match inspect_git_lfs_objects(git, repository, &entries, &lfs_paths) {
            Ok(objects) => {
                for object in &objects {
                    hash_lfs_profile_object(&mut profile, object);
                }
                lfs_objects = objects;
            }
            Err(explanation) => blockers.push(RepositoryCompatibilityBlocker {
                kind: RepositoryCompatibilityBlockerKind::GitLfs,
                explanation,
            }),
        }
    }
    Ok(CompatibilityAnalysis {
        report: RepositoryCompatibilityReport {
            commit: resolved.commit.clone(),
            tree: resolved.tree.clone(),
            compatible: blockers.is_empty(),
            blockers,
        },
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        checkout_profile: profile.finalize().to_vec(),
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        checkout_paths: paths,
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        checkout_config,
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        lfs_objects,
    })
}

fn classify_in_tree_attributes(attributes: &[GitAttribute]) -> Result<Vec<PathBuf>, String> {
    let mut by_path = BTreeMap::<PathBuf, Vec<&GitAttribute>>::new();
    for attribute in attributes {
        by_path
            .entry(attribute.path.clone())
            .or_default()
            .push(attribute);
    }
    let mut lfs_paths = Vec::new();
    for (path, attributes) in by_path {
        let uses_lfs = attributes.iter().any(|attribute| {
            matches!(attribute.name.as_slice(), b"filter" | b"diff" | b"merge")
                && attribute.value == b"lfs"
        });
        if uses_lfs {
            let required = [
                (&b"filter"[..], &b"lfs"[..]),
                (&b"diff"[..], &b"lfs"[..]),
                (&b"merge"[..], &b"lfs"[..]),
                (&b"text"[..], &b"unset"[..]),
            ];
            let canonical = attributes.len() == required.len()
                && required.iter().all(|(name, value)| {
                    attributes.iter().any(|attribute| {
                        attribute.name.as_slice() == *name && attribute.value.as_slice() == *value
                    })
                });
            if !canonical {
                return Err(format!(
                    "Git LFS attributes for {} must resolve exactly to filter=lfs, diff=lfs, merge=lfs, and -text; mixed or custom filter semantics remain unsupported",
                    path.display()
                ));
            }
            lfs_paths.push(path);
            continue;
        }
        if let Some(attribute) = attributes
            .iter()
            .find(|attribute| !is_supported_in_tree_attribute(attribute))
        {
            return Err(format!(
                "tree attribute {}={} for {} is outside Riftri's deterministic checkout allowlist; custom filters, encodings, ident substitution, legacy, and unknown attributes are not supported yet",
                String::from_utf8_lossy(&attribute.name),
                String::from_utf8_lossy(&attribute.value),
                attribute.path.display(),
            ));
        }
    }
    Ok(lfs_paths)
}

fn is_supported_in_tree_attribute(attribute: &GitAttribute) -> bool {
    match attribute.name.as_slice() {
        b"text" => matches!(attribute.value.as_slice(), b"set" | b"unset" | b"auto"),
        b"eol" => matches!(attribute.value.as_slice(), b"lf" | b"crlf"),
        b"binary" => attribute.value == b"set",
        b"diff" | b"merge" => attribute.value == b"unset",
        name => is_checkout_neutral_metadata_attribute(name, &attribute.value),
    }
}

/// GitHub linguist metadata attributes are read only by hosting-side tooling;
/// Git's checkout machinery (text conversion, eol, smudge/clean filters) never
/// consults them, so they cannot change materialized worktree bytes. Values
/// are restricted to linguist's documented forms so typos and lookalike
/// attributes still fail closed.
fn is_checkout_neutral_metadata_attribute(name: &[u8], value: &[u8]) -> bool {
    match name {
        b"linguist-generated"
        | b"linguist-vendored"
        | b"linguist-documentation"
        | b"linguist-detectable" => {
            matches!(value, b"set" | b"unset" | b"true" | b"false")
        }
        b"linguist-language" => !value.is_empty(),
        _ => false,
    }
}

fn parse_git_lfs_pointer(bytes: &[u8]) -> Result<GitLfsPointer, String> {
    const VERSION: &[u8] = b"version https://git-lfs.github.com/spec/v1";
    let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if lines.len() != 4 || lines[0] != VERSION || !lines[3].is_empty() {
        return Err("Git LFS pointer is not the canonical three-line v1 representation".to_owned());
    }
    let oid = lines[1]
        .strip_prefix(b"oid sha256:")
        .ok_or_else(|| "Git LFS pointer does not contain a sha256 object ID".to_owned())?;
    if oid.len() != 64
        || !oid
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err("Git LFS object ID must be 64 lowercase hexadecimal characters".to_owned());
    }
    let size = lines[2]
        .strip_prefix(b"size ")
        .ok_or_else(|| "Git LFS pointer does not contain an object size".to_owned())?;
    if size.is_empty()
        || !size.iter().all(u8::is_ascii_digit)
        || (size.len() > 1 && size[0] == b'0')
    {
        return Err("Git LFS object size is not canonical unsigned decimal".to_owned());
    }
    let size = std::str::from_utf8(size)
        .expect("ASCII decimal was validated")
        .parse::<u64>()
        .map_err(|error| format!("Git LFS object size is invalid: {error}"))?;
    Ok(GitLfsPointer {
        oid: std::str::from_utf8(oid)
            .expect("ASCII hexadecimal was validated")
            .to_owned(),
        size,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn inspect_git_lfs_objects(
    git: &Git,
    repository: &Path,
    entries: &[riftri_git::TreeEntry],
    paths: &[PathBuf],
) -> Result<Vec<GitLfsObject>, String> {
    const MAX_POINTER_BYTES: u64 = 1024;
    let repository_info = git
        .inspect_repository(repository)
        .map_err(|error| format!("could not locate the Git LFS object store: {error}"))?;
    let mut objects = Vec::with_capacity(paths.len());
    for path in paths {
        let entry = entries
            .iter()
            .find(|entry| entry.path == *path)
            .ok_or_else(|| {
                format!(
                    "Git LFS path {} is absent from the exact tree",
                    path.display()
                )
            })?;
        if entry.object_kind != b"blob" || !matches!(entry.mode, 0o100644 | 0o100755) {
            return Err(format!(
                "Git LFS path {} is not a regular file in the exact tree",
                path.display()
            ));
        }
        let blob_size = git
            .blob_size(repository, &entry.object_id)
            .map_err(|error| {
                format!(
                    "could not inspect Git LFS pointer {}: {error}",
                    path.display()
                )
            })?;
        if blob_size > MAX_POINTER_BYTES {
            return Err(format!(
                "Git LFS pointer {} is {blob_size} bytes; canonical pointers must be at most {MAX_POINTER_BYTES} bytes",
                path.display()
            ));
        }
        let bytes = git
            .read_blob(repository, &entry.object_id)
            .map_err(|error| {
                format!("could not read Git LFS pointer {}: {error}", path.display())
            })?;
        let pointer = parse_git_lfs_pointer(&bytes)
            .map_err(|error| format!("invalid Git LFS pointer {}: {error}", path.display()))?;
        let source_path = repository_info
            .identity
            .common_git_dir
            .join("lfs/objects")
            .join(&pointer.oid[..2])
            .join(&pointer.oid[2..4])
            .join(&pointer.oid);
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| {
            format!(
                "local Git LFS object {} for {} is unavailable at {}: {error}",
                pointer.oid,
                path.display(),
                source_path.display()
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "local Git LFS object {} for {} is not a regular file",
                pointer.oid,
                path.display()
            ));
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::MetadataExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!(
                    "local Git LFS object {} for {} is a reparse point",
                    pointer.oid,
                    path.display()
                ));
            }
        }
        if metadata.len() != pointer.size {
            return Err(format!(
                "local Git LFS object {} for {} has size {}, expected {}",
                pointer.oid,
                path.display(),
                metadata.len(),
                pointer.size
            ));
        }
        objects.push(GitLfsObject {
            checkout_path: path.clone(),
            source_path,
            pointer,
        });
    }
    Ok(objects)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn hash_lfs_profile_object(hasher: &mut Sha256, object: &GitLfsObject) {
    #[cfg(unix)]
    let path = object.checkout_path.as_os_str().as_bytes().to_vec();
    #[cfg(target_os = "windows")]
    let path = object
        .checkout_path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    hash_profile_input(hasher, b"git-lfs.path", Some(&path));
    hash_profile_input(hasher, b"git-lfs.oid", Some(object.pointer.oid.as_bytes()));
    hash_profile_input(
        hasher,
        b"git-lfs.size",
        Some(&object.pointer.size.to_le_bytes()),
    );
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
fn supported_worktree_backend(path: &Path) -> Result<SelectedBackend, WorktreeError> {
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
    let (capability, overlayfs_profile) = {
        let reflink = riftri_storage::ReflinkCloner::probe(path);
        match reflink.status {
            CapabilityStatus::Supported => (reflink, None),
            CapabilityStatus::Unavailable => {
                return Err(WorktreeError::Unsupported(format!(
                    "Linux reflink capability could not be established, so Riftri did not downgrade to another backend: {}",
                    reflink.explanation
                )));
            }
            CapabilityStatus::Unsupported => {
                let (overlayfs, profile) = OverlayFsMounter::probe_activation(path);
                if overlayfs.status == CapabilityStatus::Supported {
                    (overlayfs, profile)
                } else {
                    return Err(WorktreeError::Unsupported(format!(
                        "no native Linux worktree backend is available: {}; {}",
                        reflink.explanation, overlayfs.explanation
                    )));
                }
            }
        }
    };
    #[cfg(target_os = "windows")]
    let capability = riftri_storage::RefsBlockCloner::probe(path);
    if capability.status != CapabilityStatus::Supported {
        return Err(WorktreeError::Unsupported(capability.explanation));
    }
    let volume = capability.volume.ok_or_else(|| {
        WorktreeError::Unsupported(
            "native COW capability did not include a volume identity".to_owned(),
        )
    })?;
    Ok(SelectedBackend {
        kind: capability.kind,
        volume,
        #[cfg(target_os = "linux")]
        overlayfs_profile,
    })
}

pub(crate) fn destination_backend_readiness(
    path: &Path,
) -> Result<DestinationBackendReadiness, WorktreeError> {
    let selected = supported_worktree_backend(path)?;
    Ok(DestinationBackendReadiness {
        kind: selected.kind,
        #[cfg(target_os = "linux")]
        overlayfs_helper_required: selected.kind == BackendKind::OverlayFs
            && selected.overlayfs_profile == Some(OverlayFsMountProfile::PrivilegedTrustedXattr),
        #[cfg(not(target_os = "linux"))]
        overlayfs_helper_required: false,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn inspected_native_cow_volume(
    path: &Path,
    backend: BackendKind,
) -> Result<DestinationVolume, WorktreeError> {
    #[cfg(target_os = "linux")]
    let capability = riftri_storage::ReflinkCloner::probe(path);
    #[cfg(not(target_os = "linux"))]
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
    if backend == BackendKind::OverlayFs {
        if volume.read_only {
            return Err(WorktreeError::Unsupported(
                "OverlayFS state must be on a writable filesystem".to_owned(),
            ));
        }
        return Ok(volume);
    }
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

fn resolve_real_state_directory(path: &Path) -> Result<PathBuf, WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect Riftri state directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::InvalidStateDirectory {
            path: path.to_path_buf(),
        }
        .into());
    }
    fs::canonicalize(path).map_err(|source| io("resolve state directory", path, source))
}

fn resolve_real_state_directory_if_present(path: &Path) -> Result<Option<PathBuf>, WorktreeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::InvalidStateDirectory {
                    path: path.to_path_buf(),
                }
                .into());
            }
            fs::canonicalize(path)
                .map(Some)
                .map_err(|source| io("resolve state directory", path, source))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io("inspect Riftri state directory", path, source)),
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
        state_directory.join("compactions"),
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
    acquire_coordination_lock(
        &lock_path,
        "open Git worktree metadata lock",
        "lock Git worktree metadata",
    )
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
            && !store
                .path_for(&operation_id)
                .with_extension("lock")
                .exists()
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
    // Older buckets have empty completion markers and cannot prove integrity.
    // Leave them available to existing views and explicit GC; new adds use v2.
    hasher.update(b"riftri-verified-base-v2\0");
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
    let adds = JournalStore::open(state_directory).load_all()?;
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed = validated_completed_removal_ids(state_directory, &adds, &removals)?;
    let pending = removals
        .iter()
        .filter(|journal| journal.phase != RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let pending_moves = MoveJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| journal.phase != MoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id)
        .collect::<HashSet<_>>();
    let pending_compactions = CompactJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
            )
        })
        .map(|journal| journal.source_add_operation_id)
        .collect::<HashSet<_>>();
    let mut matches = adds
        .into_iter()
        .filter(|journal| {
            journal.phase == AddWorktreePhase::Active
                && journal.destination == destination
                && !completed.contains(&journal.operation_id)
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
            "a removal of {} is already pending; run `riftri repair --state-dir {}`",
            destination.display(),
            state_directory.display()
        )));
    }
    if managed
        .as_ref()
        .is_some_and(|journal| pending_moves.contains(journal.operation_id.as_str()))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a move of {} is already pending; run `riftri repair --state-dir {}`",
            destination.display(),
            state_directory.display()
        )));
    }
    if managed
        .as_ref()
        .is_some_and(|journal| pending_compactions.contains(journal.operation_id.as_str()))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a compaction of {} is already pending; run `riftri repair --state-dir {}`",
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
#[allow(clippy::too_many_arguments)]
fn diagnose_state_paths(
    state_directory: &Path,
    add_journals: &[DecodedJournal],
    removal_journals: &[DecodedRemovalJournal],
    move_journals: &[DecodedMoveJournal],
    compact_journals: &[DecodedCompactJournal],
    prune_journals: &[DecodedPruneJournal],
    collection_journals: &[DecodedCollectionJournal],
    journal_issues: &[StateDiagnosticIssue],
) -> Result<StatePathDiagnosis, WorktreeError> {
    if !state_directory.exists() {
        return Ok(StatePathDiagnosis {
            issues: journal_issues.to_vec(),
            ..StatePathDiagnosis::default()
        });
    }

    let mut issues = journal_issues.to_vec();
    let expected_roots = [
        "bases",
        "overlays",
        "operations",
        "removals",
        "moves",
        "compactions",
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
            .flat_map(|journal| {
                [
                    journal.journal_path.clone(),
                    journal.journal_path.with_extension("lock"),
                ]
            })
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
        &state_directory.join("compactions"),
        compact_journals
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
    let pending_compactions = compact_journals
        .iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
            )
        })
        .collect::<Vec<_>>();
    diagnose_temporary_directory(
        &state_directory.join("tmp"),
        pending_base_builds
            .iter()
            .map(|journal| journal.temporary_index.clone())
            .chain(
                add_journals
                    .iter()
                    .filter(|journal| journal.phase != AddWorktreePhase::RolledBack)
                    .map(pointer_staging_path),
            )
            .chain(
                pending_compactions
                    .iter()
                    .map(|journal| journal.temporary_index.clone()),
            )
            .collect(),
        &mut issues,
    )?;
    let mut coordination_locks = 0;
    for journal in add_journals {
        if is_regular_file_if_present(&journal.journal_path.with_extension("lock"))? {
            coordination_locks += 1;
        }
    }
    diagnose_base_directories(
        state_directory,
        collection_journals,
        &pending_base_builds,
        &pending_compactions,
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
    _compact_journals: &[DecodedCompactJournal],
    _prune_journals: &[DecodedPruneJournal],
    _collection_journals: &[DecodedCollectionJournal],
    journal_issues: &[StateDiagnosticIssue],
) -> Result<StatePathDiagnosis, WorktreeError> {
    Ok(StatePathDiagnosis {
        issues: journal_issues.to_vec(),
        ..StatePathDiagnosis::default()
    })
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
            if !issues.iter().any(|issue| issue.path == path) {
                add_state_issue(
                    issues,
                    path,
                    "not a recognized durable operation journal; Riftri will preserve it",
                );
            }
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
    pending_compactions: &[&DecodedCompactJournal],
    issues: &mut Vec<StateDiagnosticIssue>,
    coordination_locks: &mut usize,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases");
    if !is_real_directory_if_present(&bases)? {
        return Ok(());
    }
    for path in child_paths(&bases, "read immutable-base layout")? {
        if path != bases.join("v1") {
            add_state_issue(
                issues,
                path,
                "not part of the supported immutable-base layout version",
            );
        }
    }

    let root = bases.join("v1");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            add_state_issue(
                issues,
                root,
                "immutable-base layout root must be a real directory",
            );
            return Ok(());
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io("inspect immutable-base root", &root, source)),
    }
    let pending_staging = pending_base_builds
        .iter()
        .map(|journal| journal.base_staging.clone())
        .chain(
            pending_compactions
                .iter()
                .map(|journal| journal.base_staging.clone()),
        )
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum UnsafeBaseInventory {
    Ignore,
    Reject,
}

fn symlinked_base_parent_error(state_directory: &Path, directory: &Path) -> WorktreeError {
    WorktreeError::InvalidRequest(format!(
        "cleanup stopped: immutable-base path {} is a symbolic link, not a real directory.\n\
         Following it could access data outside Riftri's expected storage layout. \
         Riftri did not follow this link or delete data through it.\n\
         Next: run `riftri status --state-dir <STATE_DIR>` to inspect the affected state, \
         replacing <STATE_DIR> with your state directory (quote paths in your shell).\n\
         State directory: {}\n\
         Do not delete or move the linked data manually. For new worktrees, use --state-dir \
         with a real directory; this does not repair an existing redirected layout.",
        directory.display(),
        state_directory.display(),
    ))
}

fn retained_base_paths(
    state_directory: &Path,
    unsafe_inventory: UnsafeBaseInventory,
) -> Result<Vec<PathBuf>, WorktreeError> {
    let root = state_directory.join("bases/v1");
    for directory in [&state_directory.join("bases"), &root] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) if unsafe_inventory == UnsafeBaseInventory::Ignore => return Ok(Vec::new()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(symlinked_base_parent_error(state_directory, directory));
            }
            Ok(_) => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "immutable-base root {} is not a real directory",
                    directory.display()
                )));
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io("inspect immutable-base root", directory, source)),
        }
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
            if marker.extension() == Some(OsStr::new("complete")) {
                if is_regular_file(&marker)? {
                    bases.push(marker.with_extension(""));
                } else if unsafe_inventory == UnsafeBaseInventory::Reject {
                    return Err(WorktreeError::InvalidRequest(format!(
                        "immutable-base completion marker {} is not a real file",
                        marker.display()
                    )));
                }
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
    let state_directory =
        resolve_real_state_directory_if_present(&state_directory)?.unwrap_or(state_directory);
    let store = JournalStore::open(&state_directory);
    let journals = store.load_all()?;
    let removal_store = RemovalJournalStore::open(&state_directory);
    let loaded_removal_journals = removal_store.load_all()?;
    let move_store = MoveJournalStore::open(&state_directory);
    let move_journals = move_store.load_all()?;
    let compact_store = CompactJournalStore::open(&state_directory);
    let compact_journals = compact_store.load_all()?;
    let prune_store = PruneJournalStore::open(&state_directory);
    let prune_journals = prune_store.load_all()?;
    let collection_store = CollectionJournalStore::open(&state_directory);
    let collection_journals = collection_store.load_all()?;
    let mut removal_journals = Vec::with_capacity(loaded_removal_journals.len());
    let mut invalid_removal_journals = Vec::new();
    for journal in loaded_removal_journals {
        match validate_removal_against_add_journals(&state_directory, &journal, &journals) {
            Ok(()) => removal_journals.push(journal),
            Err(error) => invalid_removal_journals.push((journal.operation_id.clone(), error)),
        }
    }
    let completed_adds = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    #[cfg(target_os = "linux")]
    let removing_adds = removal_journals
        .iter()
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let mut report = RecoveryReport {
        scanned: journals
            .len()
            .saturating_add(removal_journals.len())
            .saturating_add(invalid_removal_journals.len())
            .saturating_add(move_journals.len())
            .saturating_add(compact_journals.len())
            .saturating_add(prune_journals.len())
            .saturating_add(collection_journals.len()),
        ..RecoveryReport::default()
    };
    progress::emit(ProgressEvent::RepairScanned {
        operations: report.scanned,
    });
    let git = Git::default();

    for (operation_id, error) in invalid_removal_journals {
        report
            .errors
            .push(format!("removal operation {operation_id}: {error}"));
    }

    for journal in journals {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        let _operation_lock = match try_lock_add_operation(&journal.journal_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.busy_adds += 1;
                continue;
            }
            Err(error) => {
                report
                    .errors
                    .push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        // The initial inventory may predate the current owner's last write.
        // Reload only after gaining exclusive ownership, then choose a phase.
        let journal = match store.load_operation(&journal.operation_id) {
            Ok(journal) => journal,
            Err(error) => {
                report
                    .errors
                    .push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        match journal.phase {
            AddWorktreePhase::Active => {
                if !completed_adds.contains(journal.operation_id.as_str()) {
                    report.active += 1;
                    #[cfg(target_os = "linux")]
                    if journal.backend == BackendKind::OverlayFs
                        && !removing_adds.contains(journal.operation_id.as_str())
                    {
                        match validate_recovery_paths(&state_directory, &journal)
                            .and_then(|()| recover_active_overlayfs_mount(&store, &journal))
                        {
                            Ok(true) => report.recovered_mounts += 1,
                            Ok(false) => {}
                            Err(error) => report
                                .errors
                                .push(format!("operation {}: {error}", journal.operation_id)),
                        }
                    }
                }
            }
            AddWorktreePhase::RolledBack => {}
            _ => {
                progress::emit(ProgressEvent::RepairRecovering {
                    kind: "add",
                    operation_id: journal.operation_id.clone(),
                });
                if let Err(error) = validate_recovery_paths(&state_directory, &journal)
                    .and_then(|()| adopt_overlayfs_mount_identity(&store, journal.clone()))
                    .and_then(|journal| {
                        let pending =
                            store.update_phase(&journal, AddWorktreePhase::RollbackPending)?;
                        rollback_decoded(&git, &journal)?;
                        store.update_phase(&pending, AddWorktreePhase::RolledBack)?;
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
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "removal",
            operation_id: journal.operation_id.clone(),
        });
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
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "move",
            operation_id: journal.operation_id.clone(),
        });
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
    for journal in compact_journals {
        match journal.phase {
            CompactWorktreePhase::Complete => {
                report.completed_compactions += 1;
                continue;
            }
            CompactWorktreePhase::Cancelled => continue,
            _ => {}
        }
        let add = match JournalStore::open(&state_directory)
            .load_operation(&journal.source_add_operation_id)
        {
            Ok(add) => add,
            Err(error) => {
                report.errors.push(format!(
                    "compaction operation {}: {error}",
                    journal.operation_id
                ));
                continue;
            }
        };
        let _operation_lock = match try_lock_add_operation(&add.journal_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.busy_adds += 1;
                continue;
            }
            Err(error) => {
                report.errors.push(format!(
                    "compaction operation {}: {error}",
                    journal.operation_id
                ));
                continue;
            }
        };
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "compaction",
            operation_id: journal.operation_id.clone(),
        });
        if let Err(error) = resume_compaction(&git, &compact_store, journal.clone(), None) {
            report.errors.push(format!(
                "compaction operation {}: {error}",
                journal.operation_id
            ));
        } else {
            let phase = compact_store
                .load_all()?
                .into_iter()
                .find(|candidate| candidate.operation_id == journal.operation_id)
                .map(|candidate| candidate.phase);
            if phase == Some(CompactWorktreePhase::Complete) {
                report.recovered_compactions += 1;
                report.completed_compactions += 1;
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    for journal in prune_journals {
        if journal.phase == PruneWorktreesPhase::Complete {
            report.completed_prunes += 1;
            continue;
        }
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "prune",
            operation_id: journal.operation_id.clone(),
        });
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
                progress::emit(ProgressEvent::RepairRecovering {
                    kind: "garbage-collection",
                    operation_id: journal.operation_id.clone(),
                });
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
    let adds = JournalStore::open(state_directory).load_all()?;
    validate_removal_against_add_journals(state_directory, journal, &adds)
}

fn validated_completed_removal_ids(
    state_directory: &Path,
    add_journals: &[DecodedJournal],
    removal_journals: &[DecodedRemovalJournal],
) -> Result<HashSet<String>, WorktreeError> {
    let mut completed = HashSet::new();
    for journal in removal_journals {
        validate_removal_against_add_journals(state_directory, journal, add_journals)?;
        if journal.phase == RemoveWorktreePhase::Complete {
            completed.insert(journal.source_add_operation_id.clone());
        }
    }
    Ok(completed)
}

fn validate_removal_against_add_journals(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
    add_journals: &[DecodedJournal],
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
    let source = add_journals
        .iter()
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
    validate_recovery_paths(state_directory, source)
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
    let managed = JournalStore::open(&state_directory)
        .load_all()?
        .into_iter()
        .find(|candidate| candidate.operation_id == journal.source_add_operation_id)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "removal journal {} does not reference a known add operation",
                journal.journal_path.display()
            ))
        })?;
    let mut record = store.reload(&journal)?;

    let metadata_lock = if record.phase <= RemoveWorktreePhase::CleanVerified {
        Some(acquire_git_worktree_metadata_lock_for_repository(
            git,
            &journal.repository,
        )?)
    } else {
        None
    };

    restore_staged_git_pointer(&managed)?;

    if record.phase == RemoveWorktreePhase::IntentRecorded {
        verify_recoverable_removal(git, &journal, &managed)?;
        record.transition(RemoveWorktreePhase::CleanVerified)?;
        store.persist(&record)?;
    }

    if record.phase == RemoveWorktreePhase::CleanVerified {
        let (registered, destination_exists) = removal_presence(git, &journal)?;
        if registered {
            let safe = if !destination_exists {
                true
            } else if journal.force {
                managed_worktree_matches_force_snapshot(
                    &managed,
                    journal.force_snapshot.as_deref().ok_or_else(|| {
                        WorktreeError::InvalidRequest(
                            "forced removal journal has no content snapshot".to_owned(),
                        )
                    })?,
                )?
            } else {
                managed_worktree_is_clean_for_removal(
                    git,
                    &managed,
                    journal.overlayfs_clean_snapshot.as_deref(),
                )?
            };
            if !safe {
                let reason = if journal.force {
                    "changed after forced removal intent"
                } else {
                    "has changes"
                };
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree {} {reason}; recovery preserved it",
                    journal.destination.display(),
                )));
            }
            remove_managed_worktree_files(
                git,
                &journal.repository,
                &journal.destination,
                &managed,
                journal.overlayfs_clean_snapshot.as_deref(),
                journal.force,
                journal.force_snapshot.as_deref(),
            )?;
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
        remove_file_if_present(&pointer_staging_path(&managed))?;
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
    let mut record = store.reload(&journal)?;

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
fn resume_compaction(
    git: &Git,
    store: &CompactJournalStore,
    journal: DecodedCompactJournal,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "compaction")?;
    validate_compaction_paths(&state_directory, &journal)?;
    let mut record = store.reload(&journal)?;

    if matches!(
        record.phase,
        CompactWorktreePhase::IntentRecorded | CompactWorktreePhase::ReplacementReady
    ) {
        let destination_exists = journal.destination.exists();
        let replacement_exists = journal.replacement.exists();
        let quarantine_exists = journal.quarantine.exists();

        if destination_exists && quarantine_exists && !replacement_exists {
            if record.phase == CompactWorktreePhase::IntentRecorded {
                return Err(WorktreeError::InvalidRequest(format!(
                    "compaction journal {} has an activated filesystem state before its replacement-ready phase",
                    journal.journal_path.display()
                )));
            }
            advance_compaction(
                store,
                &mut record,
                CompactWorktreePhase::ReplacementActivated,
                fail_after,
            )?;
        } else if record.phase == CompactWorktreePhase::IntentRecorded
            && destination_exists
            && !quarantine_exists
        {
            remove_tree_if_present(&journal.replacement)?;
            remove_tree_if_present(&journal.base_staging)?;
            remove_file_if_present(&journal.temporary_index)?;
            advance_compaction(
                store,
                &mut record,
                CompactWorktreePhase::Cancelled,
                fail_after,
            )?;
            return Ok(());
        } else if !destination_exists && quarantine_exists && replacement_exists {
            verify_snapshot(&journal.quarantine, &journal.expected_snapshot)?;
            remove_tree_if_present(&journal.replacement)?;
            fs::rename(&journal.quarantine, &journal.destination).map_err(|source| {
                io(
                    "restore original worktree after interrupted compaction",
                    &journal.destination,
                    source,
                )
            })?;
            sync_parent(&journal.destination)?;
            advance_compaction(
                store,
                &mut record,
                CompactWorktreePhase::Cancelled,
                fail_after,
            )?;
            return Ok(());
        } else if record.phase == CompactWorktreePhase::ReplacementReady
            && destination_exists
            && replacement_exists
            && !quarantine_exists
        {
            let _metadata_lock =
                acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
            let commit = ObjectId::parse(journal.expected_commit.clone())?;
            verify_compaction_source(
                git,
                &journal.repository,
                &journal.destination,
                &commit,
                Some(&journal.expected_snapshot),
            )?;
            verify_snapshot(&journal.replacement, &journal.expected_snapshot)?;
            fs::rename(&journal.destination, &journal.quarantine).map_err(|source| {
                io(
                    "quarantine original worktree for compaction",
                    &journal.quarantine,
                    source,
                )
            })?;
            sync_parent(&journal.quarantine)?;
            if let Err(source) = fs::rename(&journal.replacement, &journal.destination) {
                let rollback = fs::rename(&journal.quarantine, &journal.destination);
                return match rollback {
                    Ok(()) => Err(io(
                        "activate compacted worktree",
                        &journal.destination,
                        source,
                    )),
                    Err(rollback) => Err(WorktreeError::OperationAndRollback {
                        operation: io("activate compacted worktree", &journal.destination, source)
                            .to_string(),
                        rollback: io("restore original worktree", &journal.destination, rollback)
                            .to_string(),
                    }),
                };
            }
            sync_parent(&journal.destination)?;
            advance_compaction(
                store,
                &mut record,
                CompactWorktreePhase::ReplacementActivated,
                fail_after,
            )?;
        } else {
            return Err(WorktreeError::InvalidRequest(format!(
                "compaction journal {} does not match a recoverable filesystem state; all paths were preserved",
                journal.journal_path.display()
            )));
        }
    }

    if record.phase == CompactWorktreePhase::ReplacementActivated {
        if journal.replacement.exists()
            || !journal.destination.is_dir()
            || !journal.quarantine.is_dir()
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "compaction journal {} says its replacement is active, but the paths disagree",
                journal.journal_path.display()
            )));
        }
        git.refresh_worktree_index(&journal.destination)?;
        if !git
            .list_worktrees(&journal.repository)?
            .into_iter()
            .any(|worktree| paths_match(&worktree.path, &journal.destination))
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "compacted worktree {} is no longer registered; its quarantine was preserved",
                journal.destination.display()
            )));
        }
        let add_store = JournalStore::open(&state_directory);
        let managed = add_store
            .load_all()?
            .into_iter()
            .find(|candidate| candidate.operation_id == journal.source_add_operation_id)
            .ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "compaction journal {} does not reference a known add operation",
                    journal.journal_path.display()
                ))
            })?;
        add_store.update_active_base(
            &managed.journal_path,
            &journal.destination,
            &journal.old_base_path,
            &journal.base_path,
            &journal.expected_commit,
        )?;
        advance_compaction(
            store,
            &mut record,
            CompactWorktreePhase::AddJournalUpdated,
            fail_after,
        )?;
    }

    if record.phase == CompactWorktreePhase::AddJournalUpdated {
        if journal.replacement.exists()
            || !journal.destination.is_dir()
            || !git
                .list_worktrees(&journal.repository)?
                .into_iter()
                .any(|worktree| paths_match(&worktree.path, &journal.destination))
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "compacted worktree {} is missing or unregistered; its quarantine was preserved",
                journal.destination.display()
            )));
        }
        if journal.quarantine.exists() {
            verify_snapshot(&journal.quarantine, &journal.expected_snapshot)?;
            remove_tree_if_present(&journal.quarantine)?;
            sync_parent(&journal.quarantine)?;
        }
        advance_compaction(
            store,
            &mut record,
            CompactWorktreePhase::Complete,
            fail_after,
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_compaction_paths(
    state_directory: &Path,
    journal: &DecodedCompactJournal,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases/v1");
    let temporary = state_directory.join("tmp");
    let destination_parent = journal.destination.parent();
    let replacement_name = format!(".riftri-compact-new-{}", journal.operation_id);
    let quarantine_name = format!(".riftri-compact-old-{}", journal.operation_id);
    if !journal.repository.is_absolute()
        || !journal.destination.is_absolute()
        || journal.replacement.parent() != destination_parent
        || journal.quarantine.parent() != destination_parent
        || journal.replacement.file_name() != Some(OsStr::new(&replacement_name))
        || journal.quarantine.file_name() != Some(OsStr::new(&quarantine_name))
        || journal.base_path.parent().and_then(Path::parent) != Some(bases.as_path())
        || journal.old_base_path.parent().and_then(Path::parent) != Some(bases.as_path())
        || journal.base_staging.parent() != journal.base_path.parent()
        || !journal
            .base_staging
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".riftri-build-"))
        || journal.temporary_index.parent() != Some(temporary.as_path())
        || journal.backend == BackendKind::OverlayFs
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    let source =
        JournalStore::open(state_directory).load_operation(&journal.source_add_operation_id)?;
    let expected_base = if matches!(
        journal.phase,
        CompactWorktreePhase::AddJournalUpdated | CompactWorktreePhase::Complete
    ) {
        &journal.base_path
    } else if source.base_path == journal.old_base_path || source.base_path == journal.base_path {
        &source.base_path
    } else {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} does not match its add journal's immutable base",
            journal.journal_path.display()
        )));
    };
    if source.phase != AddWorktreePhase::Active
        || source.repository != journal.repository
        || source.destination != journal.destination
        || source.backend != journal.backend
        || source.base_path.as_path() != expected_base.as_path()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} does not match its active add operation",
            journal.journal_path.display()
        )));
    }
    validate_recovery_paths(state_directory, &source)?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_compaction_source(
    git: &Git,
    repository: &Path,
    destination: &Path,
    expected_commit: &ObjectId,
    expected_snapshot: Option<&str>,
) -> Result<(), WorktreeError> {
    verify_compaction_registration(git, repository, destination, expected_commit)?;
    if !git.worktree_is_pristine(destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} has tracked, untracked, or ignored entries; compaction preserved it",
            destination.display()
        )));
    }
    if let Some(expected_snapshot) = expected_snapshot {
        verify_snapshot(destination, expected_snapshot)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_compaction_registration(
    git: &Git,
    repository: &Path,
    destination: &Path,
    expected_commit: &ObjectId,
) -> Result<(), WorktreeError> {
    let registered = git
        .list_worktrees(repository)?
        .into_iter()
        .find(|worktree| paths_match(&worktree.path, destination));
    if registered
        .as_ref()
        .and_then(|worktree| worktree.head.as_ref())
        != Some(expected_commit)
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} is unregistered or its HEAD moved; compaction preserved it",
            destination.display()
        )));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_compaction_checkout_shape(
    destination: &Path,
    checkout_paths: &[PathBuf],
) -> Result<(), WorktreeError> {
    let mut expected = HashSet::new();
    for path in checkout_paths {
        expected.insert(path.clone());
        let mut parent = path.parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            expected.insert(path.to_path_buf());
            parent = path.parent();
        }
    }
    let mut pending = vec![(destination.to_path_buf(), PathBuf::new())];
    while let Some((directory, relative)) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|source| io("inspect compactable worktree", &directory, source))?
        {
            let entry =
                entry.map_err(|source| io("read compactable worktree", &directory, source))?;
            if relative.as_os_str().is_empty() && entry.file_name() == OsStr::new(".git") {
                continue;
            }
            let child = relative.join(entry.file_name());
            if !expected.contains(&child) {
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree {} contains an entry outside its exact Git tree at {}; compaction preserved it",
                    destination.display(),
                    child.display()
                )));
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
                io("inspect compactable worktree entry", &entry.path(), source)
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                pending.push((entry.path(), child));
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn directory_snapshot(path: &Path) -> Result<String, WorktreeError> {
    let marker = crate::base_integrity::marker(path)
        .map_err(|source| io("snapshot managed worktree", path, source))?;
    let mut digest = Sha256::new();
    digest.update(b"riftri-compaction-snapshot-v1\0");
    digest.update(marker);
    #[cfg(unix)]
    hash_extended_attributes(path, &mut digest)?;
    #[cfg(target_os = "windows")]
    hash_windows_attributes(path, &mut digest)?;
    Ok(crate::base_integrity::hex_lower(digest.finalize()))
}

#[cfg(unix)]
fn hash_extended_attributes(path: &Path, digest: &mut Sha256) -> Result<(), WorktreeError> {
    use std::os::unix::ffi::OsStrExt;

    let mut name_buffer: Vec<u8> = Vec::with_capacity(64 * 1024);
    rustix::fs::llistxattr(path, rustix::buffer::spare_capacity(&mut name_buffer))
        .map_err(|source| io("list worktree extended attributes", path, source.into()))?;
    let mut names = name_buffer
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    names.sort_unstable();
    digest.update((names.len() as u64).to_le_bytes());
    for name in names {
        let mut value_buffer: Vec<u8> = Vec::with_capacity(256 * 1024);
        rustix::fs::lgetxattr(
            path,
            OsStr::from_bytes(&name),
            rustix::buffer::spare_capacity(&mut value_buffer),
        )
        .map_err(|source| io("read worktree extended attribute", path, source.into()))?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(&name);
        digest.update((value_buffer.len() as u64).to_le_bytes());
        digest.update(value_buffer);
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect worktree snapshot entry", path, source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut entries = fs::read_dir(path)
            .map_err(|source| io("read worktree snapshot directory", path, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io("read worktree snapshot entry", path, source))?;
        entries.sort_unstable_by_key(|entry| entry.file_name());
        for entry in entries {
            hash_extended_attributes(&entry.path(), digest)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn hash_windows_attributes(path: &Path, digest: &mut Sha256) -> Result<(), WorktreeError> {
    use std::os::windows::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect Windows worktree metadata", path, source))?;
    digest.update(metadata.file_attributes().to_le_bytes());
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut entries = fs::read_dir(path)
            .map_err(|source| io("read Windows worktree snapshot directory", path, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io("read Windows worktree snapshot entry", path, source))?;
        entries.sort_unstable_by_key(|entry| entry.file_name());
        for entry in entries {
            hash_windows_attributes(&entry.path(), digest)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_no_windows_alternate_streams(root: &Path) -> Result<(), WorktreeError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{ERROR_HANDLE_EOF, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };

    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io("inspect Windows worktree stream path", &path, source))?;
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        wide.push(0);
        let mut data = WIN32_FIND_STREAM_DATA::default();
        // SAFETY: `wide` is NUL-terminated and `data` is a valid writable output buffer.
        let handle = unsafe {
            FindFirstStreamW(
                wide.as_ptr(),
                FindStreamInfoStandard,
                (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // SAFETY: this reads the calling thread's error immediately after the failed call.
            let error = unsafe { GetLastError() };
            if error != ERROR_HANDLE_EOF {
                return Err(io(
                    "enumerate Windows worktree streams",
                    &path,
                    std::io::Error::from_raw_os_error(error as i32),
                ));
            }
        } else {
            let mut alternate = false;
            loop {
                let length = data
                    .cStreamName
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(data.cStreamName.len());
                let name = &data.cStreamName[..length];
                let default_stream = name
                    == [
                        b':' as u16,
                        b':' as u16,
                        b'$' as u16,
                        b'D' as u16,
                        b'A' as u16,
                        b'T' as u16,
                        b'A' as u16,
                    ];
                alternate |= !default_stream;
                // SAFETY: `handle` is a live stream enumeration handle and `data` is writable.
                if unsafe {
                    FindNextStreamW(handle, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast())
                } == 0
                {
                    // SAFETY: this reads the calling thread's error immediately after iteration.
                    let error = unsafe { GetLastError() };
                    // SAFETY: `handle` came from a successful `FindFirstStreamW` call.
                    unsafe { FindClose(handle) };
                    if error != ERROR_HANDLE_EOF {
                        return Err(io(
                            "enumerate Windows worktree streams",
                            &path,
                            std::io::Error::from_raw_os_error(error as i32),
                        ));
                    }
                    break;
                }
            }
            if alternate {
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree {} contains an alternate data stream at {}; compaction preserved it",
                    root.display(),
                    path.display()
                )));
            }
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            for entry in fs::read_dir(&path)
                .map_err(|source| io("read Windows worktree stream directory", &path, source))?
            {
                pending.push(
                    entry
                        .map_err(|source| io("read Windows worktree stream entry", &path, source))?
                        .path(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn verify_snapshot(path: &Path, expected: &str) -> Result<(), WorktreeError> {
    if directory_snapshot(path)? != expected {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree content changed during compaction; preserved {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn copy_git_pointer(source: &Path, destination: &Path) -> Result<(), WorktreeError> {
    let source_path = source.join(".git");
    let destination_path = destination.join(".git");
    let mut input = crate::base_integrity::open_regular(&source_path)
        .map_err(|source| io("open linked-worktree pointer", &source_path, source))?;
    let metadata = input
        .metadata()
        .map_err(|source| io("inspect linked-worktree pointer", &source_path, source))?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(target_os = "windows")]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut output = options
        .open(&destination_path)
        .map_err(|source| io("create replacement Git pointer", &destination_path, source))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|source| io("copy linked-worktree pointer", &destination_path, source))?;
    fs::set_permissions(&destination_path, metadata.permissions()).map_err(|source| {
        io(
            "set linked-worktree pointer permissions",
            &destination_path,
            source,
        )
    })?;
    output
        .sync_all()
        .map_err(|source| io("sync linked-worktree pointer", &destination_path, source))
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
    let mut record = store.reload(&journal)?;
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
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed_removals = validated_completed_removal_ids(state_directory, &adds, &removals)?;
    if adds.iter().any(|journal| {
        !matches!(
            journal.phase,
            AddWorktreePhase::Active | AddWorktreePhase::RolledBack
        )
    }) || removals
        .iter()
        .any(|journal| journal.phase != RemoveWorktreePhase::Complete)
        || MoveJournalStore::open(state_directory)
            .load_all()?
            .iter()
            .any(|journal| journal.phase != MoveWorktreePhase::Complete)
        || CompactJournalStore::open(state_directory)
            .load_all()?
            .iter()
            .any(|journal| {
                !matches!(
                    journal.phase,
                    CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                )
            })
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
fn advance_compaction(
    store: &CompactJournalStore,
    record: &mut CompactJournalRecord,
    phase: CompactWorktreePhase,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    record.transition(phase)?;
    store.persist(record)?;
    fail_compaction_if_requested(phase, fail_after)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn fail_compaction_if_requested(
    phase: CompactWorktreePhase,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        Err(WorktreeError::InjectedCompactionFailure(phase))
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
    managed: &DecodedJournal,
) -> Result<(), WorktreeError> {
    let (registered, destination_exists) = removal_presence(git, journal)?;
    if registered && destination_exists {
        let safe = if journal.force {
            managed_worktree_matches_force_snapshot(
                managed,
                journal.force_snapshot.as_deref().ok_or_else(|| {
                    WorktreeError::InvalidRequest(
                        "forced removal journal has no content snapshot".to_owned(),
                    )
                })?,
            )?
        } else {
            managed_worktree_is_clean_for_removal(git, managed, None)?
        };
        if !safe {
            let reason = if journal.force {
                "changed after forced removal intent"
            } else {
                "has changes"
            };
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree {} {reason}; recovery preserved it",
                journal.destination.display(),
            )));
        }
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

fn snapshot_overlayfs_private_layer(
    managed: &DecodedJournal,
) -> Result<Option<String>, WorktreeError> {
    if managed.backend != BackendKind::OverlayFs {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        let overlayfs = managed
            .overlayfs
            .as_ref()
            .ok_or_else(|| changed_rollback_worktree(managed))?;
        overlayfs_layer_snapshot(&overlayfs.layout_root.join("upper")).map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    Err(WorktreeError::Unsupported(
        "OverlayFS snapshots require Linux".to_owned(),
    ))
}

fn snapshot_managed_worktree_for_force(managed: &DecodedJournal) -> Result<String, WorktreeError> {
    if managed.backend == BackendKind::OverlayFs {
        return snapshot_overlayfs_private_layer(managed)?.ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "OverlayFS worktree {} has no private-layer snapshot",
                managed.destination.display()
            ))
        });
    }
    let marker = crate::base_integrity::marker(&managed.destination).map_err(|source| {
        io(
            "snapshot managed worktree before forced removal",
            &managed.destination,
            source,
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(b"riftri-forced-removal-snapshot-v1\0");
    digest.update(marker);
    Ok(crate::base_integrity::hex_lower(digest.finalize()))
}

fn managed_worktree_matches_force_snapshot(
    managed: &DecodedJournal,
    expected: &str,
) -> Result<bool, WorktreeError> {
    if !managed.destination.exists() {
        return Ok(false);
    }
    Ok(snapshot_managed_worktree_for_force(managed)? == expected)
}

#[cfg(target_os = "linux")]
fn overlayfs_layer_snapshot(root: &Path) -> Result<String, WorktreeError> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn visit(path: &Path, digest: &mut Sha256) -> Result<(), WorktreeError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|source| io("inspect OverlayFS removal snapshot", path, source))?;
        // Inode and ctime also detect metadata-only copy-up/xattr changes that
        // could change the merged data without changing raw upper file bytes.
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.mode() as u64,
            metadata.rdev(),
            metadata.size(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            digest.update(value.to_le_bytes());
        }
        if metadata.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|source| io("read OverlayFS removal snapshot", path, source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| io("read OverlayFS snapshot entry", path, source))?;
            entries.sort_unstable_by_key(|entry| entry.file_name());
            for entry in entries {
                let name = entry.file_name();
                digest.update((name.as_bytes().len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
                visit(&entry.path(), digest)?;
            }
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)
                .map_err(|source| io("read OverlayFS snapshot symlink", path, source))?;
            digest.update(target.as_os_str().as_bytes());
        } else if metadata.is_file() {
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .map_err(|source| io("open OverlayFS snapshot file", path, source))?;
            if !file
                .metadata()
                .map_err(|source| io("inspect opened OverlayFS snapshot", path, source))?
                .is_file()
            {
                return Err(WorktreeError::InvalidRequest(
                    "OverlayFS snapshot entry changed type".to_owned(),
                ));
            }
            let mut buffer = [0; 64 * 1024];
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|source| io("read OverlayFS snapshot file", path, source))?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
        }
        // Special entries such as kernel whiteouts are represented by metadata;
        // never open a FIFO or device while inspecting the upper layer.
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(root, &mut digest)?;
    Ok(crate::base_integrity::hex_lower(digest.finalize()))
}

fn managed_worktree_is_clean_for_removal(
    git: &Git,
    managed: &DecodedJournal,
    expected_snapshot: Option<&str>,
) -> Result<bool, WorktreeError> {
    if managed.backend != BackendKind::OverlayFs {
        return git
            .worktree_is_clean(&managed.destination)
            .map_err(WorktreeError::from);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = expected_snapshot;
        Err(WorktreeError::Unsupported(
            "OverlayFS removal recovery requires Linux".to_owned(),
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let overlayfs = managed.overlayfs.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "OverlayFS journal {} has no mount intent",
                managed.journal_path.display()
            ))
        })?;
        if !overlayfs.layout_root.exists() {
            return contains_only_git_pointer(&managed.destination);
        }
        let identity = overlayfs.mount_identity.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "active OverlayFS journal {} has no mount identity",
                managed.journal_path.display()
            ))
        })?;
        let layout = OverlayFsMounter::load(
            &overlayfs.layout_root,
            &managed.base_path,
            &managed.destination,
        )?;
        match OverlayFsMounter::mount_state(&layout, identity)? {
            OverlayFsMountState::Active => git
                .worktree_is_clean(&managed.destination)
                .map_err(WorktreeError::from),
            OverlayFsMountState::Absent if expected_snapshot.is_some() => {
                Ok(Some(overlayfs_layer_snapshot(layout.upper())?.as_str()) == expected_snapshot)
            }
            OverlayFsMountState::Absent => overlayfs_private_layer_is_clean(&layout),
            OverlayFsMountState::DifferentNamespace => Err(WorktreeError::InvalidRequest(format!(
                "OverlayFS worktree {} belongs to a different mount namespace; removal preserved it",
                managed.destination.display()
            ))),
            OverlayFsMountState::Foreign => Err(WorktreeError::InvalidRequest(format!(
                "a foreign mount occupies {}; removal preserved it",
                managed.destination.display()
            ))),
        }
    }
}

fn remove_managed_worktree_files(
    git: &Git,
    repository: &Path,
    destination: &Path,
    managed: &DecodedJournal,
    expected_snapshot: Option<&str>,
    force: bool,
    force_snapshot: Option<&str>,
) -> Result<(), WorktreeError> {
    #[cfg(not(target_os = "linux"))]
    let _ = expected_snapshot;
    if managed.backend != BackendKind::OverlayFs {
        if !force || !destination.exists() {
            return git
                .remove_worktree(repository, destination)
                .map_err(WorktreeError::from);
        }
        #[cfg(test)]
        crate::test_hooks::fire(
            crate::test_hooks::FilesystemRacePoint::ForceRemovalRevalidation,
            destination,
        );
        let expected = force_snapshot.ok_or_else(|| {
            WorktreeError::InvalidRequest("forced removal has no content snapshot".to_owned())
        })?;
        if !managed_worktree_matches_force_snapshot(managed, expected)? {
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree {} changed after forced removal intent; it was preserved",
                destination.display()
            )));
        }
        return git
            .remove_worktree_force(repository, destination)
            .map_err(WorktreeError::from);
    }
    #[cfg(not(target_os = "linux"))]
    return Err(WorktreeError::Unsupported(
        "OverlayFS removal requires Linux".to_owned(),
    ));
    #[cfg(target_os = "linux")]
    {
        let overlayfs = managed.overlayfs.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "OverlayFS journal {} has no mount intent",
                managed.journal_path.display()
            ))
        })?;
        if !overlayfs.layout_root.exists() {
            return remove_pointer_only_worktree(git, repository, managed);
        }
        let identity = overlayfs.mount_identity.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "active OverlayFS journal {} has no mount identity",
                managed.journal_path.display()
            ))
        })?;
        let layout = OverlayFsMounter::load(
            &overlayfs.layout_root,
            &managed.base_path,
            &managed.destination,
        )?;
        // For legacy journals, snapshot before rechecking the still-mounted
        // view. If already unmounted, require the conservative pointer-only gate.
        let local_snapshot = overlayfs_layer_snapshot(layout.upper())?;
        match OverlayFsMounter::mount_state(&layout, identity)? {
            OverlayFsMountState::Active => {
                #[cfg(test)]
                crate::test_hooks::fire(
                    crate::test_hooks::FilesystemRacePoint::ForceRemovalRevalidation,
                    destination,
                );
                let safe = if force {
                    let current_snapshot = overlayfs_layer_snapshot(layout.upper())?;
                    force_snapshot.is_some_and(|expected| current_snapshot == expected)
                } else {
                    git.worktree_is_clean(destination)?
                };
                if !safe {
                    return Err(changed_rollback_worktree(managed));
                }
                OverlayFsMounter::unmount(&layout, identity)?;
            }
            OverlayFsMountState::Absent => {
                let safe = if force {
                    force_snapshot.is_some_and(|expected| local_snapshot == expected)
                } else {
                    expected_snapshot.is_some() || overlayfs_private_layer_is_clean(&layout)?
                };
                if !safe {
                    return Err(changed_rollback_worktree(managed));
                }
            }
            OverlayFsMountState::DifferentNamespace => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} belongs to a different mount namespace; removal preserved it",
                    destination.display()
                )));
            }
            OverlayFsMountState::Foreign => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "a foreign mount occupies {}; removal preserved it",
                    destination.display()
                )));
            }
        }
        let expected_after_unmount = force_snapshot
            .or(expected_snapshot)
            .unwrap_or(&local_snapshot);
        if overlayfs_layer_snapshot(layout.upper())? != expected_after_unmount {
            return Err(changed_rollback_worktree(managed));
        }
        restore_overlayfs_pointer(&layout)?;
        OverlayFsMounter::remove_private_layers(&layout, identity)?;
        remove_pointer_only_worktree(git, repository, managed)?;
        Ok(())
    }
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
            let context_valid = overlayfs.mount_context.as_ref().is_none_or(|context| {
                !context.boot_id.is_empty() && context.mount_namespace_inode != 0
            });
            let identity_valid = overlayfs.mount_identity.as_ref().is_none_or(|identity| {
                !identity.boot_id.is_empty()
                    && identity.mount_namespace_inode != 0
                    && identity.mount_id != 0
            });
            overlayfs.layout_root.parent() == Some(overlay_root.as_path())
                && overlayfs.layout_root.file_name() == Some(OsStr::new(&journal.operation_id))
                && overlayfs.recovery_token.len() == 64
                && context_valid
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
    let expected_branch_target = if journal.last_forward_phase
        >= AddWorktreePhase::GitMetadataCreated
    {
        journal
                .branch
                .as_ref()
                .map(|branch| {
                    let expected = ObjectId::parse(journal.expected_commit.clone())?;
                    let current = git.local_branch_target(&journal.repository, branch)?;
                    if current.as_ref().is_some_and(|current| current != &expected) {
                        let recoverable_creation_race = !journal.branch_created
                            && journal.last_forward_phase
                                == AddWorktreePhase::GitMetadataCreated
                            && journal.destination.exists()
                            && contains_only_git_pointer(&journal.destination)?;
                        if !recoverable_creation_race {
                            return Err(WorktreeError::InvalidRequest(format!(
                                "branch {} moved after creation; recovery preserved its worktree",
                                branch.to_string_lossy()
                            )));
                        }
                    }
                    if !journal.branch_created && current.is_none() {
                        return Err(WorktreeError::InvalidRequest(format!(
                            "existing branch {} disappeared after creation; recovery preserved its worktree",
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
        .find(|worktree| paths_match(&worktree.path, &journal.destination));

    if let Some(worktree) = registered {
        let expected = ObjectId::parse(journal.expected_commit.clone())?;
        let expected_worktree_head = expected_branch_target
            .as_ref()
            .and_then(|(_, target)| target.as_ref())
            .unwrap_or(&expected);
        let expected_branch = journal.branch.as_ref().map(|branch| {
            let mut reference = b"refs/heads/".to_vec();
            reference.extend_from_slice(branch.as_encoded_bytes());
            reference
        });
        if worktree.head.as_ref() != Some(expected_worktree_head)
            || worktree.branch != expected_branch
            || worktree.detached != journal.branch.is_none()
            || worktree.bare
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree HEAD changed after creation; recovery preserved {}",
                journal.destination.display()
            )));
        }
        restore_staged_git_pointer(journal)?;
        if journal.backend == BackendKind::OverlayFs {
            #[cfg(target_os = "linux")]
            rollback_overlayfs_worktree(git, journal)?;
            #[cfg(not(target_os = "linux"))]
            return Err(WorktreeError::Unsupported(
                "OverlayFS recovery requires Linux".to_owned(),
            ));
        } else {
            restore_pointer_for_rollback(journal)?;
            remove_registered_worktree_for_rollback(git, journal)?;
            remove_empty_directory_if_present(&journal.destination)?;
        }
    } else if journal.destination.exists() {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} exists but is not registered by Git; recovery preserved it",
            journal.destination.display()
        )));
    }

    remove_tree_if_present(&journal.scratch)?;
    remove_tree_if_present(&journal.base_staging)?;
    remove_file_if_present(&journal.temporary_index)?;
    remove_file_if_present(&pointer_staging_path(journal))?;

    if journal.branch_created
        && let Some((branch, Some(_))) = expected_branch_target
    {
        git.delete_branch_force(&journal.repository, branch)?;
    }
    drop(metadata_lock);
    Ok(())
}

fn remove_registered_worktree_for_rollback(
    git: &Git,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    if !journal.destination.exists() || contains_only_git_pointer(&journal.destination)? {
        remove_pointer_only_worktree(git, &journal.repository, journal)?;
    } else if git.worktree_is_clean(&journal.destination)? {
        remove_worktree_for_rollback(git, &journal.repository, &journal.destination)?;
    } else if view_matches_base(&journal.base_path, &journal.destination)? {
        git.synchronize_worktree_index(&journal.destination)?;
        if !git.worktree_is_clean(&journal.destination)? {
            return Err(changed_rollback_worktree(journal));
        }
        remove_worktree_for_rollback(git, &journal.repository, &journal.destination)?;
    } else {
        return Err(changed_rollback_worktree(journal));
    }
    Ok(())
}

fn pointer_staging_path(journal: &DecodedJournal) -> PathBuf {
    journal.temporary_index.with_extension("git-pointer")
}

fn move_pointer_without_replacement(
    source: &Path,
    destination: &Path,
) -> Result<(), WorktreeError> {
    let mut pointer = tempfile::TempPath::try_from_path(source.to_path_buf())
        .map_err(|error| io("stage linked-worktree pointer", source, error))?;
    // A failed move must leave this journal-owned pointer available to repair.
    pointer.disable_cleanup(true);
    pointer.persist_noclobber(destination).map_err(|error| {
        io(
            "move linked-worktree pointer without replacement",
            destination,
            error.error,
        )
    })?;
    sync_parent(source)?;
    sync_parent(destination)
}

fn restore_staged_git_pointer(journal: &DecodedJournal) -> Result<(), WorktreeError> {
    let staged = pointer_staging_path(journal);
    if !is_regular_file_if_present(&staged)? || !journal.destination.exists() {
        return Ok(());
    }
    let pointer = journal.destination.join(".git");
    if is_regular_file_if_present(&pointer)? && files_equal(&staged, &pointer)? {
        // A crash may leave both links during the no-replace move.
        return remove_file_if_present(&staged);
    }
    move_pointer_without_replacement(&staged, &pointer)
}

fn remove_pointer_only_worktree(
    git: &Git,
    repository: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    restore_staged_git_pointer(journal)?;
    let staged = pointer_staging_path(journal);
    if journal.destination.exists() {
        if !contains_only_git_pointer(&journal.destination)? {
            return Err(changed_rollback_worktree(journal));
        }
        move_pointer_without_replacement(&journal.destination.join(".git"), &staged)?;
        if let Err(source) = fs::remove_dir(&journal.destination) {
            restore_staged_git_pointer(journal)?;
            return Err(io(
                "remove empty pointer-only worktree",
                &journal.destination,
                source,
            ));
        }
        sync_parent(&journal.destination)?;
    }
    // Git can remove a missing directory without force. If a writer recreates
    // it first, Git's ordinary safety checks apply to that new directory.
    if let Err(error) = remove_worktree_for_rollback(git, repository, &journal.destination) {
        restore_staged_git_pointer(journal)?;
        return Err(error);
    }
    remove_file_if_present(&staged)
}

fn remove_worktree_for_rollback(
    git: &Git,
    repository: &Path,
    destination: &Path,
) -> Result<(), WorktreeError> {
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::RollbackGitRemoval,
        destination,
    );
    git.remove_worktree(repository, destination)?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn adopt_overlayfs_mount_identity(
    store: &JournalStore,
    journal: DecodedJournal,
) -> Result<DecodedJournal, WorktreeError> {
    if journal.backend != BackendKind::OverlayFs {
        return Ok(journal);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = store;
        Err(WorktreeError::Unsupported(
            "OverlayFS recovery requires Linux".to_owned(),
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let overlayfs = journal.overlayfs.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "OverlayFS journal {} has no mount intent",
                journal.journal_path.display()
            ))
        })?;
        if overlayfs.mount_identity.is_some() || !overlayfs.layout_root.exists() {
            return Ok(journal);
        }
        let context = overlayfs.mount_context.as_ref().ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "OverlayFS journal {} predates recoverable mount contexts; recovery preserved its state",
                journal.journal_path.display()
            ))
        })?;
        let layout = OverlayFsMounter::load(
            &overlayfs.layout_root,
            &journal.base_path,
            &journal.destination,
        )?;
        match OverlayFsMounter::recover_mount(&layout, context, &overlayfs.recovery_token)? {
            OverlayFsRecoveryState::Mounted(identity) => {
                Ok(store.record_overlayfs_mount_identity(&journal, identity)?)
            }
            OverlayFsRecoveryState::Absent | OverlayFsRecoveryState::Prepared => Ok(journal),
            OverlayFsRecoveryState::DifferentNamespace => {
                Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} belongs to a different boot or mount namespace; recovery preserved it",
                    journal.destination.display()
                )))
            }
            OverlayFsRecoveryState::Foreign => Err(WorktreeError::InvalidRequest(format!(
                "a foreign mount occupies {}; recovery preserved it",
                journal.destination.display()
            ))),
        }
    }
}

#[cfg(target_os = "linux")]
fn recover_active_overlayfs_mount(
    store: &JournalStore,
    journal: &DecodedJournal,
) -> Result<bool, WorktreeError> {
    let overlayfs = journal.overlayfs.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} has no mount intent",
            journal.journal_path.display()
        ))
    })?;
    let persisted_context = overlayfs.mount_context.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} predates recoverable mount contexts; recovery preserved its state",
            journal.journal_path.display()
        ))
    })?;
    let layout = if overlayfs.mount_identity.is_some() {
        OverlayFsMounter::load(
            &overlayfs.layout_root,
            &journal.base_path,
            &journal.destination,
        )?
    } else {
        OverlayFsMounter::load_for_remount(
            &overlayfs.layout_root,
            &journal.base_path,
            &journal.destination,
        )?
    };
    let current_context = OverlayFsMounter::current_mount_context_for(persisted_context.profile)?;

    let remount_journal = if let Some(identity) = overlayfs.mount_identity.as_ref() {
        match OverlayFsMounter::mount_state(&layout, identity)? {
            OverlayFsMountState::Active => {
                OverlayFsMounter::clear_recovery(&layout, &overlayfs.recovery_token)?;
                return Ok(false);
            }
            OverlayFsMountState::Absent => store.begin_overlayfs_remount(
                journal,
                persisted_context,
                Some(identity),
                current_context,
            )?,
            OverlayFsMountState::DifferentNamespace => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} is mounted in a different namespace; recovery preserved it",
                    journal.destination.display()
                )));
            }
            OverlayFsMountState::Foreign => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "a foreign mount occupies {}; recovery preserved it",
                    journal.destination.display()
                )));
            }
        }
    } else {
        match OverlayFsMounter::recover_mount(
            &layout,
            persisted_context,
            &overlayfs.recovery_token,
        )? {
            OverlayFsRecoveryState::Mounted(identity) => {
                let recovered = store.record_overlayfs_mount_identity(journal, identity)?;
                let recovered_overlayfs = recovered.overlayfs.as_ref().ok_or_else(|| {
                    WorktreeError::InvalidRequest(format!(
                        "OverlayFS journal {} lost its mount intent during recovery",
                        journal.journal_path.display()
                    ))
                })?;
                OverlayFsMounter::clear_recovery(&layout, &recovered_overlayfs.recovery_token)?;
                return Ok(true);
            }
            OverlayFsRecoveryState::Absent | OverlayFsRecoveryState::Prepared
                if persisted_context.boot_id != current_context.boot_id =>
            {
                store.begin_overlayfs_remount(journal, persisted_context, None, current_context)?
            }
            OverlayFsRecoveryState::Absent | OverlayFsRecoveryState::Prepared => journal.clone(),
            OverlayFsRecoveryState::DifferentNamespace => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} belongs to a different mount namespace; recovery preserved it",
                    journal.destination.display()
                )));
            }
            OverlayFsRecoveryState::Foreign => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "a foreign mount occupies {}; recovery preserved it",
                    journal.destination.display()
                )));
            }
        }
    };

    let remount = remount_journal.overlayfs.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} lost its mount intent during recovery",
            journal.journal_path.display()
        ))
    })?;
    let context = remount.mount_context.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} has no remount context",
            journal.journal_path.display()
        ))
    })?;
    let remount_layout = OverlayFsMounter::load_for_remount(
        &remount.layout_root,
        &remount_journal.base_path,
        &remount_journal.destination,
    )?;
    match OverlayFsMounter::recover_mount(&remount_layout, context, &remount.recovery_token)? {
        OverlayFsRecoveryState::Mounted(identity) => {
            store.record_overlayfs_mount_identity(&remount_journal, identity)?;
        }
        OverlayFsRecoveryState::Absent => {
            OverlayFsMounter::arm_recovery(&remount_layout, &remount.recovery_token)?;
            let identity = OverlayFsMounter::mount_with_profile(&remount_layout, context.profile)?;
            store.record_overlayfs_mount_identity(&remount_journal, identity)?;
        }
        OverlayFsRecoveryState::Prepared => {
            let identity = OverlayFsMounter::mount_with_profile(&remount_layout, context.profile)?;
            store.record_overlayfs_mount_identity(&remount_journal, identity)?;
        }
        OverlayFsRecoveryState::DifferentNamespace => {
            return Err(WorktreeError::InvalidRequest(format!(
                "OverlayFS worktree {} belongs to a different mount namespace; recovery preserved it",
                journal.destination.display()
            )));
        }
        OverlayFsRecoveryState::Foreign => {
            return Err(WorktreeError::InvalidRequest(format!(
                "a foreign mount occupies {}; recovery preserved it",
                journal.destination.display()
            )));
        }
    }
    OverlayFsMounter::clear_recovery(&remount_layout, &remount.recovery_token)?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn rollback_overlayfs_worktree(git: &Git, journal: &DecodedJournal) -> Result<(), WorktreeError> {
    let overlayfs = journal.overlayfs.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} has no mount intent",
            journal.journal_path.display()
        ))
    })?;
    if !overlayfs.layout_root.exists() {
        restore_pointer_for_rollback(journal)?;
        remove_registered_worktree_for_rollback(git, journal)?;
        remove_empty_directory_if_present(&journal.destination)?;
        return Ok(());
    }

    let context = overlayfs.mount_context.as_ref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "OverlayFS journal {} predates recoverable mount contexts; recovery preserved its state",
            journal.journal_path.display()
        ))
    })?;
    let layout = OverlayFsMounter::load(
        &overlayfs.layout_root,
        &journal.base_path,
        &journal.destination,
    )?;
    let identity = if let Some(identity) = overlayfs.mount_identity.clone() {
        match OverlayFsMounter::mount_state(&layout, &identity)? {
            OverlayFsMountState::Active | OverlayFsMountState::Absent => Some(identity),
            OverlayFsMountState::DifferentNamespace => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} belongs to a different mount namespace; recovery preserved it",
                    journal.destination.display()
                )));
            }
            OverlayFsMountState::Foreign => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "a foreign mount occupies {}; recovery preserved it",
                    journal.destination.display()
                )));
            }
        }
    } else {
        match OverlayFsMounter::recover_mount(&layout, context, &overlayfs.recovery_token)? {
            OverlayFsRecoveryState::Mounted(identity) => Some(identity),
            OverlayFsRecoveryState::Absent | OverlayFsRecoveryState::Prepared => None,
            OverlayFsRecoveryState::DifferentNamespace => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} belongs to a different boot or mount namespace; recovery preserved it",
                    journal.destination.display()
                )));
            }
            OverlayFsRecoveryState::Foreign => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "a foreign mount occupies {}; recovery preserved it",
                    journal.destination.display()
                )));
            }
        }
    };

    OverlayFsMounter::clear_recovery(&layout, &overlayfs.recovery_token)?;
    let mut mounted_view_verified = false;
    if let Some(identity) = identity.as_ref()
        && OverlayFsMounter::mount_state(&layout, identity)? == OverlayFsMountState::Active
    {
        let clean_snapshot = overlayfs_layer_snapshot(layout.upper())?;
        let safe_to_remove = git.worktree_is_clean(&journal.destination)?
            || view_matches_base(&journal.base_path, &journal.destination)?;
        if !safe_to_remove {
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree {} has changes; recovery preserved it",
                journal.destination.display()
            )));
        }
        mounted_view_verified = true;
        OverlayFsMounter::unmount(&layout, identity)?;
        if overlayfs_layer_snapshot(layout.upper())? != clean_snapshot {
            return Err(changed_rollback_worktree(journal));
        }
    }
    if !mounted_view_verified && !overlayfs_upper_contains_only_git_pointer(&layout)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "OverlayFS private layer for {} contains changes; recovery preserved it",
            journal.destination.display()
        )));
    }
    restore_overlayfs_pointer(&layout)?;
    match identity.as_ref() {
        Some(identity) => OverlayFsMounter::remove_private_layers(&layout, identity)?,
        None => OverlayFsMounter::remove_unmounted_private_layers(&layout, context)?,
    }
    remove_pointer_only_worktree(git, &journal.repository, journal)?;
    remove_empty_directory_if_present(&journal.destination)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn overlayfs_upper_contains_only_git_pointer(
    layout: &riftri_storage::OverlayFsLayout,
) -> Result<bool, WorktreeError> {
    contains_only_git_pointer(layout.upper())
}

#[cfg(target_os = "linux")]
fn overlayfs_private_layer_is_clean(
    layout: &riftri_storage::OverlayFsLayout,
) -> Result<bool, WorktreeError> {
    let entries = fs::read_dir(layout.upper())
        .map_err(|source| io("inspect OverlayFS private upper", layout.upper(), source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io("read OverlayFS private upper", layout.upper(), source))?;
    if is_regular_file_if_present(&layout.merged().join(".git"))? {
        Ok(entries.is_empty())
    } else {
        contains_only_git_pointer(layout.upper())
    }
}

#[cfg(target_os = "linux")]
fn restore_overlayfs_pointer(
    layout: &riftri_storage::OverlayFsLayout,
) -> Result<(), WorktreeError> {
    let destination_pointer = layout.merged().join(".git");
    if is_regular_file_if_present(&destination_pointer)? {
        return Ok(());
    }
    if fs::read_dir(layout.merged())
        .map_err(|source| {
            io(
                "inspect unmounted OverlayFS destination",
                layout.merged(),
                source,
            )
        })?
        .next()
        .is_some()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "unmounted OverlayFS destination {} is not empty; recovery preserved it",
            layout.merged().display()
        )));
    }
    let upper_pointer = layout.upper().join(".git");
    if !is_regular_file_if_present(&upper_pointer)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "OverlayFS private layer {} has no linked-worktree pointer",
            layout.upper().display()
        )));
    }
    fs::rename(&upper_pointer, &destination_pointer).map_err(|source| {
        io(
            "restore linked-worktree pointer",
            &destination_pointer,
            source,
        )
    })?;
    sync_parent(&destination_pointer)
}

fn changed_rollback_worktree(journal: &DecodedJournal) -> WorktreeError {
    WorktreeError::InvalidRequest(format!(
        "worktree {} has changes; recovery preserved it",
        journal.destination.display()
    ))
}

fn restore_pointer_for_rollback(journal: &DecodedJournal) -> Result<(), WorktreeError> {
    if is_regular_file_if_present(&journal.destination.join(".git"))? {
        return Ok(());
    }
    let scratch_pointer = journal.scratch.join(".git");
    if !is_regular_file_if_present(&scratch_pointer)? {
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
    if entries.len() != 1 {
        return Ok(false);
    }
    let entry = entries.pop().expect("one directory entry");
    Ok(entry.file_name() == OsStr::new(".git") && is_regular_file_if_present(&entry.path())?)
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
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::EmptyDirectoryRemoval,
        path,
    );
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
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[cfg(unix)]
    use super::Git;
    use super::{
        AddWorktreeRequest, BackendKind, CompactWorktreeRequest, MoveWorktreeRequest,
        PruneWorktreesRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree_inner,
        classify_in_tree_attributes, compact_worktree_inner, force_remove_worktree_inner,
        garbage_collect_inner, has_ascii_case_alias, move_worktree_inner, next_operation_id,
        prune_worktrees_inner, recover_incomplete_operations, remove_empty_directory_if_present,
        remove_worktree_inner, storage_accounting,
    };
    #[cfg(unix)]
    use crate::journal::{CollectionJournalPaths, CollectionJournalRecord, CollectionJournalStore};
    use crate::journal::{
        JournalPaths, JournalRecord, JournalStore, RemovalJournalPaths, RemovalJournalRecord,
        RemovalJournalStore,
    };
    use crate::test_support::writable_tempdir as tempdir;
    use crate::{
        AddWorktreePhase, CompactWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase,
        PruneWorktreesPhase, RemoveWorktreePhase,
    };
    #[cfg(unix)]
    use riftri_git::GitError;

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

    #[test]
    fn attribute_allowlist_accepts_checkout_neutral_linguist_metadata() {
        let attribute = |name: &str, value: &str| riftri_git::GitAttribute {
            path: PathBuf::from("pnpm-lock.yaml"),
            name: name.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        };

        for accepted in [
            attribute("linguist-generated", "set"),
            attribute("linguist-generated", "true"),
            attribute("linguist-vendored", "unset"),
            attribute("linguist-vendored", "false"),
            attribute("linguist-documentation", "set"),
            attribute("linguist-detectable", "true"),
            attribute("linguist-language", "TypeScript"),
            attribute("text", "auto"),
        ] {
            assert!(
                classify_in_tree_attributes(std::slice::from_ref(&accepted)).is_ok(),
                "rejected checkout-neutral attribute {:?}={:?}",
                String::from_utf8_lossy(&accepted.name),
                String::from_utf8_lossy(&accepted.value),
            );
        }

        for rejected in [
            attribute("linguist-generated", "sometimes"),
            attribute("linguist-detectable", ""),
            attribute("linguist-language", ""),
            attribute("linguist-unknown", "set"),
            attribute("filter", "example"),
            attribute("ident", "set"),
            attribute("working-tree-encoding", "UTF-16"),
        ] {
            assert!(
                classify_in_tree_attributes(std::slice::from_ref(&rejected)).is_err(),
                "accepted unsupported attribute {:?}={:?}",
                String::from_utf8_lossy(&rejected.name),
                String::from_utf8_lossy(&rejected.value),
            );
        }
    }

    #[test]
    fn git_lfs_pointer_parser_accepts_only_the_canonical_v1_shape() {
        let oid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 42\n");
        let parsed = super::parse_git_lfs_pointer(pointer.as_bytes()).expect("canonical pointer");
        assert_eq!(parsed.oid, oid);
        assert_eq!(parsed.size, 42);

        for rejected in [
            format!("version https://git-lfs.github.com/spec/v1\nsize 42\noid sha256:{oid}\n"),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 42\n",
                oid.to_ascii_uppercase()
            ),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 42\next-foo bar\n"
            ),
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 042\n"),
        ] {
            assert!(
                super::parse_git_lfs_pointer(rejected.as_bytes()).is_err(),
                "accepted non-canonical pointer {rejected:?}"
            );
        }
    }

    #[test]
    fn recovery_is_safe_and_idempotent_after_every_compaction_transition() {
        for phase in [
            CompactWorktreePhase::IntentRecorded,
            CompactWorktreePhase::ReplacementReady,
            CompactWorktreePhase::ReplacementActivated,
            CompactWorktreePhase::AddJournalUpdated,
            CompactWorktreePhase::Complete,
        ] {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let caller = fixture.path().join("caller");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
            git(&repository, &["add", "--", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            git(
                &repository,
                &["worktree", "add", "--detach", caller.to_str().unwrap()],
            );
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from("feature/compact-recovery")),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create managed worktree");
            fs::write(destination.join("tracked.txt"), "allocate private blocks\n")
                .expect("edit view");
            fs::write(destination.join("tracked.txt"), "base\n").expect("restore view");

            compact_worktree_inner(
                CompactWorktreeRequest {
                    repository: caller,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("inject compaction interruption");

            let first = recover_incomplete_operations(&state).expect("first repair");
            assert!(first.errors.is_empty(), "{phase:?}: {first:?}");
            let second = recover_incomplete_operations(&state).expect("second repair");
            assert!(second.errors.is_empty(), "{phase:?}: {second:?}");
            assert!(destination.is_dir(), "{phase:?}");
            assert_eq!(
                fs::read_to_string(destination.join("tracked.txt")).unwrap(),
                "base\n",
                "{phase:?}"
            );
            let output = Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&destination)
                .output()
                .expect("inspect recovered worktree");
            assert!(output.status.success(), "{phase:?}");
            assert!(output.stdout.is_empty(), "{phase:?}");
            let accounting = storage_accounting(&state).expect("account after repair");
            assert_eq!(accounting.pending_compactions, 0, "{phase:?}");
            assert_eq!(
                accounting.completed_compactions + accounting.cancelled_compactions,
                1,
                "{phase:?}"
            );
            assert!(
                accounting.diagnostic_issues.is_empty(),
                "{phase:?}: {accounting:?}"
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn git_lfs_materialization_rejects_same_size_corruption() {
        use sha2::{Digest, Sha256};

        let fixture = tempdir().expect("LFS materialization fixture");
        let staging = fixture.path().join("staging");
        fs::create_dir(&staging).expect("create staging directory");
        fs::write(staging.join("asset.bin"), b"pointer\n").expect("write pointer destination");
        let source = fixture.path().join("object");
        fs::write(&source, b"evil").expect("write corrupt local object");
        let expected = crate::base_integrity::hex_lower(Sha256::digest(b"good"));
        let object = super::GitLfsObject {
            checkout_path: PathBuf::from("asset.bin"),
            source_path: source.clone(),
            pointer: super::GitLfsPointer {
                oid: expected,
                size: 4,
            },
        };

        let error = super::materialize_git_lfs_objects(&staging, &[object])
            .expect_err("same-size corrupt object must fail SHA-256 verification");

        assert!(error.to_string().contains("failed SHA-256 verification"));
        assert_eq!(fs::read(source).expect("source preserved"), b"evil");
    }

    #[test]
    fn compaction_recovery_preserves_edits_made_after_replacement_activation() {
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
        fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/compact-live-edit")),
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create managed worktree");

        compact_worktree_inner(
            CompactWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(CompactWorktreePhase::ReplacementActivated),
        )
        .expect_err("interrupt after activation");
        fs::write(destination.join("tracked.txt"), "staged-only contents\n")
            .expect("write staged version");
        git(&destination, &["add", "--", "tracked.txt"]);
        fs::write(
            destination.join("tracked.txt"),
            "edit in the new active view\n",
        )
        .expect("edit replacement");

        let repair = recover_incomplete_operations(&state).expect("resume compaction");
        assert!(repair.errors.is_empty(), "{repair:?}");
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).unwrap(),
            "edit in the new active view\n"
        );
        let staged = Command::new("git")
            .args(["show", ":tracked.txt"])
            .current_dir(&destination)
            .output()
            .expect("read staged version");
        assert!(staged.status.success());
        assert_eq!(staged.stdout, b"staged-only contents\n");
        let accounting = storage_accounting(&state).expect("inspect completed compaction");
        assert_eq!(accounting.completed_compactions, 1);
        assert_eq!(accounting.pending_compactions, 0);
    }

    #[test]
    fn empty_directory_cleanup_preserves_a_file_created_at_the_remove_boundary() {
        let fixture = tempdir().expect("cleanup race fixture");
        let destination = fixture.path().join("worktree");
        fs::create_dir(&destination).expect("create empty destination");
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::EmptyDirectoryRemoval,
            |path| fs::write(path.join("raced.txt"), b"preserve\n").expect("race cleanup"),
        );

        let error = remove_empty_directory_if_present(&destination)
            .expect_err("concurrent file must make nonrecursive cleanup fail");

        assert!(
            error
                .to_string()
                .contains("remove empty linked-worktree directory")
        );
        assert_eq!(
            fs::read(destination.join("raced.txt")).expect("raced file is preserved"),
            b"preserve\n"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn batched_checkout_configuration_preserves_profile_and_rejections() {
        use sha2::{Digest, Sha256};

        let fixture = tempdir().unwrap();
        let repository = fixture.path();
        git(repository, &["init", "--quiet"]);
        git(repository, &["config", "core.autocrlf", "false"]);
        git(repository, &["config", "core.eol", "lf"]);
        git(repository, &["config", "core.precomposeunicode", "false"]);
        git(
            repository,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let git = riftri_git::Git::default();
        let resolved = git
            .resolve_revision(repository, std::ffi::OsStr::new("HEAD"))
            .unwrap();
        let keys = [
            "core.attributesfile",
            "core.sparsecheckout",
            "core.sparsecheckoutcone",
            "core.autocrlf",
            "core.eol",
            "core.symlinks",
            "core.filemode",
            "core.ignorecase",
            "core.precomposeunicode",
            "core.protecthfs",
            "core.protectntfs",
        ];
        for (autocrlf, compatible) in [("false", true), ("true", false)] {
            git.set_local_config(repository, "core.autocrlf", std::ffi::OsStr::new(autocrlf))
                .unwrap();
            let analysis =
                super::analyze_resolved_repository_compatibility(&git, repository, &resolved)
                    .unwrap();
            // Reconstruct the previous, individual-read profile independently.
            let mut profile = Sha256::new();
            profile.update(b"riftri-checkout-profile-v3-lfs\0");
            let version = git.detect().unwrap().version;
            super::hash_profile_input(&mut profile, b"git.version", Some(version.as_bytes()));
            let mut captured = Vec::new();
            for key in keys {
                let value = git.config_value(repository, key).unwrap();
                super::hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
                if let Some(value) = value {
                    captured.push((key.to_owned(), value));
                }
            }
            assert_eq!(analysis.checkout_profile, profile.finalize().to_vec());
            assert_eq!(analysis.checkout_config, captured);
            assert_eq!(analysis.report.compatible, compatible);
            if !compatible {
                assert!(
                    analysis
                        .report
                        .blockers
                        .iter()
                        .any(|blocker| blocker.explanation.contains("core.autocrlf=true"))
                );
            }
            assert!(
                !repository.join(".git/riftri").exists(),
                "analysis must not create lifecycle state"
            );
        }
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

    #[cfg(unix)]
    #[test]
    fn git_pointer_validation_rejects_symlinks_and_special_files() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        let view = fixture.path().join("view");
        fs::create_dir(&view).expect("view");
        let target = fixture.path().join("pointer");
        fs::write(&target, "gitdir: /repository/.git/worktrees/view\n").expect("pointer target");
        let pointer = view.join(".git");
        symlink(&target, &pointer).expect("symlink pointer");
        assert!(!super::contains_only_git_pointer(&view).expect("inspect symlink"));
        fs::remove_file(&pointer).expect("remove fixture link");
        fs::create_dir(&pointer).expect("directory pointer");
        assert!(!super::contains_only_git_pointer(&view).expect("inspect directory"));
        fs::remove_dir(&pointer).expect("remove fixture directory");
        fs::copy(&target, &pointer).expect("regular pointer");
        assert!(super::contains_only_git_pointer(&view).expect("inspect regular pointer"));
        fs::write(view.join("private.txt"), "preserve me").expect("private file");
        assert!(!super::contains_only_git_pointer(&view).expect("inspect extra file"));
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
    fn destination_path_probe_covers_unicode_and_native_bytes() {
        for path in ["é.txt", "e\u{301}.txt", "Ü/file", "directory/ü.txt"] {
            assert!(super::needs_destination_path_probe(&[PathBuf::from(path)]));
        }
        assert!(!super::needs_destination_path_probe(&[PathBuf::from(
            "plain/file.txt"
        )]));
        assert!(super::needs_destination_path_probe(&[
            PathBuf::from("Case"),
            PathBuf::from("case")
        ]));
        #[cfg(unix)]
        assert!(super::needs_destination_path_probe(&[PathBuf::from(
            OsString::from_vec(b"native-\xff".to_vec())
        )]));
    }

    #[test]
    fn ascii_case_alias_scan_includes_directory_prefixes() {
        assert!(has_ascii_case_alias(&[
            PathBuf::from("Docs/one.txt"),
            PathBuf::from("docs/two.txt"),
        ]));
        assert!(!has_ascii_case_alias(&[
            PathBuf::from("docs/one.txt"),
            PathBuf::from("docs/two.txt"),
        ]));
    }

    #[cfg(unix)]
    #[test]
    fn ascii_case_alias_scan_preserves_non_utf8_path_bytes() {
        assert!(has_ascii_case_alias(&[
            PathBuf::from(OsString::from_vec(b"\xff-Case".to_vec())),
            PathBuf::from(OsString::from_vec(b"\xff-case".to_vec())),
        ]));
    }

    /// Whether the volume holding `directory` distinguishes two paths that
    /// differ only by the given spellings. Case folding and Unicode
    /// normalization are destination properties, so the expected outcome of
    /// the probe is discovered instead of assumed.
    fn destination_distinguishes(directory: &Path, first: &str, second: &str) -> bool {
        let reference = directory.join("reference");
        fs::create_dir(&reference).expect("create path-semantics probe root");
        fs::create_dir(reference.join(first)).expect("create path-semantics probe");
        // Files and directories share one namespace, so creating the second
        // spelling as a directory answers the question for either kind.
        let distinct = fs::create_dir(reference.join(second)).is_ok();
        fs::remove_dir_all(&reference).expect("remove path-semantics probe");
        distinct
    }

    /// Two spellings that collide on the destination cannot both be checked
    /// out, so the probe must refuse the tree before anything is created.
    /// Where they stay distinct — a case-sensitive, normalization-preserving
    /// volume — the same tree must be accepted.
    fn assert_path_pair_matches_destination(first: &str, second: &str, aliases: (&str, &str)) {
        let fixture = tempfile::tempdir().expect("create probe fixture");
        let destination = fixture.path().join("worktree");
        let paths = [PathBuf::from(first), PathBuf::from(second)];

        assert!(
            super::needs_destination_path_probe(&paths),
            "{first:?} and {second:?} must reach the destination probe"
        );

        let result = super::validate_destination_path_semantics(&paths, &destination);
        if destination_distinguishes(fixture.path(), aliases.0, aliases.1) {
            assert!(
                result.is_ok(),
                "{first:?} and {second:?} are distinct here: {result:?}"
            );
        } else {
            let Err(crate::WorktreeError::Unsupported(message)) = result else {
                panic!("{first:?} and {second:?} collide here but were accepted: {result:?}");
            };
            assert!(message.contains("cannot coexist"), "{message}");
        }
        assert!(
            !destination.exists(),
            "the probe must not create the destination"
        );
    }

    #[test]
    fn non_ascii_case_aliases_follow_the_destination_filesystem() {
        // Lowercasing "Ä" is beyond the ASCII fast path, so this pair only
        // fails closed because the probe asks the filesystem itself.
        assert_path_pair_matches_destination("Ä.txt", "ä.txt", ("Ä.txt", "ä.txt"));
    }

    #[test]
    fn unicode_normalization_aliases_follow_the_destination_filesystem() {
        // Same grapheme, composed (U+00E9) versus decomposed (U+0065 U+0301).
        assert_path_pair_matches_destination(
            "caf\u{e9}.txt",
            "cafe\u{301}.txt",
            ("caf\u{e9}.txt", "cafe\u{301}.txt"),
        );
    }

    #[test]
    fn non_ascii_directory_prefixes_follow_the_destination_filesystem() {
        // A colliding parent must be caught while creating directories, before
        // any leaf is reached.
        assert_path_pair_matches_destination("Ü/one.txt", "ü/two.txt", ("Ü", "ü"));
    }

    #[test]
    fn distinct_non_ascii_paths_stay_supported() {
        let fixture = tempfile::tempdir().expect("create probe fixture");
        let paths = [PathBuf::from("café.txt"), PathBuf::from("naïve.txt")];
        super::validate_destination_path_semantics(&paths, &fixture.path().join("worktree"))
            .expect("unrelated non-ASCII paths are representable");
    }

    #[test]
    fn add_operation_lock_is_exclusive_and_released_on_drop() {
        let fixture = tempfile::tempdir().unwrap();
        let journal = fixture.path().join("operation.json");
        let owner = super::try_lock_add_operation(&journal).unwrap().unwrap();
        assert!(super::try_lock_add_operation(&journal).unwrap().is_none());
        // A transient fork or duplicate can retain the same open-file description.
        let inherited = owner.0.try_clone().unwrap();
        drop(owner);
        assert!(super::try_lock_add_operation(&journal).unwrap().is_some());
        drop(inherited);
        assert!(journal.with_extension("lock").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn add_operation_lock_rejects_a_symlink() {
        let fixture = tempfile::tempdir().unwrap();
        let journal = fixture.path().join("operation.json");
        let external = fixture.path().join("external");
        fs::write(&external, "preserve").unwrap();
        std::os::unix::fs::symlink(&external, journal.with_extension("lock")).unwrap();
        assert!(super::try_lock_add_operation(&journal).is_err());
        assert_eq!(fs::read(&external).unwrap(), b"preserve");
    }

    #[test]
    fn prune_recovery_rejects_a_journal_changed_after_validation() {
        use crate::journal::{PruneJournalRecord, PruneJournalStore};

        let fixture = tempdir().expect("fixture");
        let store = PruneJournalStore::create(fixture.path()).expect("store");
        let mut record = PruneJournalRecord::new("prune-snapshot".to_owned(), fixture.path());
        store.persist(&record).expect("persist intent");
        let snapshot = store
            .load_all()
            .expect("load snapshot")
            .pop()
            .expect("journal");
        record.phase = PruneWorktreesPhase::Complete;
        store.persist(&record).expect("concurrent replacement");
        let error = super::resume_prune(&riftri_git::Git::default(), &store, snapshot, None)
            .expect_err("recovery must reject a changed snapshot");
        assert!(error.to_string().contains("changed"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn attributes_inspection_rejects_symlinks_fifos_and_large_files() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        git(fixture.path(), &["init", "--quiet"]);
        git(
            fixture.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let attributes = fixture.path().join(".git/info/attributes");
        let empty = fixture.path().join("empty");
        fs::write(&empty, "").expect("empty file");
        symlink(&empty, &attributes).expect("symlink attributes");
        let inspect = || {
            super::inspect_repository_compatibility(
                &Git::default(),
                fixture.path(),
                std::ffi::OsStr::new("HEAD"),
            )
            .expect("compatibility report")
        };
        assert!(
            !inspect().compatible,
            "symlinks must fail closed even when empty"
        );
        fs::remove_file(&attributes).expect("remove fixture link");
        let large = fs::File::create(&attributes).expect("large attributes file");
        large
            .set_len(4 * 1024 * 1024 * 1024)
            .expect("sparse file length");
        drop(large);
        assert!(!inspect().compatible, "large files require no allocation");
        fs::remove_file(&attributes).expect("remove fixture file");
        assert!(
            Command::new("mkfifo")
                .arg(&attributes)
                .status()
                .expect("create FIFO")
                .success()
        );
        assert!(
            !inspect().compatible,
            "FIFO inspection must not wait for a writer"
        );
        fs::remove_file(&attributes).expect("remove fixture FIFO");
        fs::write(&attributes, "").expect("empty regular attributes");
        assert!(inspect().compatible);
    }

    #[cfg(unix)]
    #[test]
    fn state_operations_reject_a_symlinked_state_root() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let external_state = fixture.path().join("external-state");
        fs::create_dir(&repository).expect("create repository");
        fs::create_dir(&external_state).expect("create external state");
        git(&repository, &["init", "--quiet"]);
        let state = repository.join(".git/riftri");
        symlink(&external_state, &state).expect("symlink state root");

        let discovery = super::repository_state_directories(&repository)
            .expect_err("repository discovery must reject a symlinked state root");
        assert!(discovery.to_string().contains("not a real directory"));

        for error in [
            storage_accounting(&state).expect_err("status must reject a symlinked state root"),
            garbage_collect_inner(&state, false, None)
                .expect_err("garbage collection must reject a symlinked state root"),
            recover_incomplete_operations(&state)
                .expect_err("recovery must reject a symlinked state root"),
        ] {
            assert!(error.to_string().contains("not a real directory"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn status_and_gc_do_not_inventory_through_a_symlinked_base_root() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        let state = fixture.path().join("state");
        let outside = fixture.path().join("outside");
        let outside_base = outside
            .join("repository")
            .join("0123456789abcdef0123456789abcdef01234567");
        fs::create_dir_all(state.join("bases")).expect("create state base directory");
        fs::create_dir_all(&outside_base).expect("create outside base");
        fs::write(outside_base.join("private.txt"), "outside\n").expect("write outside file");
        fs::write(outside_base.with_extension("complete"), "").expect("write outside marker");
        let state = state.canonicalize().expect("resolve state");
        let root = state.join("bases/v1");
        symlink(&outside, &root).expect("symlink immutable-base root");

        let status = storage_accounting(&state).expect("diagnose symlinked base root");
        let collection_error = garbage_collect_inner(&state, false, None)
            .expect_err("collection must reject symlinked base root");

        assert!(status.bases.is_empty());
        assert!(
            collection_error
                .to_string()
                .contains("not a real directory")
        );
        assert!(status.diagnostic_issues.iter().any(|issue| {
            issue.path == root && issue.reason.contains("must be a real directory")
        }));
        assert_eq!(
            fs::read_to_string(outside_base.join("private.txt")).expect("read outside file"),
            "outside\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn status_and_gc_preserve_bases_behind_a_symlinked_parent() {
        use std::os::unix::fs::symlink;

        for pending_collection in [false, true] {
            let fixture = tempdir().expect("fixture");
            let state = fixture.path().join("state");
            let base = state
                .join("bases/v1/repository")
                .join("0123456789abcdef0123456789abcdef01234567");
            fs::create_dir_all(&base).expect("create base");
            fs::write(base.join("private.txt"), "outside\n").expect("write base file");
            fs::write(base.with_extension("complete"), "").expect("write base marker");
            let state = state.canonicalize().expect("resolve state");
            if pending_collection {
                assert!(matches!(
                    garbage_collect_inner(
                        &state,
                        true,
                        Some(GarbageCollectionPhase::IntentRecorded)
                    ),
                    Err(super::WorktreeError::InjectedCollectionFailure(
                        GarbageCollectionPhase::IntentRecorded
                    ))
                ));
            }
            let parent = state.join("bases");
            let outside = fixture.path().join("outside");
            fs::rename(&parent, &outside).expect("move bases outside state");
            fs::write(outside.join("v1/unexplained"), "private\n").expect("write outside entry");
            symlink(&outside, &parent).expect("symlink base parent");

            let status = storage_accounting(&state).expect("diagnose symlinked parent");
            assert!(status.bases.is_empty());
            assert!(
                status
                    .diagnostic_issues
                    .iter()
                    .any(|issue| issue.path == parent)
            );
            assert!(
                status
                    .diagnostic_issues
                    .iter()
                    .all(|issue| { issue.path == parent || !issue.path.starts_with(&parent) })
            );
            for apply in [false, true] {
                let error = garbage_collect_inner(&state, apply, None)
                    .expect_err("reject symlinked base parent")
                    .to_string();
                assert!(error.contains("cleanup stopped"), "{error}");
                assert!(error.contains("is a symbolic link"), "{error}");
                assert!(error.contains(&parent.display().to_string()), "{error}");
                assert!(
                    error.contains("outside Riftri's expected storage layout"),
                    "{error}"
                );
                assert!(
                    error.contains("did not follow this link or delete data through it"),
                    "{error}"
                );
                assert!(
                    error.contains("riftri status --state-dir <STATE_DIR>"),
                    "{error}"
                );
                assert!(error.contains(&state.display().to_string()), "{error}");
                assert!(
                    error.contains("Do not delete or move the linked data manually"),
                    "{error}"
                );
                assert!(
                    error.contains("does not repair an existing redirected layout"),
                    "{error}"
                );
            }
            if pending_collection {
                let recovery = recover_incomplete_operations(&state).expect("attempt recovery");
                assert_eq!(recovery.recovered_collections, 0);
                assert_eq!(recovery.errors.len(), 1);
                assert!(recovery.errors[0].contains("is a symbolic link"));
                assert!(recovery.errors[0].contains("riftri status --state-dir <STATE_DIR>"));
            }
            let outside_base = outside
                .join("v1/repository")
                .join("0123456789abcdef0123456789abcdef01234567");
            assert_eq!(
                fs::read_to_string(outside_base.join("private.txt")).expect("read outside file"),
                "outside\n"
            );
            assert!(outside_base.with_extension("complete").is_file());
            assert!(parent.is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn status_and_gc_ignore_symlinked_base_completion_markers() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("fixture");
        let state = fixture.path().join("state");
        let base = state
            .join("bases/v1/repository")
            .join("0123456789abcdef0123456789abcdef01234567");
        let protected = fixture.path().join("protected-marker-target");
        fs::create_dir_all(&base).expect("create base");
        fs::write(base.join("tracked.txt"), "base\n").expect("write base file");
        fs::write(&protected, "must remain unchanged\n").expect("write protected file");
        let state = state.canonicalize().expect("resolve state");
        let base = state
            .join("bases/v1/repository")
            .join("0123456789abcdef0123456789abcdef01234567");
        let marker = base.with_extension("complete");
        symlink(&protected, &marker).expect("symlink completion marker");

        let status = storage_accounting(&state).expect("diagnose symlinked marker");
        let collection_error = garbage_collect_inner(&state, false, None)
            .expect_err("collection must reject symlinked marker");

        assert!(status.bases.is_empty());
        assert!(collection_error.to_string().contains("not a real file"));
        assert!(
            status
                .diagnostic_issues
                .iter()
                .any(|issue| issue.path == marker)
        );
        assert_eq!(
            fs::read_to_string(protected).expect("read protected file"),
            "must remain unchanged\n"
        );
    }

    #[cfg(target_os = "linux")]
    fn require_overlayfs_test_namespace(path: &Path) -> bool {
        if std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return false;
        }
        let capability = riftri_storage::OverlayFsMounter::probe_current_namespace(path);
        assert_eq!(
            capability.status,
            riftri_storage::CapabilityStatus::Supported,
            "required OverlayFS namespace is unavailable: {}",
            capability.explanation
        );
        true
    }

    #[cfg(target_os = "linux")]
    fn create_overlayfs_repository(repository: &Path) {
        fs::create_dir(repository).expect("create repository");
        git(repository, &["init", "--quiet"]);
        git(repository, &["config", "user.name", "Riftri Tests"]);
        git(
            repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(repository, &["add", "--", "tracked.txt"]);
        git(repository, &["commit", "--quiet", "-m", "initial"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_recovery_is_idempotent_after_every_add_transition() {
        let probe = tempdir().expect("probe fixture");
        if !require_overlayfs_test_namespace(probe.path()) {
            return;
        }
        drop(probe);
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
            let branch = format!("feature/overlay-recover-{index}");
            create_overlayfs_repository(&repository);

            let error = add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(&branch)),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
                false,
            )
            .expect_err("simulate interrupted OverlayFS add");
            assert!(error.to_string().contains("injected failure"));

            let recovered = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("recover after {phase:?}: {error}"));
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert_eq!(recovered.recovered, 1, "phase {phase:?}: {recovered:?}");
            assert!(!destination.exists(), "phase {phase:?}");
            assert_eq!(
                fs::read_dir(state.join("overlays/v1"))
                    .expect("read OverlayFS roots")
                    .count(),
                0,
                "phase {phase:?}"
            );

            let repeated = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("repeat recovery after {phase:?}: {error}"));
            assert_eq!(repeated.recovered, 0, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_recovery_adopts_the_mount_identity_gap_after_process_exit() {
        let fixture = tempdir().expect("fixture");
        if !require_overlayfs_test_namespace(fixture.path()) {
            return;
        }
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        create_overlayfs_repository(&repository);

        let status = Command::new(std::env::current_exe().expect("unit test executable"))
            .arg("--exact")
            .arg("worktree::tests::overlayfs_mount_gap_helper")
            .arg("--nocapture")
            .env("RIFTRI_OVERLAYFS_CORE_HELPER", "1")
            .env("RIFTRI_TEST_EXIT_AFTER_OVERLAYFS_MOUNT", "1")
            .env("RIFTRI_OVERLAYFS_CORE_REPOSITORY", &repository)
            .env("RIFTRI_OVERLAYFS_CORE_DESTINATION", &destination)
            .env("RIFTRI_OVERLAYFS_CORE_STATE", &state)
            .status()
            .expect("run mount-gap helper");
        assert_eq!(status.code(), Some(86));
        assert!(
            Command::new("mountpoint")
                .arg("--quiet")
                .arg(&destination)
                .status()
                .expect("inspect interrupted mount")
                .success()
        );

        let recovered = recover_incomplete_operations(&state).expect("recover mount gap");
        assert_eq!(recovered.recovered, 1);
        assert!(recovered.errors.is_empty());
        assert!(!destination.exists());
        assert_eq!(
            fs::read_dir(state.join("overlays/v1"))
                .expect("read OverlayFS roots")
                .count(),
            0
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_mount_gap_helper() {
        if std::env::var_os("RIFTRI_OVERLAYFS_CORE_HELPER").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        let required_path = |name| {
            std::env::var_os(name)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        add_worktree_inner(
            AddWorktreeRequest {
                repository: required_path("RIFTRI_OVERLAYFS_CORE_REPOSITORY"),
                destination: required_path("RIFTRI_OVERLAYFS_CORE_DESTINATION"),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/overlay-gap")),
                state_dir: Some(required_path("RIFTRI_OVERLAYFS_CORE_STATE")),
            },
            None,
            false,
        )
        .expect("the helper exits from the post-mount test hook");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_removal_recovers_after_every_persisted_transition() {
        let probe = tempdir().expect("probe fixture");
        if !require_overlayfs_test_namespace(probe.path()) {
            return;
        }
        drop(probe);
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
            create_overlayfs_repository(&repository);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!(
                        "feature/overlay-remove-{index}"
                    ))),
                    state_dir: Some(state.clone()),
                },
                None,
                true,
            )
            .expect("create OverlayFS worktree");

            let error = remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("simulate interrupted OverlayFS removal");
            assert!(error.to_string().contains("injected removal failure"));

            let recovered = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("recover removal after {phase:?}: {error}"));
            assert!(recovered.errors.is_empty(), "phase {phase:?}");
            assert!(!destination.exists(), "phase {phase:?}");
            assert_eq!(
                fs::read_dir(state.join("overlays/v1"))
                    .expect("read OverlayFS roots")
                    .count(),
                0,
                "phase {phase:?}"
            );

            let repeated = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("repeat removal after {phase:?}: {error}"));
            assert_eq!(repeated.recovered_removals, 0, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_removal_preserves_a_write_after_the_clean_checkpoint_and_unmount() {
        let fixture = tempdir().expect("fixture");
        if !require_overlayfs_test_namespace(fixture.path()) {
            return;
        }
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        create_overlayfs_repository(&repository);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create view");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::CleanVerified),
        )
        .expect_err("interrupt after clean checkpoint");
        fs::write(
            destination.join("tracked.txt"),
            "write after clean checkpoint\n",
        )
        .expect("concurrent edit");
        let managed = JournalStore::open(&state)
            .load_all()
            .unwrap()
            .pop()
            .unwrap();
        let overlay = managed.overlayfs.as_ref().unwrap();
        let layout = riftri_storage::OverlayFsMounter::load(
            &overlay.layout_root,
            &managed.base_path,
            &managed.destination,
        )
        .unwrap();
        riftri_storage::OverlayFsMounter::unmount(
            &layout,
            overlay.mount_identity.as_ref().unwrap(),
        )
        .expect("simulate crash after unmount");
        let report = recover_incomplete_operations(&state).expect("recovery report");
        assert!(
            !report.errors.is_empty(),
            "changed upper must not be released"
        );
        assert_eq!(
            fs::read(layout.upper().join("tracked.txt")).expect("retained edit"),
            b"write after clean checkpoint\n"
        );
        assert!(destination.exists());
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
    fn status_does_not_scan_an_unregistered_journal_destination() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let destination = fixture.path().join("unregistered-directory");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        fs::create_dir(&destination).expect("create unregistered directory");
        fs::write(destination.join("private.txt"), "must not be inventoried\n")
            .expect("write external file");

        let base_parent = state.join("bases/v1/repository");
        let base_path = base_parent.join("0123456789abcdef0123456789abcdef01234567");
        fs::create_dir_all(&base_path).expect("create base");
        fs::write(base_path.with_extension("complete"), "").expect("write completion marker");
        fs::create_dir_all(state.join("tmp")).expect("create temporary directory");
        let repository = repository.canonicalize().expect("resolve repository");
        let destination = destination.canonicalize().expect("resolve destination");
        let state = state.canonicalize().expect("resolve state");
        let base_parent = state.join("bases/v1/repository");
        let base_path = base_parent.join("0123456789abcdef0123456789abcdef01234567");
        let store = JournalStore::create(&state).expect("create journal store");
        let mut record = JournalRecord::new(
            "operation".to_owned(),
            JournalPaths {
                repository: &repository,
                destination: &destination,
                scratch: &fixture.path().join(".riftri-view-operation"),
                base_staging: &base_parent.join(".riftri-build-operation"),
                base_path: &base_path,
                temporary_index: &state.join("tmp/index-operation"),
                branch: None,
                branch_created: false,
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            BackendKind::ApfsClone,
        );
        record.phase = AddWorktreePhase::Active;
        record.last_forward_phase = AddWorktreePhase::Active;
        let journal_path = store.persist(&record).expect("persist forged journal");
        let journal_path = journal_path.canonicalize().expect("resolve journal path");

        let report = storage_accounting(&state).expect("status must reject unsafe references");

        assert!(report.views.is_empty());
        assert!(
            report.diagnostic_issues.iter().any(|issue| {
                issue.path == journal_path && issue.reason.contains("not registered by Git")
            }),
            "diagnostics: {:?}",
            report.diagnostic_issues
        );
        assert_eq!(
            fs::read_to_string(destination.join("private.txt")).expect("external file remains"),
            "must not be inventoried\n"
        );
    }

    #[test]
    fn status_reports_malformed_journals_without_changing_them() {
        let fixture = tempdir().expect("fixture");
        let state = fixture.path().join("state");
        let mut malformed = HashSet::new();
        for directory in [
            "operations",
            "removals",
            "moves",
            "compactions",
            "prunes",
            "collections",
        ] {
            let path = state.join(directory).join("corrupt.json");
            fs::create_dir_all(path.parent().expect("journal parent"))
                .expect("create journal directory");
            fs::write(&path, b"{not-json\n").expect("write malformed journal");
            malformed.insert(path.canonicalize().expect("resolve malformed journal"));
        }

        let report = storage_accounting(&state).expect("status must diagnose malformed journals");
        let reported = report
            .diagnostic_issues
            .iter()
            .filter(|issue| issue.reason.contains("malformed durable operation journal"))
            .map(|issue| issue.path.clone())
            .collect::<HashSet<_>>();

        assert_eq!(reported, malformed);
        for path in &malformed {
            assert_eq!(
                fs::read(path).expect("malformed journal must remain"),
                b"{not-json\n"
            );
        }
        assert!(
            recover_incomplete_operations(&state).is_err(),
            "recovery must continue to fail closed"
        );
    }

    #[test]
    fn invalid_completed_removal_does_not_release_an_active_base() {
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
        fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let added = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/live-view")),
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create managed worktree");
        let add_journal = JournalStore::open(&state)
            .load_all()
            .expect("load add journal")
            .pop()
            .expect("active add journal");
        let state = state.canonicalize().expect("resolve state");
        let repository = repository.canonicalize().expect("resolve repository");
        let mut forged = RemovalJournalRecord::new(
            "forged-removal".to_owned(),
            RemovalJournalPaths {
                repository: &repository,
                destination: &fixture.path().join("different-worktree"),
                base_path: &added.base_path,
            },
            add_journal.operation_id,
        );
        forged.phase = RemoveWorktreePhase::Complete;
        let forged_path = RemovalJournalStore::create(&state)
            .expect("create removal store")
            .persist(&forged)
            .expect("persist forged completed removal");

        let status = storage_accounting(&state).expect("status preserves active view");
        assert_eq!(status.active_views, 1);
        assert_eq!(status.bases.len(), 1);
        assert_eq!(status.bases[0].reference_count, 1);
        assert!(
            status.diagnostic_issues.iter().any(|issue| {
                issue.path == forged_path && issue.reason.contains("does not match")
            })
        );

        let collection = garbage_collect_inner(&state, false, None)
            .expect_err("garbage collection must reject the invalid removal");
        assert!(collection.to_string().contains("does not match"));
        assert!(added.base_path.is_dir());
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
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(destination.exists());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved file"),
            "user change\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rollback_preserves_a_write_that_races_with_git_removal() {
        for phase in [
            AddWorktreePhase::GitMetadataCreated,
            AddWorktreePhase::GitPointerRestored,
        ] {
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
                    mode: WorktreeMode::NewBranch(OsString::from("feature/raced-rollback")),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
                false,
            )
            .expect_err("simulate process termination");
            let journal = JournalStore::open(&state)
                .load_all()
                .expect("load add journal")
                .pop()
                .expect("incomplete add journal");

            let _hook = crate::test_hooks::install(
                crate::test_hooks::FilesystemRacePoint::RollbackGitRemoval,
                |path| {
                    fs::create_dir_all(path).expect("restore raced destination");
                    fs::write(path.join("raced.txt"), "raced write\n").expect("write raced file");
                },
            );

            let error = super::rollback_decoded(&Git::default(), &journal)
                .expect_err("rollback must preserve the concurrent write");

            assert!(
                matches!(
                    &error,
                    super::WorktreeError::Git(GitError::CommandFailed { .. })
                ),
                "unexpected rollback error: {error}"
            );
            assert_eq!(
                fs::read_to_string(destination.join("raced.txt")).expect("read preserved write"),
                "raced write\n"
            );
        }
    }

    #[test]
    fn rollback_recovers_staged_pointers_and_preserves_new_files() {
        for (missing, changed) in [(false, false), (true, false), (false, true)] {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
            git(&repository, &["add", "."]);
            git(
                &repository,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "initial",
                ],
            );
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::Detached,
                    state_dir: Some(state.clone()),
                },
                Some(AddWorktreePhase::GitMetadataCreated),
                false,
            )
            .expect_err("interrupted add");
            let journal = JournalStore::open(&state)
                .load_all()
                .unwrap()
                .pop()
                .unwrap();
            let staged = super::pointer_staging_path(&journal);
            super::move_pointer_without_replacement(&destination.join(".git"), &staged)
                .expect("stage pointer before simulated crash");
            if missing {
                fs::remove_dir(&destination).unwrap();
            }
            if changed {
                fs::write(destination.join("new.txt"), "private\n").unwrap();
            }
            let report = recover_incomplete_operations(&state).expect("recovery");
            assert_eq!(report.errors.is_empty(), !changed, "{report:?}");
            assert!(!staged.exists());
            if changed {
                assert_eq!(fs::read(destination.join("new.txt")).unwrap(), b"private\n");
                assert!(destination.join(".git").is_file());
            } else {
                assert!(!destination.exists());
            }
        }
    }

    #[test]
    fn recovery_preserves_an_interrupted_view_whose_head_changed() {
        for (detached, change) in [
            (true, "commit"),
            (true, "switch"),
            (false, "switch"),
            (false, "detach"),
        ] {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
            git(&repository, &["add", "."]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: if detached {
                        WorktreeMode::Detached
                    } else {
                        WorktreeMode::NewBranch(OsString::from("feature/interrupted"))
                    },
                    state_dir: Some(state.clone()),
                },
                Some(AddWorktreePhase::IndexSynchronized),
                false,
            )
            .expect_err("simulate process termination");
            match change {
                "commit" => {
                    fs::write(destination.join("tracked.txt"), "committed change\n").unwrap();
                    git(&destination, &["add", "."]);
                    git(&destination, &["commit", "--quiet", "-m", "preserve me"]);
                }
                "switch" => {
                    git(
                        &destination,
                        &["switch", "--quiet", "-c", "feature/private"],
                    );
                }
                "detach" => {
                    git(&destination, &["switch", "--quiet", "--detach"]);
                }
                _ => unreachable!(),
            }
            let head = riftri_git::Git::default()
                .resolve_revision(&destination, std::ffi::OsStr::new("HEAD"))
                .unwrap();
            for _ in 0..2 {
                let report = recover_incomplete_operations(&state).expect("repair report");
                assert_eq!(report.recovered, 0, "{detached}/{change}: {report:?}");
                assert_eq!(report.errors.len(), 1, "{report:?}");
                assert!(report.errors[0].contains("HEAD"), "{report:?}");
                assert!(destination.exists());
                assert_eq!(
                    riftri_git::Git::default()
                        .resolve_revision(&destination, std::ffi::OsStr::new("HEAD"))
                        .unwrap(),
                    head
                );
            }
        }
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
        assert!(retry.to_string().contains("riftri repair --state-dir"));
        assert!(!retry.to_string().contains("riftri recover"));

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
    fn forced_removal_preserves_a_write_at_the_delete_boundary_and_during_recovery() {
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
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create worktree");
        fs::write(destination.join("tracked.txt"), "discard requested\n")
            .expect("dirty tracked file");
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::ForceRemovalRevalidation,
            |path| {
                fs::write(path.join("late.txt"), "preserve late write\n")
                    .expect("write at force boundary");
            },
        );

        let error = force_remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("a write after force intent must stop deletion");

        assert!(
            error
                .to_string()
                .contains("changed after forced removal intent")
        );
        assert_eq!(
            fs::read(destination.join("late.txt")).expect("late write preserved"),
            b"preserve late write\n"
        );
        for _ in 0..2 {
            let recovery = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(recovery.recovered_removals, 0);
            assert_eq!(recovery.errors.len(), 1, "{recovery:?}");
            assert!(recovery.errors[0].contains("changed after forced removal intent"));
            assert!(destination.exists());
        }
        let accounting = storage_accounting(&state).expect("retained accounting");
        assert_eq!(accounting.active_views, 1);
        assert_eq!(accounting.bases[0].reference_count, 1);
    }

    #[test]
    fn forced_removal_recovers_an_unchanged_dirty_view_after_intent() {
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
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
            },
            None,
            true,
        )
        .expect("create worktree");
        fs::write(destination.join("untracked.txt"), "explicitly discarded\n")
            .expect("dirty worktree");

        let error = force_remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("simulate interruption after durable force intent");
        assert!(error.to_string().contains("injected removal failure"));
        assert!(destination.exists());

        let recovery = recover_incomplete_operations(&state).expect("recover forced removal");
        assert!(recovery.errors.is_empty(), "{recovery:?}");
        assert_eq!(recovery.recovered_removals, 1);
        assert_eq!(recovery.completed_removals, 1);
        assert!(!destination.exists());
        let accounting = storage_accounting(&state).expect("released accounting");
        assert_eq!(accounting.active_views, 0);
        assert_eq!(accounting.bases[0].reference_count, 0);
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
            let caller = fixture.path().join("caller");
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
            git(
                &repository,
                &["worktree", "add", "--detach", caller.to_str().unwrap()],
            );
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
                    repository: caller,
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
        let accounting = storage_accounting(&state).expect("account rejected collection");
        assert_eq!(accounting.pending_collections, 0);
        assert_eq!(
            fs::read_to_string(&protected_file).expect("read protected file"),
            "preserve\n"
        );

        let operation_id = "gc-existing-invalid".to_owned();
        let quarantine_path = added
            .base_path
            .parent()
            .expect("base parent")
            .join(format!(".riftri-gc-{operation_id}"));
        let store = CollectionJournalStore::create(&state).expect("open collection store");
        store
            .persist(&CollectionJournalRecord::new(
                operation_id,
                CollectionJournalPaths {
                    base_path: &added.base_path,
                    quarantine_path: &quarantine_path,
                    marker_path: &marker,
                },
            ))
            .expect("persist prior invalid collection intent");
        let recovery = recover_incomplete_operations(&state).expect("recover prior intent");
        assert_eq!(recovery.errors.len(), 1);
        let accounting = storage_accounting(&state).expect("account cancelled collection");
        assert_eq!(accounting.pending_collections, 0);
        assert_eq!(accounting.cancelled_collections, 1);

        fs::remove_file(&marker).expect("remove unsafe marker");
        fs::write(&marker, []).expect("restore real marker");
        let collected = garbage_collect_inner(&state, true, None)
            .expect("collect after repairing completion marker");
        assert_eq!(collected.resumed_collections, 0);
        assert_eq!(collected.collected, vec![added.base_path]);
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
            let caller = fixture.path().join("caller");
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
            git(
                &repository,
                &["worktree", "add", "--detach", caller.to_str().unwrap()],
            );
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
                    repository: caller,
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
