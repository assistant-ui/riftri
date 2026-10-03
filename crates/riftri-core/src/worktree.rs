use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

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
use riftri_git::{ConfigValues, Git, GitAttribute, GitError, ObjectId, ResolvedRevision};
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

/// Every configuration key that can affect checkout compatibility or the
/// immutable-base profile. Capturing the superset once lets add preflight use
/// the sparse settings and the compatibility values from one coherent Git
/// snapshot; LFS-only values remain ignored unless the exact tree uses LFS.
const CHECKOUT_CONFIG_KEYS: &[&str] = &[
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
    "filter.lfs.clean",
    "filter.lfs.smudge",
    "filter.lfs.process",
    "filter.lfs.required",
    "lfs.storage",
    "core.hookspath",
];

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
    /// `core.hooksPath` from the same batched read the blockers use, so
    /// running the hook costs no extra Git invocation.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    hooks_path: Option<Vec<u8>>,
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
    /// Cone-mode sparse-checkout directories, relative to the repository root
    /// with `/` separators. Empty materializes the full tree. Directories are
    /// canonicalized (sorted, deduplicated, nested cones collapsed into their
    /// ancestors) and become part of the versioned checkout profile, so
    /// different selections at the same commit never share a base. Requests
    /// outside the supported cone subset are refused before any state exists.
    pub sparse_directories: Vec<String>,
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
    /// The repository's `post-checkout` hook run, when it has one Git would
    /// execute. None means there was no such hook.
    pub post_checkout: Option<PostCheckoutOutcome>,
}

/// What Riftri's `post-checkout` hook run did.
///
/// `git worktree add` runs this hook and reports a failure without undoing the
/// worktree, so Riftri matches that: the caller decides what a non-zero hook
/// means, and the worktree exists either way.
#[derive(Debug, Clone)]
pub struct PostCheckoutOutcome {
    /// The hook Git would have run.
    pub hook: PathBuf,
    /// Its exit code, or None when it was killed by a signal.
    pub exit_code: Option<i32>,
    /// False when the hook could not be started at all.
    pub started: bool,
}

impl PostCheckoutOutcome {
    /// Whether the hook ran and reported success.
    pub fn succeeded(&self) -> bool {
        self.started && self.exit_code == Some(0)
    }
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
    /// An add found this base failing its integrity check. It is preserved and
    /// never reused; `riftri gc --apply` deletes it once no view uses it.
    pub damaged: bool,
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

/// Whether a state diagnostic can conceal a base reference the surrounding
/// report did not count.
///
/// This is deliberately one bit rather than a taxonomy of causes. A consumer
/// only needs to know whether the reference counts can be trusted, and #598
/// showed what happens when the renderer asserts a specific cause it cannot
/// derive. Classify a diagnostic as [`CountsUnaffected`] only where the claim
/// is provably counted elsewhere; everything else stays conservative.
///
/// [`CountsUnaffected`]: BaseCountImpact::CountsUnaffected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseCountImpact {
    /// Something was not read, not trusted, or not traversed, so a base
    /// reference may be missing from the counts.
    MayHideReference,
    /// Whatever this diagnostic describes is already reflected in the counts.
    CountsUnaffected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDiagnosticIssue {
    pub path: PathBuf,
    pub reason: String,
    pub base_count_impact: BaseCountImpact,
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

/// A retained immutable base that garbage collection refused to consider
/// because a durable journal still claims it, and which journal claims it.
///
/// Without this record `gc` reported "Eligible bases: 0 / Skipped: 0" for a
/// base that `status` simultaneously reported as unreferenced, leaving the
/// user no way to learn which operation pins the storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedBase {
    pub base_path: PathBuf,
    pub operation_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GarbageCollectionReport {
    pub applied: bool,
    pub candidates: Vec<GarbageCollectionCandidate>,
    pub collected: Vec<PathBuf>,
    pub skipped_in_use: Vec<PathBuf>,
    pub skipped_protected: Vec<ProtectedBase>,
    pub resumed_collections: usize,
    pub removed_logical_bytes: u64,
    pub removed_allocated_bytes: u64,
    /// Finished journals an `--apply` run would delete: the history of
    /// worktrees that are gone for good, and finished prune and collection
    /// records. Planned only; see `retired_journals`.
    pub retirable_journals: usize,
    /// Finished journals this run deleted.
    pub retired_journals: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageAccountingReport {
    pub active_views: usize,
    pub pending_adds: usize,
    pub completed_removals: usize,
    pub cancelled_removals: usize,
    pub pending_removals: usize,
    pub completed_moves: usize,
    pub cancelled_moves: usize,
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

/// A journaled worktree that Git no longer registers at the journal's
/// destination but does register somewhere else.
///
/// Riftri reports this instead of acting: the live worktree still holds user
/// data, and Git's registry alone cannot prove that the worktree at the new
/// path is the journal's worktree rather than an unrelated one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocatedWorktree {
    pub operation_id: String,
    pub journal_destination: PathBuf,
    pub registered_path: PathBuf,
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
    /// Pending removals whose worktree changed before anything was removed.
    pub cancelled_removals: usize,
    pub completed_moves: usize,
    pub recovered_moves: usize,
    /// Pending moves Git refused while both paths proved nothing had moved.
    pub cancelled_moves: usize,
    pub completed_compactions: usize,
    pub recovered_compactions: usize,
    pub completed_prunes: usize,
    pub recovered_prunes: usize,
    pub completed_collections: usize,
    pub recovered_collections: usize,
    /// Add journals whose worktree Git no longer registers and whose
    /// destination is absent, retired by a journaled completion.
    pub retired_adds: usize,
    /// Unfinished adds whose complete view Git commands had already used
    /// (HEAD or branch moved): rolled back, but their worktrees were kept as
    /// plain Git worktrees that Riftri no longer manages.
    pub released_adds: Vec<PathBuf>,
    /// Add journals that named a vanished linked worktree as their repository,
    /// re-homed to the repository's main worktree.
    pub rehomed_adds: usize,
    /// Journaled worktrees Git registers under a different path.
    pub relocations: Vec<RelocatedWorktree>,
    /// Worktrees Git lists without a resolvable HEAD (corrupt or empty HEAD
    /// file). Repair reports them instead of touching them; `git worktree
    /// repair` or removing the worktree clears the state.
    pub unresolvable_worktrees: Vec<PathBuf>,
    /// Riftri's own interrupted atomic-write temporaries removed this pass.
    pub reaped_artifacts: Vec<PathBuf>,
    /// Abandoned OverlayFS probe roots removed this pass after the kernel
    /// mount inventory proved nothing was mounted at or below them.
    pub reaped_probe_roots: Vec<PathBuf>,
    /// Abandoned OverlayFS probe roots preserved because a mount still covers
    /// them; repair never touches a probe root that may be live.
    pub preserved_probe_mounts: Vec<PathBuf>,
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
    /// The requested start point is `HEAD`, and `HEAD` names no commit: the
    /// repository or its current branch has no commits yet. A policy refusal
    /// like `InvalidRequest`; it is typed separately so the Git shim can hand
    /// such an add to Git, which may create an orphan worktree.
    #[error("invalid worktree request: {0}")]
    UnbornHead(String),

    /// A durable journal records an interrupted lifecycle operation, so this
    /// request is refused until `riftri repair` runs against
    /// `state_directory`. Distinct from [`WorktreeError::Busy`], which means
    /// another live process currently holds the operation lock.
    #[error("{message}")]
    RecoveryPending {
        message: String,
        state_directory: PathBuf,
    },

    /// A parent directory of the immutable-base storage is a symbolic link,
    /// so the operation stopped before following it. Nothing was modified
    /// and nothing needs repair; the caller should inspect `state_directory`
    /// with the `riftri status` command echoed in the message.
    #[error("{message}")]
    SymlinkedBaseParent {
        message: String,
        state_directory: PathBuf,
    },

    /// A repository-local locator names a state directory that no longer
    /// exists. Cross-state lifecycle checks must remain fail-closed, but the
    /// registration can be removed explicitly without touching files.
    #[error("{message}")]
    StaleStateRegistration {
        message: String,
        repository: PathBuf,
        state_directory: PathBuf,
    },

    /// Another live process holds the operation lock right now. Nothing needs
    /// repair; the caller should wait for the concurrent operation to finish
    /// and retry.
    #[error("{message}")]
    Busy { message: String },

    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("creation failed: {operation}; rollback also needs attention: {rollback}")]
    OperationAndRollback {
        #[source]
        operation: Box<WorktreeError>,
        rollback: Box<WorktreeError>,
    },

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

impl WorktreeError {
    /// Whether this failure, including both sides of a failed rollback, was
    /// caused by the destination volume running out of storage.
    pub(crate) fn contains_storage_full(&self) -> bool {
        if let Self::OperationAndRollback {
            operation,
            rollback,
        } = self
        {
            return operation.contains_storage_full() || rollback.contains_storage_full();
        }
        // `Git` is transparent, so the chain below never visits the
        // `GitError` itself; see `is_absent_repository`.
        if let Self::Git(git) = self
            && git_reported_storage_full(git)
        {
            return true;
        }

        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = current {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(io_error_is_storage_full)
            {
                return true;
            }
            current = error.source();
        }
        false
    }
}

/// A Git child that ran out of space reports it only as text, such as a
/// checkout into a new base failing with "unable to create file …: No space
/// left on device". Git runs under `LC_ALL=C`, so the C library's wording is
/// stable. Without this, such a failure was a `git-failed` to inspect rather
/// than `storage-full`.
pub(crate) fn git_reported_storage_full(error: &GitError) -> bool {
    matches!(error, GitError::CommandFailed { stderr, .. }
        if stderr.contains("No space left on device"))
}

#[cfg(unix)]
pub(crate) fn io_error_is_storage_full(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOSPC)
}

#[cfg(target_os = "windows")]
pub(crate) fn io_error_is_storage_full(error: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL};

    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_DISK_FULL as i32 || code == ERROR_HANDLE_DISK_FULL as i32
    )
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn io_error_is_storage_full(_error: &std::io::Error) -> bool {
    false
}

pub fn add_worktree(request: AddWorktreeRequest) -> Result<AddWorktreeResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    add_worktree_inner(request, None, true)
}

/// Refuse a proposed new-worktree destination with exactly the refusals
/// `add_worktree` applies before any durable mutation: the destination must
/// not already exist (a symlink to an existing target counts as existing),
/// must name a new directory under an existing parent, and every checkout
/// path of the requested revision must be able to coexist on the destination
/// filesystem (case and Unicode-normalization collisions). Nothing is
/// created. Interactive flows call this so a doomed plan is refused before
/// any confirmation prompt, with the same diagnostics the explicit add
/// prints; it deliberately calls the same validation functions as
/// `add_worktree_inner` rather than restating their rules.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn validate_new_worktree_destination(
    repository: &Path,
    destination: &Path,
    revision: &OsStr,
) -> Result<(), WorktreeError> {
    validate_lifecycle_git_environment()?;
    let git = Git::default();
    let repository = git.inspect_repository(repository)?;
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let destination = normalize_new_destination(destination, DestinationRules::WorktreeAdd)?;
    let resolved = resolve_requested_revision(&git, &repository_root, revision)?;
    let compatibility = validate_resolved_compatibility(
        &git,
        &repository_root,
        &repository.identity.common_git_dir,
        &resolved,
        &[],
        None,
    )?;
    validate_destination_path_semantics(&compatibility.checkout_paths, &destination)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn validate_new_worktree_destination(
    _repository: &Path,
    _destination: &Path,
    _revision: &OsStr,
) -> Result<(), WorktreeError> {
    Err(WorktreeError::Unsupported(
        "this build has no supported native copy-on-write worktree backend".to_owned(),
    ))
}

pub fn remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    remove_worktree_inner(request, None)
}

/// Remove one explicitly selected managed worktree even when it has changes.
/// The durable operation records and revalidates an exact content snapshot so
/// recovery cannot discard changes made after this request.
pub fn force_remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    force_remove_worktree_inner(request, None)
}

pub fn move_worktree(request: MoveWorktreeRequest) -> Result<MoveWorktreeResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    move_worktree_inner(request, None)
}

pub fn compact_worktree(
    request: CompactWorktreeRequest,
) -> Result<CompactWorktreeResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    compact_worktree_inner(request, None)
}

pub fn prune_worktrees(
    request: PruneWorktreesRequest,
) -> Result<PruneWorktreesResult, WorktreeError> {
    validate_lifecycle_git_environment()?;
    prune_worktrees_inner(request, None)
}

/// Plan or apply collection of immutable bases with no journaled references.
pub fn garbage_collect(
    state_directory: &Path,
    apply: bool,
) -> Result<GarbageCollectionReport, WorktreeError> {
    validate_lifecycle_git_environment()?;
    garbage_collect_inner(state_directory, apply, None)
}

/// Internal lifecycle commands address several different Git worktrees. A
/// caller's repository/index/object-store override would take precedence over
/// each command's working directory, potentially resetting or deleting the
/// wrong Git state, and environment-based configuration injection reaches
/// every internal Git invocation exactly like `-c` options would. Refuse
/// before mutation rather than silently changing the caller's context.
/// Ordinary Git passthrough deliberately does not use this guard.
fn validate_lifecycle_git_environment() -> Result<(), WorktreeError> {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        if std::env::var_os(name).is_some() {
            return Err(WorktreeError::Unsupported(format!(
                "{name} is set and can redirect internal Git operations; unset {name} before using Riftri lifecycle commands, and select the repository with --repository instead"
            )));
        }
    }
    // `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` entries only take effect through
    // `GIT_CONFIG_COUNT`, so refusing the count refuses the whole family.
    for name in ["GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS"] {
        if std::env::var_os(name).is_some() {
            return Err(WorktreeError::Unsupported(format!(
                "{name} is set and can inject Git configuration into internal Git operations; unset {name} before using Riftri lifecycle commands"
            )));
        }
    }
    Ok(())
}

/// The directory Riftri runs Git in for a repository: its working-tree root,
/// or the Git directory of a bare repository. Git's worktree, ref, and config
/// commands work from either, and no lifecycle operation needs a main working
/// tree, so a bare repository with linked worktrees is managed like any other.
pub(crate) fn git_command_root(repository: &riftri_git::RepositoryInfo) -> Option<&Path> {
    repository.root.as_deref().or_else(|| {
        repository
            .is_bare
            .then_some(repository.identity.common_git_dir.as_path())
    })
}

/// The directory lifecycle commands run Git from and journals record for a
/// repository: its main worktree, which Git lists first (a bare repository's
/// own directory). The worktree a command was run from can be the one being
/// removed (#475), or a linked worktree that is removed later and would strand
/// every journal naming it (#476). Falls back to `live_root` when the main
/// worktree is not an existing directory.
fn stable_repository_root(git: &Git, live_root: &Path) -> Result<PathBuf, WorktreeError> {
    Ok(main_worktree_root(
        &git.list_worktrees(live_root)?,
        live_root,
    ))
}

/// [`stable_repository_root`] from an inventory the caller already listed.
fn main_worktree_root(inventory: &[riftri_git::WorktreeInfo], live_root: &Path) -> PathBuf {
    inventory
        .first()
        .map(|main| main.path.clone())
        .filter(|main| main.is_dir())
        .unwrap_or_else(|| live_root.to_path_buf())
}

/// The default state directory of the repository at `repository`,
/// `<common-git-dir>/riftri`, for bare repositories as well.
pub fn default_state_directory(repository: &Path) -> Result<PathBuf, WorktreeError> {
    Ok(Git::default()
        .inspect_repository(repository)?
        .identity
        .common_git_dir
        .join("riftri"))
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
    validate_lifecycle_git_environment()?;
    let git = Git::default();
    let repository = git.inspect_repository(repository)?;
    let repository_root = git_command_root(&repository).ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a repository directory".to_owned())
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
        let snapshot = ManagedJournalSnapshot::load(&state_directory)?;
        let mut active_add = false;
        for destination in &destinations {
            if snapshot.find(&state_directory, destination)?.is_some() {
                active_add = true;
                break;
            }
        }
        let pending_move = snapshot.moves.iter().any(|journal| {
            !journal.phase.is_finished()
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
    let repository_root = git_command_root(repository).ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a repository directory".to_owned())
    })?;
    let default = repository.identity.common_git_dir.join("riftri");
    let mut directories = Vec::new();
    if let Some(default) = resolve_real_state_directory_if_present(&default)? {
        directories.push(default);
    }
    for configured in git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)? {
        directories.push(resolve_registered_state_directory(
            repository_root,
            &configured,
        )?);
    }
    directories.sort_unstable();
    directories.dedup();
    Ok(directories)
}

fn resolve_registered_state_directory(
    repository: &Path,
    state_directory: &Path,
) -> Result<PathBuf, WorktreeError> {
    if !state_directory.is_absolute() {
        return Err(WorktreeError::InvalidRequest(format!(
            "registered Riftri state directory is not absolute: {}",
            state_directory.display()
        )));
    }
    match resolve_real_state_directory(state_directory) {
        Ok(resolved) => Ok(resolved),
        Err(WorktreeError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(stale_state_registration_error(repository, state_directory))
        }
        Err(error) => Err(error),
    }
}

fn stale_state_registration_error(repository: &Path, state_directory: &Path) -> WorktreeError {
    let message = match crate::unregister_state_command(repository, state_directory) {
        Some(command) => format!(
            "registered Riftri state directory is missing: {}; run {command} before retrying",
            state_directory.display()
        ),
        None => format!(
            "registered Riftri state directory is missing: {}; unregister this exact path with `riftri state unregister <path> --repository <repository>` before retrying",
            state_directory.display()
        ),
    };
    WorktreeError::StaleStateRegistration {
        message,
        repository: repository.to_path_buf(),
        state_directory: state_directory.to_path_buf(),
    }
}

/// Origin of one discovered state directory in an all-states inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateDirectorySource {
    /// The implicit `<common-git-dir>/riftri` location.
    Default,
    /// A `riftri.stateDirectory` registration in the repository-local config.
    Registered,
}

impl StateDirectorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Registered => "registered",
        }
    }
}

/// Read-only worktree inventory for one discovered state directory.
#[derive(Debug, Clone)]
pub struct StateWorktreeInventory {
    /// Resolved real state-directory path.
    pub state_directory: PathBuf,
    pub source: StateDirectorySource,
    /// Active managed worktrees owned by the queried repository.
    pub views: Vec<ViewStorageAccounting>,
    /// Diagnostic findings inside this state directory.
    pub diagnostic_issues: Vec<StateDiagnosticIssue>,
}

/// Read-only worktree inventory across every state directory the repository
/// registers, including its implicit default location.
#[derive(Debug, Clone, Default)]
pub struct AllStatesWorktreeInventory {
    pub states: Vec<StateWorktreeInventory>,
    /// Registrations that discovery reported instead of traversing: missing,
    /// malformed, or unsafe state-directory registrations.
    pub registration_issues: Vec<StateDiagnosticIssue>,
}

/// Inventory managed worktrees across the default state location and every
/// registered state directory of `repository`, without mutating anything.
///
/// Registrations that are missing, not absolute, or not real directories are
/// reported as diagnostic entries and never traversed. Worktrees belonging to
/// other repositories that share a state directory are filtered out by
/// repository identity.
pub fn worktree_inventory_across_states(
    repository: &Path,
) -> Result<AllStatesWorktreeInventory, WorktreeError> {
    let git = Git::default();
    let repository_info = git.inspect_repository(repository)?;
    let repository_root = git_command_root(&repository_info).ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a repository directory".to_owned())
    })?;
    let query_identity = repository_info.identity.common_git_dir.clone();

    let mut inventory = AllStatesWorktreeInventory::default();
    let mut discovered: Vec<(PathBuf, StateDirectorySource)> = Vec::new();

    let default = repository_info.identity.common_git_dir.join("riftri");
    match resolve_real_state_directory_if_present(&default) {
        Ok(Some(resolved)) => discovered.push((resolved, StateDirectorySource::Default)),
        Ok(None) => {}
        Err(error) => inventory.registration_issues.push(StateDiagnosticIssue {
            path: default,
            base_count_impact: BaseCountImpact::MayHideReference,
            reason: format!(
                "default Riftri state path is not a real directory; discovery did not traverse it: {error}"
            ),
        }),
    }

    for configured in git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)? {
        if !configured.is_absolute() {
            inventory.registration_issues.push(StateDiagnosticIssue {
                path: configured,
                base_count_impact: BaseCountImpact::MayHideReference,
                reason: "registered Riftri state directory is not absolute; discovery skipped it"
                    .to_owned(),
            });
            continue;
        }
        match fs::symlink_metadata(&configured) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                inventory.registration_issues.push(StateDiagnosticIssue {
                    path: configured,
                    base_count_impact: BaseCountImpact::MayHideReference,
                    reason: "registered Riftri state directory is missing; \
                             `riftri state unregister` can remove the stale registration"
                        .to_owned(),
                });
                continue;
            }
            Err(source) => {
                inventory.registration_issues.push(StateDiagnosticIssue {
                    path: configured,
                    base_count_impact: BaseCountImpact::MayHideReference,
                    reason: format!(
                        "registered Riftri state directory could not be inspected: {source}"
                    ),
                });
                continue;
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                inventory.registration_issues.push(StateDiagnosticIssue {
                    path: configured,
                    base_count_impact: BaseCountImpact::MayHideReference,
                    reason: "registered Riftri state path is not a real directory; \
                             discovery did not traverse it"
                        .to_owned(),
                });
                continue;
            }
            Ok(_) => {}
        }
        match resolve_real_state_directory(&configured) {
            Ok(resolved) => {
                if !discovered
                    .iter()
                    .any(|(existing, _)| paths_match(existing, &resolved))
                {
                    discovered.push((resolved, StateDirectorySource::Registered));
                }
            }
            Err(error) => inventory.registration_issues.push(StateDiagnosticIssue {
                path: configured,
                base_count_impact: BaseCountImpact::MayHideReference,
                reason: format!("registered Riftri state directory could not be resolved: {error}"),
            }),
        }
    }

    let mut identity_cache: BTreeMap<PathBuf, Option<PathBuf>> = BTreeMap::new();
    // The query root has already passed the same full inspection. Reuse that
    // snapshot only for this exact path; distinct recorded roots (including a
    // main root queried from a linked worktree) still get their own inspection.
    identity_cache.insert(repository_root.to_path_buf(), Some(query_identity.clone()));
    for (state_directory, source) in discovered {
        let report = match storage_accounting(&state_directory) {
            Ok(report) => report,
            Err(error) => {
                inventory.states.push(StateWorktreeInventory {
                    diagnostic_issues: vec![StateDiagnosticIssue {
                        path: state_directory.clone(),
                        base_count_impact: BaseCountImpact::MayHideReference,
                        reason: format!(
                            "state directory could not be inventoried; discovery reported it instead of guessing: {error}"
                        ),
                    }],
                    state_directory,
                    source,
                    views: Vec::new(),
                });
                continue;
            }
        };
        let mut views = Vec::new();
        let mut diagnostic_issues = report.diagnostic_issues;
        for view in report.views {
            let identity = identity_cache
                .entry(view.repository.clone())
                .or_insert_with(|| {
                    git.inspect_repository(&view.repository)
                        .ok()
                        .map(|info| info.identity.common_git_dir)
                });
            match identity {
                Some(identity) if paths_match(identity, &query_identity) => views.push(view),
                Some(_) => {}
                None => diagnostic_issues.push(StateDiagnosticIssue {
                    path: view.destination,
                    base_count_impact: BaseCountImpact::MayHideReference,
                    reason: format!(
                        "managed worktree belongs to a repository that could not be inspected; \
                         it was left out of the repository-filtered inventory: {}",
                        view.repository.display()
                    ),
                }),
            }
        }
        inventory.states.push(StateWorktreeInventory {
            state_directory,
            source,
            views,
            diagnostic_issues,
        });
    }
    Ok(inventory)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn register_state_directory(
    git: &Git,
    repository: &riftri_git::RepositoryInfo,
    state_directory: &Path,
) -> Result<(), WorktreeError> {
    let repository_root = git_command_root(repository).ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a repository directory".to_owned())
    })?;
    let default = repository.identity.common_git_dir.join("riftri");
    if resolve_real_state_directory_if_present(&default)?.as_deref() == Some(state_directory) {
        return Ok(());
    }
    let _lock = acquire_state_directory_locator_lock(&repository.identity.common_git_dir)?;
    let registered = git.local_config_paths(repository_root, STATE_DIRECTORY_CONFIG_KEY)?;
    for registered in registered {
        if registered == state_directory
            || resolve_registered_state_directory(repository_root, &registered)? == state_directory
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

/// Claim the per-worktree operation lock for a managed worktree.
///
/// Every lifecycle operation that mutates one worktree — add, remove, move, and
/// compact — takes this lock, keyed on the add journal that owns the worktree,
/// so that at most one of them is ever in flight for a given worktree.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn lock_managed_worktree(
    managed: &DecodedJournal,
    destination: &Path,
) -> Result<AddOperationLock, WorktreeError> {
    try_lock_add_operation(&managed.journal_path)?.ok_or_else(|| WorktreeError::Busy {
        message: format!(
            "worktree {} is busy with another Riftri operation; wait for it to finish and retry",
            destination.display()
        ),
    })
}

/// Re-resolve a managed worktree after its operation lock has been taken.
///
/// The lookup that precedes the lock is only advisory: a competing lifecycle
/// operation can publish its journal between that read and the moment the lock
/// is acquired. Without this re-check the loser of that race proceeds against a
/// worktree another operation already claimed, and leaves behind a journal whose
/// filesystem shape no `resume_*` branch can classify — which permanently blocks
/// `repair`, `prune`, and base reclamation. Confirming the claim under the lock
/// turns that silent wedge into a clean, retryable refusal.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn revalidate_managed_add_journal(
    state_directory: &Path,
    destination: &Path,
    expected: &DecodedJournal,
) -> Result<(), WorktreeError> {
    let current = find_managed_add_journal(state_directory, destination)?.ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "{} is no longer an active Riftri-managed worktree in {}",
            destination.display(),
            state_directory.display()
        ))
    })?;
    if current.operation_id != expected.operation_id {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} was claimed by another Riftri operation while waiting for its worktree lock",
            destination.display()
        )));
    }
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

/// Approximate the ACTUAL physical footprint of every retained base and active
/// view, counting shared copy-on-write blocks once.
///
/// Each view is a copy-on-write clone that shares physical blocks with its
/// base, so the view's own `allocated_bytes` (blocks * 512) re-counts blocks
/// that already belong to the base. Naively summing base and view allocations
/// therefore double-counts shared storage: a freshly created view would report
/// roughly its base's full allocation even though almost nothing new was
/// physically written — badly overstating usage for a tool whose headline value
/// is space savings.
///
/// Instead this counts every base's blocks once and, for each view, adds only
/// the blocks that diverge from its base: `max(0, view - base)`. Views are
/// linked to their base by `ViewStorageAccounting::base_path`, which matches a
/// `BaseStorageAccounting::path`. When a view's base cannot be resolved (e.g.
/// the base directory is gone) it falls back to counting the view's full
/// allocation, which is conservative — it never under-reports.
///
/// The subtraction only applies to backends whose per-tree measurement
/// re-counts the shared base blocks (APFS clone, reflink, ReFS block clone).
/// OverlayFS measures only its private upper/work layers, which are already
/// base-exclusive, so its `allocated_bytes` is added whole — subtracting the
/// base again would saturate every OverlayFS view to zero and silently drop
/// its real on-disk write cost from the total.
///
/// This remains an approximation: it assumes a view's extra allocation is
/// entirely unshared and does not detect blocks shared between sibling views.
fn cow_aware_allocated_total(
    bases: &[BaseStorageAccounting],
    views: &[ViewStorageAccounting],
) -> u64 {
    let base_allocated: BTreeMap<&Path, u64> = bases
        .iter()
        .map(|base| (base.path.as_path(), base.allocated_bytes))
        .collect();
    bases
        .iter()
        .map(|base| base.allocated_bytes)
        .chain(views.iter().map(|view| {
            if view.backend.allocation_excludes_shared_base() {
                // Already base-exclusive: the private layers are the unique cost.
                return view.allocated_bytes;
            }
            match base_allocated.get(view.base_path.as_path()) {
                Some(&base_bytes) => view.allocated_bytes.saturating_sub(base_bytes),
                None => view.allocated_bytes,
            }
        }))
        .fold(0_u64, u64::saturating_add)
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
            base_count_impact: BaseCountImpact::MayHideReference,
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
    let adds_by_id = index_journals_by(&loaded_add_journals, |journal| {
        journal.operation_id.as_str()
    });
    for journal in loaded_removal_journals {
        let source = adds_by_id
            .get(journal.source_add_operation_id.as_str())
            .and_then(|group| group.first().copied());
        match validate_removal_against_add_source(
            &state_directory,
            &journal,
            source,
        ) {
            Ok(()) => removal_journals.push(journal),
            // A leftover of an interrupted retirement: finished, and named by
            // its add journal's `.retired` marker.
            Err(_) if retired_removal_without_add(&state_directory, &journal, source.is_none()) => {
                removal_journals.push(journal)
            }
            Err(error) => invalid_removal_journals.push(StateDiagnosticIssue {
                path: journal.journal_path.clone(),
                // Distrusting this journal keeps its operation out of
                // `completed`, so the add it claims to have retired stays
                // counted. That can only over-count a base, never hide one.
                base_count_impact: BaseCountImpact::CountsUnaffected,
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
    // Name every worktree Git itself lists without a resolvable HEAD, managed
    // or not: it needs `git worktree repair` (or removal), and nothing else in
    // this report may treat it as clean. Listing failures are not reported
    // here; the per-journal validation below already surfaces them.
    let mut unresolvable_worktree_issues = Vec::new();
    let mut inspected_repositories = HashSet::new();
    // One listing per repository serves every journal below: listing again
    // per journal made status quadratic in the number of worktrees.
    let mut inventories = HashMap::new();
    for journal in &loaded_add_journals {
        if !inspected_repositories.insert(journal.repository.clone()) {
            continue;
        }
        let Ok(registered) = git.list_worktrees(&journal.repository) else {
            continue;
        };
        for worktree in &registered {
            if worktree.head_unresolvable {
                unresolvable_worktree_issues.push(StateDiagnosticIssue {
                    path: worktree.path.clone(),
                    base_count_impact: BaseCountImpact::MayHideReference,
                    reason: "Git cannot resolve this worktree's HEAD; Riftri left it \
                             alone. `git worktree repair` or removing the worktree \
                             clears this"
                        .to_owned(),
                });
            }
        }
        inventories.insert(
            journal.repository.clone(),
            WorktreeInventory::new(registered),
        );
    }
    let mut add_journals = Vec::with_capacity(loaded_add_journals.len());
    let mut registered_worktrees = BTreeMap::new();
    let mut invalid_add_journals = Vec::new();
    // Bases still claimed by an active add journal whose destination Git no
    // longer registers. `validate_status_add_journal` only reaches its
    // `Unregistered` arm after rejecting non-active and removal-complete
    // journals, so these are exactly the claims `protected_bases` refuses to
    // collect for `gc`. Reporting them as unreferenced would contradict the
    // command that actually holds them.
    let mut unregistered_base_claims = Vec::new();
    let claimed = status_claimed_destinations(&loaded_add_journals);
    for journal in loaded_add_journals {
        match validate_status_add_journal(
            &git,
            &inventories,
            &state_directory,
            &journal,
            completed.contains(&journal.operation_id),
            &claimed,
        ) {
            Ok(StatusAddJournal::Tracked(worktree)) => {
                if let Some(worktree) = worktree {
                    registered_worktrees.insert(journal.operation_id.clone(), worktree);
                }
                add_journals.push(journal);
            }
            Ok(StatusAddJournal::Unregistered(reason)) => {
                unregistered_base_claims.push(journal.base_path.clone());
                invalid_add_journals.push(StateDiagnosticIssue {
                    path: journal.journal_path.clone(),
                    // The push above counts this journal's base, so the claim
                    // this diagnostic describes is already in the totals.
                    base_count_impact: BaseCountImpact::CountsUnaffected,
                    reason,
                })
            }
            Err(error) => invalid_add_journals.push(StateDiagnosticIssue {
                path: journal.journal_path.clone(),
                base_count_impact: BaseCountImpact::MayHideReference,
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
                base_count_impact: BaseCountImpact::MayHideReference,
                reason: format!(
                    "unsafe durable compaction journal; Riftri did not trust its lifecycle claim: {error}"
                ),
            }),
        }
    }
    let mut references = BTreeMap::<PathBuf, usize>::new();
    for base_path in unregistered_base_claims {
        *references.entry(base_path).or_default() += 1;
    }
    let mut views = Vec::new();
    let mut view_usage_requests = Vec::new();
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
            #[cfg(target_os = "linux")]
            let allocated_path = if journal.backend == BackendKind::OverlayFs
                && let Some(overlayfs) = journal.overlayfs.as_ref()
                && overlayfs.layout_root.exists()
            {
                Some(overlayfs.layout_root.clone())
            } else {
                None
            };
            #[cfg(not(target_os = "linux"))]
            let allocated_path = None;
            view_usage_requests.push(TreeUsageRequest {
                path: journal.destination.clone(),
                allocated_path,
            });
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
                logical_bytes: 0,
                allocated_bytes: 0,
            });
        }
    }
    for (view, (logical_bytes, allocated_bytes)) in
        views.iter_mut().zip(tree_usages(&view_usage_requests)?)
    {
        view.logical_bytes = logical_bytes;
        view.allocated_bytes = allocated_bytes;
    }
    views.sort_unstable_by(|left, right| left.destination.cmp(&right.destination));

    for base_path in retained_base_paths(&state_directory, UnsafeBaseInventory::Ignore)? {
        references.entry(base_path).or_default();
    }
    let references = references.into_iter().collect::<Vec<_>>();
    let mut base_usage_requests = Vec::new();
    let base_usage_indices = references
        .iter()
        .map(|(path, _)| {
            path.is_dir().then(|| {
                let index = base_usage_requests.len();
                base_usage_requests.push(TreeUsageRequest {
                    path: path.clone(),
                    allocated_path: None,
                });
                index
            })
        })
        .collect::<Vec<_>>();
    let base_usages = tree_usages(&base_usage_requests)?;
    let mut bases = Vec::with_capacity(references.len());
    for ((path, reference_count), usage_index) in references.into_iter().zip(base_usage_indices) {
        let (logical_bytes, allocated_bytes) = usage_index
            .map(|index| base_usages[index])
            .unwrap_or((0, 0));
        let damaged = is_regular_file_if_present(&damaged_base_marker(&path))?;
        bases.push(BaseStorageAccounting {
            path,
            reference_count,
            logical_bytes,
            allocated_bytes,
            damaged,
        });
    }

    let total_logical_bytes = bases
        .iter()
        .map(|base| base.logical_bytes)
        .chain(views.iter().map(|view| view.logical_bytes))
        .fold(0_u64, u64::saturating_add);
    let total_allocated_bytes = cow_aware_allocated_total(&bases, &views);
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
    state_diagnosis.issues.extend(unresolvable_worktree_issues);
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
        cancelled_removals: removal_journals
            .iter()
            .filter(|journal| journal.phase == RemoveWorktreePhase::Cancelled)
            .count(),
        pending_removals: removal_journals
            .iter()
            .filter(|journal| !journal.phase.is_finished())
            .count(),
        completed_moves: move_journals
            .iter()
            .filter(|journal| journal.phase == MoveWorktreePhase::Complete)
            .count(),
        cancelled_moves: move_journals
            .iter()
            .filter(|journal| journal.phase == MoveWorktreePhase::Cancelled)
            .count(),
        pending_moves: move_journals
            .iter()
            .filter(|journal| !journal.phase.is_finished())
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn status_claimed_destinations(journals: &[DecodedJournal]) -> HashSet<PathBuf> {
    claimed_destinations(journals)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn status_claimed_destinations(_journals: &[DecodedJournal]) -> HashSet<PathBuf> {
    HashSet::new()
}

/// How `status` accounts for one add journal.
enum StatusAddJournal {
    /// A journal status tracks, with Git's registration when it is active.
    Tracked(Option<riftri_git::WorktreeInfo>),
    /// An active journal whose worktree Git no longer registers at its path,
    /// with the explanation and remedy to report instead.
    Unregistered(String),
}

struct WorktreeInventory {
    entries: Vec<riftri_git::WorktreeInfo>,
    by_path: HashMap<PathBuf, usize>,
    by_alias: HashMap<Vec<u8>, usize>,
}

impl WorktreeInventory {
    fn new(entries: Vec<riftri_git::WorktreeInfo>) -> Self {
        let mut by_path = HashMap::with_capacity(entries.len());
        let mut by_alias = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            // Preserve the previous linear search's first-match behavior.
            by_path.entry(entry.path.clone()).or_insert(index);
            if let Some(key) = inventory_path_key(&entry.path) {
                by_alias.entry(key).or_insert(index);
            }
        }
        Self {
            entries,
            by_path,
            by_alias,
        }
    }

    fn find(&self, path: &Path) -> Option<&riftri_git::WorktreeInfo> {
        let alias = inventory_path_key(path).and_then(|key| self.by_alias.get(&key));
        // Windows paths_match uses only its normalized wide representation.
        let exact = if cfg!(target_os = "windows") {
            None
        } else {
            self.by_path.get(path)
        };
        let index = exact.into_iter().chain(alias).min();
        index.map(|index| &self.entries[*index])
    }
}

// This key is deliberately identical to paths_match, including platform
// aliases and lossless non-UTF-8 paths. It is scoped to one status snapshot.
fn inventory_path_key(path: &Path) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        use unicode_normalization::UnicodeNormalization;
        if let Some(text) = path.to_str() {
            return Some(text.nfc().collect::<String>().into_bytes());
        }
    }
    #[cfg(target_os = "windows")]
    {
        Some(
            windows_path_key(path)
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect(),
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        None
    }
}

fn validate_status_add_journal(
    git: &Git,
    inventories: &HashMap<PathBuf, WorktreeInventory>,
    state_directory: &Path,
    journal: &DecodedJournal,
    removal_complete: bool,
    claimed: &HashSet<PathBuf>,
) -> Result<StatusAddJournal, WorktreeError> {
    validate_recovery_paths(state_directory, journal)?;
    if journal.phase != AddWorktreePhase::Active || removal_complete {
        return Ok(StatusAddJournal::Tracked(None));
    }
    if !journal.repository.is_dir() && journal.destination.is_dir() {
        return Ok(StatusAddJournal::Unregistered(format!(
            "this journal records repository {}, which no longer exists (likely a linked worktree the add ran in that has since been removed); `riftri repair` re-homes it to the repository's main worktree while the worktree still belongs to this repository",
            journal.repository.display()
        )));
    }
    // A repository whose listing failed above is listed again here, so its
    // error is reported against this journal exactly as before.
    let listed;
    let inventory = match inventories.get(&journal.repository) {
        Some(inventory) => inventory,
        None => {
            listed = WorktreeInventory::new(git.list_worktrees(&journal.repository)?);
            &listed
        }
    };
    let Some(registered) = inventory.find(&journal.destination).cloned() else {
        return unregistered_destination_advice(&inventory.entries, claimed, journal)
            .map(StatusAddJournal::Unregistered);
    };
    if registered.head_unresolvable {
        return Err(WorktreeError::InvalidRequest(format!(
            "Git cannot resolve the worktree HEAD of active destination {}; \
             `git worktree repair` or removing the worktree clears this",
            journal.destination.display()
        )));
    }
    if registered.head.is_none() {
        return Err(WorktreeError::InvalidRequest(format!(
            "active destination {} has no Git HEAD commit",
            journal.destination.display()
        )));
    }
    Ok(StatusAddJournal::Tracked(Some(registered)))
}

/// Explain an active journal whose worktree Git no longer registers at its
/// path, which is what `git worktree remove` or `git worktree move` run
/// directly on a managed worktree leaves behind, and name what clears it.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn unregistered_destination_advice(
    inventory: &[riftri_git::WorktreeInfo],
    claimed: &HashSet<PathBuf>,
    journal: &DecodedJournal,
) -> Result<String, WorktreeError> {
    let destination = journal.destination.display();
    Ok(
        match classify_active_destination(inventory, claimed, journal)? {
            ActiveDestinationState::Vanished => format!(
                "managed worktree {destination} was removed outside Riftri; \
             `riftri repair` retires this journal and releases its base"
            ),
            ActiveDestinationState::Relocated(path) => format!(
                "managed worktree {destination} was moved outside Riftri and Git now registers it at {}; \
             Riftri does not adopt the new path. Move it back with `git worktree move {} {destination}`, \
             or remove it with `git worktree remove {}` and then run `riftri repair`",
                path.display(),
                path.display(),
                path.display()
            ),
            ActiveDestinationState::Present | ActiveDestinationState::Registered => format!(
                "{destination} still exists but is not registered by Git as a worktree; \
             Riftri left it and its journal alone"
            ),
        },
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn unregistered_destination_advice(
    _inventory: &[riftri_git::WorktreeInfo],
    _claimed: &HashSet<PathBuf>,
    journal: &DecodedJournal,
) -> Result<String, WorktreeError> {
    Ok(format!(
        "active destination {} is not registered by Git for {}",
        journal.destination.display(),
        journal.repository.display()
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn garbage_collect_inner(
    _state_directory: &Path,
    _apply: bool,
    _fail_after: Option<GarbageCollectionPhase>,
) -> Result<GarbageCollectionReport, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "immutable-base garbage collection currently requires macOS, Linux, or Windows".to_owned(),
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

    // History finished before this run is retired first, so the journals
    // this run resumes or writes stay visible until the next run.
    let retired_journals = if apply {
        retire_finished_journals(&state_directory, true)?
    } else {
        0
    };
    let resumed_collections = if apply {
        recover_collection_journals(&state_directory)?
    } else {
        0
    };
    let (candidates, skipped_protected) = garbage_collection_candidates(&state_directory)?;
    progress::emit(ProgressEvent::GcPlanned {
        candidates: candidates.len(),
    });
    let mut report = GarbageCollectionReport {
        applied: apply,
        candidates: candidates.clone(),
        skipped_protected,
        resumed_collections,
        ..GarbageCollectionReport::default()
    };
    if !apply {
        report.retirable_journals = retire_finished_journals(&state_directory, false)?;
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
    report.retired_journals = retired_journals;
    Ok(report)
}

/// The journals of one finished lineage, read while its add lock is held.
struct RetirableLineage {
    /// Completed removals, deleted after the add journal becomes its marker.
    removals: Vec<PathBuf>,
    /// Finished moves and compactions, deleted first.
    dependents: Vec<PathBuf>,
}

/// Re-read one add operation's lineage under its lock and return it only if
/// it is still finished: the worktree is gone for good and nothing of its
/// history is unfinished. `None` leaves it for a later run.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn locked_retirable_lineage(
    state_directory: &Path,
    add_operation_id: &str,
) -> Result<Option<RetirableLineage>, WorktreeError> {
    let Ok(add) = JournalStore::open(state_directory).load_operation(add_operation_id) else {
        return Ok(None);
    };
    let removals = RemovalJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| journal.source_add_operation_id == add_operation_id)
        .collect::<Vec<_>>();
    let moves = MoveJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| journal.source_add_operation_id == add_operation_id)
        .collect::<Vec<_>>();
    let compactions = CompactJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| journal.source_add_operation_id == add_operation_id)
        .collect::<Vec<_>>();
    let removal_completed = removals
        .iter()
        .any(|removal| removal.phase == RemoveWorktreePhase::Complete);
    let gone_for_good = add.phase == AddWorktreePhase::RolledBack
        || (add.phase == AddWorktreePhase::Active && removal_completed);
    let history_finished = removals.iter().all(|removal| removal.phase.is_finished())
        && moves.iter().all(|journal| journal.phase.is_finished())
        && compactions.iter().all(|journal| {
            matches!(
                journal.phase,
                CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
            )
        });
    if !gone_for_good || !history_finished {
        return Ok(None);
    }
    Ok(Some(RetirableLineage {
        removals: removals
            .into_iter()
            .map(|journal| journal.journal_path)
            .collect(),
        dependents: moves
            .into_iter()
            .map(|journal| journal.journal_path)
            .chain(compactions.into_iter().map(|journal| journal.journal_path))
            .collect(),
    }))
}

/// Beside an add journal: its retirement has started. See
/// `retire_finished_journals`.
fn retiring_add_marker(add_journal: &Path) -> PathBuf {
    add_journal.with_extension("retired")
}

/// Whether the add operation `add_operation_id` is being retired: a finished
/// journal that names it is then a leftover of that retirement, not a claim.
fn add_operation_is_retiring(state_directory: &Path, add_operation_id: &str) -> bool {
    retiring_add_marker(&JournalStore::open(state_directory).path_for(add_operation_id)).is_file()
}

/// Count, or with `apply` delete, finished journal history (#536).
///
/// Completed journals were never deleted, and every lifecycle command reads
/// them all, so a busy repository slowed down for good. A lineage is finished
/// once its worktree is gone for good: the add rolled back, or a removal
/// completed. Then its add journal, its finished moves and compactions, and
/// its completed removals are deleted, along with completed prune and
/// finished collection journals. Nothing is retired while any journal cannot
/// be read, since an unreadable journal may still hold a claim.
///
/// The order keeps every interrupted state explained. Finished moves and
/// compactions go first, leaving a valid add-plus-removal history. The add
/// journal is then renamed to `<id>.retired` in one atomic step, which marks
/// the completed removals that still name it as leftovers; they go next, and
/// the marker and lock last. A later run finishes any retirement it finds.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn retire_finished_journals(state_directory: &Path, apply: bool) -> Result<usize, WorktreeError> {
    let adds = JournalStore::open(state_directory).load_all_for_status()?;
    let removals = RemovalJournalStore::open(state_directory).load_all_for_status()?;
    let moves = MoveJournalStore::open(state_directory).load_all_for_status()?;
    let compactions = CompactJournalStore::open(state_directory).load_all_for_status()?;
    let prunes = PruneJournalStore::open(state_directory).load_all_for_status()?;
    let collections = CollectionJournalStore::open(state_directory).load_all_for_status()?;
    if !(adds.issues.is_empty()
        && removals.issues.is_empty()
        && moves.issues.is_empty()
        && compactions.issues.is_empty()
        && prunes.issues.is_empty()
        && collections.issues.is_empty())
    {
        return Ok(0);
    }
    let mut retired = 0;
    let removals_by_add = index_journals_by(&removals.journals, |journal| {
        journal.source_add_operation_id.as_str()
    });
    let moves_by_add = index_journals_by(&moves.journals, |journal| {
        journal.source_add_operation_id.as_str()
    });
    let compactions_by_add = index_journals_by(&compactions.journals, |journal| {
        journal.source_add_operation_id.as_str()
    });

    // Finish retirements an earlier run started.
    let operations = state_directory.join("operations");
    if is_real_directory_if_present(&operations)? {
        for marker in child_paths(&operations, "read Riftri journal directory")? {
            let Some(operation_id) = marker
                .extension()
                .filter(|extension| *extension == OsStr::new("retired"))
                .and(marker.file_stem())
                .and_then(OsStr::to_str)
                .filter(|stem| journal_identifier_like(stem))
            else {
                continue;
            };
            let leftovers = RemovalJournalStore::open(state_directory)
                .load_all()?
                .iter()
                .filter(|removal| {
                    removal.source_add_operation_id == operation_id && removal.phase.is_finished()
                })
                .map(|removal| removal.journal_path.clone())
                .collect::<Vec<_>>();
            retired += leftovers.len() + 1;
            if apply {
                for leftover in &leftovers {
                    remove_file_if_present(leftover)?;
                }
                remove_file_if_present(&marker)?;
                remove_file_if_present(&marker.with_extension("lock"))?;
                sync_parent(&marker)?;
            }
        }
    }

    for add in &adds.journals {
        let own_removals = removals_by_add
            .get(add.operation_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let own_moves = moves_by_add
            .get(add.operation_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let own_compactions = compactions_by_add
            .get(add.operation_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let removal_completed = own_removals
            .iter()
            .any(|removal| removal.phase == RemoveWorktreePhase::Complete);
        let gone_for_good = add.phase == AddWorktreePhase::RolledBack
            || (add.phase == AddWorktreePhase::Active && removal_completed);
        let history_finished = own_removals
            .iter()
            .all(|removal| removal.phase.is_finished())
            && own_moves.iter().all(|journal| journal.phase.is_finished())
            && own_compactions.iter().all(|journal| {
                matches!(
                    journal.phase,
                    CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                )
            });
        if !gone_for_good || !history_finished {
            continue;
        }
        if !apply {
            retired += 1 + own_removals.len() + own_moves.len() + own_compactions.len();
            continue;
        }
        // Another process may still be finishing this operation.
        let Some(_lock) = try_lock_add_operation(&add.journal_path)? else {
            continue;
        };
        #[cfg(test)]
        crate::test_hooks::fire(
            crate::test_hooks::FilesystemRacePoint::JournalRetirementLocked,
            &add.journal_path,
        );
        // Decide again under the lock, from journals read now. The snapshot
        // above predates it: a concurrent repair may since have recorded
        // another removal of this worktree, and deleting only what the
        // snapshot knew left that one behind with no add journal, which then
        // failed every lifecycle command in the state directory.
        let Some(lineage) = locked_retirable_lineage(state_directory, &add.operation_id)? else {
            continue;
        };
        for path in &lineage.dependents {
            remove_file_if_present(path)?;
        }
        let marker = retiring_add_marker(&add.journal_path);
        if lineage.removals.is_empty() {
            remove_file_if_present(&add.journal_path)?;
        } else {
            fs::rename(&add.journal_path, &marker)
                .map_err(|source| io("mark add journal for retirement", &marker, source))?;
            sync_parent(&marker)?;
            for removal in &lineage.removals {
                remove_file_if_present(removal)?;
            }
            remove_file_if_present(&marker)?;
        }
        remove_file_if_present(&add.journal_path.with_extension("lock"))?;
        sync_parent(&add.journal_path)?;
        retired += 1 + lineage.removals.len() + lineage.dependents.len();
    }

    for path in prunes
        .journals
        .iter()
        .filter(|journal| journal.phase == PruneWorktreesPhase::Complete)
        .map(|journal| &journal.journal_path)
        .chain(
            collections
                .journals
                .iter()
                .filter(|journal| {
                    matches!(
                        journal.phase,
                        GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
                    )
                })
                .map(|journal| &journal.journal_path),
        )
    {
        retired += 1;
        if apply {
            remove_file_if_present(path)?;
        }
    }
    Ok(retired)
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
) -> Result<(Vec<GarbageCollectionCandidate>, Vec<ProtectedBase>), WorktreeError> {
    let protections = protected_bases(state_directory)?;
    let protected = protections
        .iter()
        .map(|protection| protection.base_path.clone())
        .collect::<HashSet<_>>();
    let pending = CollectionJournalStore::open(state_directory)
        .load_all()?
        .into_iter()
        .filter(|journal| {
            !matches!(
                journal.phase,
                GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
            )
        })
        .map(|journal| (journal.base_path, journal.operation_id))
        .collect::<Vec<_>>();
    let pending_paths = pending
        .iter()
        .map(|(base_path, _)| base_path.clone())
        .collect::<HashSet<_>>();
    let mut candidates = Vec::new();
    // A base skipped here is storage the user cannot reclaim, so name every
    // skip and the journal responsible for it instead of silently dropping it.
    let mut skipped = Vec::new();
    for base_path in retained_base_paths(state_directory, UnsafeBaseInventory::Reject)? {
        if protected.contains(&base_path) || pending_paths.contains(&base_path) {
            skipped.extend(
                protections
                    .iter()
                    .filter(|protection| protection.base_path == base_path)
                    .cloned(),
            );
            skipped.extend(pending.iter().filter(|(path, _)| path == &base_path).map(
                |(path, operation_id)| {
                    ProtectedBase {
                        base_path: path.clone(),
                        operation_id: operation_id.clone(),
                        reason: "an unfinished garbage-collection journal already claims this base"
                            .to_owned(),
                    }
                },
            ));
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
    skipped.sort_unstable_by(|left, right| {
        (&left.base_path, &left.operation_id).cmp(&(&right.base_path, &right.operation_id))
    });
    skipped.dedup();
    Ok((candidates, skipped))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn protected_base_paths(state_directory: &Path) -> Result<HashSet<PathBuf>, WorktreeError> {
    Ok(protected_bases(state_directory)?
        .into_iter()
        .map(|protection| protection.base_path)
        .collect())
}

/// Every retained base a durable journal still claims, paired with the journal
/// that claims it. `gc` must never collect these, but it must be able to say
/// why, so this returns the reasons rather than only the path set.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn protected_bases(state_directory: &Path) -> Result<Vec<ProtectedBase>, WorktreeError> {
    let adds = JournalStore::open(state_directory).load_all()?;
    let removals = RemovalJournalStore::open(state_directory).load_all()?;
    let completed = validated_completed_removal_ids(state_directory, &adds, &removals)?;
    let mut protected = Vec::new();
    for journal in adds {
        if journal.phase == AddWorktreePhase::RolledBack
            || completed.contains(&journal.operation_id)
        {
            continue;
        }
        let reason = if journal.phase == AddWorktreePhase::Active {
            "an active add journal references this base".to_owned()
        } else {
            format!(
                "an add journal stopped in phase {:?} still references this base",
                journal.phase
            )
        };
        protected.push(ProtectedBase {
            base_path: journal.base_path,
            operation_id: journal.operation_id,
            reason,
        });
    }
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
        for base_path in [journal.old_base_path, journal.base_path] {
            protected.push(ProtectedBase {
                base_path,
                operation_id: journal.operation_id.clone(),
                reason: "an unfinished compaction journal still references this base".to_owned(),
            });
        }
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
            // A new reference appeared after this journal removed the
            // completion marker. `retained_base_paths` enumerates only via
            // markers, so cancelling without restoring it would leave a fully
            // materialized base that no later `gc` or `status` inventory can
            // ever see again. Rebuild the marker from the base content in the
            // same journaled step as the cancellation; on failure the journal
            // stays at `MarkerRemoved`, which keeps the base explained and
            // makes recovery retry the restore.
            restore_completion_marker(journal)?;
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
        clear_damaged_base_marker(&journal.base_path)?;
        // An interrupted marker restore may have staged a marker before the
        // protecting reference disappeared again; the collection is finishing
        // now, so retire that leftover instead of reporting it forever.
        remove_file_if_present(&restored_marker_staging_path(journal)?)?;
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

/// Deterministic staging path for a marker rebuilt by a cancelling
/// collection. A fixed name lets an interrupted restore retry over its own
/// leftover instead of accumulating temporaries.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn restored_marker_staging_path(
    journal: &DecodedCollectionJournal,
) -> Result<PathBuf, WorktreeError> {
    Ok(journal
        .marker_path
        .expect_parent()?
        .join(format!(".riftri-gc-{}.marker", journal.operation_id)))
}

/// Rebuild the completion marker for a base whose collection is cancelling
/// after `MarkerRemoved`. The caller holds the exclusive base coordination
/// lock, so the base is stable while it is re-hashed; recomputing the current
/// (v2) integrity marker from the tree on disk (instead of replaying
/// remembered bytes) means the restored marker never vouches for anything
/// except what reuse verification will re-hash later, so it can never bless a
/// base that was modified behind Riftri's back. The marker is staged and
/// renamed into place so no interruption window can leave a truncated marker.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn restore_completion_marker(journal: &DecodedCollectionJournal) -> Result<(), WorktreeError> {
    match fs::symlink_metadata(&journal.marker_path) {
        // A prior restore (or a concurrent rebuild between resume attempts)
        // already produced a real marker; the base is enumerable again.
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            return Ok(());
        }
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
    match fs::symlink_metadata(&journal.base_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(WorktreeError::InvalidRequest(format!(
                "collectible base {} is not a real directory",
                journal.base_path.display()
            )));
        }
        // Nothing is materialized, so there is no storage to make enumerable
        // again; the referencing operation builds base and marker together.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io("inspect collectible base", &journal.base_path, source)),
    }
    let integrity = crate::base_integrity::marker_v2(&journal.base_path).map_err(|source| {
        io(
            "recompute immutable-base integrity",
            &journal.base_path,
            source,
        )
    })?;
    let staging = restored_marker_staging_path(journal)?;
    remove_file_if_present(&staging)?;
    let mut staged = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staging)
        .map_err(|source| io("stage restored immutable-base marker", &staging, source))?;
    use std::io::Write;
    staged
        .write_all(&integrity)
        .map_err(|source| io("write restored immutable-base marker", &staging, source))?;
    staged
        .sync_all()
        .map_err(|source| io("sync restored immutable-base marker", &staging, source))?;
    drop(staged);
    fs::rename(&staging, &journal.marker_path).map_err(|source| {
        io(
            "activate restored immutable-base marker",
            &journal.marker_path,
            source,
        )
    })?;
    sync_parent(&journal.marker_path)?;
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
        "journaled Riftri removal currently requires macOS, Linux, or Windows".to_owned(),
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
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let repository_root = stable_repository_root(&git, &repository_root)?;
    let state_given = request.state_dir.is_some();
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let destination = existing_managed_destination(&request.destination, &requested_state)?;
    let state_directory = existing_state_directory_for_worktree(
        &request.repository,
        &requested_state,
        state_given,
        &destination,
    )?;
    let managed = find_managed_add_journal(&state_directory, &destination)?.ok_or_else(|| {
        not_managed_in_state_directory(
            &request.repository,
            &destination,
            &state_directory,
            state_given,
        )
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    // Claim the worktree before reading any state that decides what to mutate,
    // then confirm the claim: a competing compaction or move may have published
    // its journal after the lookup above.
    let _operation_lock = lock_managed_worktree(&managed, &destination)?;
    revalidate_managed_add_journal(&state_directory, &destination, &managed)?;
    // Name the worktree exactly as its add journal does: every journal of one
    // lineage must agree byte for byte, and a request may spell the same path
    // differently (Unicode normalization on macOS).
    let destination = managed.destination.clone();
    let metadata_lock = acquire_git_worktree_metadata_lock(&repository.identity.common_git_dir)?;
    let registered = git
        .list_worktrees(&repository_root)?
        .into_iter()
        .find(|worktree| paths_match(&worktree.path, &destination))
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "{} is not registered as a Git linked worktree",
                destination.display()
            ))
        })?;
    if registered.head_unresolvable {
        // Fail closed: without a resolvable HEAD neither cleanliness nor the
        // recorded branch state can be verified, so a journaled removal could
        // destroy unsaved work.
        return Err(WorktreeError::InvalidRequest(format!(
            "Git cannot resolve the worktree HEAD of {}; run `git worktree repair` \
             before removing it",
            destination.display()
        )));
    }
    refuse_locked_worktree(
        &destination,
        registered.locked_reason.as_deref(),
        "removing",
    )?;
    let overlayfs_clean_snapshot = snapshot_overlayfs_private_layer(&managed)?;
    let force_snapshot = if force {
        Some(snapshot_managed_worktree_for_force(&managed)?)
    } else {
        None
    };
    if !force && let Some(reason) = non_force_removal_blocker(&git, &destination)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} {reason}",
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

    if let Err(error) = remove_managed_worktree_files(
        &git,
        &repository_root,
        &destination,
        &removal_quarantine_path(&destination, &journal.operation_id)?,
        &managed,
        journal.overlayfs_clean_snapshot.as_deref(),
        journal.force,
        journal.force_snapshot.as_deref(),
    ) {
        // A refusal that put the view back removed nothing; cancelling makes
        // the error's promise true instead of leaving a journal that `repair`
        // would refuse forever, or finish once the user undid their change.
        // Should cancelling itself fail, the journal stays for repair.
        let _ = cancel_untouched_removal(&git, &store, &mut journal, &repository_root, &managed);
        return Err(error);
    }
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
        "journaled Riftri moves currently require macOS, Linux, or Windows".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn move_worktree_inner(
    request: MoveWorktreeRequest,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<MoveWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let repository_root = stable_repository_root(&git, &repository_root)?;
    let state_given = request.state_dir.is_some();
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let source = existing_managed_destination(&request.source, &requested_state)?;
    let destination =
        normalize_new_destination(&request.destination, DestinationRules::WorktreeMove)?;
    let state_directory = existing_state_directory_for_worktree(
        &request.repository,
        &requested_state,
        state_given,
        &source,
    )?;
    let managed = find_managed_add_journal(&state_directory, &source)?.ok_or_else(|| {
        not_managed_in_state_directory(&request.repository, &source, &state_directory, state_given)
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    // Claim the worktree before reading any state that decides what to mutate,
    // then confirm the claim: a competing removal or compaction may have
    // published its journal after the lookup above.
    let _operation_lock = lock_managed_worktree(&managed, &source)?;
    revalidate_managed_add_journal(&state_directory, &source, &managed)?;
    // Name the worktree exactly as its add journal does: every journal of one
    // lineage must agree byte for byte, and a request may spell the same path
    // differently (Unicode normalization on macOS).
    let source = managed.destination.clone();
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
    let registered = inventory
        .iter()
        .find(|worktree| paths_match(&worktree.path, &source))
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "{} is not registered as a Git linked worktree",
                source.display()
            ))
        })?;
    if registered.head_unresolvable {
        // Fail closed: a move re-registers the worktree with Git, and a
        // worktree whose HEAD cannot be resolved must be repaired first.
        return Err(WorktreeError::InvalidRequest(format!(
            "Git cannot resolve the worktree HEAD of {}; run `git worktree repair` \
             before moving it",
            source.display()
        )));
    }
    refuse_locked_worktree(&source, registered.locked_reason.as_deref(), "moving")?;
    if destination
        .ancestors()
        .any(|ancestor| paths_match(ancestor, &source))
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "cannot move {} into itself",
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
    if let MoveResumption::Cancelled(error) = resume_move(
        &git,
        &store,
        journal.decode(journal_path.clone())?,
        fail_after,
    )? {
        return Err(WorktreeError::InvalidRequest(format!(
            "Git refused to move {}; nothing was changed: {error}",
            source.display()
        )));
    }

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
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let repository_root = stable_repository_root(&git, &repository_root)?;
    let state_given = request.state_dir.is_some();
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let destination = existing_managed_destination(&request.destination, &requested_state)?;
    let state_directory = existing_state_directory_for_worktree(
        &request.repository,
        &requested_state,
        state_given,
        &destination,
    )?;
    let managed = find_managed_add_journal(&state_directory, &destination)?.ok_or_else(|| {
        not_managed_in_state_directory(
            &request.repository,
            &destination,
            &state_directory,
            state_given,
        )
    })?;
    validate_recovery_paths(&state_directory, &managed)?;
    if managed.backend == BackendKind::OverlayFs {
        return Err(WorktreeError::Unsupported(
            "compacting an active OverlayFS worktree is not yet supported; remove and recreate it to reset the private upper layer"
                .to_owned(),
        ));
    }
    // Compaction rebuilds the view from its creation profile, so it can only
    // run while the worktree still holds that profile. Ordinary Git owns later
    // selection changes and Riftri never rewrites a worktree's immutable
    // creation base, so a view that was widened, narrowed, or disabled since
    // it was created no longer has a base that describes it. Compare the two
    // and say which is which, instead of refusing every sparse worktree.
    let sparse_state = git.sparse_checkout_state(&destination)?;
    if sparse_state.enabled && !sparse_state.cone {
        return Err(WorktreeError::Unsupported(
            "compacting a worktree whose sparse checkout is not in cone mode is not supported yet; Riftri does not model non-cone pattern semantics"
                .to_owned(),
        ));
    }
    let sparse_directories = canonicalize_sparse_directories(&sparse_state.directories)?;
    if sparse_directories != managed.sparse_directories {
        let describe = |directories: &[String]| {
            if directories.is_empty() {
                "a full checkout".to_owned()
            } else {
                format!("cone {}", directories.join(", "))
            }
        };
        return Err(WorktreeError::Unsupported(format!(
            "worktree was created as {} but now holds {}; compaction rebuilds a view from its creation profile, so remove and recreate the worktree to reset its storage",
            describe(&managed.sparse_directories),
            describe(&sparse_directories)
        )));
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
        return Err(pending_lifecycle_error(
            "compaction",
            &destination,
            &state_directory,
        ));
    }

    let _operation_lock = lock_managed_worktree(&managed, &destination)?;
    // The pending-lifecycle check above ran before this lock was held, so a
    // competing removal or move may have claimed the worktree since. Confirm
    // the claim now that no other lifecycle operation can be in flight.
    revalidate_managed_add_journal(&state_directory, &destination, &managed)?;
    // Name the worktree exactly as its add journal does: every journal of one
    // lineage must agree byte for byte, and a request may spell the same path
    // differently (Unicode normalization on macOS).
    let destination = managed.destination.clone();
    let resolved = git.resolve_revision(&destination, OsStr::new("HEAD"))?;
    verify_compaction_source(&git, &repository_root, &destination, &resolved.commit, None)
        .map_err(|error| vanished_during_compaction(error, &destination))?;
    // `verify_compaction_source` proved above that `destination` is one of
    // this repository's registered linked worktrees, so it shares the same
    // common Git directory that `inspect_repository` resolved.
    let compatibility = validate_resolved_compatibility(
        &git,
        &destination,
        &repository.identity.common_git_dir,
        &resolved,
        &sparse_directories,
        None,
    )?;
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
    let expected_snapshot = directory_snapshot(&destination)
        .map_err(|error| vanished_during_compaction(error, &destination))?;
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
    validate_materialization_path_lengths(
        &compatibility.checkout_paths,
        &[&base_path, &base_staging, &replacement, &quarantine],
    )?;
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
    journal.old_expected_commit = Some(managed.expected_commit.clone());
    let journal_path = store.persist(&journal)?;
    fail_compaction_if_requested(journal.phase, fail_after)?;

    let prepared = (|| {
        let reused_base = prepare_base(
            &git,
            &destination,
            &resolved.tree,
            &base_path,
            &base_staging,
            &temporary_index,
            &compatibility.checkout_config,
            &compatibility.lfs_objects,
            &sparse_directories,
        )?;
        NativeCowCloner::clone_tree_owner_writable(&base_path, &replacement)?;
        copy_git_pointer(&destination, &replacement)?;
        if directory_snapshot(&replacement)? != journal.expected_snapshot {
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree {} differs from a fresh checkout of its commit in a way Git does not report, such as a modified file marked assume-unchanged or skip-worktree, or an extended attribute; compaction was cancelled and nothing changed",
                destination.display()
            )));
        }
        sync_parent(&replacement)?;
        Ok(reused_base)
    })();
    let reused_base = match prepared {
        Ok(reused_base) => reused_base,
        Err(error) => {
            // Nothing has touched the worktree yet, so cancel now, exactly as
            // repair would: remove the replacement and staging and retire the
            // journal. A pending compaction blocks every other lifecycle
            // command on this worktree until someone runs repair. If the
            // cancellation itself fails, the journal stays for repair.
            let _ = resume_compaction(&git, &store, journal.decode(journal_path.clone())?, None);
            return Err(error);
        }
    };
    advance_compaction(
        &store,
        &mut journal,
        CompactWorktreePhase::ReplacementReady,
        fail_after,
    )?;
    let operation_id = journal.operation_id.clone();
    resume_compaction(
        &git,
        &store,
        journal.decode(journal_path.clone())?,
        fail_after,
    )?;
    let cancelled = store.load_all()?.into_iter().any(|candidate| {
        candidate.operation_id == operation_id && candidate.phase == CompactWorktreePhase::Cancelled
    });
    if cancelled {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} changed while it was being compacted; compaction was cancelled and nothing changed",
            destination.display()
        )));
    }

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
        "journaled Riftri pruning currently requires macOS, Linux, or Windows".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn prune_worktrees_inner(
    request: PruneWorktreesRequest,
    fail_after: Option<PruneWorktreesPhase>,
) -> Result<PruneWorktreesResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    // An enabled repository has no state directory until its first managed
    // add, and pruning is still meaningful there: `git worktree prune` removes
    // stale registrations left by ordinary Git worktrees, which can exist long
    // before Riftri manages one. Create the layout the way an add does rather
    // than failing, and rather than reporting success while skipping the Git
    // prune the command exists to perform. A state directory that exists but
    // is not a real directory still fails here, as does any other I/O error.
    let state_directory = absolute_path(&requested_state)?;
    create_state_layout(&state_directory)?;
    let state_directory = resolve_real_state_directory(&state_directory)?;
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
    let repository = git.inspect_repository_with_head_tree(&request.repository)?;
    let repository_root = git_command_root(&repository)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
        })?;
    let destination =
        normalize_new_destination(&request.destination, DestinationRules::WorktreeAdd)?;
    // An existing-branch add checks out the branch, as `git worktree add
    // <path> <branch>` does, even when a tag has the same name: resolving the
    // bare name would pick the tag and then refuse because "the branch moved".
    let requested = match &request.mode {
        WorktreeMode::ExistingBranch(branch) => {
            let mut qualified = OsString::from("refs/heads/");
            qualified.push(branch);
            qualified
        }
        WorktreeMode::NewBranch(_) | WorktreeMode::Detached => request.revision.clone(),
    };
    let resolved = if requested == OsStr::new("HEAD") {
        match (repository.head_commit.clone(), repository.head_tree.clone()) {
            (Some(commit), Some(tree)) => ResolvedRevision { commit, tree },
            _ => resolve_requested_revision(&git, &repository_root, &requested)?,
        }
    } else if let WorktreeMode::ExistingBranch(branch) = &request.mode {
        // Resolving the fully qualified branch already answers whether it
        // exists. Keep the later target lookup as a race-safety recheck, but
        // do not spawn an earlier existence-only process whose answer would
        // immediately be discarded.
        git.resolve_requested_revision(&repository_root, &requested)?
            .ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "existing local branch does not exist: {}",
                    branch.to_string_lossy()
                ))
            })?
    } else {
        resolve_requested_revision(&git, &repository_root, &requested)?
    };
    // The mirror of the existing-branch check below. Git enforces this too,
    // but only after Riftri has journaled the add, so the refusal arrived as
    // an operational `git-failed` with unknown cleanup — and re-running a task
    // with the same branch name is one of the commonest agent mistakes
    // (#425). Checking here refuses before anything is written.
    if let WorktreeMode::NewBranch(branch) = &request.mode
        && git.local_branch_target(&repository_root, branch)?.is_some()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "a branch named {} already exists; choose another name or check it out with an existing-branch add",
            branch.to_string_lossy()
        )));
    }
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
    // Git also refuses a destination it still registers, even once the
    // directory is gone, but only after Riftri has journaled the add, and
    // rollback then mistook that stale registration for its own (#461).
    let registered_worktrees = git.list_worktrees(&repository_root)?;
    if let Some(registered) = registered_worktrees
        .iter()
        .find(|worktree| paths_match(&worktree.path, &destination))
    {
        let clear = if registered.locked_reason.is_some() {
            "run `git worktree unlock` and then `git worktree prune` to clear it"
        } else {
            "run `git worktree prune` to clear it"
        };
        return Err(WorktreeError::InvalidRequest(format!(
            "{} is already registered as a Git worktree; if that worktree was deleted, {clear}",
            destination.display()
        )));
    }
    // Git refuses a branch another worktree has checked out, but only after
    // Riftri has journaled the add, so re-running a task on the same branch
    // ended as an operational `git-failed` with unknown cleanup (#425).
    if let WorktreeMode::ExistingBranch(branch) = &request.mode {
        let mut reference = b"refs/heads/".to_vec();
        reference.extend_from_slice(branch.as_encoded_bytes());
        if let Some(holder) = registered_worktrees
            .iter()
            .find(|worktree| worktree.branch.as_deref() == Some(reference.as_slice()))
        {
            let clear = if holder.prunable_reason.is_some() {
                "; that worktree is gone, so run `git worktree prune` to release the branch"
            } else {
                ""
            };
            return Err(WorktreeError::InvalidRequest(format!(
                "branch {} is already checked out at {}{clear}",
                branch.to_string_lossy(),
                holder.path.display()
            )));
        }
    }
    let checkout_config = git.config_values(&repository_root, CHECKOUT_CONFIG_KEYS)?;
    let sparse_directories = resolve_sparse_profile(
        &git,
        &repository_root,
        &request.sparse_directories,
        Some(&checkout_config),
    )?;
    let compatibility = validate_resolved_compatibility(
        &git,
        &repository_root,
        &repository.identity.common_git_dir,
        &resolved,
        &sparse_directories,
        Some(checkout_config),
    )?;
    let hooks_path = resolve_post_checkout_hooks_path(&git, compatibility.hooks_path.as_deref())?;
    validate_destination_path_semantics(&compatibility.checkout_paths, &destination)?;
    if !sparse_directories.is_empty() {
        validate_sparse_directories_in_tree(
            &sparse_directories,
            &compatibility.checkout_paths,
            &resolved.tree,
        )?;
        if !compatibility.lfs_objects.is_empty() {
            return Err(WorktreeError::Unsupported(
                "sparse worktrees for trees with Git LFS-managed paths are not supported yet; request the full tree instead".to_owned(),
            ));
        }
    }
    refuse_destination_in_git_worktree_admin(&repository.identity.common_git_dir, &destination)?;
    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = planned_state_directory(&requested_state, &destination)?;
    // A state path that exists as a regular file or a symbolic link can never
    // hold Riftri state. It is a caller mistake, so refuse it before probing
    // any backend — otherwise a volume without copy-on-write support answers
    // first with `unsupported-checkout`, and one with it misreports the file
    // as a missing volume identity or fails with "File exists" (#425). A path
    // that does not exist yet is fine: the add creates it.
    resolve_real_state_directory_if_present(&state_directory)?;
    let selected_backend = supported_worktree_backend(&destination)?;
    let destination_volume = &selected_backend.volume;
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
    // A journal left active by a worktree the user deleted by hand still
    // claims this path. Reclaim it, then refuse if anything still claims the
    // destination: a second active journal for one path wedges every later
    // `remove` and `repair` with "multiple active Riftri journals reference".
    //
    // The reclaim itself is opportunistic — `riftri repair` is the
    // authoritative reconciliation pass — so a concurrent operation racing
    // this scan must never fail this add. The claim check that follows stays
    // fail-closed on anything unreadable, which is what actually prevents a
    // duplicate journal.
    let _ = reconcile_active_add_journals(&git, &state_directory, Some(&destination));
    // Everything from the claim check to Git's registration of the worktree
    // runs under the repository's worktree-metadata lock, so two adds of one
    // destination can never both record intent. Otherwise the loser's rollback
    // found the winner's registration, took it for its own, and removed it
    // (#469). Taken only after the reconcile above, which locks it itself.
    let metadata_lock = acquire_git_worktree_metadata_lock(&repository.identity.common_git_dir)?;
    if let Some(claimant) = add_journals_claiming_destination(&state_directory, &destination)?
        .into_iter()
        .next()
    {
        return Err(WorktreeError::InvalidRequest(
            if claimant.phase == AddWorktreePhase::Active {
                format!(
                    "{} is already claimed by active Riftri journal {}; remove that worktree with `riftri worktree remove`, or run `riftri repair` if it no longer exists",
                    destination.display(),
                    claimant.operation_id
                )
            } else {
                format!(
                    "another Riftri add of {} is in progress or was interrupted (journal {}); retry once it finishes, or run `riftri repair --state-dir {}` if it was interrupted",
                    destination.display(),
                    claimant.operation_id,
                    state_directory.display()
                )
            },
        ));
    }
    // The destination was validated before the lock was taken; a concurrent
    // add or plain Git may have used it since.
    let inventory = git.list_worktrees(&repository_root)?;
    if inventory
        .iter()
        .any(|worktree| paths_match(&worktree.path, &destination))
        || (fs::symlink_metadata(&destination).is_ok() && !is_empty_real_directory(&destination)?)
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} was taken by another worktree while this add was being prepared",
            destination.display()
        )));
    }
    let base_directory = state_directory.join("bases/v1").join(repository_cache_id(
        &repository.identity.common_git_dir,
        &compatibility.checkout_profile,
    ));
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
    validate_materialization_path_lengths(
        &compatibility.checkout_paths,
        &[&base_path, &base_staging, &scratch],
    )?;
    let (branch, branch_created) = match &request.mode {
        WorktreeMode::NewBranch(branch) => (Some(branch.as_os_str()), true),
        WorktreeMode::ExistingBranch(branch) => (Some(branch.as_os_str()), false),
        WorktreeMode::Detached => (None, false),
    };
    // `HEAD` and every Git call above mean the worktree the add was run in, as
    // with Git; the journal records a location that outlives that worktree.
    let recorded_repository = main_worktree_root(&inventory, &repository_root);
    let journal_paths = JournalPaths {
        repository: &recorded_repository,
        destination: &destination,
        scratch: &scratch,
        base_staging: &base_staging,
        base_path: &base_path,
        temporary_index: &temporary_index,
        branch,
        branch_created,
        sparse_directories: &sparse_directories,
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
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::AddIntentPersist,
        &base_directory,
    );
    let journal_path = store.persist(&journal)?;
    progress::emit(ProgressEvent::AddPhase {
        phase: journal.phase,
    });

    let mut metadata_lock = Some(metadata_lock);
    let operation = fail_add_if_requested(journal.phase, fail_after)
        .and_then(|()| {
            // Created only once intent is journaled: every refusal above
            // leaves no empty bucket behind, and an add killed after this
            // point has a journal whose rollback removes the bucket. Created
            // before the journal, a kill in between left an empty bucket that
            // status reported as unowned forever.
            ensure_real_state_directory(&base_directory, "create repository base directory")?;
            sync_parent(&base_directory)
        })
        .and_then(|()| {
            perform_add(
                &git,
                metadata_lock
                    .take()
                    .expect("the metadata lock is handed over once"),
                &store,
                &mut journal,
                &repository_root,
                &destination,
                &scratch,
                &base_path,
                &base_staging,
                &temporary_index,
                &resolved.commit,
                &request.revision,
                &request.mode,
                &resolved.tree,
                &compatibility.checkout_config,
                &compatibility.lfs_objects,
                &sparse_directories,
                fail_after,
            )
        });

    match operation {
        Ok(reused_base) => {
            // Git runs post-checkout after creating a worktree. Riftri builds
            // the worktree by cloning a base instead of checking out, so Git
            // never fires it; run it here so the result matches `git worktree
            // add`. A failing hook is reported, not rolled back, exactly as
            // Git leaves the worktree in place.
            let post_checkout = post_checkout_hook_path(
                &repository_root,
                &repository.identity.common_git_dir,
                hooks_path.as_deref(),
            )
            .filter(|hook| hook_is_executable(hook))
            .map(|hook| run_post_checkout_hook(&hook, &destination, &resolved.commit));
            Ok(AddWorktreeResult {
                destination,
                commit: resolved.commit,
                tree: resolved.tree,
                base_path,
                journal_path,
                reused_base,
                backend,
                post_checkout,
            })
        }
        Err(operation_error) => {
            // Rollback takes the metadata lock itself.
            drop(metadata_lock.take());
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
                .and_then(|decoded| rollback_decoded(&git, &state_directory, &decoded))
                .and_then(|outcome| {
                    journal.transition(AddWorktreePhase::RolledBack)?;
                    store.persist(&journal)?;
                    progress::emit(ProgressEvent::AddPhase {
                        phase: AddWorktreePhase::RolledBack,
                    });
                    Ok(outcome)
                });

            match rollback {
                Ok(AddRollback::RolledBack) => Err(operation_error),
                Ok(AddRollback::Released) => Err(WorktreeError::InvalidRequest(format!(
                    "{operation_error}; Git commands already ran in {}, so it was kept as a \
                     plain Git worktree that Riftri does not manage",
                    destination.display()
                ))),
                Err(rollback_error) => Err(WorktreeError::OperationAndRollback {
                    operation: Box::new(operation_error),
                    rollback: Box::new(rollback_error),
                }),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn perform_add(
    git: &Git,
    metadata_lock: File,
    store: &JournalStore,
    journal: &mut JournalRecord,
    repository: &Path,
    destination: &Path,
    scratch: &Path,
    base_path: &Path,
    base_staging: &Path,
    temporary_index: &Path,
    commit: &ObjectId,
    start_point: &OsStr,
    mode: &WorktreeMode,
    tree: &ObjectId,
    checkout_config: &[(String, Vec<u8>)],
    lfs_objects: &[GitLfsObject],
    sparse_directories: &[String],
    fail_after: Option<AddWorktreePhase>,
) -> Result<bool, WorktreeError> {
    let head = match mode {
        WorktreeMode::NewBranch(branch) => WorktreeHead::NewBranch(branch),
        WorktreeMode::ExistingBranch(branch) => WorktreeHead::ExistingBranch(branch),
        WorktreeMode::Detached => WorktreeHead::Detached,
    };
    // A new branch is created from the start point exactly as the caller named
    // it, not from the commit it resolved to: Git decides upstream tracking
    // from that name (`branch.autoSetupMerge`, `--track` configuration), and a
    // raw commit never gets an upstream. Passing the commit made
    // `-b task <path> origin/main` succeed with no upstream, silently unlike
    // Git. Pinning is kept by verifying the result below.
    let revision = match mode {
        WorktreeMode::NewBranch(_) => start_point,
        WorktreeMode::ExistingBranch(_) | WorktreeMode::Detached => OsStr::new(commit.as_str()),
    };
    git.add_worktree_no_checkout(repository, destination, revision, head)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitMetadataCreated,
        fail_after,
    )?;
    match mode {
        WorktreeMode::ExistingBranch(_) => {
            let attached = git.resolve_revision(destination, OsStr::new("HEAD"))?;
            if attached.commit != *commit {
                return Err(WorktreeError::InvalidRequest(format!(
                    "existing branch moved from {} to {} while its worktree was being created",
                    commit.as_str(),
                    attached.commit.as_str()
                )));
            }
        }
        // The add uses the revision resolved before mutation. If the start
        // point moved in between, Git created the new branch at the moved
        // commit; this operation owns that branch, so pin it back with a
        // compare-and-swap update. The upstream Git configured refers to the
        // start point's name, not its commit, and nothing has been checked
        // out yet, so the reference is all that needs correcting.
        WorktreeMode::NewBranch(branch) => {
            let attached = git.resolve_revision(destination, OsStr::new("HEAD"))?;
            if attached.commit != *commit {
                git.move_branch_if_unchanged(repository, branch, commit, &attached.commit)?;
            }
        }
        WorktreeMode::Detached => {}
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
        sparse_directories,
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
        NativeCowCloner::clone_tree_owner_writable(base_path, scratch)?;
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

    if sparse_directories.is_empty() {
        git.synchronize_worktree_index(destination)?;
    } else {
        git.synchronize_sparse_worktree_index(destination, sparse_directories)?;
    }
    advance(
        store,
        journal,
        AddWorktreePhase::IndexSynchronized,
        fail_after,
    )?;
    // This clean check is also the index refresh: `git status` performs the
    // full stat-and-content comparison that a separate `update-index
    // --refresh` used to run, so any divergence between the cloned view and
    // the exact tree still fails the add before activation.
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::AddCleanCheck,
        destination,
    );
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
        let mismatch = || Err(damaged_base_error(base_path));
        if stored.starts_with(crate::base_integrity::MARKER_V2_PREFIX) {
            if stored
                != crate::base_integrity::marker_v2(base_path)
                    .map_err(|source| io("verify immutable-base integrity", base_path, source))?
            {
                return mismatch();
            }
        } else if stored.starts_with(crate::base_integrity::MARKER_V1_PREFIX) {
            if stored
                != crate::base_integrity::marker(base_path)
                    .map_err(|source| io("verify immutable-base integrity", base_path, source))?
            {
                return mismatch();
            }
            // The content matches, but a v1 marker attests nothing about
            // special permission bits, extended attributes, or macOS ACLs —
            // exactly the metadata the native cloners propagate into views.
            // Report a cache miss so this base is rebuilt once under the
            // exclusive lock and records a v2 marker, instead of trusting
            // metadata no marker ever covered. Legacy content tampering
            // still refuses above; existing views are never disturbed.
            clear_damaged_base_marker(base_path)?;
            return Ok(false);
        } else {
            return mismatch();
        }
        clear_damaged_base_marker(base_path)?;
        return Ok(true);
    }
    // No complete base: whatever an earlier add found damaged is gone or is
    // about to be rebuilt, so its marker no longer describes anything.
    clear_damaged_base_marker(base_path)?;
    Ok(false)
}

/// Beside a base, records that an add found it failing its integrity check,
/// so `status` can say so without re-reading every base. Evidence only: it
/// never decides reuse, which always recomputes the digest.
fn damaged_base_marker(base_path: &Path) -> PathBuf {
    base_path.with_extension("damaged")
}

fn clear_damaged_base_marker(base_path: &Path) -> Result<(), WorktreeError> {
    remove_file_if_present(&damaged_base_marker(base_path))
}

/// Refuse a damaged base, and say how to get the tree back: the base stays as
/// evidence while any view uses it, and garbage collection deletes it after.
fn damaged_base_error(base_path: &Path) -> WorktreeError {
    // Best effort: failing to record the marker must not hide the refusal.
    let _ = fs::write(damaged_base_marker(base_path), b"");
    let state_directory = base_path.ancestors().nth(4);
    let users = state_directory
        .map(|state| base_users(base_path, state))
        .unwrap_or_default();
    let collect = match state_directory {
        Some(state) => format!("riftri gc --apply --state-dir {}", state.display()),
        None => "riftri gc --apply".to_owned(),
    };
    let guidance = if users.is_empty() {
        format!("No active worktree uses it; run `{collect}` to delete it and add the tree again.")
    } else {
        format!(
            "Worktrees still using it: {}. To add this tree again, remove them, then run `{collect}` to delete the damaged base.",
            users
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    WorktreeError::InvalidRequest(format!(
        "immutable-base integrity check failed for {}; the base was preserved and cannot be reused. {guidance}",
        base_path.display()
    ))
}

/// Destinations whose active add still references `base_path`, for guidance
/// only; unreadable journals yield
/// fewer names, never an error.
fn base_users(base_path: &Path, state_directory: &Path) -> Vec<PathBuf> {
    let removed = RemovalJournalStore::open(state_directory)
        .load_all()
        .map(|journals| {
            journals
                .into_iter()
                .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
                .map(|journal| journal.source_add_operation_id)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut users = JournalStore::open(state_directory)
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|journal| {
            journal.phase == AddWorktreePhase::Active
                && journal.base_path == base_path
                && !removed.contains(&journal.operation_id)
        })
        .map(|journal| journal.destination)
        .collect::<Vec<_>>();
    users.sort();
    users
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
    sparse_directories: &[String],
) -> Result<bool, WorktreeError> {
    let base_parent = base_path.expect_parent()?;
    // A rolled-back add or cancelled compaction removes its bucket when it is
    // empty, possibly just after this add created it; bring it back first.
    ensure_real_state_directory(base_parent, "create repository base directory")?;
    let lock_path = base_parent.join(format!("{}.lock", tree.as_str()));
    let complete_path = base_parent.join(format!("{}.complete", tree.as_str()));
    let read_lock = acquire_base_read_lock(&lock_path)?;
    if verify_existing_base(base_path, &complete_path)? {
        progress::emit(ProgressEvent::BaseReused);
        return Ok(true);
    }
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::BaseReadMiss,
        base_path,
    );
    // Never upgrade a held shared lock: concurrent cold callers could deadlock.
    // Another builder or collector may run in the gap, so revalidate all state
    // after acquiring the same stable lock file exclusively.
    drop(read_lock);
    let mut write_lock = acquire_coordination_lock(
        &lock_path,
        "open immutable-base lock",
        "lock immutable base",
    )?;
    if completed_base_paths_present(base_path, &complete_path)? {
        // A builder may have completed the base while this caller waited for
        // exclusive ownership. Do not hash that base while retaining the
        // writer lock: release it and repeat the full integrity check under a
        // shared lock so every waiter can verify concurrently. A collection,
        // legacy marker, or incomplete replacement can still intervene in the
        // gap, so a shared miss falls back to a fresh exclusive acquisition
        // and the existing mandatory revalidation below.
        drop(write_lock);
        let read_lock = acquire_base_read_lock(&lock_path)?;
        #[cfg(test)]
        crate::test_hooks::fire(
            crate::test_hooks::FilesystemRacePoint::BaseReuseAfterExclusiveWait,
            base_path,
        );
        if verify_existing_base(base_path, &complete_path)? {
            progress::emit(ProgressEvent::BaseReused);
            return Ok(true);
        }
        drop(read_lock);
        write_lock = acquire_coordination_lock(
            &lock_path,
            "open immutable-base lock",
            "lock immutable base",
        )?;
    }
    let _write_lock = write_lock;
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
    if sparse_directories.is_empty() {
        git.materialize_tree_with_config(
            repository,
            tree,
            base_staging,
            temporary_index,
            checkout_config,
        )?;
    } else {
        git.materialize_sparse_tree_with_config(
            repository,
            tree,
            base_staging,
            temporary_index,
            checkout_config,
            sparse_directories,
        )?;
    }
    materialize_git_lfs_objects(base_staging, lfs_objects)?;
    remove_temporary_index(temporary_index)?;
    fs::rename(base_staging, base_path)
        .map_err(|source| io("activate immutable base", base_path, source))?;
    NativeCowCloner::make_tree_read_only(base_path)?;
    let integrity = crate::base_integrity::marker_v2(base_path)
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
fn completed_base_paths_present(
    base_path: &Path,
    complete_path: &Path,
) -> Result<bool, WorktreeError> {
    let base_exists = base_path
        .try_exists()
        .map_err(|source| io("inspect immutable base", base_path, source))?;
    let complete_exists = match fs::symlink_metadata(complete_path) {
        Ok(_) => true,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(io(
                "inspect immutable-base completion marker",
                complete_path,
                source,
            ));
        }
    };
    Ok(base_exists && complete_exists)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_resolved_compatibility(
    git: &Git,
    repository: &Path,
    common_git_dir: &Path,
    resolved: &ResolvedRevision,
    sparse_directories: &[String],
    checkout_config: Option<ConfigValues>,
) -> Result<CompatibilityAnalysis, WorktreeError> {
    let analysis = analyze_resolved_repository_compatibility(
        git,
        repository,
        common_git_dir,
        resolved,
        sparse_directories,
        checkout_config,
    )?;
    if let Some(blocker) = analysis.report.blockers.first() {
        return Err(WorktreeError::Unsupported(blocker.explanation.clone()));
    }
    Ok(analysis)
}

/// Resolve the cone directory list the new worktree will actually materialize.
///
/// An explicit request always wins. Otherwise Riftri inherits the source
/// worktree's cone, because that is what Git itself does: `git worktree add`
/// copies the current worktree's sparse-checkout into the new one, so an add
/// issued from inside a sparse worktree produces a sparse worktree with no
/// sparse argument anywhere on the command line. Inheriting it here keeps Git
/// the source of truth and keys the immutable base by the profile that is
/// really materialized.
///
/// Sparse configuration outside the supported cone subset resolves to an empty
/// list on purpose, leaving the repository compatibility blocker to refuse the
/// add before any state exists rather than quietly materializing a full tree.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn resolve_sparse_profile(
    git: &Git,
    repository: &Path,
    requested: &[String],
    checkout_config: Option<&ConfigValues>,
) -> Result<Vec<String>, WorktreeError> {
    if !requested.is_empty() {
        return canonicalize_sparse_directories(requested);
    }
    let state = match checkout_config {
        Some(config) => git.sparse_checkout_state_with_config(repository, config)?,
        None => git.sparse_checkout_state(repository)?,
    };
    if !state.enabled || !state.cone || state.directories.is_empty() {
        return Ok(Vec::new());
    }
    canonicalize_sparse_directories(&state.directories)
}

/// Canonicalize a requested cone-mode sparse directory list into the exact
/// form that keys the immutable base: sorted, deduplicated, trailing-slash
/// free, with nested cones collapsed into their listed ancestors so base
/// identity always equals materialized content. Anything outside the
/// supported literal-directory subset is refused before any state exists.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn canonicalize_sparse_directories(requested: &[String]) -> Result<Vec<String>, WorktreeError> {
    let mut normalized = Vec::new();
    for raw in requested {
        let directory = raw.strip_suffix('/').unwrap_or(raw);
        if directory.is_empty() {
            return Err(WorktreeError::InvalidRequest(
                "sparse directory names cannot be empty; cone mode selects repository-relative directories such as `crates/riftri-core`"
                    .to_owned(),
            ));
        }
        if raw.starts_with('/') {
            return Err(WorktreeError::InvalidRequest(format!(
                "sparse directory {raw} is absolute; cone mode selects directories relative to the repository root"
            )));
        }
        if let Some(unsupported) = directory
            .chars()
            .find(|c| matches!(c, '*' | '?' | '[' | ']' | '\\') || c.is_control())
        {
            return Err(WorktreeError::InvalidRequest(format!(
                "sparse directory {raw} contains {unsupported:?}; only literal directory paths with `/` separators are supported, not sparse patterns"
            )));
        }
        if directory.starts_with('!') {
            return Err(WorktreeError::InvalidRequest(format!(
                "sparse directory {raw} looks like a negated sparse pattern; only literal cone-mode directory lists are supported"
            )));
        }
        for component in directory.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(WorktreeError::InvalidRequest(format!(
                    "sparse directory {raw} must use non-empty path components without `.` or `..`"
                )));
            }
            if component.eq_ignore_ascii_case(".git") {
                return Err(WorktreeError::InvalidRequest(format!(
                    "sparse directory {raw} names a Git administrative path"
                )));
            }
        }
        normalized.push(directory.to_owned());
    }
    normalized.sort_unstable();
    normalized.dedup();
    if normalized.len() < 2 {
        return Ok(normalized);
    }
    // Only component-boundary ancestors can cover a cone. Looking them up
    // avoids comparing each selection with every previously retained cone.
    let requested = normalized
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let keep = normalized
        .iter()
        .map(|directory| {
            !directory
                .match_indices('/')
                .any(|(end, _)| requested.contains(&directory[..end]))
        })
        .collect::<Vec<_>>();
    drop(requested);
    Ok(normalized
        .into_iter()
        .zip(keep)
        .filter_map(|(directory, keep)| keep.then_some(directory))
        .collect())
}

/// Require every requested cone directory to exist as a directory in the
/// exact requested tree, so a misspelled selection cannot silently
/// materialize a nearly empty worktree.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn validate_sparse_directories_in_tree(
    sparse_directories: &[String],
    checkout_paths: &[PathBuf],
    tree: &ObjectId,
) -> Result<(), WorktreeError> {
    let mut directories = SparseDirectoryIndex::new(checkout_paths);
    for directory in sparse_directories {
        let prefix = Path::new(directory);
        if directories.contains(prefix) {
            continue;
        }
        if checkout_paths.iter().any(|path| path.as_path() == prefix) {
            return Err(WorktreeError::InvalidRequest(format!(
                "sparse directory {directory} is a file in tree {}; cone mode selects directories",
                tree.as_str()
            )));
        }
        return Err(WorktreeError::InvalidRequest(format!(
            "sparse directory {directory} does not exist in tree {}",
            tree.as_str()
        )));
    }
    Ok(())
}

/// Cheap early matches need no index. Bound total linear work to one tree
/// pass before indexing the exact native parents for the remaining queries.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct SparseDirectoryIndex<'a> {
    paths: &'a [PathBuf],
    remaining_scan: usize,
    directories: Option<HashSet<&'a Path>>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl<'a> SparseDirectoryIndex<'a> {
    fn new(paths: &'a [PathBuf]) -> Self {
        Self {
            paths,
            remaining_scan: paths.len(),
            directories: None,
        }
    }

    fn contains(&mut self, prefix: &Path) -> bool {
        if let Some(directories) = &self.directories {
            return directories.contains(prefix);
        }
        for path in self.paths {
            if self.remaining_scan == 0 {
                let directories = self
                    .paths
                    .iter()
                    .flat_map(|path| path.ancestors().skip(1))
                    .collect::<HashSet<_>>();
                let found = directories.contains(prefix);
                self.directories = Some(directories);
                return found;
            }
            self.remaining_scan -= 1;
            if path.as_path() != prefix && path.starts_with(prefix) {
                return true;
            }
        }
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
/// Refuse a tree whose longest path fits in Git's own checkout but not under a
/// longer location Riftri also writes the tree to: the immutable base, its
/// build staging, and view siblings. Without this, the base build failed deep
/// inside `git checkout-index` with "File name too long", reported as an
/// operational failure that invites a retry which can never succeed.
#[cfg(unix)]
fn validate_materialization_path_lengths(
    paths: &[PathBuf],
    roots: &[&Path],
) -> Result<(), WorktreeError> {
    let Some(longest) = paths.iter().max_by_key(|path| path.as_os_str().len()) else {
        return Ok(());
    };
    // PATH_MAX counts the terminating NUL.
    let limit = libc::PATH_MAX as usize - 1;
    for root in roots {
        if root.as_os_str().len() + 1 + longest.as_os_str().len() > limit {
            return Err(WorktreeError::Unsupported(format!(
                "Git tree path {} ({} bytes) does not fit under Riftri's working location {} within the platform path limit of {limit} bytes; pass a shorter --state-dir, or choose a destination whose parent path is shorter",
                longest.display(),
                longest.as_os_str().len(),
                root.display()
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_materialization_path_lengths(
    _paths: &[PathBuf],
    _roots: &[&Path],
) -> Result<(), WorktreeError> {
    Ok(())
}

fn validate_destination_path_semantics(
    paths: &[PathBuf],
    destination: &Path,
) -> Result<(), WorktreeError> {
    for path in paths {
        let mut components = path.components();
        if components.clone().next().is_none()
            || components.any(|component| !matches!(component, Component::Normal(_)))
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
            let mut components = path.components();
            // The leaf is created separately below. Components is double-ended,
            // so retaining a heap vector just to exclude the leaf is unnecessary.
            components.next_back();
            let mut relative_parent = PathBuf::new();
            for component in components {
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
    common_git_dir: &Path,
    revision: &OsStr,
) -> Result<RepositoryCompatibilityReport, WorktreeError> {
    validate_lifecycle_git_environment()?;
    // Judge the add that would actually run: with no explicit sparse request
    // it inherits a cone-mode source's cone, as Git does, and then the
    // source's sparse settings block nothing.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        let resolved = resolve_requested_revision(git, repository, revision)?;
        let sparse_directories = resolve_sparse_profile(git, repository, &[], None)?;
        Ok(analyze_resolved_repository_compatibility(
            git,
            repository,
            common_git_dir,
            &resolved,
            &sparse_directories,
            None,
        )?
        .report)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    Ok(analyze_repository_compatibility(git, repository, common_git_dir, revision)?.report)
}

/// Resolve a revision the caller supplied, reporting a name that resolves to
/// nothing as the request error it is rather than a Git failure (#425).
fn resolve_requested_revision(
    git: &Git,
    repository: &Path,
    revision: &OsStr,
) -> Result<ResolvedRevision, WorktreeError> {
    if let Some(resolved) = git.resolve_requested_revision(repository, revision)? {
        return Ok(resolved);
    }
    if revision == OsStr::new("HEAD") {
        return Err(WorktreeError::UnbornHead(unresolved_head_message(
            git, repository,
        )));
    }
    Err(WorktreeError::InvalidRequest(format!(
        "revision does not name a commit in this repository: {}",
        revision.to_string_lossy()
    )))
}

/// Explain an unresolvable `HEAD`: an orphaned branch in a repository that
/// has other history is not an empty repository. Only computed on failure.
fn unresolved_head_message(git: &Git, repository: &Path) -> String {
    match (
        git.symbolic_head_branch(repository).ok().flatten(),
        git.has_any_reference(repository).unwrap_or(false),
    ) {
        (Some(branch), true) => format!(
            "HEAD is on branch {}, which has no commits yet; name a commit or branch to start from",
            String::from_utf8_lossy(&branch)
        ),
        _ => "HEAD does not name a commit: the repository has no commits yet".to_owned(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn analyze_repository_compatibility(
    git: &Git,
    repository: &Path,
    common_git_dir: &Path,
    revision: &OsStr,
) -> Result<CompatibilityAnalysis, WorktreeError> {
    let resolved = resolve_requested_revision(git, repository, revision)?;
    analyze_resolved_repository_compatibility(git, repository, common_git_dir, &resolved, &[], None)
}

/// Where Git would look for `post-checkout` when invoked from `repository`.
///
/// A relative `core.hooksPath` resolves against the working tree the command
/// runs in, which for `git worktree add` is the invoking worktree rather than
/// the one being created. Verified against Git rather than assumed: a hook
/// under a relative `core.hooksPath` fires even when that directory does not
/// exist in the new worktree.
fn post_checkout_hook_path(
    repository: &Path,
    common_git_dir: &Path,
    hooks_path: Option<&Path>,
) -> Option<PathBuf> {
    let directory = match hooks_path {
        Some(configured) => {
            if configured.as_os_str().is_empty() {
                return None;
            }
            if configured.is_absolute() {
                configured.to_path_buf()
            } else {
                repository.join(configured)
            }
        }
        None => common_git_dir.join("hooks"),
    };
    Some(directory.join("post-checkout"))
}

/// Resolve the captured `core.hooksPath` using Git's pathname rules.
///
/// Most configured hook paths are already literal relative or absolute paths,
/// so keep reusing the compatibility pass's batched value without another Git
/// process. Tilde and installation-prefix forms require Git's platform-aware
/// expansion and take the uncommon one-process path before durable mutation.
fn resolve_post_checkout_hooks_path(
    git: &Git,
    configured: Option<&[u8]>,
) -> Result<Option<PathBuf>, WorktreeError> {
    let Some(value) = configured else {
        return Ok(None);
    };
    let path = configured_hooks_path(value).ok_or_else(|| {
        WorktreeError::Unsupported(
            "core.hooksPath is set to a value Riftri cannot interpret as a path; use ordinary git worktree add"
                .to_owned(),
        )
    })?;
    if value.starts_with(b"~") || value.starts_with(b"%(prefix)/") {
        return Ok(Some(git.expand_config_path(path.as_os_str())?));
    }
    Ok(Some(path))
}

/// `core.hooksPath` as a path. Git stores configuration as bytes; Windows
/// paths are UTF-16, so a non-UTF-8 value there is left undecidable.
#[cfg(unix)]
fn configured_hooks_path(value: &[u8]) -> Option<PathBuf> {
    Some(PathBuf::from(OsStr::from_bytes(value).to_os_string()))
}

#[cfg(not(unix))]
fn configured_hooks_path(value: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(value).ok().map(PathBuf::from)
}

/// Whether Git would execute this hook: present, and on Unix executable.
fn hook_is_executable(hook: &Path) -> bool {
    let Ok(metadata) = fs::metadata(hook) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run `post-checkout` exactly as `git worktree add` does.
///
/// Git passes the null object id, the new HEAD, and `1` for a branch
/// checkout, runs the hook from the new worktree, and does not export its own
/// `GIT_DIR` into it. A failing hook does not undo the worktree; Git reports
/// the failure and leaves the worktree in place, so Riftri does the same.
fn run_post_checkout_hook(
    hook: &Path,
    destination: &Path,
    commit: &ObjectId,
) -> PostCheckoutOutcome {
    let mut command = Command::new(hook);
    command
        .current_dir(destination)
        // Git passes the null object id for a new worktree. Match the
        // repository's hash width so SHA-256 repositories get 64 zeroes.
        .arg("0".repeat(commit.as_str().len()))
        .arg(commit.as_str())
        .arg("1");
    for leaked in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        command.env_remove(leaked);
    }
    // As Git runs it: no stdin, and stdout sent to stderr. A hook printing on
    // stdout corrupted the `--json` document there, so a harness parsing it
    // saw a successful add fail; one reading stdin consumed the caller's input.
    command.stdin(std::process::Stdio::null());
    if let Some(stderr) = stderr_for_child_stdout() {
        command.stdout(stderr);
    }
    match command.status() {
        Ok(status) => PostCheckoutOutcome {
            hook: hook.to_path_buf(),
            exit_code: status.code(),
            started: true,
        },
        Err(_) => PostCheckoutOutcome {
            hook: hook.to_path_buf(),
            exit_code: None,
            started: false,
        },
    }
}

/// A duplicate of this process's stderr to give a child as its stdout, or
/// `None` when there is no stderr to duplicate (the child then inherits).
fn stderr_for_child_stdout() -> Option<std::process::Stdio> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .ok()
            .map(std::process::Stdio::from)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        std::io::stderr()
            .as_handle()
            .try_clone_to_owned()
            .ok()
            .map(std::process::Stdio::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Riftri runs `post-checkout` itself after creating a worktree, so a hook is
/// no longer a reason to refuse the repository. What remains a blocker is a
/// hook Riftri cannot decide about: a `core.hooksPath` value this platform
/// cannot interpret as a path.
fn checkout_hook_blocker(hooks_path: Option<&[u8]>) -> Option<String> {
    let configured = hooks_path?;
    if configured.is_empty() || configured_hooks_path(configured).is_some() {
        return None;
    }
    Some(
        "core.hooksPath is set to a value Riftri cannot interpret as a path, so the post-checkout hook it names cannot be run; use ordinary git worktree add"
            .to_owned(),
    )
}

fn analyze_resolved_repository_compatibility(
    git: &Git,
    repository: &Path,
    common_git_dir: &Path,
    resolved: &ResolvedRevision,
    sparse_directories: &[String],
    captured_config: Option<ConfigValues>,
) -> Result<CompatibilityAnalysis, WorktreeError> {
    let entries = git.list_tree(repository, &resolved.tree)?;
    let paths = entries
        .iter()
        .map(|entry| entry.path.as_path())
        .collect::<Vec<_>>();
    let mut blockers = Vec::new();
    let mut has_submodules = false;
    let mut has_in_tree_attribute_file = false;
    for entry in &entries {
        if entry.path == Path::new(".gitmodules") || entry.object_kind == b"commit" {
            has_submodules = true;
        }
        if entry.path.file_name() == Some(OsStr::new(".gitattributes")) {
            has_in_tree_attribute_file = true;
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

    // Callers pass the common Git directory that `inspect_repository` already
    // resolved with `rev-parse --path-format=absolute --git-common-dir`, the
    // exact command `Git::info_attributes_path` would re-run here; joining the
    // fixed relative path avoids one Git invocation without changing which
    // file is inspected. `--git-path` is still avoided because its absolute
    // form can resolve a symlink at the final path and hide it from the
    // symlink-refusing metadata checks below.
    let info_attributes_path = common_git_dir.join("info/attributes");
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
    if info_attributes_safe && !paths.is_empty() {
        // Both attribute passes query the same exact tree, so populate one
        // temporary index once instead of running `git read-tree` twice; the
        // isolated and effective environments still apply per `check-attr`
        // query, which never writes the shared index.
        let tree_index = git.tree_attribute_index(repository, &resolved.tree)?;
        // With no `.gitattributes` entry anywhere in the exact tree, the
        // isolated in-tree result is necessarily empty. Keep the effective
        // query below: global and system attributes must still be detected
        // and refused rather than becoming part of a supposedly immutable
        // checkout profile.
        let mut in_tree = if has_in_tree_attribute_file {
            git.in_tree_attributes_for_index(repository, &tree_index, &paths)?
        } else {
            Vec::new()
        };
        match classify_in_tree_attributes(&in_tree) {
            Ok(paths) => lfs_paths = paths,
            Err(explanation) => {
                blockers.push(RepositoryCompatibilityBlocker {
                    kind: RepositoryCompatibilityBlockerKind::InTreeAttributes,
                    explanation,
                });
            }
        }

        let mut effective = git.effective_attributes_for_index(repository, &tree_index, &paths)?;
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
        profile.update(b"riftri-checkout-profile-v4-sparse\0");
        let git_version = git.detect()?.version;
        hash_profile_input(&mut profile, b"git.version", Some(git_version.as_bytes()));
        // The canonical cone directory list is part of the checkout profile,
        // so two sparse selections at the same tree, or a sparse and a full
        // request, can never resolve to the same immutable-base key. Full
        // requests add no input and keep their existing base identities.
        if !sparse_directories.is_empty() {
            hash_profile_input(&mut profile, b"riftri.sparse.mode", Some(b"cone"));
            for directory in sparse_directories {
                hash_profile_input(
                    &mut profile,
                    b"riftri.sparse.directory",
                    Some(directory.as_bytes()),
                );
            }
        }
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
    config_keys.push("core.hookspath");
    let config = match captured_config {
        Some(config) => config,
        None => git.config_values(repository, &config_keys)?,
    };
    if config.has_conditional_includes
        && !git.conditional_config_has_only(
            repository,
            &[
                "user.name",
                "user.email",
                "user.signingkey",
                "user.useconfigonly",
            ],
        )?
    {
        blockers.push(RepositoryCompatibilityBlocker {
            kind: RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
            explanation: "conditional Git configuration includes must contain only identity settings (user.name, user.email, user.signingKey, user.useConfigOnly); other keys, nested includes, and unreadable targets are not supported"
                .to_owned(),
        });
    }
    let config_values = config.values;
    if let Some(explanation) =
        checkout_hook_blocker(config_values.get("core.hookspath").map(Vec::as_slice))
    {
        blockers.push(RepositoryCompatibilityBlocker {
            kind: RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
            explanation,
        });
    }
    let config_is_true = |key: &str| {
        config_values
            .get(key)
            .is_some_and(|value| value.eq_ignore_ascii_case(b"true"))
    };
    // Sparse checkout that is on but not in cone mode, which Riftri cannot
    // reproduce from a cone directory list.
    let source_sparse_is_unsupported =
        config_is_true("core.sparsecheckout") && !config_is_true("core.sparsecheckoutcone");
    for (key, accepted, kind) in checked_config {
        let value = config_values.get(key).cloned();
        // The sparse keys describe where the command ran, not what gets
        // materialized: `core.sparseCheckout` is worktree-scoped, so the same
        // cone reads differently from the repository root than from inside a
        // sparse worktree. The canonical cone list hashed above already
        // describes the materialization exactly, so hashing these too would
        // split one profile across several base buckets — which made
        // compacting a sparse worktree allocate a second base for identical
        // content instead of reusing the one it already had (#391). They stay
        // in `checked_config` because they still decide what is refused.
        let describes_where_not_what =
            matches!(kind, RepositoryCompatibilityBlockerKind::SparseCheckout);
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        if !describes_where_not_what {
            hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        if let Some(value) = &value
            && !describes_where_not_what
        {
            checkout_config.push((key.to_owned(), value.clone()));
        }
        // A resolved cone list is materialized explicitly into the new
        // worktree, so a cone-mode source cannot change the bytes Riftri
        // writes and must not block the add. Without this, a sparse worktree
        // is a dead end: Riftri sets core.sparseCheckout in the worktree it
        // creates, and every later add from inside it is refused for
        // configuration Riftri itself wrote.
        //
        // A source outside cone mode still blocks even for an explicit
        // request. Its settings are replayed into the materialization as
        // checkout configuration, and Riftri does not model non-cone pattern
        // semantics well enough to predict the result.
        let superseded_by_resolved_cone =
            matches!(kind, RepositoryCompatibilityBlockerKind::SparseCheckout)
                && !sparse_directories.is_empty()
                && !source_sparse_is_unsupported;
        if let Some(value) = value
            && !superseded_by_resolved_cone
            && !accepted
                .iter()
                .any(|accepted| value.eq_ignore_ascii_case(accepted))
        {
            let sources = checkout_config_source_diagnostic(git, repository, key);
            blockers.push(RepositoryCompatibilityBlocker {
                kind,
                explanation: format!(
                    "Git configuration {key}={} can change checkout bytes and is not supported yet{sources}",
                    String::from_utf8_lossy(&value),
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
        match inspect_git_lfs_objects(git, repository, common_git_dir, &entries, &lfs_paths) {
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
        // Attribute and LFS inspection only borrow these paths. Transfer the
        // original buffers after inspection instead of cloning the whole tree.
        checkout_paths: entries.into_iter().map(|entry| entry.path).collect(),
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        checkout_config,
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        hooks_path: config_values.get("core.hookspath").cloned(),
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

fn checkout_config_source_diagnostic(git: &Git, repository: &Path, key: &str) -> String {
    let Ok(origins) = git.config_value_origins(repository, key) else {
        // Origin lookup is supplementary. A malformed or changing config must
        // not hide the deterministic-checkout refusal that prompted it.
        return String::new();
    };
    if origins.is_empty() {
        return String::new();
    }
    let sources = origins
        .iter()
        .map(|origin| {
            format!(
                "{} {}={}",
                String::from_utf8_lossy(&origin.scope),
                String::from_utf8_lossy(&origin.origin),
                String::from_utf8_lossy(&origin.value),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("; configuration sources: {sources}")
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
    common_git_dir: &Path,
    entries: &[riftri_git::TreeEntry],
    paths: &[PathBuf],
) -> Result<Vec<GitLfsObject>, String> {
    const MAX_POINTER_BYTES: usize = 1024;
    // The caller already inspected repository identity and HEAD. Reuse only
    // that operation-local identity; pointer and local-object checks stay fresh.
    let mut objects = Vec::with_capacity(paths.len());
    let entries_by_path = entries
        .iter()
        .map(|entry| (entry.path.as_path(), entry))
        .collect::<HashMap<_, _>>();
    let selected = paths
        .iter()
        .map(|path| {
            entries_by_path.get(path.as_path()).copied().ok_or_else(|| {
                format!(
                    "Git LFS path {} is absent from the exact tree",
                    path.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (path, entry) in paths.iter().zip(&selected) {
        if entry.object_kind != b"blob" || !matches!(entry.mode, 0o100644 | 0o100755) {
            return Err(format!(
                "Git LFS path {} is not a regular file in the exact tree",
                path.display()
            ));
        }
    }
    if paths.is_empty() {
        return Ok(objects);
    }
    let mut reader = git
        .small_blob_reader(repository)
        .map_err(|error| format!("could not read Git LFS pointers: {error}"))?;
    // Bound both individual bodies and aggregate buffered pointer bytes.
    // Reuse only the process, not pointer or local-object validation results.
    // All local LFS objects still undergo the same validation below.
    for (paths, entries) in paths.chunks(128).zip(selected.chunks(128)) {
        let ids = entries
            .iter()
            .map(|entry| entry.object_id.clone())
            .collect::<Vec<_>>();
        let blobs = reader
            .read(&ids, MAX_POINTER_BYTES)
            .map_err(|error| format!("could not read Git LFS pointers: {error}"))?;
        for (path, bytes) in paths.iter().zip(blobs) {
            let pointer = parse_git_lfs_pointer(&bytes)
                .map_err(|error| format!("invalid Git LFS pointer {}: {error}", path.display()))?;
            let source_path = common_git_dir
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
    }
    reader
        .finish()
        .map_err(|error| format!("could not read Git LFS pointers: {error}"))?;
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
/// How Git treats a new destination, which differs between `add` and `move`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DestinationRules {
    /// `git worktree add` creates missing leading directories (#423) and
    /// accepts an existing empty directory as the destination (#437).
    WorktreeAdd,
    /// `git worktree move` does neither: it fails when the target's parent is
    /// missing or the target exists, so a Riftri move refuses the same request.
    WorktreeMove,
}

/// Whether `path` is a real directory — not a symbolic link — with no entries.
fn is_empty_real_directory(path: &Path) -> Result<bool, WorktreeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io("inspect worktree destination", path, source)),
    };
    if !metadata.is_dir() {
        return Ok(false);
    }
    let mut entries =
        fs::read_dir(path).map_err(|source| io("read worktree destination", path, source))?;
    Ok(entries.next().is_none())
}

/// The add's own checks on a destination that may already exist: it must be
/// absent or an empty real directory. `doctor` reports what the add refuses.
pub(crate) fn validate_new_add_destination(destination: &Path) -> Result<(), WorktreeError> {
    normalize_new_destination(destination, DestinationRules::WorktreeAdd).map(|_| ())
}

fn normalize_new_destination(
    destination: &Path,
    rules: DestinationRules,
) -> Result<PathBuf, WorktreeError> {
    if destination.as_os_str().is_empty() {
        return Err(WorktreeError::InvalidRequest(
            "destination cannot be empty".to_owned(),
        ));
    }
    let exists = match destination.try_exists() {
        Ok(exists) => exists,
        // An ancestor that is a regular file means the destination cannot
        // exist; the parent check below refuses the request with that
        // diagnosis instead of an I/O error.
        Err(source) if source.kind() == std::io::ErrorKind::NotADirectory => false,
        Err(source) => return Err(io("inspect worktree destination", destination, source)),
    };
    // Git's own rule: an existing destination is refused unless it is an empty
    // directory, which `git worktree add` fills — `dir=$(mktemp -d); git
    // worktree add "$dir" …` is a common script pattern (#437). A symbolic
    // link is still refused, even to an empty directory.
    let accept_empty = rules == DestinationRules::WorktreeAdd;
    if exists && !(accept_empty && is_empty_real_directory(destination)?) {
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
    let parent = match rules {
        DestinationRules::WorktreeAdd => planned_destination_parent(&absolute)?,
        DestinationRules::WorktreeMove => resolve_destination_parent(&absolute)?,
    };
    let normalized = parent.join(file_name);
    if normalized
        .try_exists()
        .map_err(|source| io("inspect normalized destination", &normalized, source))?
        && !(accept_empty && is_empty_real_directory(&normalized)?)
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination already exists: {}",
            normalized.display()
        )));
    }
    Ok(normalized)
}

/// Refuse a destination inside `<common-git-dir>/worktrees`, where Git keeps
/// each linked worktree's metadata under the worktree's name. There a new
/// worktree named `w` would be its own metadata directory: Git writes its
/// files into the destination, the view cannot replace them, and the add was
/// left pending with nothing `riftri repair` could roll back. Elsewhere in the
/// Git directory, Riftri accepts what Git accepts.
fn refuse_destination_in_git_worktree_admin(
    common_git_dir: &Path,
    destination: &Path,
) -> Result<(), WorktreeError> {
    let common = fs::canonicalize(common_git_dir).unwrap_or_else(|_| common_git_dir.to_path_buf());
    let admin = common.join("worktrees");
    if destination.starts_with(&admin) {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} would be inside Git's linked-worktree metadata directory {}; choose a destination outside it",
            destination.display(),
            admin.display()
        )));
    }
    Ok(())
}

/// The state directory an add will create, spelled without `.` components or
/// trailing separators, which `create_dir_all` cannot create as given.
///
/// A `..` after a directory that does not exist yet could only be resolved by
/// creating that directory, which a refused or failed add would leave behind,
/// so it is refused, as for worktree destinations. So is a state directory
/// inside the new worktree or a worktree inside the state directory: the state
/// would be created first and then make the destination look taken. The path
/// is not otherwise resolved, so a symbolic link is still refused as a state
/// directory later instead of being silently followed.
fn planned_state_directory(state: &Path, destination: &Path) -> Result<PathBuf, WorktreeError> {
    let planned: PathBuf = absolute_path(state)?.components().collect();
    let mut prefix = PathBuf::new();
    for component in planned.components() {
        if component == std::path::Component::ParentDir && !prefix.is_dir() {
            return Err(WorktreeError::InvalidRequest(format!(
                "state directory {} climbs out of a directory that does not exist yet",
                state.display()
            )));
        }
        prefix.push(component);
    }
    // A file or symbolic link here is its own, more specific refusal.
    resolve_real_state_directory_if_present(&planned)?;
    // Compare where both actually land: the destination is already resolved
    // through its existing ancestors, so resolve the state path the same way.
    let landing = planned_destination_parent(&planned.join("state"))?;
    if landing.starts_with(destination) {
        return Err(WorktreeError::InvalidRequest(format!(
            "state directory {} would be inside the new worktree {}; choose a state directory outside it",
            state.display(),
            destination.display()
        )));
    }
    if destination.starts_with(&landing) {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} would be inside the Riftri state directory {}; choose a destination outside it",
            destination.display(),
            state.display()
        )));
    }
    Ok(planned)
}

/// The canonical parent a new worktree destination will have once its missing
/// leading directories exist, computed without creating anything.
///
/// `git worktree add` creates missing leading directories, so a Riftri add
/// does too (#423) — literally: the destination comes into being through the
/// add's own internal `git worktree add --no-checkout`, on every backend, so
/// Git creates them, after every validation has passed. A refused request
/// therefore creates nothing, and a later failure leaves the directories in
/// place exactly as Git does. The part of the path that exists is resolved through the
/// filesystem, so symbolic links in it are followed exactly as they will be
/// when the directories are created; the missing components are appended as
/// given, since a component that does not exist cannot be a link. An existing
/// ancestor that is not a directory, or a dangling link along the way, is a
/// request Git would also fail, and is refused as one.
pub(crate) fn planned_destination_parent(destination: &Path) -> Result<PathBuf, WorktreeError> {
    let absolute = absolute_path(destination)?;
    let parent = absolute.parent().unwrap_or(&absolute);
    let mut existing = parent;
    let mut missing = Vec::new();
    loop {
        match fs::metadata(existing) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                return Err(WorktreeError::InvalidRequest(format!(
                    "worktree parent is not a directory: {}",
                    existing.display()
                )));
            }
            Err(source)
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                #[cfg(test)]
                crate::test_hooks::fire(
                    crate::test_hooks::FilesystemRacePoint::DestinationParentProbe,
                    existing,
                );
                if let Ok(entry) = fs::symlink_metadata(existing) {
                    // Something exists here after all. Only a link that still
                    // resolves to nothing is dangling; anything else appeared
                    // since the probe above (a concurrent add creating its
                    // state directory, say), so look at it again.
                    if entry.file_type().is_symlink() && fs::metadata(existing).is_err() {
                        return Err(WorktreeError::InvalidRequest(format!(
                            "worktree parent path contains a dangling symbolic link: {}",
                            existing.display()
                        )));
                    }
                    continue;
                }
                // `..` after a directory that does not exist yet cannot be
                // resolved without creating that directory first; refuse it
                // rather than guess.
                let (Some(name), Some(above)) = (existing.file_name(), existing.parent()) else {
                    return Err(WorktreeError::InvalidRequest(format!(
                        "worktree destination climbs out of a directory that does not exist yet: {}",
                        destination.display()
                    )));
                };
                missing.push(name.to_owned());
                existing = above;
            }
            Err(source) => return Err(io("resolve worktree parent", existing, source)),
        }
    }
    let mut planned = fs::canonicalize(existing)
        .map_err(|source| io("resolve worktree parent", existing, source))?;
    for name in missing.iter().rev() {
        planned.push(name);
    }
    Ok(planned)
}

pub(crate) fn resolve_destination_parent(destination: &Path) -> Result<PathBuf, WorktreeError> {
    let absolute = absolute_path(destination)?;
    let parent = absolute.parent().unwrap_or(&absolute);
    // A missing parent is a precondition the caller has to satisfy, not an
    // operational failure: `riftri doctor` already reports it as a
    // `destination-parent` blocker, and nothing has been attempted yet. It was
    // reported as `filesystem-io-failed` with unknown cleanup, telling a
    // harness that retrying might help (#423). The parent-is-a-file case just
    // below was already this refusal.
    let resolved = match fs::canonicalize(parent) {
        Ok(resolved) => resolved,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(WorktreeError::InvalidRequest(format!(
                "worktree parent does not exist: {}; create it first",
                parent.display()
            )));
        }
        Err(source) => return Err(io("resolve worktree parent", parent, source)),
    };
    let metadata = fs::metadata(&resolved)
        .map_err(|source| io("inspect worktree parent", &resolved, source))?;
    if !metadata.is_dir() {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree parent is not a directory: {}",
            parent.display()
        )));
    }
    Ok(resolved)
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
        .map_err(|source| state_directory_io("inspect Riftri state directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::InvalidStateDirectory {
            path: path.to_path_buf(),
        }
        .into());
    }
    fs::canonicalize(path).map_err(|source| io("resolve state directory", path, source))
}

/// Resolve the state directory for an operation on an existing managed
/// worktree.
///
/// A repository that has never completed a managed add has no state directory,
/// and that is not a filesystem failure worth inspecting: it means nothing in
/// this repository is managed, so the worktree the caller named cannot be
/// either. Reporting the absent directory as `filesystem-io-failed` told a
/// harness to investigate a healthy repository and left `cleanup` unknown,
/// while the identical request against a repository that had completed one add
/// reported a plain policy error (#396). An existing directory that cannot be
/// read still fails as the I/O error it is.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn existing_state_directory_for_worktree(
    repository: &Path,
    requested_state: &Path,
    state_given: bool,
    target: &Path,
) -> Result<PathBuf, WorktreeError> {
    let requested_state = absolute_path(requested_state)?;
    resolve_real_state_directory_if_present(&requested_state)?.ok_or_else(|| {
        not_managed_in_state_directory(repository, target, &requested_state, state_given)
    })
}

/// Refuse a worktree the selected state directory does not manage. Without
/// `--state-dir`, only the default location is read (D026), but the worktree
/// may be managed in a state directory the repository registers; then say
/// which, instead of implying the worktree is not managed at all.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn not_managed_in_state_directory(
    repository: &Path,
    target: &Path,
    state_directory: &Path,
    state_given: bool,
) -> WorktreeError {
    let mut message = format!(
        "{} is not an active Riftri-managed worktree in {}",
        target.display(),
        state_directory.display()
    );
    if !state_given
        && let Ok(Some(owner)) = managed_worktree_state_directory(repository, target)
        && !paths_match(&owner, state_directory)
    {
        let flag =
            crate::shell::shell_quoted_path(&owner).unwrap_or_else(|| owner.display().to_string());
        message.push_str(&format!(
            "; it is managed in the registered state directory {}, so pass --state-dir {flag}",
            owner.display()
        ));
    }
    WorktreeError::InvalidRequest(message)
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
        Err(source) => Err(state_directory_io(
            "inspect Riftri state directory",
            path,
            source,
        )),
    }
}

/// An I/O failure on a requested state directory. A path that runs through a
/// regular file ("Not a directory") is the same caller mistake as a state path
/// that is a file, so it gets the same policy refusal instead of an
/// operational error that invites a retry.
fn state_directory_io(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> WorktreeError {
    if source.kind() == std::io::ErrorKind::NotADirectory {
        JournalError::InvalidStateDirectory {
            path: path.to_path_buf(),
        }
        .into()
    } else {
        io(operation, path, source)
    }
}

#[cfg(target_os = "windows")]
fn paths_match(left: &Path, right: &Path) -> bool {
    windows_path_key(left) == windows_path_key(right)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn paths_match(left: &Path, right: &Path) -> bool {
    left == right
}

/// APFS is normalization-insensitive, and macOS Git registers worktree paths
/// precomposed (NFC, `core.precomposeunicode`), while the file system keeps
/// the spelling it was given. Compare composed forms so a worktree named in
/// decomposed Unicode (NFD), as Finder and many apps write names, is still
/// recognized as the worktree Git registered. Paths that are not UTF-8 are
/// compared exactly.
#[cfg(target_os = "macos")]
fn paths_match(left: &Path, right: &Path) -> bool {
    use unicode_normalization::UnicodeNormalization;

    left == right
        || matches!(
            (left.to_str(), right.to_str()),
            (Some(left), Some(right)) if left.nfc().eq(right.nfc())
        )
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
    fs::create_dir_all(state_directory).map_err(|source| {
        state_directory_io("create Riftri state directory", state_directory, source)
    })?;
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

/// Resolve the path of an existing managed worktree the caller named, telling
/// apart the two things a missing path can mean.
///
/// A path that no longer exists used to fail as `filesystem-io-failed`, an
/// operational error with nothing to act on. It is either a request for a
/// worktree that is not managed — a typo, or one already removed, which is a
/// policy refusal — or a managed worktree whose directory was deleted outside
/// Riftri, commonly with `rm -rf`. The second leaves an active add journal that
/// keeps its immutable base in use, so `gc` can never reclaim it; `riftri
/// repair` retires that journal (#336), so the refusal names it.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn existing_managed_destination(
    destination: &Path,
    requested_state: &Path,
) -> Result<PathBuf, WorktreeError> {
    match normalize_existing_destination(destination) {
        Err(WorktreeError::Io { ref source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Err(vanished_destination_error(destination, requested_state)?)
        }
        other => other,
    }
}

/// The refusal for a named worktree whose path does not exist. Errors met
/// while deciding — an unreadable journal, say — propagate as themselves
/// rather than being folded into "not managed".
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn vanished_destination_error(
    destination: &Path,
    requested_state: &Path,
) -> Result<WorktreeError, WorktreeError> {
    let not_managed = || {
        WorktreeError::InvalidRequest(format!(
            "{} does not exist and is not an active Riftri-managed worktree",
            destination.display()
        ))
    };
    let requested_state = absolute_path(requested_state)?;
    let Some(state_directory) = resolve_real_state_directory_if_present(&requested_state)? else {
        return Ok(not_managed());
    };
    for candidate in managed_destination_candidates(destination)? {
        if find_managed_add_journal(&state_directory, &candidate)?.is_some() {
            return Ok(recovery_pending_error(
                format!(
                    "{} no longer exists, but its Riftri add is still recorded as active and keeps its immutable base in use",
                    destination.display()
                ),
                &state_directory,
            ));
        }
    }
    Ok(not_managed())
}

fn find_managed_add_journal(
    state_directory: &Path,
    destination: &Path,
) -> Result<Option<DecodedJournal>, WorktreeError> {
    // Always reload here: lifecycle callers use this again after taking their
    // operation lock. Discovery snapshots must never cross that boundary.
    ManagedJournalSnapshot::load(state_directory)?
        .find(state_directory, destination)
        .map(|journal| journal.cloned())
}

/// One advisory discovery pass, shared only across aliases of the same path.
struct ManagedJournalSnapshot {
    adds: Vec<DecodedJournal>,
    completed: HashSet<String>,
    pending: HashSet<String>,
    moves: Vec<DecodedMoveJournal>,
    pending_moves: HashSet<String>,
    pending_compactions: HashSet<String>,
}

impl ManagedJournalSnapshot {
    fn load(state_directory: &Path) -> Result<Self, WorktreeError> {
        let adds = JournalStore::open(state_directory).load_all()?;
        let removals = RemovalJournalStore::open(state_directory).load_all()?;
        let completed = validated_completed_removal_ids(state_directory, &adds, &removals)?;
        let pending = removals
            .into_iter()
            .filter(|journal| !journal.phase.is_finished())
            .map(|journal| journal.source_add_operation_id)
            .collect::<HashSet<_>>();
        let moves = MoveJournalStore::open(state_directory).load_all()?;
        let pending_moves = moves
            .iter()
            .filter(|journal| !journal.phase.is_finished())
            .map(|journal| journal.source_add_operation_id.clone())
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
        Ok(Self {
            adds,
            completed,
            pending,
            moves,
            pending_moves,
            pending_compactions,
        })
    }

    fn find(
        &self,
        state_directory: &Path,
        destination: &Path,
    ) -> Result<Option<&DecodedJournal>, WorktreeError> {
        let mut matches = self
            .adds
            .iter()
            .filter(|journal| {
                journal.phase == AddWorktreePhase::Active
                    && paths_match(&journal.destination, destination)
                    && !self.completed.contains(&journal.operation_id)
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
            .is_some_and(|journal| self.pending.contains(journal.operation_id.as_str()))
        {
            return Err(pending_lifecycle_error(
                "removal",
                destination,
                state_directory,
            ));
        }
        if managed
            .as_ref()
            .is_some_and(|journal| self.pending_moves.contains(journal.operation_id.as_str()))
        {
            return Err(pending_lifecycle_error(
                "move",
                destination,
                state_directory,
            ));
        }
        if managed.as_ref().is_some_and(|journal| {
            self.pending_compactions
                .contains(journal.operation_id.as_str())
        }) {
            return Err(pending_lifecycle_error(
                "compaction",
                destination,
                state_directory,
            ));
        }
        Ok(managed)
    }
}

/// A durable journal shows an interrupted lifecycle operation touching this
/// worktree. Journals record durable phases, not liveness, so this cannot tell
/// an interrupted operation from one still running in another process; repair
/// is safe either way because it takes the same per-operation locks and skips
/// live operations.
fn pending_lifecycle_error(
    operation: &str,
    subject: &Path,
    state_directory: &Path,
) -> WorktreeError {
    recovery_pending_error(
        format!("a {operation} of {} is already pending", subject.display()),
        state_directory,
    )
}

/// Build a pending-recovery error whose prose names exactly the command the
/// caller can run against the state directory that holds the journal.
///
/// The directory is made absolute first, because the caller may act on this
/// guidance from a different working directory and the machine-readable
/// receipt repeats this value verbatim. The command is omitted entirely when
/// the path cannot be written as a shell argument: an unquoted or lossy
/// rendering names a *different* directory, and repair reports a confident
/// all-clear for any state directory it does not find.
pub fn recovery_pending_error(situation: String, state_directory: &Path) -> WorktreeError {
    let state_directory = crate::command_path(state_directory);
    let message = match crate::repair_command(&state_directory) {
        Some(command) => format!("{situation}; run `{command}`"),
        None => format!(
            "{situation}; run riftri repair against the state directory {} \
             (its exact native path is in the JSON receipt, because it cannot \
             be written as a shell argument)",
            state_directory.display()
        ),
    };
    WorktreeError::RecoveryPending {
        message,
        state_directory,
    }
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
        &|_, _| true,
        &mut issues,
    )?;
    let finished = finished_lifecycle_operations(
        removal_journals,
        move_journals,
        compact_journals,
        prune_journals,
        collection_journals,
    );
    let reapable = |directory: &'static str| {
        let finished = finished
            .iter()
            .find(|(name, _)| *name == directory)
            .map(|(_, operations)| operations);
        move |owner: &str, path: &Path| {
            finished.is_some_and(|operations| operations.contains(owner))
                || unpublished_intent_add_journal(state_directory, directory, owner, path)
                    .is_ok_and(|add_journal| add_journal.is_some())
        }
    };
    diagnose_journal_directory(
        &state_directory.join("removals"),
        removal_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &reapable("removals"),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("moves"),
        move_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &reapable("moves"),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("compactions"),
        compact_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &reapable("compactions"),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("prunes"),
        prune_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &reapable("prunes"),
        &mut issues,
    )?;
    diagnose_journal_directory(
        &state_directory.join("collections"),
        collection_journals
            .iter()
            .map(|journal| journal.journal_path.clone())
            .collect(),
        &reapable("collections"),
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
            .flat_map(|journal| {
                [
                    journal.temporary_index.clone(),
                    git_lock_path(&journal.temporary_index),
                ]
            })
            .chain(
                add_journals
                    .iter()
                    .filter(|journal| journal.phase != AddWorktreePhase::RolledBack)
                    .map(pointer_staging_path),
            )
            .chain(pending_compactions.iter().flat_map(|journal| {
                [
                    journal.temporary_index.clone(),
                    git_lock_path(&journal.temporary_index),
                ]
            }))
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

    // `gc` refuses to collect a base any unfinished journal still claims, but
    // `status` counts only active journals as references. Without this, a base
    // reported as unreferenced silently stays on disk with nothing explaining
    // why. Report the pinning operation so both commands tell the same story.
    let active_base_references = add_journals
        .iter()
        .filter(|journal| {
            journal.phase == AddWorktreePhase::Active
                && !completed_removals.contains(journal.operation_id.as_str())
        })
        .map(|journal| journal.base_path.as_path())
        .collect::<HashSet<_>>();
    let retained_bases = retained_base_paths(state_directory, UnsafeBaseInventory::Ignore)?
        .into_iter()
        .collect::<HashSet<_>>();
    for (base_path, operation_id) in pending_adds
        .iter()
        .map(|journal| (&journal.base_path, &journal.operation_id))
        .chain(
            pending_compactions
                .iter()
                .map(|journal| (&journal.base_path, &journal.operation_id)),
        )
    {
        if active_base_references.contains(base_path.as_path())
            || !retained_bases.contains(base_path)
        {
            continue;
        }
        add_state_issue(
            &mut issues,
            base_path.clone(),
            format!(
                "no active worktree references this immutable base, but unfinished operation {operation_id} still claims it; `riftri gc` skips it until `riftri repair` retires that operation"
            ),
        );
    }

    // Two active journals for one path make `remove` and `repair` refuse
    // forever. `add` now reconciles before creating a journal, but say so for
    // any state that already reached this shape.
    let mut claims = BTreeMap::<&Path, Vec<&str>>::new();
    for journal in add_journals.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active
            && !completed_removals.contains(journal.operation_id.as_str())
    }) {
        claims
            .entry(journal.destination.as_path())
            .or_default()
            .push(journal.operation_id.as_str());
    }
    for (destination, operations) in claims {
        if operations.len() > 1 {
            add_state_issue(
                &mut issues,
                destination.to_path_buf(),
                format!(
                    "several active add journals claim this worktree ({}); retire the stale ones with `riftri repair`",
                    operations.join(", ")
                ),
            );
        }
    }

    // A probe unmount that failed through every retry deliberately abandons
    // its mount and layer directories next to the user's worktrees instead of
    // deleting under a possibly live mount. Name each leftover so the leak
    // stops being invisible; only `riftri repair` removes one, and only when
    // the kernel mount inventory proves it unmounted.
    for directory in probe_scan_directories(add_journals) {
        for path in abandoned_probe_roots(&directory) {
            add_state_issue(
                &mut issues,
                path,
                "an abandoned OverlayFS probe mount was preserved here after a failed \
                 unmount; `riftri repair` removes it only when the kernel reports it \
                 unmounted",
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

/// Directories a Riftri capability probe may have run in: each journaled
/// destination and its parent, because probing resolves to the nearest
/// existing ancestor of the requested destination.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn probe_scan_directories(add_journals: &[DecodedJournal]) -> BTreeSet<PathBuf> {
    let mut directories = BTreeSet::new();
    for journal in add_journals {
        if let Some(parent) = journal.destination.parent() {
            directories.insert(parent.to_path_buf());
        }
        directories.insert(journal.destination.clone());
    }
    directories
}

/// Riftri OverlayFS probe roots present in `directory`, best effort: these
/// live in user-owned directories, so an unreadable entry is skipped rather
/// than failing the whole diagnosis.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn abandoned_probe_roots(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for entry in entries.flatten() {
        if !riftri_storage::OverlayFsMounter::is_abandoned_probe_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if matches!(
            fs::symlink_metadata(&path),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()
        ) {
            roots.push(path);
        }
    }
    roots.sort_unstable();
    roots
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

/// Riftri writes every journal update to `.{operation-id}.{random}.tmp` beside
/// the journal and renames it into place; a crash between the two steps leaves
/// the temporary behind. Recognize exactly that shape — a leading dot, a
/// `.tmp` suffix and two identifier-like components — and nothing else, so a
/// file that could be a user's is never claimed as Riftri's own.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn interrupted_journal_temporary_owner(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let (operation_id, random) = name
        .strip_prefix('.')?
        .strip_suffix(".tmp")?
        .rsplit_once('.')?;
    (journal_identifier_like(operation_id) && journal_identifier_like(random))
        .then(|| operation_id.to_owned())
}

/// Whether `name` looks like a Riftri operation ID or a `tempfile` suffix.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn journal_identifier_like(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether `path` is one of Riftri's own add-operation coordination locks.
///
/// These are deliberately never unlinked (another opener may already hold the
/// same inode), so they outlive their journal and must not be reported as
/// foreign files.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
/// An add journal renamed while `gc --apply` retires its finished history;
/// see `retire_finished_journals`.
fn is_add_retirement_marker(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("retired"))
        && path.parent().and_then(Path::file_name) == Some(OsStr::new("operations"))
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(journal_identifier_like)
}

fn is_operation_coordination_lock(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("lock"))
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(journal_identifier_like)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
/// The add-journal directory, whose interrupted atomic-write temporaries
/// `riftri repair` reaps under each operation's coordination lock; see
/// `reap_interrupted_journal_temporaries`.
const REAPABLE_JOURNAL_DIRECTORY: &str = "operations";

/// Operation IDs whose lifecycle journal is terminal, per journal directory.
///
/// A terminal journal is never rewritten, so a temporary named for one of
/// these operations cannot belong to a write still in progress: it is an
/// orphan of a write that a kill interrupted before the operation finished.
/// Both `status` advice and the reaper use this one predicate, so the advice
/// never promises a cleanup that repair does not perform. Temporaries whose
/// owner is unknown or still pending stay preserved: the first write of a new
/// operation looks exactly like that.
/// The add journal whose operation lock the writer of `path` held, when `path`
/// is a complete intent record for `owner` in lifecycle directory `name` that
/// never reached its atomic rename (its own journal does not exist).
///
/// Removal, move, and compaction take their source add's operation lock
/// before their first journal write and hold it until they return, and a
/// failed write cleans up its own temporary. So once that lock can be taken,
/// no writer of this temporary is still alive. A torn record proves nothing
/// about its writer and yields `None`, as does a source add that does not
/// exist, since its lock could never have been held.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn unpublished_intent_add_journal(
    state_directory: &Path,
    name: &str,
    owner: &str,
    path: &Path,
) -> Result<Option<PathBuf>, WorktreeError> {
    let published = state_directory.join(name).join(format!("{owner}.json"));
    if symlink_metadata_if_present(&published)?.is_some() || !is_regular_file_if_present(path)? {
        return Ok(None);
    }
    let Ok(bytes) = fs::read(path) else {
        return Ok(None);
    };
    let intent = match name {
        "removals" => serde_json::from_slice::<RemovalJournalRecord>(&bytes)
            .ok()
            .filter(|record| record.phase == RemoveWorktreePhase::IntentRecorded)
            .map(|record| (record.operation_id, record.source_add_operation_id)),
        "moves" => serde_json::from_slice::<MoveJournalRecord>(&bytes)
            .ok()
            .filter(|record| record.phase == MoveWorktreePhase::IntentRecorded)
            .map(|record| (record.operation_id, record.source_add_operation_id)),
        "compactions" => serde_json::from_slice::<CompactJournalRecord>(&bytes)
            .ok()
            .filter(|record| record.phase == CompactWorktreePhase::IntentRecorded)
            .map(|record| (record.operation_id, record.source_add_operation_id)),
        _ => None,
    };
    let Some((operation_id, source)) = intent else {
        return Ok(None);
    };
    if operation_id != owner || !journal_identifier_like(&source) {
        return Ok(None);
    }
    let add_journal = state_directory
        .join(REAPABLE_JOURNAL_DIRECTORY)
        .join(format!("{source}.json"));
    if !is_regular_file_if_present(&add_journal)?
        || !is_regular_file_if_present(&add_journal.with_extension("lock"))?
    {
        return Ok(None);
    }
    Ok(Some(add_journal))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn finished_lifecycle_operations(
    removals: &[DecodedRemovalJournal],
    moves: &[DecodedMoveJournal],
    compactions: &[DecodedCompactJournal],
    prunes: &[DecodedPruneJournal],
    collections: &[DecodedCollectionJournal],
) -> [(&'static str, HashSet<String>); 5] {
    fn finished<J>(
        journals: &[J],
        terminal: impl Fn(&J) -> bool,
        id: impl Fn(&J) -> &String,
    ) -> HashSet<String> {
        journals
            .iter()
            .filter(|journal| terminal(journal))
            .map(|journal| id(journal).clone())
            .collect()
    }
    [
        (
            "removals",
            finished(
                removals,
                |journal| journal.phase == RemoveWorktreePhase::Complete,
                |journal| &journal.operation_id,
            ),
        ),
        (
            "moves",
            finished(
                moves,
                |journal| journal.phase.is_finished(),
                |journal| &journal.operation_id,
            ),
        ),
        (
            "compactions",
            finished(
                compactions,
                |journal| {
                    matches!(
                        journal.phase,
                        CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                    )
                },
                |journal| &journal.operation_id,
            ),
        ),
        (
            "prunes",
            finished(
                prunes,
                |journal| journal.phase == PruneWorktreesPhase::Complete,
                |journal| &journal.operation_id,
            ),
        ),
        (
            "collections",
            finished(
                collections,
                |journal| {
                    matches!(
                        journal.phase,
                        GarbageCollectionPhase::Complete | GarbageCollectionPhase::Cancelled
                    )
                },
                |journal| &journal.operation_id,
            ),
        ),
    ]
}

fn diagnose_journal_directory(
    directory: &Path,
    expected: HashSet<PathBuf>,
    reapable: &dyn Fn(&str, &Path) -> bool,
    issues: &mut Vec<StateDiagnosticIssue>,
) -> Result<(), WorktreeError> {
    if !is_real_directory_if_present(directory)? {
        return Ok(());
    }
    for path in child_paths(directory, "read Riftri journal directory")? {
        if !expected.contains(&path) {
            if issues.iter().any(|issue| issue.path == path) {
                continue;
            }
            // Riftri's own artifacts are not foreign files: an interrupted
            // atomic write is reapable by `riftri repair`, and a coordination
            // lock outliving its journal is expected by design.
            if let Some(owner) = interrupted_journal_temporary_owner(&path) {
                // Promise `riftri repair` only where the reaper provably acts;
                // anything else would send the user in a loop, because repair
                // reports success and leaves the file in place.
                let advice = if reapable(&owner, &path) {
                    "an interrupted Riftri journal write left this temporary file; `riftri repair` removes it"
                } else {
                    "an interrupted Riftri journal write left this temporary file; it holds no operation and Riftri preserves it"
                };
                add_state_issue(issues, path, advice);
            } else if !is_operation_coordination_lock(&path) && !is_add_retirement_marker(&path) {
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
fn marker_removed_bases(journals: &[DecodedCollectionJournal]) -> HashSet<&Path> {
    journals
        .iter()
        .filter(|journal| journal.phase == GarbageCollectionPhase::MarkerRemoved)
        .map(|journal| journal.base_path.as_path())
        .collect()
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

    // Index only this decoded snapshot. Keep every filesystem check below
    // fresh, including the completion-marker check before consulting a claim.
    let marker_removed = marker_removed_bases(collection_journals);
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
                || marker_removed.contains(path.as_path());
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
            // Reported on the base itself; see `damaged_base_marker`.
            let is_damaged_marker = path.extension() == Some(OsStr::new("damaged"))
                && is_regular_file(&path)?
                && path
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .is_some_and(looks_like_object_id);

            if is_complete_marker || is_lock || is_damaged_marker {
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
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            add_state_issue(
                issues,
                root,
                "OverlayFS layout root must be a real directory",
            );
            return Ok(());
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io("inspect OverlayFS layout root", &root, source)),
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
        base_count_impact: BaseCountImpact::MayHideReference,
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
fn symlink_metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>, WorktreeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io("inspect path", path, source)),
    }
}

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
    // Name the affected state directory inside the command when it can be
    // written as a shell argument; otherwise keep the placeholder rather than
    // print a path the caller's shell would split or mangle. The command comes
    // from the same helper the failure receipt's `nextCommand` uses, so the
    // two can never disagree.
    let state_directory = crate::command_path(state_directory);
    let next = match crate::status_command(&state_directory) {
        Some(command) => format!("run `{command}`"),
        None => "run `riftri status --state-dir <STATE_DIR>`, replacing <STATE_DIR> with the \
                 exact state directory below"
            .to_owned(),
    };
    let message = format!(
        "cleanup stopped: immutable-base path {} is a symbolic link, not a real directory.\n\
         Following it could access data outside Riftri's expected storage layout. \
         Riftri did not follow this link or delete data through it.\n\
         Next: {next} to inspect the affected state.\n\
         State directory: {}\n\
         Do not delete or move the linked data manually. For new worktrees, use --state-dir \
         with a real directory; this does not repair an existing redirected layout.",
        directory.display(),
        state_directory.display(),
    );
    WorktreeError::SymlinkedBaseParent {
        message,
        state_directory,
    }
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

/// Logical and allocated bytes under `path`.
///
/// Accounting is a snapshot taken while other lifecycle operations run: a
/// removal, a move, a compaction's directory swap, or a collection can take a
/// path away between listing it and measuring it. What vanished that way
/// counts as nothing; failing instead made `status` and `worktree list` exit 1
/// whenever another operation happened to be in flight.
fn tree_usage(path: &Path) -> Result<(u64, u64), WorktreeError> {
    let vanished = |source: &std::io::Error| source.kind() == std::io::ErrorKind::NotFound;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if vanished(&source) => return Ok((0, 0)),
        Err(source) => return Err(io("inspect storage accounting path", path, source)),
    };
    let mut logical_bytes = if metadata.is_dir() { 0 } else { metadata.len() };
    let mut allocated_bytes = match allocated_bytes(path, &metadata) {
        Ok(bytes) => bytes,
        Err(WorktreeError::Io { source, .. }) if vanished(&source) => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(source) if vanished(&source) => return Ok((logical_bytes, allocated_bytes)),
            Err(source) => return Err(io("read storage accounting directory", path, source)),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) if vanished(&source) => continue,
                Err(source) => return Err(io("read storage accounting entry", path, source)),
            };
            let (entry_logical, entry_allocated) = tree_usage(&entry.path())?;
            logical_bytes = logical_bytes.saturating_add(entry_logical);
            allocated_bytes = allocated_bytes.saturating_add(entry_allocated);
        }
    }
    Ok((logical_bytes, allocated_bytes))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct TreeUsageRequest {
    path: PathBuf,
    /// OverlayFS views account logical bytes from the merged view but physical
    /// bytes from their private upper/work layout. Native COW views use the
    /// primary path for both values.
    allocated_path: Option<PathBuf>,
}

/// Measure independent trees concurrently while retaining request order.
///
/// `status` and `worktree list` are read-only snapshots, and each active view
/// owns a disjoint directory tree. A bounded worker set overlaps their metadata
/// reads without changing what any individual traversal counts. Results are
/// stored by request index so an error is still reported in journal order.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn tree_usages(requests: &[TreeUsageRequest]) -> Result<Vec<(u64, u64)>, WorktreeError> {
    const MAX_WORKERS: usize = 4;

    if requests.len() <= 1 {
        return requests.iter().map(measure_tree_usage).collect();
    }

    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_WORKERS)
        .min(requests.len());
    scheduled_tree_usages(requests, worker_count, &measure_tree_usage)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn scheduled_tree_usages(
    requests: &[TreeUsageRequest],
    worker_count: usize,
    measure: &(impl Fn(&TreeUsageRequest) -> Result<(u64, u64), WorktreeError> + Sync),
) -> Result<Vec<(u64, u64)>, WorktreeError> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers = (0..worker_count)
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        // This counter only assigns independent requests; no
                        // filesystem state is published through the atomic.
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(request) = requests.get(index) else {
                            break;
                        };
                        results.push((index, measure(request)));
                    }
                    results
                })
            })
            .collect::<Vec<_>>();
        let mut usages = Vec::with_capacity(requests.len());
        for worker in workers {
            usages.extend(worker.join().expect("tree-usage worker panicked"));
        }
        // Preserve report and first-error order independently of completion
        // order. All workers are joined before returning either outcome.
        usages.sort_unstable_by_key(|(index, _)| *index);
        usages.into_iter().map(|(_, result)| result).collect()
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn measure_tree_usage(request: &TreeUsageRequest) -> Result<(u64, u64), WorktreeError> {
    let (logical_bytes, allocated_bytes) = tree_usage(&request.path)?;
    let allocated_bytes = match &request.allocated_path {
        Some(path) => tree_usage(path)?.1,
        None => allocated_bytes,
    };
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

/// What Git's worktree registry says about an active add journal's
/// destination. Repair and `add` both reconcile against this before deciding
/// whether a journal still describes a live worktree.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveDestinationState {
    /// Git still registers the journal's destination: the journal is live.
    Registered,
    /// Git does not register the destination, but something is still on disk
    /// there. Never retire and never delete: the content could be user data.
    Present,
    /// Git registers a worktree that this journal plausibly owns under a
    /// different path. Report; do not act.
    Relocated(PathBuf),
    /// Git does not register the destination and nothing is on disk there.
    /// The journal describes a worktree that no longer exists.
    Vanished,
}

/// Classify one active add journal against Git's worktree registry.
///
/// `claimed` holds every destination some add journal already accounts for, so
/// an unrelated managed worktree is never mistaken for a relocation.
///
/// Fail-closed ordering matters here: registration is checked first, then
/// on-disk presence, and only a destination that is both unregistered and
/// entirely absent can ever be classified `Vanished`. The relocation check
/// runs before `Vanished` so that a worktree Git moved out from under the
/// journal blocks retirement rather than losing a live view.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn classify_active_destination(
    registered: &[riftri_git::WorktreeInfo],
    claimed: &HashSet<PathBuf>,
    journal: &DecodedJournal,
) -> Result<ActiveDestinationState, WorktreeError> {
    // A registration Git itself marks prunable is a leftover, not a live
    // worktree: Git keeps the entry when the directory disappears rather than
    // dropping it. Treating it as registered short-circuits the retire path,
    // so the journal stays `Active` forever, `repair` reports nothing to do
    // while `status` keeps flagging it, and the base can never be reclaimed.
    // Falling through reuses the presence check below, which still classifies
    // a destination that exists (including a dangling symlink) as `Present`.
    if registered.iter().any(|worktree| {
        paths_match(&worktree.path, &journal.destination) && worktree.prunable_reason.is_none()
    }) {
        return Ok(ActiveDestinationState::Registered);
    }
    // `symlink_metadata` so a dangling symlink still counts as present.
    match fs::symlink_metadata(&journal.destination) {
        Ok(_) => return Ok(ActiveDestinationState::Present),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io(
                "inspect journaled worktree destination",
                &journal.destination,
                source,
            ));
        }
    }
    let expected_branch = journal.branch.as_ref().map(|branch| {
        let mut reference = b"refs/heads/".to_vec();
        reference.extend_from_slice(branch.as_encoded_bytes());
        reference
    });
    for worktree in registered {
        // Git reports its registry in its own spelling (forward slashes, no
        // verbatim prefix on Windows), so comparisons against journal paths
        // must go through `paths_match`, never raw equality: a missed match
        // here would let a worktree another journal owns pass as this
        // journal's relocation.
        if worktree.bare
            || claimed
                .iter()
                .any(|destination| paths_match(destination, &worktree.path))
        {
            continue;
        }
        let plausible = match &expected_branch {
            // Git refuses to check one branch out in two worktrees, so a
            // worktree holding this journal's branch is the only worktree that
            // can be this journal's relocated view.
            Some(expected) => worktree.branch.as_ref() == Some(expected),
            // A detached journal has no such unique key. Match conservatively
            // on the recorded commit: over-matching only blocks retirement.
            None => {
                worktree.detached
                    && worktree
                        .head
                        .as_ref()
                        .is_some_and(|head| head.as_str() == journal.expected_commit)
            }
        };
        if plausible {
            // Report the location in the same canonical filesystem form that
            // journal destinations (and therefore `worktree list`) use, not
            // Git's slash-normalized spelling — on Windows those differ
            // (`R:/...` versus `\\?\R:\...`). The worktree exists, so
            // canonicalization normally succeeds; if it does not, the raw
            // registry spelling is still a truthful report.
            let registered_path =
                fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
            return Ok(ActiveDestinationState::Relocated(registered_path));
        }
    }
    Ok(ActiveDestinationState::Vanished)
}

/// Active add journals that still claim `destination` and that no completed
/// removal has retired.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn add_journals_claiming_destination(
    state_directory: &Path,
    destination: &Path,
) -> Result<Vec<DecodedJournal>, WorktreeError> {
    let load = JournalStore::open(state_directory).load_all_reconciling()?;
    // Fail closed on anything this scan could not judge. A journal that
    // stayed unreadable after retries is an active claim of unknown shape —
    // it might name this destination — so refusing the add is the only answer
    // that cannot create a duplicate claim. An invalid journal likewise
    // refuses, exactly as the previous whole-inventory load did.
    if let Some(blocked) = load.unreadable.first() {
        return Err(WorktreeError::InvalidRequest(format!(
            "another process is updating Riftri journal {}; retry this command ({})",
            blocked.path.display(),
            blocked.reason
        )));
    }
    if let Some(issue) = load.issues.first() {
        return Err(WorktreeError::InvalidRequest(format!(
            "unsafe durable add journal {}: {}",
            issue.path.display(),
            issue.reason
        )));
    }
    // An unreadable removal journal is treated as not completed: the add
    // journal then still counts as claiming its destination, which can only
    // refuse an add, never permit a duplicate.
    let completed = RemovalJournalStore::open(state_directory)
        .load_all_for_status()?
        .journals
        .into_iter()
        .filter(|removal| removal.phase == RemoveWorktreePhase::Complete)
        .map(|removal| removal.source_add_operation_id)
        .collect::<HashSet<_>>();
    Ok(load
        .journals
        .into_iter()
        // Every add that has not been rolled back claims its destination, in
        // flight or interrupted included: repair must be able to trust that a
        // registration there belongs to that add.
        .filter(|journal| {
            journal.phase != AddWorktreePhase::RolledBack
                && paths_match(&journal.destination, destination)
                && !completed.contains(journal.operation_id.as_str())
        })
        .collect())
}

/// Destinations already accounted for by an add journal that has not been
/// rolled back, used to keep relocation detection from claiming a worktree
/// that another journal owns.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn claimed_destinations(journals: &[DecodedJournal]) -> HashSet<PathBuf> {
    journals
        .iter()
        .filter(|journal| journal.phase != AddWorktreePhase::RolledBack)
        .map(|journal| journal.destination.clone())
        .collect()
}

/// Retire an add journal whose worktree Git no longer registers and whose
/// destination is gone, by journaling the completion that the interrupted
/// cleanup never recorded.
///
/// Safety: the whole decision and the durable record are taken under the
/// repository's Git worktree-metadata lock, and the destination is re-checked
/// while that lock is held. The only file removed is this operation's own
/// staged Git pointer, exactly as a normal removal does once the worktree is
/// gone. The branch is deliberately left alone: the worktree is already gone,
/// and the user may still want its commits.
///
/// This records a removal journal rather than rolling the add back so that the
/// existing `Active` invariant (an active journal is never rolled back in
/// place) stays intact, and so that every consumer that already understands a
/// completed removal — base protection, `status`, `find_managed_add_journal` —
/// sees the retirement without new special cases.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn retire_vanished_add_journal(
    git: &Git,
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    validate_recovery_paths(state_directory, journal)?;
    let metadata_lock =
        acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
    let registered = git.list_worktrees(&journal.repository)?;
    // Same rule as `classify_active_destination`: an entry Git marks prunable
    // is a leftover registration, not the worktree coming back.
    if registered.iter().any(|worktree| {
        paths_match(&worktree.path, &journal.destination) && worktree.prunable_reason.is_none()
    }) {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} became registered again; Riftri preserved its journal",
            journal.destination.display()
        )));
    }
    if fs::symlink_metadata(&journal.destination).is_ok() {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} reappeared on disk; Riftri preserved it and its journal",
            journal.destination.display()
        )));
    }

    // The caller's removal snapshot predates this operation's lock: a removal
    // may have finished since, which is why the destination vanished. A second
    // record for the same worktree is never needed, and one written while
    // `gc --apply` retired the first was left behind with no add journal.
    if RemovalJournalStore::open(state_directory)
        .load_all()?
        .iter()
        .any(|removal| removal.source_add_operation_id == journal.operation_id)
    {
        return Ok(());
    }
    let store = RemovalJournalStore::create(state_directory)?;
    let operation_id = allocate_removal_operation_id(&store)?;
    let mut record = RemovalJournalRecord::new(
        operation_id,
        RemovalJournalPaths {
            repository: &journal.repository,
            destination: &journal.destination,
            base_path: &journal.base_path,
        },
        journal.operation_id.clone(),
    );
    // Reject a record the normal removal path would also reject *before* it
    // reaches disk. A persisted journal that validation rejects would make
    // every later repair fail, which is the class of wedge this change ends.
    let decoded = record
        .clone()
        .decode(store.path_for(&record.operation_id))?;
    validate_removal_against_add_journals(
        state_directory,
        &decoded,
        std::slice::from_ref(journal),
    )?;
    store.persist(&record)?;
    advance_removal(
        &store,
        &mut record,
        RemoveWorktreePhase::CleanVerified,
        None,
    )?;
    // Nothing to unlink: the destination is unregistered and absent, verified
    // above under the same metadata lock that is still held here.
    advance_removal(
        &store,
        &mut record,
        RemoveWorktreePhase::WorktreeRemoved,
        None,
    )?;
    drop(metadata_lock);
    remove_file_if_present(&pointer_staging_path(journal))?;
    advance_removal(&store, &mut record, RemoveWorktreePhase::BaseReleased, None)?;
    advance_removal(&store, &mut record, RemoveWorktreePhase::Complete, None)?;
    Ok(())
}

/// Re-home active add journals whose recorded repository no longer exists.
///
/// An older Riftri recorded the worktree an add ran in, which can be a linked
/// worktree removed since (#476); every Git call for the journal then failed.
/// A journal is re-homed only when nothing else is pending for it, its
/// destination is still a live worktree root, this state directory belongs to
/// the repository found through that worktree, and Git registers the
/// destination there. Anything else is left for `status` to report.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn rehome_stranded_add_journals(
    git: &Git,
    state_directory: &Path,
) -> Result<(usize, Vec<String>), WorktreeError> {
    let store = JournalStore::open(state_directory);
    let journals = store.load_all_reconciling()?.journals;
    let removals = RemovalJournalStore::open(state_directory).load_all_for_status()?;
    let moves = MoveJournalStore::open(state_directory).load_all_for_status()?;
    let compactions = CompactJournalStore::open(state_directory).load_all_for_status()?;
    if !removals.issues.is_empty() || !moves.issues.is_empty() || !compactions.issues.is_empty() {
        // Without every lifecycle journal, "nothing else is pending" is unknown.
        return Ok((0, Vec::new()));
    }
    let mut claimed = HashSet::new();
    claimed.extend(
        removals
            .journals
            .iter()
            .map(|removal| removal.source_add_operation_id.as_str()),
    );
    claimed.extend(
        moves
            .journals
            .iter()
            .filter(|journal| !journal.phase.is_finished())
            .map(|journal| journal.source_add_operation_id.as_str()),
    );
    claimed.extend(
        compactions
            .journals
            .iter()
            .filter(|journal| {
                !matches!(
                    journal.phase,
                    CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
                )
            })
            .map(|journal| journal.source_add_operation_id.as_str()),
    );
    let mut rehomed = 0;
    let mut errors = Vec::new();
    for journal in &journals {
        if journal.phase != AddWorktreePhase::Active
            || claimed.contains(journal.operation_id.as_str())
            || journal.repository.is_dir()
            || !is_real_directory_if_present(&journal.destination)?
        {
            continue;
        }
        let _operation_lock = match try_lock_add_operation(&journal.journal_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => continue,
            Err(error) => {
                errors.push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        match rehome_add_journal(git, state_directory, &store, &journal.operation_id) {
            Ok(true) => rehomed += 1,
            Ok(false) => {}
            Err(error) => errors.push(format!("operation {}: {error}", journal.operation_id)),
        }
    }
    Ok((rehomed, errors))
}

/// Re-home one add journal whose operation lock the caller holds.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn rehome_add_journal(
    git: &Git,
    state_directory: &Path,
    store: &JournalStore,
    operation_id: &str,
) -> Result<bool, WorktreeError> {
    // Reload under the lock: the inventory may predate the owner's writes.
    let journal = store.load_operation(operation_id)?;
    if journal.phase != AddWorktreePhase::Active || journal.repository.is_dir() {
        return Ok(false);
    }
    let repository = git.inspect_repository(&journal.destination)?;
    if !repository
        .root
        .as_deref()
        .is_some_and(|root| paths_match(root, &journal.destination))
    {
        return Ok(false);
    }
    // The same path may now hold another repository's worktree.
    if !repository_state_directories_with_git(git, &repository)?
        .iter()
        .any(|owned| owned == state_directory)
    {
        return Ok(false);
    }
    let main = stable_repository_root(git, &journal.destination)?;
    if !git.list_worktrees(&main)?.iter().any(|worktree| {
        paths_match(&worktree.path, &journal.destination) && worktree.prunable_reason.is_none()
    }) {
        return Ok(false);
    }
    store.update_active_repository(
        &journal.journal_path,
        &journal.destination,
        &journal.repository,
        &main,
    )?;
    Ok(true)
}

/// Reconcile every active add journal in `state_directory` against Git's
/// worktree registry, retiring the journals whose worktree no longer exists.
///
/// Returns the retirement count and the relocations that were only reported.
/// Each journal is reconciled under its own add-operation lock; a busy journal
/// is left untouched.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn reconcile_active_add_journals(
    git: &Git,
    state_directory: &Path,
    only_destination: Option<&Path>,
) -> Result<(usize, Vec<RelocatedWorktree>, Vec<String>), WorktreeError> {
    let store = JournalStore::open(state_directory);
    // A journal another process is replacing right now, or one that is
    // invalid, is skipped: this pass only ever acts on journals it can read
    // and validate, and taking no action is always safe. `status` owns the
    // reporting of invalid journals.
    let journals = store.load_all_reconciling()?.journals;
    let claimed = claimed_destinations(&journals);
    let removal_load = RemovalJournalStore::open(state_directory).load_all_for_status()?;
    if !removal_load.issues.is_empty() {
        // With any removal journal unreadable, no add journal's "already
        // removed / removal pending" standing can be trusted. Do nothing this
        // pass rather than risk retiring on stale knowledge.
        return Ok((0, Vec::new(), Vec::new()));
    }
    let removals = removal_load.journals;
    let completed = removals
        .iter()
        .filter(|removal| removal.phase == RemoveWorktreePhase::Complete)
        .map(|removal| removal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let pending_removals = removals
        .iter()
        .filter(|removal| removal.phase != RemoveWorktreePhase::Complete)
        .map(|removal| removal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    // One listing per repository, taken on first use, confirms every journal
    // whose worktree Git still registers: those need nothing. Anything else is
    // decided on a fresh listing under that journal's lock, as before. A
    // stale snapshot cannot make a moved worktree look registered (the
    // reloaded journal names its new path), and one removed since is merely
    // left for the next pass. Listing per journal made repair quadratic.
    let mut inventories = HashMap::new();
    let mut retired = 0;
    let mut relocations = Vec::new();
    let mut errors = Vec::new();
    for journal in &journals {
        if journal.phase != AddWorktreePhase::Active
            || completed.contains(journal.operation_id.as_str())
            // A pending removal already owns this add journal's fate.
            || pending_removals.contains(journal.operation_id.as_str())
            || only_destination.is_some_and(|destination| !paths_match(&journal.destination, destination))
        {
            continue;
        }
        let _operation_lock = match try_lock_add_operation(&journal.journal_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => continue,
            Err(error) => {
                errors.push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        // Reload under the lock: the inventory may predate the owner's writes.
        let journal = match store.load_operation(&journal.operation_id) {
            Ok(journal) => journal,
            Err(error) if crate::journal::journal_error_is_in_flight(&error) => {
                // The owner replaced the journal between our inventory and
                // this reload. In flight means untouched, not broken.
                continue;
            }
            Err(error) => {
                errors.push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        if journal.phase != AddWorktreePhase::Active {
            continue;
        }
        if !inventories.contains_key(&journal.repository)
            && let Ok(listed) = git.list_worktrees(&journal.repository)
        {
            inventories.insert(journal.repository.clone(), listed);
        }
        if inventories
            .get(&journal.repository)
            .is_some_and(|inventory| {
                matches!(
                    classify_active_destination(inventory, &claimed, &journal),
                    Ok(ActiveDestinationState::Registered)
                )
            })
        {
            continue;
        }
        let registered = match git.list_worktrees(&journal.repository) {
            Ok(registered) => registered,
            Err(error) => {
                errors.push(format!("operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        match classify_active_destination(&registered, &claimed, &journal) {
            Ok(ActiveDestinationState::Registered | ActiveDestinationState::Present) => {}
            Ok(ActiveDestinationState::Relocated(path)) => relocations.push(RelocatedWorktree {
                operation_id: journal.operation_id.clone(),
                journal_destination: journal.destination.clone(),
                registered_path: path,
            }),
            Ok(ActiveDestinationState::Vanished) => {
                match retire_vanished_add_journal(git, state_directory, &journal) {
                    Ok(()) => retired += 1,
                    Err(error) => {
                        errors.push(format!("operation {}: {error}", journal.operation_id));
                    }
                }
            }
            Err(error) => errors.push(format!("operation {}: {error}", journal.operation_id)),
        }
    }
    relocations.sort_unstable_by(|left, right| left.operation_id.cmp(&right.operation_id));
    Ok((retired, relocations, errors))
}

/// The active add journal that owns `destination`, if a different operation
/// already holds it. Used to retire a journal whose rollback must not touch a
/// worktree that belongs to someone else.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn superseding_add_journal(
    git: &Git,
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<Option<DecodedJournal>, WorktreeError> {
    // A journal that cannot be read right now cannot prove ownership, and an
    // in-flight read must not surface as a repair error; both simply mean "no
    // supersession found this pass", which retires nothing.
    let owner = JournalStore::open(state_directory)
        .load_all_reconciling()?
        .journals
        .into_iter()
        .find(|candidate| {
            candidate.operation_id != journal.operation_id
                && candidate.phase == AddWorktreePhase::Active
                && paths_match(&candidate.destination, &journal.destination)
                && paths_match(&candidate.repository, &journal.repository)
        });
    let Some(owner) = owner else {
        return Ok(None);
    };
    // Only trust an owner Riftri would itself accept, and only while Git
    // actually registers the contested destination for it.
    validate_recovery_paths(state_directory, &owner)?;
    let registered = git
        .list_worktrees(&owner.repository)?
        .into_iter()
        .find(|worktree| paths_match(&worktree.path, &owner.destination));
    match registered {
        Some(worktree) if worktree.head.is_some() => Ok(Some(owner)),
        _ => Ok(None),
    }
}

/// Retire a losing add journal whose destination is owned by a different,
/// still-active add operation.
///
/// Safety: this deletes nothing at the destination and touches no Git
/// metadata. It removes only artifacts whose names embed this operation's own
/// ID — the scratch view, the base staging directory and the temporary index —
/// so the winner's worktree, the shared immutable base and the winner's
/// journal are all untouched. It refuses outright if this journal still holds
/// the destination's staged `.git` pointer, because then the destination's
/// state depends on this journal and only a real rollback may proceed.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn retire_superseded_add_journal(
    store: &JournalStore,
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    validate_recovery_paths(state_directory, journal)?;
    let staged_pointer = pointer_staging_path(journal);
    if staged_pointer.exists() {
        return Err(WorktreeError::InvalidRequest(format!(
            "operation {} still holds the staged Git pointer for {}; Riftri preserved both",
            journal.operation_id,
            journal.destination.display()
        )));
    }
    let pending = if journal.phase == AddWorktreePhase::RollbackPending {
        journal.clone()
    } else {
        store.update_phase(journal, AddWorktreePhase::RollbackPending)?
    };
    remove_tree_if_present(&journal.scratch)?;
    remove_tree_if_present(&journal.base_staging)?;
    remove_temporary_index(&journal.temporary_index)?;
    store.update_phase(&pending, AddWorktreePhase::RolledBack)?;
    Ok(())
}

/// Remove the `.{operation-id}.{random}.tmp` files an interrupted journal
/// write leaves in `operations/`.
///
/// Safety: only names matching Riftri's own atomic-write shape are considered,
/// only regular files are removed, and each removal happens while this process
/// exclusively holds that operation's coordination lock. `add` takes that lock
/// before its first journal write and holds it for the whole operation, so
/// holding it proves no live writer can be mid-rename for that operation ID. A
/// busy operation is skipped, never forced.
///
/// Scope is deliberately limited to `operations/`: it is the only journal
/// directory with a per-operation lock that makes the reap provably safe, and
/// it is where an interrupted `worktree add` leaves these files.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn reap_interrupted_journal_temporaries(
    state_directory: &Path,
) -> Result<Vec<PathBuf>, WorktreeError> {
    let directory = state_directory.join(REAPABLE_JOURNAL_DIRECTORY);
    if !is_real_directory_if_present(&directory)? {
        return Ok(Vec::new());
    }
    let mut reaped = Vec::new();
    for path in child_paths(&directory, "read Riftri journal directory")? {
        let Some(operation_id) = interrupted_journal_temporary_owner(&path) else {
            continue;
        };
        if !is_regular_file(&path)? {
            continue;
        }
        let journal_path = directory.join(format!("{operation_id}.json"));
        // An add takes its coordination lock before its first journal write and
        // never unlinks it, so a missing lock file proves no operation with
        // this ID is running and the temporary is definitively orphaned.
        // Taking the lock in that case would only litter a new lock file.
        let _operation_lock = if journal_path.with_extension("lock").exists() {
            match try_lock_add_operation(&journal_path)? {
                Some(lock) => Some(lock),
                None => continue,
            }
        } else {
            None
        };
        // Re-check under the lock: the owner may have renamed it into place.
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io("inspect interrupted journal write", &path, source)),
        }
        remove_file_if_present(&path)?;
        reaped.push(path);
    }
    if let Some(path) = reaped.last() {
        sync_parent(path)?;
    }
    reap_finished_lifecycle_temporaries(state_directory, &mut reaped)?;
    Ok(reaped)
}

/// Reap the temporaries that interrupted lifecycle journal writes left beside
/// journals that have since reached a terminal phase. See
/// `finished_lifecycle_operations` for why this needs no lock.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn reap_finished_lifecycle_temporaries(
    state_directory: &Path,
    reaped: &mut Vec<PathBuf>,
) -> Result<(), WorktreeError> {
    let finished = finished_lifecycle_operations(
        &RemovalJournalStore::open(state_directory)
            .load_all_for_status()?
            .journals,
        &MoveJournalStore::open(state_directory)
            .load_all_for_status()?
            .journals,
        &CompactJournalStore::open(state_directory)
            .load_all_for_status()?
            .journals,
        &PruneJournalStore::open(state_directory)
            .load_all_for_status()?
            .journals,
        &CollectionJournalStore::open(state_directory)
            .load_all_for_status()?
            .journals,
    );
    for (name, operations) in finished {
        let directory = state_directory.join(name);
        if !is_real_directory_if_present(&directory)? {
            continue;
        }
        let mut reaped_here = None;
        for path in child_paths(&directory, "read Riftri journal directory")? {
            let Some(owner) = interrupted_journal_temporary_owner(&path) else {
                continue;
            };
            if !is_regular_file(&path)? {
                continue;
            }
            // Held only while this temporary is proven orphaned.
            let mut _writer_lock = None;
            if !operations.contains(&owner) {
                let Some(add_journal) =
                    unpublished_intent_add_journal(state_directory, name, &owner, &path)?
                else {
                    continue;
                };
                let Some(lock) = try_lock_add_operation(&add_journal)? else {
                    continue;
                };
                _writer_lock = Some(lock);
                // Re-check under the lock: the writer may have published it.
                if unpublished_intent_add_journal(state_directory, name, &owner, &path)?.is_none() {
                    continue;
                }
            }
            remove_file_if_present(&path)?;
            reaped_here = Some(path.clone());
            reaped.push(path);
        }
        if let Some(path) = reaped_here {
            sync_parent(&path)?;
        }
    }
    Ok(())
}

pub fn recover_incomplete_operations(
    state_directory: &Path,
) -> Result<RecoveryReport, WorktreeError> {
    validate_lifecycle_git_environment()?;
    let state_directory = absolute_path(state_directory)?;
    let state_directory =
        resolve_real_state_directory_if_present(&state_directory)?.unwrap_or(state_directory);
    // Reconcile active journals against Git's registry first: an add journal
    // whose worktree no longer exists must stop referencing its destination
    // and its base before anything else inspects either. Doing this before the
    // inventory is loaded also means the rest of the pass sees the retirement.
    // Re-home journals that name a vanished linked worktree first: every later
    // step, reconciliation included, runs Git from a journal's repository.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let rehoming = rehome_stranded_add_journals(&Git::default(), &state_directory);
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let reconciliation = reconcile_active_add_journals(&Git::default(), &state_directory, None);
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
            // Finished, and named by a retirement marker: nothing to recover.
            Err(_) if retired_removal_leftover(&state_directory, &journal, &journals) => {
                removal_journals.push(journal)
            }
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
    #[cfg(target_os = "linux")]
    let probe_directories = probe_scan_directories(&journals);
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

    // Report every worktree Git itself lists without a resolvable HEAD, so a
    // repair pass names the corruption it cannot fix instead of silently
    // working around it. Listing failures are surfaced by the per-journal
    // recovery below, not here.
    let mut inspected_repositories = HashSet::new();
    for journal in &journals {
        if !inspected_repositories.insert(journal.repository.clone()) {
            continue;
        }
        let Ok(registered) = git.list_worktrees(&journal.repository) else {
            continue;
        };
        report.unresolvable_worktrees.extend(
            registered
                .into_iter()
                .filter(|worktree| worktree.head_unresolvable)
                .map(|worktree| worktree.path),
        );
    }
    report.unresolvable_worktrees.sort_unstable();
    report.unresolvable_worktrees.dedup();

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    match rehoming {
        Ok((rehomed, errors)) => {
            report.rehomed_adds = rehomed;
            report.errors.extend(errors);
        }
        Err(error) => report
            .errors
            .push(format!("add-journal re-homing: {error}")),
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    match reconciliation {
        Ok((retired, relocations, errors)) => {
            report.retired_adds = retired;
            report.relocations = relocations;
            report.errors.extend(errors);
        }
        Err(error) => report
            .errors
            .push(format!("active add-journal reconciliation: {error}")),
    }

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
            // `gc --apply` retired this finished operation (D043) while the
            // pass waited for its lock. Taking the lock re-created the lock
            // file, so remove that again rather than leave it behind.
            Err(JournalError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound
                    && !journal.journal_path.exists() =>
            {
                let _ = fs::remove_file(journal.journal_path.with_extension("lock"));
                continue;
            }
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
                // A losing racer never owned the destination it names: another
                // still-active operation does. Rolling it back would refuse
                // forever ("worktree HEAD changed after creation"), leaving a
                // journal nothing could retire. Retire it without touching the
                // winner's worktree instead.
                #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
                match superseding_add_journal(&git, &state_directory, &journal) {
                    Ok(Some(_)) => {
                        match retire_superseded_add_journal(&store, &state_directory, &journal) {
                            Ok(()) => report.recovered += 1,
                            Err(error) => report
                                .errors
                                .push(format!("operation {}: {error}", journal.operation_id)),
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        report
                            .errors
                            .push(format!("operation {}: {error}", journal.operation_id));
                        continue;
                    }
                }
                match validate_recovery_paths(&state_directory, &journal)
                    .and_then(|()| adopt_overlayfs_mount_identity(&store, journal.clone()))
                    .and_then(|journal| {
                        let pending =
                            store.update_phase(&journal, AddWorktreePhase::RollbackPending)?;
                        let outcome = rollback_decoded(&git, &state_directory, &journal)?;
                        store.update_phase(&pending, AddWorktreePhase::RolledBack)?;
                        Ok(outcome)
                    }) {
                    Err(error) => report
                        .errors
                        .push(format!("operation {}: {error}", journal.operation_id)),
                    Ok(AddRollback::RolledBack) => report.recovered += 1,
                    Ok(AddRollback::Released) => {
                        report.recovered += 1;
                        report.released_adds.push(journal.destination.clone());
                    }
                }
            }
        }
    }

    for journal in removal_journals {
        match journal.phase {
            RemoveWorktreePhase::Complete => {
                report.completed_removals += 1;
                continue;
            }
            RemoveWorktreePhase::Cancelled => continue,
            _ => {}
        }
        // A running `riftri worktree remove` holds its worktree's add lock for
        // the whole transaction; resuming its journal concurrently drove one
        // removal from two processes. Skip it while busy, as for compaction,
        // and re-read it under the lock in case its owner just finished.
        let _operation_lock = match try_lock_add_operation(
            &JournalStore::open(&state_directory).path_for(&journal.source_add_operation_id),
        ) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.busy_adds += 1;
                continue;
            }
            Err(error) => {
                report.errors.push(format!(
                    "removal operation {}: {error}",
                    journal.operation_id
                ));
                continue;
            }
        };
        let journal = match removal_store
            .load_all()?
            .into_iter()
            .find(|candidate| candidate.operation_id == journal.operation_id)
        {
            Some(journal) if !journal.phase.is_finished() => journal,
            Some(journal) => {
                if journal.phase == RemoveWorktreePhase::Complete {
                    report.completed_removals += 1;
                }
                continue;
            }
            None => continue,
        };
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "removal",
            operation_id: journal.operation_id.clone(),
        });
        match resume_removal(&git, &removal_store, journal.clone()) {
            Err(error) => report.errors.push(format!(
                "removal operation {}: {error}",
                journal.operation_id
            )),
            Ok(RemovalResumption::Cancelled) => report.cancelled_removals += 1,
            Ok(RemovalResumption::Completed) => {
                report.recovered_removals += 1;
                report.completed_removals += 1;
                report.active = report.active.saturating_sub(1);
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    for journal in move_journals {
        match journal.phase {
            MoveWorktreePhase::Complete => {
                report.completed_moves += 1;
                continue;
            }
            MoveWorktreePhase::Cancelled => continue,
            _ => {}
        }
        // A running `riftri worktree move` holds the same lock; see removals.
        let _operation_lock = match try_lock_add_operation(
            &JournalStore::open(&state_directory).path_for(&journal.source_add_operation_id),
        ) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.busy_adds += 1;
                continue;
            }
            Err(error) => {
                report
                    .errors
                    .push(format!("move operation {}: {error}", journal.operation_id));
                continue;
            }
        };
        let journal = match move_store
            .load_all()?
            .into_iter()
            .find(|candidate| candidate.operation_id == journal.operation_id)
        {
            Some(journal) if !journal.phase.is_finished() => journal,
            Some(journal) => {
                if journal.phase == MoveWorktreePhase::Complete {
                    report.completed_moves += 1;
                }
                continue;
            }
            None => continue,
        };
        progress::emit(ProgressEvent::RepairRecovering {
            kind: "move",
            operation_id: journal.operation_id.clone(),
        });
        match resume_move(&git, &move_store, journal.clone(), None) {
            Ok(MoveResumption::Moved) => {
                report.recovered_moves += 1;
                report.completed_moves += 1;
            }
            // Git refuses this move for good; retrying it on every repair
            // left the worktree stuck. The paths proved nothing moved.
            Ok(MoveResumption::Cancelled(_)) => report.cancelled_moves += 1,
            Err(error) => report
                .errors
                .push(format!("move operation {}: {error}", journal.operation_id)),
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

    // Last, once no journal in this pass is still being written: clear the
    // atomic-write leftovers an interrupted operation left in Riftri's own
    // state directory, so they stop being reported forever.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    match reap_interrupted_journal_temporaries(&state_directory) {
        Ok(reaped) => report.reaped_artifacts = reaped,
        Err(error) => report
            .errors
            .push(format!("interrupted journal writes: {error}")),
    }

    // A failed probe unmount deliberately abandons its mount and layers next
    // to the user's worktrees; the leak is unbounded because probing reruns on
    // every add. Remove each leftover only when the kernel mount inventory
    // proves nothing is mounted at or below it, and preserve — but report —
    // any root a mount still covers.
    #[cfg(target_os = "linux")]
    for directory in probe_directories {
        for root in abandoned_probe_roots(&directory) {
            match OverlayFsMounter::remove_abandoned_probe_root(&root) {
                Ok(true) => report.reaped_probe_roots.push(root),
                Ok(false) => report.preserved_probe_mounts.push(root),
                Err(error) => report.errors.push(format!(
                    "abandoned OverlayFS probe {}: {error}",
                    root.display()
                )),
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
    let adds_by_id = index_journals_by(add_journals, |journal| journal.operation_id.as_str());
    for journal in removal_journals {
        let source = adds_by_id
            .get(journal.source_add_operation_id.as_str())
            .and_then(|group| group.first().copied());
        // A retirement leftover names no live add operation and holds no claim.
        if retired_removal_without_add(state_directory, journal, source.is_none()) {
            continue;
        }
        validate_removal_against_add_source(state_directory, journal, source)?;
        if journal.phase == RemoveWorktreePhase::Complete {
            completed.insert(journal.source_add_operation_id.clone());
        }
    }
    Ok(completed)
}

/// A completed removal whose add journal has been renamed to its retirement
/// marker: a leftover `gc --apply` deletes, holding no claim.
fn retired_removal_leftover(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
    add_journals: &[DecodedJournal],
) -> bool {
    let missing = !add_journals
        .iter()
        .any(|candidate| candidate.operation_id == journal.source_add_operation_id);
    retired_removal_without_add(state_directory, journal, missing)
}

fn retired_removal_without_add(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
    missing: bool,
) -> bool {
    journal.phase == RemoveWorktreePhase::Complete
        && missing
        && add_operation_is_retiring(state_directory, &journal.source_add_operation_id)
}

fn validate_removal_against_add_journals(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
    add_journals: &[DecodedJournal],
) -> Result<(), WorktreeError> {
    let source = add_journals
        .iter()
        .find(|candidate| candidate.operation_id == journal.source_add_operation_id);
    validate_removal_against_add_source(state_directory, journal, source)
}

// Operation-local indexes only: callers still load a fresh snapshot, and GC
// re-reads the complete lineage under its add lock before deleting anything.
// Keep all duplicates and their original order, including first-match lookup.
fn index_journals_by<'a, T>(
    journals: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
) -> HashMap<&'a str, Vec<&'a T>> {
    let mut indexed = HashMap::<&str, Vec<&T>>::new();
    for journal in journals {
        indexed.entry(key(journal)).or_default().push(journal);
    }
    indexed
}

fn validate_removal_against_add_source(
    state_directory: &Path,
    journal: &DecodedRemovalJournal,
    source: Option<&DecodedJournal>,
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
    let source = source.ok_or_else(|| {
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

/// How `repair` left a pending removal.
enum RemovalResumption {
    Completed,
    /// The worktree changed before anything was removed, so it stays.
    Cancelled,
}

fn resume_removal(
    git: &Git,
    store: &RemovalJournalStore,
    journal: DecodedRemovalJournal,
) -> Result<RemovalResumption, WorktreeError> {
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
    if record.phase == RemoveWorktreePhase::Cancelled {
        return Ok(RemovalResumption::Cancelled);
    }

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
        if verify_recoverable_removal(git, &journal, &managed)?
            && cancel_untouched_removal(git, store, &mut record, &journal.repository, &managed)?
        {
            return Ok(RemovalResumption::Cancelled);
        }
        record.transition(RemoveWorktreePhase::CleanVerified)?;
        store.persist(&record)?;
    }

    if record.phase == RemoveWorktreePhase::CleanVerified {
        let quarantine = removal_quarantine_path(&journal.destination, &journal.operation_id)?;
        let (mut registered, mut destination_exists) = removal_presence(git, &journal)?;
        if let Some(metadata) = symlink_metadata_if_present(&quarantine)? {
            if !metadata.is_dir() || destination_exists {
                return Err(WorktreeError::InvalidRequest(format!(
                    "removal quarantine {} does not match a recoverable state beside {}; recovery preserved both",
                    quarantine.display(),
                    journal.destination.display()
                )));
            }
            if registered {
                // Killed after the view was quarantined but before Git dropped
                // its registration: put the view back so every check below
                // decides again exactly as for a view that never moved.
                rename_quarantined_view(&quarantine, &journal.destination)?;
                destination_exists = true;
            } else {
                // Git already unregistered the view, so the quarantine is no
                // longer a worktree. Only this removal creates its name.
                remove_tree_if_present(&quarantine)?;
                sync_parent(&quarantine)?;
                (registered, destination_exists) = removal_presence(git, &journal)?;
            }
        }
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
                if cancel_untouched_removal(git, store, &mut record, &journal.repository, &managed)?
                {
                    return Ok(RemovalResumption::Cancelled);
                }
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
                &quarantine,
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
    Ok(RemovalResumption::Completed)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn resume_move(
    git: &Git,
    store: &MoveJournalStore,
    journal: DecodedMoveJournal,
    fail_after: Option<MoveWorktreePhase>,
) -> Result<MoveResumption, WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "move")?;
    if store.path_for(&journal.operation_id) != journal.journal_path {
        return Err(WorktreeError::InvalidRequest(format!(
            "move journal {} has an operation ID that does not match its filename",
            journal.journal_path.display()
        )));
    }
    validate_move_paths(&state_directory, &journal)?;
    let mut record = store.reload(&journal)?;
    if record.phase == MoveWorktreePhase::Cancelled {
        return Err(WorktreeError::InvalidRequest(format!(
            "move journal {} was cancelled; nothing is left to resume",
            journal.journal_path.display()
        )));
    }

    if record.phase == MoveWorktreePhase::IntentRecorded {
        let metadata_lock =
            acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
        let (source_registered, destination_registered) = move_registration(git, &journal)?;
        let source_exists = journal.source.exists();
        let destination_exists = journal.destination.exists();
        if source_registered && source_exists && !destination_registered && !destination_exists {
            if let Err(error) =
                git.move_worktree(&journal.repository, &journal.source, &journal.destination)
            {
                // Git refuses some moves only once asked, and for good (a
                // worktree holding submodules). Retrying would refuse again
                // forever while the pending journal blocks every other
                // lifecycle command, so cancel it when both paths prove
                // nothing moved. Anything else stays for manual attention.
                let (source_registered, destination_registered) = move_registration(git, &journal)?;
                if source_registered
                    && journal.source.exists()
                    && !destination_registered
                    && !journal.destination.exists()
                {
                    advance_move(store, &mut record, MoveWorktreePhase::Cancelled, fail_after)?;
                    return Ok(MoveResumption::Cancelled(error));
                }
                return Err(error.into());
            }
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
    Ok(MoveResumption::Moved)
}

/// How a move journal ended once resumed.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
enum MoveResumption {
    Moved,
    /// Git refused the move and nothing changed; the journal is cancelled.
    Cancelled(GitError),
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
/// A path that vanished while compaction walked the worktree, such as a file
/// a concurrent `git switch` deleted, or a file whose length changed while it
/// was hashed, means the worktree changed under it. That is the same refusal
/// as any other change, not an I/O failure to inspect.
/// Git failures are left alone: a missing `git` is not a changed worktree.
fn vanished_during_compaction(error: WorktreeError, destination: &Path) -> WorktreeError {
    if !matches!(error, WorktreeError::Io { .. } | WorktreeError::Storage(_)) {
        return error;
    }
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(cause) = current {
        if cause.downcast_ref::<std::io::Error>().is_some_and(|cause| {
            cause.kind() == std::io::ErrorKind::NotFound
                || cause
                    .get_ref()
                    .is_some_and(|inner| inner.is::<crate::base_integrity::ChangedWhileReading>())
        }) {
            return WorktreeError::InvalidRequest(format!(
                "worktree {} changed while it was being compacted; compaction was cancelled and nothing changed",
                destination.display()
            ));
        }
        current = cause.source();
    }
    error
}

/// Whether Git's view of a compacted worktree moved since the compaction
/// verified it: HEAD left the compacted commit, or the index holds changes.
fn compacted_git_state_moved(
    git: &Git,
    journal: &DecodedCompactJournal,
) -> Result<bool, WorktreeError> {
    let expected = ObjectId::parse(journal.expected_commit.clone())?;
    let head_moved = git
        .list_worktrees(&journal.repository)?
        .into_iter()
        .find(|worktree| paths_match(&worktree.path, &journal.destination))
        .is_none_or(|worktree| worktree.head.as_ref() != Some(&expected));
    Ok(head_moved || git.worktree_index_has_changes(&journal.destination)?)
}

/// Undo a compaction whose old view a Git command updated after the swap.
///
/// Each step is decided from the paths alone, so recovery repeats whatever a
/// kill left undone: move the untouched replacement back to its staging
/// name, rename Git's view back into place, point the add journal at its
/// original base and commit again, then delete the replacement.
fn restore_original_after_compaction(
    git: &Git,
    store: &CompactJournalStore,
    mut record: CompactJournalRecord,
    journal: &DecodedCompactJournal,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "compaction")?;
    let old_expected_commit = journal.old_expected_commit.as_deref().ok_or_else(|| {
        WorktreeError::InvalidRequest(format!(
            "compaction journal {} cannot restore its original view without the original commit",
            journal.journal_path.display()
        ))
    })?;
    let _metadata_lock =
        acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
    if journal.destination.is_dir() && journal.quarantine.is_dir() && !journal.replacement.exists()
    {
        if directory_snapshot(&journal.destination)? != journal.expected_snapshot {
            return Err(WorktreeError::InvalidRequest(format!(
                "a Git command updated the pre-compaction view of {} kept at {}, but {} itself \
                 also changed, so neither can replace the other; both were preserved",
                journal.destination.display(),
                journal.quarantine.display(),
                journal.destination.display()
            )));
        }
        fs::rename(&journal.destination, &journal.replacement)
            .map_err(|source| io("set the compacted view aside", &journal.replacement, source))?;
        sync_parent(&journal.replacement)?;
    }
    if !journal.destination.exists() && journal.quarantine.is_dir() {
        fs::rename(&journal.quarantine, &journal.destination).map_err(|source| {
            io(
                "restore the worktree Git updated during compaction",
                &journal.destination,
                source,
            )
        })?;
        sync_parent(&journal.destination)?;
    }
    if !journal.destination.is_dir() || journal.quarantine.exists() {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} is restoring {}, but the paths disagree; all paths were preserved",
            journal.journal_path.display(),
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
        &journal.base_path,
        &journal.old_base_path,
        old_expected_commit,
    )?;
    remove_tree_if_present(&journal.replacement)?;
    sync_parent(&journal.replacement)?;
    advance_compaction(
        store,
        &mut record,
        CompactWorktreePhase::Cancelled,
        fail_after,
    )
}

fn resume_compaction(
    git: &Git,
    store: &CompactJournalStore,
    journal: DecodedCompactJournal,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    let state_directory = lifecycle_state_directory(&journal.journal_path, "compaction")?;
    validate_compaction_paths(&state_directory, &journal)?;
    let mut record = store.reload(&journal)?;
    if record.phase == CompactWorktreePhase::RestoringOriginal {
        return restore_original_after_compaction(git, store, record, &journal, fail_after);
    }

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
            cancel_unactivated_compaction(store, &mut record, &journal, fail_after)?;
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
            match verify_compaction_source(
                git,
                &journal.repository,
                &journal.destination,
                &commit,
                Some(&journal.expected_snapshot),
            ) {
                Ok(()) => {}
                // The worktree changed after its replacement was built: a
                // write, a new file, a checkout. Nothing has touched the
                // worktree yet, so cancel and remove only the replacement.
                // Left pending, repair re-ran this check forever while remove
                // and compact refused until it passed.
                Err(error) => match vanished_during_compaction(error, &journal.destination) {
                    WorktreeError::InvalidRequest(_) => {
                        cancel_unactivated_compaction(store, &mut record, &journal, fail_after)?;
                        return Ok(());
                    }
                    error => return Err(error),
                },
            }
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
                        operation: Box::new(io(
                            "activate compacted worktree",
                            &journal.destination,
                            source,
                        )),
                        rollback: Box::new(io(
                            "restore original worktree",
                            &journal.destination,
                            rollback,
                        )),
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
        // A failure while the lock exists is reported as `IndexLocked`; a
        // command that released it in between surfaces as a plain failure, so
        // that one is tried once more.
        let refreshed = match git.refresh_worktree_index(&journal.destination) {
            Err(GitError::CommandFailed { .. }) => git.refresh_worktree_index(&journal.destination),
            refreshed => refreshed,
        };
        match refreshed {
            // The refresh only updates Git's stat cache, which Git refreshes
            // itself later. A lock means a Git command is running in the
            // worktree right now; failing here left the compaction pending,
            // when that command is exactly what the old-view check below
            // detects and undoes.
            Ok(()) | Err(GitError::IndexLocked { .. }) => {}
            Err(error) => return Err(error.into()),
        }
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
        // The verified old view is renamed before it is deleted, so a kill
        // mid-delete leaves a partial tree under a name only this cleanup
        // creates. Re-verifying that tree could never succeed; it was already
        // verified, so recovery just finishes deleting it.
        let dropped = compaction_drop_path(&journal)?;
        if journal.quarantine.exists() {
            if symlink_metadata_if_present(&dropped)?.is_some() {
                return Err(WorktreeError::InvalidRequest(format!(
                    "compaction cleanup path {} already exists beside quarantine {}; both were preserved",
                    dropped.display(),
                    journal.quarantine.display()
                )));
            }
            // A Git command that ran in the old view after the swap (a
            // commit, a switch) moved HEAD or the index with that view, so it,
            // not the untouched replacement, matches Git. Swap them back.
            if directory_snapshot(&journal.quarantine)? != journal.expected_snapshot
                && journal.old_expected_commit.is_some()
                && compacted_git_state_moved(git, &journal)?
                && directory_snapshot(&journal.destination)? == journal.expected_snapshot
            {
                advance_compaction(
                    store,
                    &mut record,
                    CompactWorktreePhase::RestoringOriginal,
                    fail_after,
                )?;
                return restore_original_after_compaction(git, store, record, &journal, fail_after);
            }
            // A write that reached the old view after it was verified is the
            // caller's data: keep the view and say how to finish. This is not
            // a refusal — the compacted replacement is already in place — so
            // it is reported as recovery the caller completes, then `repair`.
            if directory_snapshot(&journal.quarantine)? != journal.expected_snapshot {
                return Err(recovery_pending_error(
                    format!(
                        "{} was compacted, but files written during the compaction reached \
                         its old view, which was kept at {}; copy anything you need from \
                         there into {} and delete it",
                        journal.destination.display(),
                        journal.quarantine.display(),
                        journal.destination.display()
                    ),
                    &state_directory,
                ));
            }
            fs::rename(&journal.quarantine, &dropped).map_err(|source| {
                io("hand the compacted-away view to cleanup", &dropped, source)
            })?;
            sync_parent(&dropped)?;
        }
        remove_tree_if_present(&dropped)?;
        sync_parent(&dropped)?;
        advance_compaction(
            store,
            &mut record,
            CompactWorktreePhase::Complete,
            fail_after,
        )?;
    }
    Ok(())
}

/// Remove the immutable-base bucket a cancelled compaction created for its
/// recomputed checkout profile, if nothing was ever materialized into it.
///
/// A compaction whose checkout profile differs from the add's creates its new
/// bucket before recording intent, so a cancellation before the base build
/// can otherwise strand an empty directory that storage accounting must
/// forever report as unexplained. Removal is non-recursive: a bucket that
/// gained any entry — a base tree, its completion marker, a builder's
/// coordination lock, another operation's staging — makes `remove_dir` fail
/// and is left exactly as it is. `validate_compaction_paths` already proved
/// the bucket sits directly under this state directory's `bases/v1`.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn remove_empty_base_bucket(journal: &DecodedCompactJournal) -> Result<(), WorktreeError> {
    match journal.base_path.parent() {
        Some(bucket) => remove_empty_bucket(bucket),
        None => Ok(()),
    }
}

/// Remove an immutable-base bucket only if it is empty. Removal is
/// non-recursive, so a bucket holding any base, marker, lock, or another
/// operation's staging is left exactly as it is.
fn remove_empty_bucket(bucket: &Path) -> Result<(), WorktreeError> {
    match fs::remove_dir(bucket) {
        Ok(()) => sync_parent(bucket),
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(io("remove empty immutable-base bucket", bucket, source)),
    }
}

/// Where a compaction moves its verified old view to delete it, derived from
/// the journal so recovery needs no extra record. See `resume_compaction`.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn compaction_drop_path(journal: &DecodedCompactJournal) -> Result<PathBuf, WorktreeError> {
    journal
        .quarantine
        .parent()
        .map(|parent| parent.join(format!(".riftri-compact-drop-{}", journal.operation_id)))
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "compaction journal {} has a quarantine without a parent directory",
                journal.journal_path.display()
            ))
        })
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
    if source.phase != AddWorktreePhase::Active
        || source.repository != journal.repository
        || source.backend != journal.backend
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} does not match its active add operation",
            journal.journal_path.display()
        )));
    }
    validate_recovery_paths(state_directory, &source)?;
    if matches!(
        journal.phase,
        CompactWorktreePhase::Complete | CompactWorktreePhase::Cancelled
    ) {
        return Ok(());
    }
    let expected_base = if journal.phase == CompactWorktreePhase::AddJournalUpdated {
        &journal.base_path
    } else if source.base_path == journal.old_base_path || source.base_path == journal.base_path {
        &source.base_path
    } else {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} does not match its add journal's immutable base",
            journal.journal_path.display()
        )));
    };
    if source.destination != journal.destination
        || source.base_path.as_path() != expected_base.as_path()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "compaction journal {} does not match its active add operation",
            journal.journal_path.display()
        )));
    }
    Ok(())
}

/// Cancel a compaction whose replacement was never activated: the worktree
/// was not touched, so only the compaction's own artifacts are removed.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn cancel_unactivated_compaction(
    store: &CompactJournalStore,
    record: &mut CompactJournalRecord,
    journal: &DecodedCompactJournal,
    fail_after: Option<CompactWorktreePhase>,
) -> Result<(), WorktreeError> {
    remove_tree_if_present(&journal.replacement)?;
    remove_tree_if_present(&journal.base_staging)?;
    remove_temporary_index(&journal.temporary_index)?;
    remove_empty_base_bucket(journal)?;
    advance_compaction(store, record, CompactWorktreePhase::Cancelled, fail_after)
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
    let marker = crate::base_integrity::worktree_marker(path)
        .map_err(|source| io("snapshot managed worktree", path, source))?;
    let mut digest = Sha256::new();
    digest.update(b"riftri-compaction-snapshot-v1\0");
    digest.update(marker);
    #[cfg(unix)]
    hash_extended_attributes(path, &mut digest, &mut SnapshotScratch::default())?;
    #[cfg(target_os = "windows")]
    hash_windows_attributes(path, &mut digest)?;
    Ok(crate::base_integrity::hex_lower(digest.finalize()))
}

#[cfg(unix)]
fn hash_entry_xattrs(
    path: &Path,
    digest: &mut Sha256,
    scratch: &mut SnapshotScratch,
) -> Result<(), WorktreeError> {
    use std::os::unix::ffi::OsStrExt;

    scratch.names.clear();
    scratch.names.reserve(64 * 1024);
    rustix::fs::llistxattr(path, rustix::buffer::spare_capacity(&mut scratch.names))
        .map_err(|source| io("list worktree extended attributes", path, source.into()))?;
    let mut names = scratch
        .names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort_unstable();
    digest.update((names.len() as u64).to_le_bytes());
    for name in names {
        scratch.value.clear();
        scratch.value.reserve(256 * 1024);
        rustix::fs::lgetxattr(
            path,
            OsStr::from_bytes(name),
            rustix::buffer::spare_capacity(&mut scratch.value),
        )
        .map_err(|source| io("read worktree extended attribute", path, source.into()))?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name);
        digest.update((scratch.value.len() as u64).to_le_bytes());
        digest.update(&scratch.value);
    }
    Ok(())
}

fn sorted_snapshot_names(entries: Vec<fs::DirEntry>) -> Vec<OsString> {
    // Cache each native name once, not on every sorting comparison. Construct
    // child paths only when visiting them, rather than retaining all full paths.
    let mut names = entries
        .into_iter()
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

// Scratch lives for one metadata walk, never across snapshots. Buffers are
// allocated lazily and overwritten on every read; no filesystem data is cached.
#[derive(Default)]
#[cfg(unix)]
struct SnapshotScratch {
    names: Vec<u8>,
    value: Vec<u8>,
}

#[cfg(unix)]
fn hash_extended_attributes(
    path: &Path,
    digest: &mut Sha256,
    scratch: &mut SnapshotScratch,
) -> Result<(), WorktreeError> {
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "macos")]
    if riftri_storage::has_macos_acl(path)? {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} has a macOS ACL that Git cannot reproduce; compaction preserved it",
            path.display()
        )));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect worktree snapshot entry", path, source))?;
    // Git does not reproduce these bits from its tree. Refuse both new
    // compactions and recovery cleanup instead of silently dropping them.
    // Leave the legacy digest format unchanged for ordinary permissions.
    if !metadata.file_type().is_symlink() && metadata.permissions().mode() & 0o7000 != 0 {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} has special Unix permissions (setuid, setgid, or sticky); compaction preserved it",
            path.display()
        )));
    }

    hash_entry_xattrs(path, digest, scratch)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let entries = fs::read_dir(path)
            .map_err(|source| io("read worktree snapshot directory", path, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io("read worktree snapshot entry", path, source))?;
        for name in sorted_snapshot_names(entries) {
            hash_extended_attributes(&path.join(name), digest, scratch)?;
        }
    }
    Ok(())
}

/// Hash metadata Git does not reproduce from its tree: the full native mode
/// (including setuid, setgid, and sticky bits) and, on Unix, every extended
/// attribute name and value. The persisted snapshot composes the v1 content
/// digest, which deliberately ignores these, so the forced-removal snapshot
/// adds them separately; metadata-only edits after force intent must stop
/// deletion. This layout stays frozen — journals recorded it durably.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn hash_forced_removal_metadata(
    path: &Path,
    digest: &mut Sha256,
    #[cfg(unix)] scratch: &mut SnapshotScratch,
) -> Result<(), WorktreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io("inspect forced removal snapshot metadata", path, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.mode().to_le_bytes());
        hash_entry_xattrs(path, digest, scratch)?;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        digest.update(metadata.file_attributes().to_le_bytes());
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let entries = fs::read_dir(path)
            .map_err(|source| io("read forced removal snapshot directory", path, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io("read forced removal snapshot entry", path, source))?;
        digest.update((entries.len() as u64).to_le_bytes());
        for name in sorted_snapshot_names(entries) {
            #[cfg(unix)]
            {
                let bytes = name.as_bytes();
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
            #[cfg(target_os = "windows")]
            {
                let units = name.encode_wide().collect::<Vec<_>>();
                digest.update((units.len() as u64).to_le_bytes());
                for unit in units {
                    digest.update(unit.to_le_bytes());
                }
            }
            hash_forced_removal_metadata(
                &path.join(name),
                digest,
                #[cfg(unix)]
                scratch,
            )?;
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
        let entries = fs::read_dir(path)
            .map_err(|source| io("read Windows worktree snapshot directory", path, source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| io("read Windows worktree snapshot entry", path, source))?;
        for name in sorted_snapshot_names(entries) {
            hash_windows_attributes(&path.join(name), digest)?;
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
    }) || removals.iter().any(|journal| !journal.phase.is_finished())
        || MoveJournalStore::open(state_directory)
            .load_all()?
            .iter()
            .any(|journal| !journal.phase.is_finished())
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
        return Err(recovery_pending_error(
            "another Riftri lifecycle operation is pending".to_owned(),
            state_directory,
        ));
    }
    let inventory = git.list_worktrees(repository)?;
    let claimed = claimed_destinations(&adds);
    for journal in adds.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active
            && !completed_removals.contains(&journal.operation_id)
    }) {
        if !journal.destination.is_dir()
            || !inventory
                .iter()
                .any(|worktree| paths_match(&worktree.path, &journal.destination))
        {
            // Prune never touches a managed worktree, so it stops here either
            // way. The commonest cause is `rm -rf <worktree>` before
            // `git worktree prune`, and `riftri repair` retires exactly that
            // stale journal — but only when repair's own classification says
            // the worktree vanished, so this asks the same question rather
            // than suggesting a repair that would do nothing (#437).
            return Err(
                match classify_active_destination(&inventory, &claimed, journal)? {
                    ActiveDestinationState::Vanished => recovery_pending_error(
                        format!(
                            "managed worktree {} no longer exists, but its Riftri add is still recorded as active; prune was not run",
                            journal.destination.display()
                        ),
                        state_directory,
                    ),
                    ActiveDestinationState::Relocated(path) => {
                        WorktreeError::InvalidRequest(format!(
                            "managed worktree {} appears to have been moved to {} outside Riftri; prune was not run",
                            journal.destination.display(),
                            path.display()
                        ))
                    }
                    ActiveDestinationState::Registered | ActiveDestinationState::Present => {
                        WorktreeError::InvalidRequest(format!(
                            "managed worktree {} exists but Git no longer lists it as a worktree directory; prune was not run",
                            journal.destination.display()
                        ))
                    }
                },
            );
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

/// Check a removal that never got past its intent. Returns whether the
/// worktree, still registered and in place, changed since then: nothing was
/// removed, so the caller cancels the removal instead of refusing forever.
fn verify_recoverable_removal(
    git: &Git,
    journal: &DecodedRemovalJournal,
    managed: &DecodedJournal,
) -> Result<bool, WorktreeError> {
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
        return Ok(!safe);
    }
    if !registered && destination_exists {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} exists but is not registered by Git; recovery preserved it",
            journal.destination.display()
        )));
    }
    Ok(false)
}

/// Cancel a removal that removed nothing: its native view is back at its
/// path, Git still registers it, and no quarantine remains. Otherwise the
/// pending journal refused every later `repair` and blocked other lifecycle
/// commands on a worktree that was simply never removed. Returns whether the
/// removal was cancelled; OverlayFS views are left for manual attention,
/// because a failed removal may have left them unmounted.
fn cancel_untouched_removal(
    git: &Git,
    store: &RemovalJournalStore,
    record: &mut RemovalJournalRecord,
    repository: &Path,
    managed: &DecodedJournal,
) -> Result<bool, WorktreeError> {
    let destination = &managed.destination;
    if managed.backend == BackendKind::OverlayFs
        || !destination.is_dir()
        || symlink_metadata_if_present(&removal_quarantine_path(
            destination,
            &record.operation_id,
        )?)?
        .is_some()
        || !git
            .list_worktrees(repository)?
            .iter()
            .any(|worktree| paths_match(&worktree.path, destination))
    {
        return Ok(false);
    }
    record.transition(RemoveWorktreePhase::Cancelled)?;
    store.persist(record)?;
    Ok(true)
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
    let mut git_root = managed.destination.clone();
    let content = if managed.backend == BackendKind::OverlayFs {
        // The private .git pointer remains usable even while the merged view
        // is unmounted during removal recovery.
        if let Some(overlayfs) = &managed.overlayfs {
            let upper = overlayfs.layout_root.join("upper");
            if upper
                .join(".git")
                .try_exists()
                .map_err(|source| io("inspect OverlayFS Git pointer", &upper, source))?
            {
                git_root = upper;
            }
        }
        snapshot_overlayfs_private_layer(managed)?
            .ok_or_else(|| {
                WorktreeError::InvalidRequest(format!(
                    "OverlayFS worktree {} has no private-layer snapshot",
                    managed.destination.display()
                ))
            })?
            .into_bytes()
    } else {
        crate::base_integrity::worktree_marker(&managed.destination).map_err(|source| {
            io(
                "snapshot managed worktree before forced removal",
                &managed.destination,
                source,
            )
        })?
    };
    let git_state = Git::default().worktree_removal_state(&git_root)?;
    let mut digest = Sha256::new();
    // Older snapshot formats deliberately cannot authorize deletion after this
    // upgrade: v1 could not prove that the staged index or HEAD was unchanged,
    // and v2 could not prove that special permission bits or extended
    // attributes were unchanged. A pending older snapshot never matches, so
    // recovery preserves the worktree instead of trusting it.
    digest.update(b"riftri-forced-removal-snapshot-v3\0");
    digest.update((content.len() as u64).to_le_bytes());
    digest.update(content);
    if managed.backend != BackendKind::OverlayFs {
        // The OverlayFS private-layer snapshot above already covers native
        // modes and metadata-driven ctime changes; other backends need an
        // explicit metadata pass over the worktree itself.
        hash_forced_removal_metadata(
            &managed.destination,
            &mut digest,
            #[cfg(unix)]
            &mut SnapshotScratch::default(),
        )?;
    }
    digest.update(git_state);
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
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn visit(path: &Path, digest: &mut Sha256, buffer: &mut Vec<u8>) -> Result<(), WorktreeError> {
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
            let entries = fs::read_dir(path)
                .map_err(|source| io("read OverlayFS removal snapshot", path, source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| io("read OverlayFS snapshot entry", path, source))?;
            for name in sorted_snapshot_names(entries) {
                digest.update((name.as_bytes().len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
                visit(&path.join(name), digest, buffer)?;
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
            hash_overlayfs_file_bytes(&mut file, path, digest, buffer)?;
        }
        // Special entries such as kernel whiteouts are represented by metadata;
        // never open a FIFO or device while inspecting the upper layer.
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(root, &mut digest, &mut Vec::new())?;
    Ok(crate::base_integrity::hex_lower(digest.finalize()))
}

// Keep file storage on the heap, outside the recursive directory frames.
#[cfg(any(target_os = "linux", test))]
fn hash_overlayfs_file_bytes(
    file: &mut impl std::io::Read,
    path: &Path,
    digest: &mut Sha256,
    buffer: &mut Vec<u8>,
) -> Result<(), WorktreeError> {
    // Allocate and zero once, lazily at the first file in this snapshot.
    // Later files overwrite only the bytes that are included in the digest.
    buffer.resize(64 * 1024, 0);
    loop {
        let count = file
            .read(buffer)
            .map_err(|source| io("read OverlayFS snapshot file", path, source))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(())
}

#[cfg(test)]
mod overlay_read_buffer_tests {
    use super::*;

    #[test]
    fn overlay_file_hash_matches_bytes_across_long_short_and_empty_reads() {
        let mut scratch = Vec::new();
        let mut actual = Sha256::new();
        let mut expected = Sha256::new();
        let mut allocation = None;
        for bytes in [
            vec![42; 150_000],
            vec![0, 255, 1],
            Vec::new(),
            vec![7; 65_537],
        ] {
            expected.update(&bytes);
            hash_overlayfs_file_bytes(
                &mut std::io::Cursor::new(bytes),
                Path::new("fixture"),
                &mut actual,
                &mut scratch,
            )
            .unwrap();
            let current = (scratch.as_ptr(), scratch.capacity());
            assert_eq!(*allocation.get_or_insert(current), current);
            assert_eq!(scratch.len(), 64 * 1024);
        }
        assert_eq!(actual.finalize(), expected.finalize());
    }

    #[test]
    fn overlay_file_hash_preserves_read_errors() {
        struct FailedReader;
        impl std::io::Read for FailedReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected read failure"))
            }
        }
        let error = hash_overlayfs_file_bytes(
            &mut FailedReader,
            Path::new("fixture"),
            &mut Sha256::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected read failure"));
    }
}

/// Why Git's own non-force removal would refuse `worktree`, if it would.
///
/// Riftri moves a view aside before asking Git to unregister it, and Git skips
/// its submodule check for a missing directory, deleting the worktree's
/// submodule repositories with its administrative directory. So Riftri makes
/// the same refusal itself, as `git worktree remove` does without `--force`.
fn non_force_removal_blocker(
    git: &Git,
    worktree: &Path,
) -> Result<Option<&'static str>, WorktreeError> {
    if !git.worktree_is_clean(worktree)? {
        return Ok(Some(
            "has changes; commit, stash, or remove them before retrying",
        ));
    }
    if git.worktree_has_submodules(worktree)? {
        return Ok(Some(
            "contains submodules, which Git removes only with --force; that also deletes the submodule repositories",
        ));
    }
    Ok(None)
}

fn managed_worktree_is_clean_for_removal(
    git: &Git,
    managed: &DecodedJournal,
    expected_snapshot: Option<&str>,
) -> Result<bool, WorktreeError> {
    if managed.backend != BackendKind::OverlayFs {
        return Ok(non_force_removal_blocker(git, &managed.destination)?.is_none());
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
            OverlayFsMountState::Active => {
                Ok(non_force_removal_blocker(git, &managed.destination)?.is_none())
            }
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

/// Refuse a worktree Git has locked, before any journal records intent.
///
/// Git's own `move` and `remove` refuse a locked worktree unless forced twice,
/// which Riftri's single `--force` never is. Letting Git refuse after intent
/// was recorded left a pending journal that blocked every later command.
fn refuse_locked_worktree(
    worktree: &Path,
    locked_reason: Option<&[u8]>,
    action: &str,
) -> Result<(), WorktreeError> {
    let Some(reason) = locked_reason else {
        return Ok(());
    };
    let reason = if reason.is_empty() {
        String::new()
    } else {
        format!(" (reason: {})", String::from_utf8_lossy(reason))
    };
    Err(WorktreeError::InvalidRequest(format!(
        "worktree {} is locked{reason}; run `git worktree unlock {}` before {action} it",
        worktree.display(),
        worktree.display()
    )))
}

/// The sibling a removal renames its native view to before deleting it. The
/// name is derived from the removal's own operation ID, so recovery can find
/// it from the journal alone and nothing else ever creates it.
fn removal_quarantine_path(
    destination: &Path,
    operation_id: &str,
) -> Result<PathBuf, WorktreeError> {
    destination
        .parent()
        .map(|parent| parent.join(format!(".riftri-remove-{operation_id}")))
        .ok_or_else(|| {
            WorktreeError::InvalidRequest(format!(
                "worktree {} has no parent directory to quarantine it in",
                destination.display()
            ))
        })
}

fn rename_quarantined_view(quarantine: &Path, destination: &Path) -> Result<(), WorktreeError> {
    if symlink_metadata_if_present(destination)?.is_some() {
        return Err(WorktreeError::InvalidRequest(format!(
            "{} reappeared while its view was quarantined at {}; both were preserved",
            destination.display(),
            quarantine.display()
        )));
    }
    fs::rename(quarantine, destination)
        .map_err(|source| io("restore quarantined worktree", destination, source))?;
    sync_parent(destination)
}

/// Remove a native view so that a crash at any point leaves a state recovery
/// can finish, instead of the half-deleted, still-registered tree that Git's
/// own recursive delete leaves when it is killed.
///
/// The view is renamed to its journal-derived quarantine in one atomic step.
/// Git then unregisters the now-missing path through its ordinary missing-path
/// checks, which never delete a destination recreated in the meantime. Only
/// then is the quarantine, no longer a Git worktree, deleted. A failure before
/// Git drops the registration renames the view back untouched.
fn remove_native_worktree(
    git: &Git,
    repository: &Path,
    destination: &Path,
    quarantine: &Path,
    managed: &DecodedJournal,
    force: bool,
    force_snapshot: Option<&str>,
) -> Result<(), WorktreeError> {
    if !destination.exists() {
        return git
            .remove_worktree(repository, destination)
            .map_err(WorktreeError::from);
    }
    let changed_after_force = || {
        WorktreeError::InvalidRequest(format!(
            "worktree {} changed after forced removal intent; it was preserved",
            destination.display()
        ))
    };
    if force {
        #[cfg(test)]
        crate::test_hooks::fire(
            crate::test_hooks::FilesystemRacePoint::ForceRemovalRevalidation,
            destination,
        );
        let expected = force_snapshot.ok_or_else(|| {
            WorktreeError::InvalidRequest("forced removal has no content snapshot".to_owned())
        })?;
        if !managed_worktree_matches_force_snapshot(managed, expected)? {
            return Err(changed_after_force());
        }
    }
    if symlink_metadata_if_present(quarantine)?.is_some() {
        return Err(WorktreeError::InvalidRequest(format!(
            "removal quarantine {} already exists; {} was preserved",
            quarantine.display(),
            destination.display()
        )));
    }
    fs::rename(destination, quarantine)
        .map_err(|source| io("quarantine worktree for removal", quarantine, source))?;
    sync_parent(quarantine)?;
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::RemovalQuarantined,
        quarantine,
    );
    // A write can land between the check above and the rename, and would be
    // deleted with the quarantine, so the view is rechecked where it now
    // lives. A clean removal rejects any change, as Git's own non-force
    // removal would; a forced one rejects anything beyond its snapshot.
    let unregistered = if force {
        let mut quarantined = managed.clone();
        quarantined.destination = quarantine.to_path_buf();
        match force_snapshot
            .map(|expected| managed_worktree_matches_force_snapshot(&quarantined, expected))
        {
            Some(Ok(true)) => Ok(()),
            Some(Ok(false)) | None => Err(changed_after_force()),
            Some(Err(error)) => Err(error),
        }
    } else {
        match non_force_removal_blocker(git, quarantine) {
            Ok(None) => Ok(()),
            Ok(Some(reason)) => Err(WorktreeError::InvalidRequest(format!(
                "worktree {} {reason}",
                destination.display()
            ))),
            Err(error) => Err(error),
        }
    }
    .and_then(|()| {
        git.remove_worktree(repository, destination)
            .map_err(WorktreeError::from)
    });
    if let Err(operation) = unregistered {
        return match rename_quarantined_view(quarantine, destination) {
            Ok(()) => Err(operation),
            Err(rollback) => Err(WorktreeError::OperationAndRollback {
                operation: Box::new(operation),
                rollback: Box::new(rollback),
            }),
        };
    }
    remove_tree_if_present(quarantine)?;
    sync_parent(quarantine)
}

#[allow(clippy::too_many_arguments)]
fn remove_managed_worktree_files(
    git: &Git,
    repository: &Path,
    destination: &Path,
    quarantine: &Path,
    managed: &DecodedJournal,
    expected_snapshot: Option<&str>,
    force: bool,
    force_snapshot: Option<&str>,
) -> Result<(), WorktreeError> {
    #[cfg(not(target_os = "linux"))]
    let _ = expected_snapshot;
    if managed.backend != BackendKind::OverlayFs {
        return remove_native_worktree(
            git,
            repository,
            destination,
            quarantine,
            managed,
            force,
            force_snapshot,
        );
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
                    let current_snapshot = snapshot_managed_worktree_for_force(managed)?;
                    force_snapshot.is_some_and(|expected| current_snapshot == expected)
                } else {
                    non_force_removal_blocker(git, destination)?.is_none()
                };
                if !safe {
                    return Err(changed_rollback_worktree(managed));
                }
                OverlayFsMounter::unmount(&layout, identity)?;
            }
            OverlayFsMountState::Absent => {
                let safe = if force {
                    let current_snapshot = snapshot_managed_worktree_for_force(managed)?;
                    force_snapshot.is_some_and(|expected| current_snapshot == expected)
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
        let unchanged = if force {
            let current = snapshot_managed_worktree_for_force(managed)?;
            force_snapshot.is_some_and(|expected| current == expected)
        } else {
            overlayfs_layer_snapshot(layout.upper())?
                == expected_snapshot.unwrap_or(&local_snapshot)
        };
        if !unchanged {
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
    // `base_path` and `base_staging` share a parent bucket in every journal
    // this Riftri writes: compaction retargets both in one durable write.
    // Earlier versions retargeted only `base_path` across a checkout-profile
    // change, so an Active journal may still carry its staging record in the
    // bucket the add originally built in. Staging no longer exists once a
    // journal is Active, so — exactly like the scratch-parent check the move
    // seam relaxed for Active journals below — recovery only needs it
    // confined to the immutable-base layout; every other phase keeps the
    // strict shared-bucket invariant.
    let base_staging_confined = base_repository == journal.base_staging.parent()
        || (journal.phase == AddWorktreePhase::Active
            && journal.base_staging.parent().and_then(Path::parent) == Some(bases.as_path()));
    if !base_staging_confined
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

/// How a rollback left an add's destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddRollback {
    RolledBack,
    /// Git commands already ran in the complete view (its HEAD or branch
    /// moved), so the worktree was left registered and in place as a plain Git
    /// worktree that Riftri does not manage.
    Released,
}

fn rollback_decoded(
    git: &Git,
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<AddRollback, WorktreeError> {
    let metadata_lock =
        acquire_git_worktree_metadata_lock_for_repository(git, &journal.repository)?;
    // A branch that moved since the add holds someone's commits: never delete
    // it. Refusing the whole rollback instead left the add pending forever,
    // and the only way out was deleting that branch.
    let mut branch_moved = false;
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
                            branch_moved = true;
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

    // Set when the registration proves this operation's own `git worktree add
    // -b` created the branch; see the unlock below.
    let mut branch_created_by_interrupted_add = None;
    let mut released = false;
    let was_registered = registered.is_some();
    if let Some(worktree) = &registered {
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
        let head_moved = worktree.head.as_ref() != Some(expected_worktree_head)
            || worktree.branch != expected_branch
            || worktree.detached != journal.branch.is_none();
        if worktree.bare || head_moved || branch_moved {
            // Someone already used the worktree: a commit, a switch. Once the
            // native view was complete it is an ordinary checkout, so it is
            // released to them as a plain Git worktree. An unfinished view
            // stays for manual attention.
            let materialized = journal.backend != BackendKind::OverlayFs
                && journal.last_forward_phase >= AddWorktreePhase::IndexSynchronized;
            if worktree.bare || !materialized {
                return Err(WorktreeError::InvalidRequest(if branch_moved {
                    format!(
                        "branch {} moved after creation; recovery preserved its worktree",
                        journal
                            .branch
                            .as_deref()
                            .map(OsStr::to_string_lossy)
                            .unwrap_or_default()
                    )
                } else {
                    format!(
                        "worktree HEAD changed after creation; recovery preserved {}",
                        journal.destination.display()
                    )
                }));
            }
            released = true;
        }
    }
    if let Some(worktree) = registered.filter(|_| !released) {
        // `git worktree add` registers a new worktree locked as
        // "initializing" and unlocks it as its very last step. A journal that
        // never reached `git-metadata-created` means this operation's own Git
        // call was interrupted, so a lock on the registration it matched above
        // is that in-progress marker, not a lock someone placed on a finished
        // worktree. Left in place it makes `git worktree remove` refuse on
        // every `repair`, and the state directory can never recover (#444).
        if journal.last_forward_phase < AddWorktreePhase::GitMetadataCreated {
            if worktree.locked_reason.is_some() {
                git.unlock_worktree(&journal.repository, &journal.destination)?;
            }
            // Git refuses `-b` for an existing branch before registering
            // anything, so a matched registration on this journal's branch
            // proves the interrupted call created it.
            if journal.branch_created {
                branch_created_by_interrupted_add = journal.branch.as_ref();
            }
        }
        restore_staged_git_pointer(journal)?;
        let removed = if journal.backend == BackendKind::OverlayFs {
            #[cfg(target_os = "linux")]
            {
                rollback_overlayfs_worktree(git, journal)
            }
            #[cfg(not(target_os = "linux"))]
            return Err(WorktreeError::Unsupported(
                "OverlayFS recovery requires Linux".to_owned(),
            ));
        } else {
            restore_pointer_for_rollback(journal)
                .and_then(|()| remove_registered_worktree_for_rollback(git, journal))
                .and_then(|()| remove_empty_directory_if_present(&journal.destination))
        };
        removed.map_err(|error| changed_add_destination(error, journal, state_directory))?;
    } else if !was_registered
        && journal.destination.exists()
        && !is_empty_real_directory(&journal.destination)?
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "destination {} exists but is not registered by Git; recovery preserved it",
            journal.destination.display()
        )));
    }
    // An unregistered destination that is an empty directory holds nothing to
    // undo. An add may target a pre-existing empty directory (#437); when it
    // fails before Git registers anything, that directory is still the
    // caller's, so it stays. Refusing here instead left the journal pending,
    // and `riftri repair` met the same refusal every time.

    remove_tree_if_present(&journal.scratch)?;
    remove_tree_if_present(&journal.base_staging)?;
    remove_unfinished_base(state_directory, journal)?;
    if let Some(bucket) = journal.base_path.parent() {
        remove_empty_bucket(bucket)?;
    }
    remove_temporary_index(&journal.temporary_index)?;
    remove_file_if_present(&pointer_staging_path(journal))?;

    if released || branch_moved {
        // The branch and the worktree are the user's now.
    } else if journal.branch_created
        && let Some((branch, Some(_))) = expected_branch_target
    {
        git.delete_branch_force(&journal.repository, branch)?;
    } else if let Some(branch) = branch_created_by_interrupted_add {
        let expected = ObjectId::parse(journal.expected_commit.clone())?;
        if git
            .local_branch_target(&journal.repository, branch)?
            .as_ref()
            == Some(&expected)
        {
            git.delete_branch_force(&journal.repository, branch)?;
        }
    }
    drop(metadata_lock);
    Ok(if released {
        AddRollback::Released
    } else {
        AddRollback::RolledBack
    })
}

/// Remove the immutable base a rolled-back add left without its completion
/// marker.
///
/// `prepare_base` renames the staged base into place, digests every file, and
/// only then writes the marker, so a kill during the digest leaves a whole base
/// with no marker. Rollback removed only `base_staging`, and — per D020 —
/// neither repair nor gc deletes an unmarked path on inference, so the base
/// leaked for good (#444). This journal names the base, which is what
/// authorizes removing it, and the removal follows the rule `prepare_base`
/// itself applies before rebuilding, under the same exclusive base lock.
/// Anything else keeps the base: a marker that is present (a cache other adds
/// reuse, or corruption for diagnostics to report), a lock another process
/// holds, or another journal that still claims the base.
fn remove_unfinished_base(
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    let base = &journal.base_path;
    let (Some(parent), Some(tree)) = (base.parent(), base.file_name()) else {
        return Ok(());
    };
    if !base.exists() {
        return Ok(());
    }
    let mut marker_name = tree.to_os_string();
    marker_name.push(".complete");
    let marker = parent.join(marker_name);
    let mut lock_name = tree.to_os_string();
    lock_name.push(".lock");
    let lock_path = parent.join(lock_name);

    let lock = open_coordination_lock(&lock_path, "open immutable-base lock")?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        // A live process holds the base: it owns whatever state it is in.
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            return Ok(());
        }
        Err(source) => return Err(io("lock immutable base", &lock_path, source)),
    }
    validate_coordination_lock(&lock, &lock_path)?;

    if marker
        .try_exists()
        .map_err(|source| io("inspect immutable-base marker", &marker, source))?
    {
        return Ok(());
    }
    if protected_bases(state_directory)?.iter().any(|protection| {
        protection.base_path == *base && protection.operation_id != journal.operation_id
    }) {
        return Ok(());
    }
    remove_tree_if_present(base)
}

fn remove_registered_worktree_for_rollback(
    git: &Git,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    if journal.destination.exists() && git.worktree_index_has_changes(&journal.destination)? {
        return Err(changed_rollback_worktree(journal));
    }
    if !journal.destination.exists() || contains_only_git_pointer(&journal.destination)? {
        remove_pointer_only_worktree(git, &journal.repository, journal)?;
    } else if git.worktree_is_clean(&journal.destination)? {
        remove_worktree_for_rollback(
            git,
            &journal.repository,
            &journal.destination,
            Some(&journal.base_path),
        )?;
    } else if view_matches_base(&journal.base_path, &journal.destination)? {
        if !git
            .initialize_missing_worktree_index(&journal.destination, &journal.sparse_directories)?
        {
            return Err(changed_rollback_worktree(journal));
        }
        if !git.worktree_is_clean(&journal.destination)? {
            return Err(changed_rollback_worktree(journal));
        }
        remove_worktree_for_rollback(
            git,
            &journal.repository,
            &journal.destination,
            Some(&journal.base_path),
        )?;
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
    if let Err(error) = remove_worktree_for_rollback(git, repository, &journal.destination, None) {
        restore_staged_git_pointer(journal)?;
        return Err(error);
    }
    remove_file_if_present(&staged)
}

fn remove_worktree_for_rollback(
    git: &Git,
    repository: &Path,
    destination: &Path,
    expected_base: Option<&Path>,
) -> Result<(), WorktreeError> {
    #[cfg(test)]
    crate::test_hooks::fire(
        crate::test_hooks::FilesystemRacePoint::RollbackGitRemoval,
        destination,
    );
    // Git's ordinary clean status omits ignored files and empty directories.
    // Add rollback is not an explicit request to discard those private entries.
    // Revalidate the complete view at the final removal boundary, not only when
    // a missing index made Git report the initial checkout as dirty.
    if let Some(base) = expected_base
        && !view_matches_base(base, destination)?
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "worktree {} has changes or private entries; recovery preserved it",
            destination.display()
        )));
    }
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
    // Both branches load non-destructively: the boot/namespace/liveness
    // determination below (`mount_state` or `recover_mount`) must run before
    // any destructive reset, because a mount created in another namespace of
    // this boot is invisible here and `load_for_remount` would wipe the work
    // directory of that still-live overlay. The disposable work directory is
    // reset only on the remount path, after the guards preserved-and-reported
    // every live-elsewhere shape.
    let layout = if overlayfs.mount_identity.is_some() {
        OverlayFsMounter::load(
            &overlayfs.layout_root,
            &journal.base_path,
            &journal.destination,
        )?
    } else {
        OverlayFsMounter::load_for_recovery(
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
        context,
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
    // The private pointer also works when the merged view is unmounted.
    let index_root = if layout.upper().join(".git").exists() {
        layout.upper()
    } else {
        &journal.destination
    };
    if index_root.join(".git").exists() && git.worktree_index_has_changes(index_root)? {
        return Err(changed_rollback_worktree(journal));
    }
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
        let safe_to_remove = view_matches_base(&journal.base_path, &journal.destination)?;
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

/// Rolling back an add found its unfinished worktree changed, typically by a
/// write that landed before the add returned. The change is kept, but the
/// bare "has changes" refusal left the add pending with every `repair`
/// failing the same way and nothing saying how to finish, so name the kept
/// worktree and the steps that let repair complete the rollback.
fn changed_add_destination(
    error: WorktreeError,
    journal: &DecodedJournal,
    state_directory: &Path,
) -> WorktreeError {
    if !matches!(
        (&error, changed_rollback_worktree(journal)),
        (WorktreeError::InvalidRequest(found), WorktreeError::InvalidRequest(changed))
            if *found == changed
    ) {
        return error;
    }
    let destination = journal.destination.display();
    recovery_pending_error(
        format!(
            "{destination} was not created as a worktree, but it changed before the add \
             finished, so rollback kept it; copy anything you need out of {destination} \
             and delete it"
        ),
        state_directory,
    )
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

/// Git's lockfile for `path`: the same name with `.lock` appended.
fn git_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// Remove a temporary index and the lockfile Git writes beside it. A Git
/// child killed while writing the index leaves `<index>.lock`; both live in
/// Riftri's own state under this operation's name, so nothing else owns them,
/// and status would otherwise report the lock as unexplained forever.
fn remove_temporary_index(path: &Path) -> Result<(), WorktreeError> {
    remove_file_if_present(path)?;
    remove_file_if_present(&git_lock_path(path))
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
    use crate::BaseCountImpact;
    #[test]
    fn sparse_directory_index_is_lazy_and_bounds_repeated_scans() {
        let paths = (0..100)
            .map(|i| PathBuf::from(format!("pkg-{i:03}/file")))
            .collect::<Vec<_>>();
        let mut index = super::SparseDirectoryIndex::new(&paths);
        assert!(index.contains(Path::new("pkg-000")));
        assert!(index.contains(Path::new("pkg-001")));
        assert!(index.directories.is_none());
        assert_eq!(index.remaining_scan, 97);
        assert!(index.contains(Path::new("pkg-099")));
        assert!(index.directories.is_some());
        assert_eq!(index.remaining_scan, 0);
        assert!(!index.contains(Path::new("pkg")));
        assert!(!index.contains(Path::new("pkg-000/file")));
        let mut single = super::SparseDirectoryIndex::new(&paths);
        assert!(single.contains(Path::new("pkg-099")));
        assert!(single.directories.is_none());
    }

    #[test]
    fn sparse_selection_lookup_matches_linear_reference() {
        let tree = riftri_git::ObjectId::parse("a".repeat(40)).unwrap();
        let mut paths = (0..200)
            .map(|i| PathBuf::from(format!("pkg-{i:03}/src/file")))
            .collect::<Vec<_>>();
        paths.extend([
            PathBuf::from("pkg/file"),
            PathBuf::from("pkg-foo/file"),
            PathBuf::from("é/src/file"),
            PathBuf::from("plain"),
        ]);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            paths.push(PathBuf::from(std::ffi::OsString::from_vec(
                b"native/\xff".to_vec(),
            )));
        }
        for selections in [
            vec![],
            vec!["pkg"],
            vec!["pkg", "é"],
            vec!["plain", "missing"],
            vec!["missing", "plain"],
            vec!["pk", "pkg"],
            vec!["pkg-199", "pkg-000"],
        ] {
            let selections = selections
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let expected = selections.iter().find_map(|directory| {
                let prefix = Path::new(directory);
                if paths.iter().any(|p| p != prefix && p.starts_with(prefix)) {
                    None
                } else {
                    Some((directory, paths.iter().any(|p| p == prefix)))
                }
            });
            let result = super::validate_sparse_directories_in_tree(&selections, &paths, &tree);
            match expected {
                None => result.unwrap(),
                Some((directory, is_file)) => {
                    let message = result.unwrap_err().to_string();
                    assert!(message.contains(&format!("sparse directory {directory} ")));
                    assert!(message.contains(if is_file {
                        "is a file"
                    } else {
                        "does not exist"
                    }));
                }
            }
        }
        let input = [
            "pkg/a",
            "pkg-foo/x",
            "pkg",
            "pkg/a/b",
            "é/z",
            "é",
            "pkg/",
            "pkg.foo/a",
            "pkg.foo",
        ]
        .map(str::to_owned);
        assert_eq!(
            super::canonicalize_sparse_directories(&input).unwrap(),
            ["pkg", "pkg-foo/x", "pkg.foo", "é"]
        );
    }

    #[test]
    #[ignore = "manual sparse selection scaling probe"]
    fn reports_sparse_selection_latency() {
        let tree = riftri_git::ObjectId::parse("a".repeat(40)).unwrap();
        let paths = (0..20_000)
            .map(|i| PathBuf::from(format!("pkg-{i:05}/src/file")))
            .collect::<Vec<_>>();
        let selections = (19_000..20_000)
            .map(|i| format!("pkg-{i:05}"))
            .collect::<Vec<_>>();
        for round in 0..4 {
            for indexed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                if indexed {
                    super::validate_sparse_directories_in_tree(&selections, &paths, &tree).unwrap();
                } else {
                    for directory in &selections {
                        let prefix = Path::new(directory);
                        assert!(paths.iter().any(|p| p != prefix && p.starts_with(prefix)));
                    }
                }
                eprintln!(
                    "round={round} indexed={indexed} microseconds={}",
                    start.elapsed().as_micros()
                );
            }
        }
    }

    #[test]
    fn marker_removed_index_preserves_exact_paths_and_phase_selection() {
        let fixture = tempdir().unwrap();
        let paths = [
            fixture.path().join("a"),
            fixture.path().join("a.complete"),
            fixture.path().join("A"),
        ];
        let phases = [
            GarbageCollectionPhase::IntentRecorded,
            GarbageCollectionPhase::MarkerRemoved,
            GarbageCollectionPhase::BaseQuarantined,
            GarbageCollectionPhase::Complete,
            GarbageCollectionPhase::Cancelled,
        ];
        let mut journals = Vec::new();
        for (i, phase) in phases.into_iter().enumerate() {
            journals.push(super::DecodedCollectionJournal {
                journal_path: fixture.path().join(format!("{i}.json")),
                operation_id: i.to_string(),
                base_path: paths[i % paths.len()].clone(),
                quarantine_path: fixture.path().join(format!("q-{i}")),
                marker_path: fixture.path().join(format!("m-{i}")),
                phase,
            });
        }
        journals.push(journals[1].clone());
        let index = super::marker_removed_bases(&journals);
        assert_eq!(index.len(), 1);
        for path in &paths {
            assert_eq!(
                index.contains(path.as_path()),
                journals
                    .iter()
                    .any(|j| j.phase == GarbageCollectionPhase::MarkerRemoved
                        && j.base_path == *path)
            );
        }
        assert!(!index.contains(fixture.path().join("missing").as_path()));
        assert!(super::marker_removed_bases(&[]).is_empty());
    }

    #[test]
    fn marker_removed_claim_still_requires_a_real_directory() {
        let fixture = tempdir().unwrap();
        let bucket = fixture.path().join("bases/v1/bucket");
        fs::create_dir_all(&bucket).unwrap();
        let base = bucket.join("a".repeat(40));
        fs::write(&base, b"preserve").unwrap();
        let mut journal = super::DecodedCollectionJournal {
            journal_path: fixture.path().join("collections/gc.json"),
            operation_id: "gc".into(),
            base_path: base.clone(),
            quarantine_path: bucket.join(".riftri-gc-gc"),
            marker_path: base.with_extension("complete"),
            phase: GarbageCollectionPhase::MarkerRemoved,
        };
        let inspect = |journal| {
            let mut issues = Vec::new();
            super::diagnose_base_directories(
                fixture.path(),
                &[journal],
                &[],
                &[],
                &mut issues,
                &mut 0,
            )
            .unwrap();
            issues
        };
        let issues = inspect(journal.clone());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].reason.contains("must be a real directory"));
        assert_eq!(fs::read(&base).unwrap(), b"preserve");
        fs::remove_file(&base).unwrap();
        fs::create_dir(&base).unwrap();
        assert!(inspect(journal.clone()).is_empty());
        journal.phase = GarbageCollectionPhase::Complete;
        assert_eq!(
            inspect(journal).len(),
            1,
            "completed collection must not explain an unmarked base"
        );
    }

    #[test]
    #[ignore = "paired diagnostic lookup probe; includes index construction"]
    fn reports_marker_removed_lookup_latency() {
        let fixture = tempdir().unwrap();
        let journals = (0..4000)
            .map(|index| {
                let base_path = fixture.path().join(format!("{index:040x}"));
                super::DecodedCollectionJournal {
                    journal_path: fixture.path().join(format!("{index}.json")),
                    operation_id: index.to_string(),
                    marker_path: base_path.with_extension("complete"),
                    base_path,
                    quarantine_path: fixture.path().join(format!("q-{index}")),
                    phase: if index % 11 == 0 {
                        GarbageCollectionPhase::MarkerRemoved
                    } else {
                        GarbageCollectionPhase::Complete
                    },
                }
            })
            .collect::<Vec<_>>();
        let paths = (0..8000)
            .map(|index| fixture.path().join(format!("{index:040x}")))
            .collect::<Vec<_>>();
        let expected = paths
            .iter()
            .map(|path| {
                journals.iter().any(|j| {
                    j.phase == GarbageCollectionPhase::MarkerRemoved && j.base_path == *path
                })
            })
            .collect::<Vec<_>>();
        for round in 0..4 {
            for indexed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                let result = if indexed {
                    let index = super::marker_removed_bases(&journals);
                    paths
                        .iter()
                        .map(|path| index.contains(path.as_path()))
                        .collect::<Vec<_>>()
                } else {
                    paths
                        .iter()
                        .map(|path| {
                            journals.iter().any(|j| {
                                j.phase == GarbageCollectionPhase::MarkerRemoved
                                    && j.base_path == *path
                            })
                        })
                        .collect::<Vec<_>>()
                };
                let elapsed = start.elapsed();
                assert_eq!(std::hint::black_box(result), expected);
                println!(
                    "base-claims round={round} indexed={indexed} elapsed_us={}",
                    elapsed.as_micros()
                );
            }
        }
    }

    #[test]
    fn accounting_workers_take_later_work_while_the_first_tree_is_blocked() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex};
        let requests = (0..8)
            .map(|index| super::TreeUsageRequest {
                path: std::path::PathBuf::from(index.to_string()),
                allocated_path: None,
            })
            .collect::<Vec<_>>();
        let ready = (Mutex::new(false), Condvar::new());
        let calls = (0..8).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();
        let result = super::scheduled_tree_usages(&requests, 2, &|request| {
            let index = request.path.to_str().unwrap().parse::<usize>().unwrap();
            calls[index].fetch_add(1, Ordering::Relaxed);
            if index == 0 {
                let (guard, _) = ready
                    .1
                    .wait_timeout_while(
                        ready.0.lock().unwrap(),
                        std::time::Duration::from_secs(5),
                        |ready| !*ready,
                    )
                    .unwrap();
                if !*guard {
                    return Err(super::WorktreeError::InvalidRequest(
                        "later work stayed pinned behind the first tree".into(),
                    ));
                }
            } else if index == 1 {
                *ready.0.lock().unwrap() = true;
                ready.1.notify_all();
            }
            Ok((index as u64, 100 + index as u64))
        })
        .unwrap();
        assert_eq!(result, (0..8).map(|i| (i, 100 + i)).collect::<Vec<_>>());
        assert!(calls.iter().all(|count| count.load(Ordering::Relaxed) == 1));
    }

    #[test]
    fn accounting_worker_errors_stay_in_request_order() {
        let requests = (0..8)
            .map(|index| super::TreeUsageRequest {
                path: std::path::PathBuf::from(index.to_string()),
                allocated_path: None,
            })
            .collect::<Vec<_>>();
        let error = super::scheduled_tree_usages(&requests, 2, &|request| {
            let index = request.path.to_str().unwrap().parse::<u64>().unwrap();
            if index == 1 || index == 5 {
                Err(super::WorktreeError::InvalidRequest(format!(
                    "error-{index}"
                )))
            } else {
                Ok((index, index))
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("error-1"));
    }

    #[test]
    fn journal_relationship_index_preserves_order_and_duplicates() {
        let journals = vec![("b", 1), ("a", 2), ("b", 3), ("", 4)];
        let visits = std::cell::Cell::new(0);
        let indexed = super::index_journals_by(&journals, |item| {
            visits.set(visits.get() + 1);
            item.0
        });
        assert_eq!(visits.get(), journals.len());
        for key in ["a", "b", "", "missing"] {
            let expected = journals
                .iter()
                .filter(|item| item.0 == key)
                .collect::<Vec<_>>();
            assert_eq!(
                indexed.get(key).map(Vec::as_slice).unwrap_or_default(),
                expected
            );
        }
    }

    #[test]
    #[ignore = "manual journal relationship benchmark; includes index construction"]
    fn reports_journal_relationship_index_latency() {
        let journals = (0..10_000)
            .map(|index| (format!("operation-{}", index / 3), index))
            .collect::<Vec<_>>();
        let keys = journals
            .iter()
            .step_by(3)
            .map(|item| item.0.as_str())
            .collect::<Vec<_>>();
        for round in 0..4 {
            for indexed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let started = std::time::Instant::now();
                let groups = if indexed {
                    let index = super::index_journals_by(&journals, |item| item.0.as_str());
                    keys.iter()
                        .map(|key| index[*key].iter().map(|item| item.1).collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                } else {
                    keys.iter()
                        .map(|key| {
                            journals
                                .iter()
                                .filter(|item| item.0 == *key)
                                .map(|item| item.1)
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                };
                let elapsed = started.elapsed();
                assert_eq!(
                    groups.into_iter().flatten().collect::<Vec<_>>(),
                    (0..10_000).collect::<Vec<_>>()
                );
                println!(
                    "journal-index round={round} indexed={indexed} elapsed_us={}",
                    elapsed.as_micros()
                );
            }
        }
    }
    #[test]
    fn inventory_keys_preserve_path_matching() {
        let mut paths = vec![
            std::path::PathBuf::from("/work/plain"),
            std::path::PathBuf::from("/work/caf\u{00e9}"),
            std::path::PathBuf::from("/work/cafe\u{0301}"),
            std::path::PathBuf::from("/work/./caf\u{00e9}"),
            std::path::PathBuf::from("/WORK/PLAIN"),
            std::path::PathBuf::from(r"C:\work\plain"),
            std::path::PathBuf::from(r"\\?\C:\work\plain"),
        ];
        #[cfg(unix)]
        paths.push(std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/work/\xff".to_vec(),
        )));
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            paths.push(PathBuf::from(OsString::from_wide(&[0xd800])));
        }
        for left in &paths {
            for right in &paths {
                let aliases = matches!((super::inventory_path_key(left), super::inventory_path_key(right)), (Some(left), Some(right)) if left == right);
                assert_eq!(
                    (!cfg!(target_os = "windows") && left == right) || aliases,
                    super::paths_match(left, right),
                    "{left:?} {right:?}"
                );
            }
        }
        let entries = paths
            .iter()
            .chain(paths.iter())
            .map(|path| inventory_entry(path.clone()))
            .collect::<Vec<_>>();
        let inventory = super::WorktreeInventory::new(entries.clone());
        for path in &paths {
            let expected = entries
                .iter()
                .find(|entry| super::paths_match(&entry.path, path));
            assert_eq!(inventory.find(path), expected);
        }
        assert!(inventory.find(Path::new("/not-registered")).is_none());
    }

    fn inventory_entry(path: PathBuf) -> riftri_git::WorktreeInfo {
        riftri_git::WorktreeInfo {
            path,
            head: None,
            branch: None,
            detached: true,
            bare: false,
            head_unresolvable: false,
            locked_reason: None,
            prunable_reason: None,
        }
    }

    #[test]
    #[ignore = "manual paired inventory lookup benchmark; includes index construction"]
    fn reports_inventory_lookup_latency() {
        let entries = (0..1000)
            .map(|index| inventory_entry(PathBuf::from(format!("/work/view-{index:04}"))))
            .collect::<Vec<_>>();
        for round in 0..4 {
            for indexed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let started = std::time::Instant::now();
                if indexed {
                    let inventory = super::WorktreeInventory::new(entries.clone());
                    for entry in &entries {
                        assert_eq!(
                            std::hint::black_box(inventory.find(&entry.path)),
                            Some(entry)
                        );
                    }
                } else {
                    for entry in &entries {
                        assert_eq!(
                            std::hint::black_box(entries.iter().find(|candidate| {
                                super::paths_match(&candidate.path, &entry.path)
                            })),
                            Some(entry)
                        );
                    }
                }
                println!(
                    "inventory round={round} indexed={indexed} elapsed_us={}",
                    started.elapsed().as_micros()
                );
            }
        }
    }
    use std::collections::HashSet;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::Git;
    use super::{
        AddWorktreeRequest, BackendKind, BaseStorageAccounting, CompactWorktreeRequest,
        MoveWorktreeRequest, ObjectId, PruneWorktreesRequest, RemoveWorktreeRequest,
        TreeUsageRequest, ViewStorageAccounting, WorktreeMode, add_worktree_inner,
        classify_in_tree_attributes, compact_worktree_inner, cow_aware_allocated_total,
        force_remove_worktree_inner, garbage_collect_inner, has_ascii_case_alias,
        move_worktree_inner, next_operation_id, prune_worktrees_inner,
        recover_incomplete_operations, remove_empty_directory_if_present, remove_worktree_inner,
        storage_accounting, tree_usage, tree_usages,
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
    fn cow_aware_total_counts_shared_base_blocks_once() {
        // A null (all-zero) object ID is a valid 40-char hex string.
        let head = ObjectId::parse("0".repeat(40)).expect("null object id");
        let base_path = PathBuf::from("/state/bases/base-a");
        let base_allocated = 4_096_000_u64;
        let bases = vec![BaseStorageAccounting {
            path: base_path.clone(),
            reference_count: 3,
            logical_bytes: base_allocated,
            allocated_bytes: base_allocated,
            damaged: false,
        }];
        let fresh_view = |destination: &str| ViewStorageAccounting {
            repository: PathBuf::from("/repo"),
            destination: PathBuf::from(destination),
            base_path: base_path.clone(),
            backend: BackendKind::ApfsClone,
            head: head.clone(),
            branch: None,
            detached: false,
            locked_reason: None,
            prunable_reason: None,
            // A freshly cloned CoW view shares every base block, so its
            // per-tree allocation equals the base's allocation.
            logical_bytes: base_allocated,
            allocated_bytes: base_allocated,
        };

        // One base plus three fresh views that share all of the base's blocks.
        // The naive per-tree sum would report 4 * base_allocated; the CoW-aware
        // total must report ~base_allocated because nothing new was written.
        let views = vec![
            fresh_view("/views/one"),
            fresh_view("/views/two"),
            fresh_view("/views/three"),
        ];
        assert_eq!(
            cow_aware_allocated_total(&bases, &views),
            base_allocated,
            "shared base blocks must be counted once, not once per view"
        );

        // A diverged view that wrote extra private blocks adds only its delta.
        let divergence = 512_000_u64;
        let mut diverged = fresh_view("/views/diverged");
        diverged.allocated_bytes = base_allocated + divergence;
        let views = vec![fresh_view("/views/one"), diverged];
        assert_eq!(
            cow_aware_allocated_total(&bases, &views),
            base_allocated + divergence,
            "a diverged view must add exactly its allocation above the base"
        );

        // An unresolvable base (view.base_path matches no base) falls back to
        // counting the view's full allocation rather than dropping it.
        let mut orphan = fresh_view("/views/orphan");
        orphan.base_path = PathBuf::from("/state/bases/missing");
        orphan.allocated_bytes = 777_000;
        assert_eq!(
            cow_aware_allocated_total(&bases, std::slice::from_ref(&orphan)),
            base_allocated + 777_000,
            "a view whose base is missing must count its full allocation"
        );
    }

    #[test]
    fn parallel_tree_usage_preserves_order_and_separate_allocation_paths() {
        let fixture = tempdir().expect("tree-usage fixture");
        let private = fixture.path().join("private");
        fs::create_dir(&private).expect("private allocation tree");
        fs::write(private.join("upper"), vec![b'c'; 12_289]).expect("private file");

        let private_usage = tree_usage(&private).expect("measure private tree");
        let mut expected = Vec::new();
        let mut requests = Vec::new();
        for index in 0..7 {
            let path = fixture.path().join(format!("tree-{index}"));
            fs::create_dir(&path).expect("usage tree");
            fs::write(
                path.join("file"),
                vec![b'a' + index as u8; 4_097 + index * 1_024],
            )
            .expect("usage file");
            let mut usage = tree_usage(&path).expect("measure expected tree usage");
            let allocated_path = (index == 3).then(|| private.clone());
            if allocated_path.is_some() {
                usage.1 = private_usage.1;
            }
            expected.push(usage);
            requests.push(TreeUsageRequest {
                path,
                allocated_path,
            });
        }

        assert_eq!(tree_usages(&requests).expect("measure trees"), expected);
    }

    #[test]
    fn cow_aware_total_counts_overlayfs_private_layers_whole() {
        // A null (all-zero) object ID is a valid 40-char hex string.
        let head = ObjectId::parse("0".repeat(40)).expect("null object id");
        let base_path = PathBuf::from("/state/bases/base-a");
        let base_allocated = 4_096_000_u64;
        let bases = vec![BaseStorageAccounting {
            path: base_path.clone(),
            reference_count: 1,
            logical_bytes: base_allocated,
            allocated_bytes: base_allocated,
            damaged: false,
        }];
        // Unlike a CoW clone, an OverlayFS view's `allocated_bytes` measures
        // only its private upper/work layers, so it is already base-exclusive.
        // Here that private cost is much smaller than the base allocation, so
        // the old blanket `view - base` subtraction would saturate it to zero
        // and silently drop the view's real write cost from the total.
        let private_layers = 512_000_u64;
        let overlay_view = ViewStorageAccounting {
            repository: PathBuf::from("/repo"),
            destination: PathBuf::from("/views/overlay"),
            base_path: base_path.clone(),
            backend: BackendKind::OverlayFs,
            head: head.clone(),
            branch: None,
            detached: false,
            locked_reason: None,
            prunable_reason: None,
            logical_bytes: private_layers,
            allocated_bytes: private_layers,
        };
        assert_eq!(
            cow_aware_allocated_total(&bases, std::slice::from_ref(&overlay_view)),
            base_allocated + private_layers,
            "an OverlayFS view must add its full private allocation, not saturate to zero"
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

    /// `remove`, `move`, and `compact` must all claim the same per-worktree
    /// operation lock. Before they did, a `remove` could slip between a
    /// competing operation's pending-journal check and the moment that
    /// operation published its own journal, leaving behind a journal shape no
    /// `resume_*` branch can classify — which permanently blocks `repair`,
    /// `prune`, and base reclamation. Holding the lock must make every other
    /// lifecycle operation fail fast and cleanly instead.
    #[test]
    fn lifecycle_operations_refuse_a_worktree_locked_by_another_operation() {
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/lock-exclusion")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");

        // Journals record canonical paths, so resolve both before looking the
        // managed worktree up (on macOS the fixture lives under /private/tmp).
        let canonical_state = fs::canonicalize(&state).expect("canonical state directory");
        let canonical_destination = fs::canonicalize(&destination).expect("canonical destination");
        let managed = super::find_managed_add_journal(&canonical_state, &canonical_destination)
            .expect("load managed journal")
            .expect("worktree is managed");

        {
            // Stand in for a competing lifecycle operation that is in flight
            // but has not published its journal yet.
            let _held = super::try_lock_add_operation(&managed.journal_path)
                .expect("take operation lock")
                .expect("lock is available");

            let removal = remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .expect_err("remove must refuse a locked worktree");
            assert!(
                matches!(removal, super::WorktreeError::Busy { .. }),
                "expected Busy, got {removal:?}"
            );

            let moved = move_worktree_inner(
                MoveWorktreeRequest {
                    repository: repository.clone(),
                    source: destination.clone(),
                    destination: fixture.path().join("moved"),
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .expect_err("move must refuse a locked worktree");
            assert!(
                matches!(moved, super::WorktreeError::Busy { .. }),
                "expected Busy, got {moved:?}"
            );

            assert!(
                destination.is_dir(),
                "a refused operation must leave the worktree untouched"
            );
        }

        // Once the competing operation releases the worktree, the same request
        // succeeds: the lock refuses, it does not wedge.
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove succeeds after the lock is released");
        assert!(!destination.exists());
        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
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
                    sparse_directories: Vec::new(),
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

            move_worktree_inner(
                MoveWorktreeRequest {
                    repository,
                    source: destination,
                    destination: fixture.path().join("moved"),
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .expect("move recovered worktree");
            let moved = storage_accounting(&state).expect("account after move");
            assert_eq!(
                moved.completed_compactions,
                accounting.completed_compactions
            );
            assert_eq!(
                moved.cancelled_compactions,
                accounting.cancelled_compactions
            );
            assert!(moved.diagnostic_issues.is_empty(), "{phase:?}: {moved:?}");
        }
    }

    /// A compaction of a fresh managed worktree, stopped after `phase` as a
    /// kill would leave it.
    fn stopped_compaction(
        phase: CompactWorktreePhase,
    ) -> (crate::test_support::WritableTempDir, PathBuf, PathBuf) {
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
        fs::create_dir(repository.join("nested")).expect("create directory");
        fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
        fs::write(repository.join("nested/deep.txt"), "deep\n").expect("write file");
        git(&repository, &["add", "."]);
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
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");
        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: caller,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(phase),
        )
        .expect_err("stop the compaction after the requested phase");
        (fixture, destination, state)
    }

    /// A compaction stopped at `add-journal-updated`: the replacement is
    /// active and only the verified quarantine of the old view remains.
    fn compaction_awaiting_quarantine_cleanup() -> (
        crate::test_support::WritableTempDir,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let (fixture, destination, state) =
            stopped_compaction(CompactWorktreePhase::AddJournalUpdated);
        let compactions = crate::journal::CompactJournalStore::open(&state)
            .load_all()
            .expect("load compaction");
        assert_eq!(compactions.len(), 1);
        let quarantine = compactions[0].quarantine.clone();
        assert!(quarantine.is_dir(), "the old view waits in its quarantine");
        (fixture, destination, state, quarantine)
    }

    fn compaction_drop_path(quarantine: &Path) -> PathBuf {
        let name = quarantine.file_name().unwrap().to_str().unwrap();
        quarantine.with_file_name(name.replace(".riftri-compact-old-", ".riftri-compact-drop-"))
    }

    #[test]
    fn repair_finishes_a_compaction_killed_while_deleting_its_old_view() {
        let (_fixture, destination, state, quarantine) = compaction_awaiting_quarantine_cleanup();
        // The kill lands mid-way through deleting the verified old view.
        let dropped = compaction_drop_path(&quarantine);
        fs::rename(&quarantine, &dropped).expect("hand the old view to cleanup");
        fs::remove_file(dropped.join("tracked.txt")).expect("partial delete");

        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(!dropped.exists(), "cleanup must finish");
        assert!(!quarantine.exists());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).unwrap(),
            "base\n"
        );
        let accounting = storage_accounting(&state).expect("account after repair");
        assert_eq!(accounting.pending_compactions, 0);
        assert_eq!(accounting.completed_compactions, 1);
    }

    fn git_output(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A Git command run in the worktree just after the swap acts on the old
    /// view, its working directory, while HEAD and the index move with it.
    /// The compaction used to keep that view aside and leave the replacement
    /// on the pre-command files, so `git status` showed changes undoing the
    /// command. It now swaps the views back and cancels the compaction.
    #[test]
    fn a_git_command_in_the_old_view_undoes_the_compaction() {
        for command in ["commit", "switch", "commit-then-killed-mid-restore"] {
            let (fixture, destination, state) =
                stopped_compaction(CompactWorktreePhase::ReplacementActivated);
            let repository = fixture.path().join("repository");
            let compactions = crate::journal::CompactJournalStore::open(&state);
            let journal = compactions.load_all().expect("load").remove(0);
            let old_base = journal.old_base_path.clone();
            let old_view = journal.quarantine.clone();
            let head = if command == "switch" {
                fs::write(repository.join("other.txt"), "other\n").unwrap();
                git(&repository, &["add", "other.txt"]);
                git(&repository, &["commit", "--quiet", "-m", "other"]);
                let other = git_output(&repository, &["rev-parse", "HEAD"]);
                git(&old_view, &["switch", "--quiet", "--detach", &other]);
                other
            } else {
                fs::write(old_view.join("new.txt"), "new\n").unwrap();
                git(&old_view, &["add", "new.txt"]);
                git(&old_view, &["commit", "--quiet", "-m", "during compaction"]);
                git_output(&old_view, &["rev-parse", "HEAD"])
            };
            if command == "commit-then-killed-mid-restore" {
                // Killed after the compacted view was set aside, before Git's
                // view was renamed back.
                let store =
                    crate::journal::CompactJournalStore::open(&fs::canonicalize(&state).unwrap());
                let decoded = store.load_all().unwrap().remove(0);
                let mut record = store.reload(&decoded).unwrap();
                for phase in [
                    CompactWorktreePhase::AddJournalUpdated,
                    CompactWorktreePhase::RestoringOriginal,
                ] {
                    if phase == CompactWorktreePhase::AddJournalUpdated {
                        // Repair would retarget the add journal first.
                        let add_store = JournalStore::open(&fs::canonicalize(&state).unwrap());
                        let managed = add_store.load_all().unwrap().remove(0);
                        add_store
                            .update_active_base(
                                &managed.journal_path,
                                &decoded.destination,
                                &decoded.old_base_path,
                                &decoded.base_path,
                                &decoded.expected_commit,
                            )
                            .unwrap();
                    }
                    record.transition(phase).unwrap();
                    store.persist(&record).unwrap();
                }
                fs::rename(&destination, &decoded.replacement).expect("set aside");
            }

            let report = recover_incomplete_operations(&state).expect("repair");

            assert!(report.errors.is_empty(), "{command}: {report:?}");
            assert_eq!(
                git_output(&destination, &["rev-parse", "HEAD"]),
                head,
                "{command}"
            );
            assert_eq!(
                git_output(&destination, &["status", "--porcelain", "--ignored"]),
                "",
                "{command}: the worktree matches what Git left"
            );
            assert!(!old_view.exists(), "{command}");
            assert!(!journal.replacement.exists(), "{command}");
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(
                accounting.pending_compactions, 0,
                "{command}: {accounting:?}"
            );
            assert_eq!(
                accounting.cancelled_compactions, 1,
                "{command}: {accounting:?}"
            );
            assert!(
                accounting.diagnostic_issues.is_empty(),
                "{command}: {accounting:?}"
            );
            let managed = JournalStore::open(&state).load_all().unwrap().remove(0);
            assert_eq!(
                managed.base_path, old_base,
                "{command}: the add journal is restored"
            );
        }
    }

    /// A Git command running in the worktree holds its index lock. The
    /// post-swap stat refresh then failed and left the compaction pending,
    /// although the refresh is only a cache Git rebuilds itself.
    #[test]
    fn compaction_finishes_while_another_git_command_holds_the_index() {
        let (_fixture, destination, state) =
            stopped_compaction(CompactWorktreePhase::ReplacementActivated);
        let lock = PathBuf::from(git_output(
            &destination,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "index.lock",
            ],
        ));
        fs::write(&lock, b"").expect("hold the index lock");

        let report = recover_incomplete_operations(&state).expect("repair");

        assert!(report.errors.is_empty(), "{report:?}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
        assert_eq!(accounting.completed_compactions, 1, "{accounting:?}");
        assert!(lock.exists(), "another command's lock is never touched");
        fs::remove_file(&lock).unwrap();
        assert_eq!(git_output(&destination, &["status", "--porcelain"]), "");
    }

    #[test]
    fn a_path_vanishing_mid_walk_is_a_change_not_an_io_failure() {
        let destination = Path::new("/worktree");
        let vanished = super::vanished_during_compaction(
            super::io(
                "snapshot managed worktree",
                destination,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ),
            destination,
        );
        assert!(
            matches!(&vanished, super::WorktreeError::InvalidRequest(message)
                if message.contains("changed while it was being compacted")),
            "{vanished}"
        );
        // A file written while it was hashed.
        let rewritten = super::vanished_during_compaction(
            super::io(
                "snapshot managed worktree",
                destination,
                std::io::Error::other(crate::base_integrity::ChangedWhileReading),
            ),
            destination,
        );
        assert!(
            matches!(rewritten, super::WorktreeError::InvalidRequest(_)),
            "{rewritten}"
        );
        // The macOS ACL walker reports through a storage error.
        let storage = super::vanished_during_compaction(
            super::WorktreeError::Storage(riftri_storage::StorageError::Io {
                operation: "inspect macOS ACL",
                path: destination.join("gone.txt"),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
            destination,
        );
        assert!(
            matches!(storage, super::WorktreeError::InvalidRequest(_)),
            "{storage}"
        );
        let denied = super::vanished_during_compaction(
            super::io(
                "snapshot managed worktree",
                destination,
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ),
            destination,
        );
        assert!(
            matches!(denied, super::WorktreeError::Io { .. }),
            "{denied}"
        );
    }

    /// The swap-back needs the replacement untouched. When both views
    /// changed, neither may replace the other.
    #[test]
    fn a_git_command_in_the_old_view_keeps_both_when_the_replacement_changed() {
        let (_fixture, destination, state) =
            stopped_compaction(CompactWorktreePhase::ReplacementActivated);
        let old_view = crate::journal::CompactJournalStore::open(&state)
            .load_all()
            .unwrap()
            .remove(0)
            .quarantine;
        fs::write(old_view.join("new.txt"), "new\n").unwrap();
        git(&old_view, &["add", "new.txt"]);
        git(&old_view, &["commit", "--quiet", "-m", "during compaction"]);
        fs::write(destination.join("mine.txt"), "mine\n").unwrap();

        for _ in 0..2 {
            let report = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(report.errors.len(), 1, "{report:?}");
            assert_eq!(fs::read(old_view.join("new.txt")).unwrap(), b"new\n");
            assert_eq!(fs::read(destination.join("mine.txt")).unwrap(), b"mine\n");
        }
    }

    /// A write that reached the old view after it was verified is kept, but
    /// the error named only the kept path. Repair failed the same way forever
    /// and remove refused until it passed, with nothing saying what to do.
    #[test]
    fn a_late_write_to_the_old_view_is_kept_with_steps_to_finish() {
        let (_fixture, destination, state, quarantine) = compaction_awaiting_quarantine_cleanup();
        fs::write(quarantine.join("late.txt"), "late\n").expect("write after verification");

        let store = crate::journal::CompactJournalStore::open(
            &fs::canonicalize(&state).expect("resolve state directory"),
        );
        let journal = store.load_all().expect("load compaction").remove(0);
        let error = super::resume_compaction(&Git::default(), &store, journal, None)
            .expect_err("the changed old view must be kept");
        let message = error.to_string();
        assert!(
            matches!(error, super::WorktreeError::RecoveryPending { .. }),
            "the compacted view is already active, so this is not a refusal: {message}"
        );
        assert!(
            message.contains(&quarantine.display().to_string()),
            "{message}"
        );
        assert!(message.contains("copy anything you need"), "{message}");
        assert!(message.contains("riftri repair"), "{message}");
        assert_eq!(fs::read(quarantine.join("late.txt")).unwrap(), b"late\n");

        // Following the steps finishes the compaction.
        fs::copy(quarantine.join("late.txt"), destination.join("late.txt")).expect("copy back");
        fs::remove_dir_all(&quarantine).expect("delete the kept old view");
        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
        assert_eq!(accounting.completed_compactions, 1, "{accounting:?}");
        assert_eq!(fs::read(destination.join("late.txt")).unwrap(), b"late\n");
    }

    /// The worktree changed after its replacement was built but before the
    /// swap. Nothing had touched it, yet the compaction stayed pending forever:
    /// repair re-ran the clean check, and remove and compact refused.
    #[test]
    fn repair_cancels_a_compaction_whose_worktree_changed_before_the_swap() {
        let (_fixture, destination, state) =
            stopped_compaction(CompactWorktreePhase::ReplacementReady);
        let replacement = crate::journal::CompactJournalStore::open(&state)
            .load_all()
            .expect("load compaction")
            .remove(0)
            .replacement;
        assert!(
            replacement.is_dir(),
            "the replacement waits beside the view"
        );
        fs::write(destination.join("late.txt"), "late\n").expect("write before the swap");

        let report = recover_incomplete_operations(&state).expect("repair");

        assert!(report.errors.is_empty(), "{report:?}");
        assert!(!replacement.exists(), "only the replacement is removed");
        assert_eq!(fs::read(destination.join("late.txt")).unwrap(), b"late\n");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
        assert_eq!(accounting.cancelled_compactions, 1, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    #[test]
    fn repair_preserves_both_old_view_paths_when_they_coexist() {
        let (_fixture, _destination, state, quarantine) = compaction_awaiting_quarantine_cleanup();
        let dropped = compaction_drop_path(&quarantine);
        fs::create_dir(&dropped).expect("create an unexpected cleanup directory");
        fs::write(dropped.join("keep.txt"), "keep\n").expect("write content");

        for _ in 0..2 {
            let report = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(report.errors.len(), 1, "{report:?}");
            assert_eq!(fs::read(dropped.join("keep.txt")).unwrap(), b"keep\n");
            assert_eq!(fs::read(quarantine.join("tracked.txt")).unwrap(), b"base\n");
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
    fn snapshot_entry_order_matches_legacy_sorting() {
        let fixture = tempdir().unwrap();
        for name in ["z", "alpha", "unicode-\u{e9}", "space name", "10", "2"] {
            fs::write(fixture.path().join(name), b"file").unwrap();
        }
        // APFS rejects invalid UTF-8 filenames; Linux fixtures cover native bytes.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            fs::write(
                fixture
                    .path()
                    .join(OsString::from_vec(b"native-\xff".to_vec())),
                b"file",
            )
            .unwrap();
        }
        let read = || {
            fs::read_dir(fixture.path())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let mut legacy = read();
        legacy.sort_unstable_by_key(|entry| entry.file_name());
        let expected = legacy
            .into_iter()
            .map(|entry| (entry.file_name(), entry.path()))
            .collect::<Vec<_>>();
        assert_eq!(
            super::sorted_snapshot_names(read())
                .into_iter()
                .map(|name| {
                    let path = fixture.path().join(&name);
                    (name, path)
                })
                .collect::<Vec<_>>(),
            expected
        );
        assert!(super::sorted_snapshot_names(Vec::new()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_xattrs_match_legacy_layout_across_different_entries() {
        use sha2::{Digest, Sha256};
        use std::os::unix::ffi::OsStrExt;
        let fixture = tempdir().unwrap();
        #[cfg(target_os = "macos")]
        let prefix = "com.riftri";
        #[cfg(not(target_os = "macos"))]
        let prefix = "user.riftri";
        let mut expected = Sha256::new();
        let mut actual = Sha256::new();
        let mut scratch = super::SnapshotScratch::default();
        let mut allocations = None;
        for (index, values) in [vec![vec![7; 4096], vec![]], vec![vec![0, 255]], vec![]]
            .into_iter()
            .enumerate()
        {
            let path = fixture.path().join(index.to_string());
            fs::write(&path, b"file").unwrap();
            for (index, value) in values.iter().enumerate() {
                rustix::fs::setxattr(
                    &path,
                    format!("{prefix}.{index}").as_str(),
                    value,
                    rustix::fs::XattrFlags::empty(),
                )
                .unwrap();
            }
            // Independent legacy algorithm: fresh buffers and owned names.
            let mut buffer = Vec::with_capacity(64 * 1024);
            rustix::fs::llistxattr(&path, rustix::buffer::spare_capacity(&mut buffer)).unwrap();
            let mut names = buffer
                .split(|b| *b == 0)
                .filter(|n| !n.is_empty())
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>();
            names.sort_unstable();
            expected.update((names.len() as u64).to_le_bytes());
            for name in names {
                let mut value = Vec::with_capacity(256 * 1024);
                rustix::fs::lgetxattr(
                    &path,
                    std::ffi::OsStr::from_bytes(&name),
                    rustix::buffer::spare_capacity(&mut value),
                )
                .unwrap();
                expected.update((name.len() as u64).to_le_bytes());
                expected.update(name);
                expected.update((value.len() as u64).to_le_bytes());
                expected.update(value);
            }
            super::hash_entry_xattrs(&path, &mut actual, &mut scratch).unwrap();
            let current = (
                scratch.names.as_ptr(),
                scratch.value.as_ptr(),
                scratch.names.capacity(),
                scratch.value.capacity(),
            );
            assert_eq!(
                *allocations.get_or_insert(current),
                current,
                "reuse both allocations across entries"
            );
        }
        assert_eq!(actual.finalize(), expected.finalize());
    }

    #[cfg(unix)]
    #[test]
    fn compaction_snapshots_preserve_special_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempdir().unwrap();
        let child = fixture.path().join("file");
        fs::write(&child, "private\n").unwrap();
        for (path, mode) in [
            (fixture.path(), 0o1755),
            (child.as_path(), 0o2644),
            (child.as_path(), 0o4644),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            let error = super::directory_snapshot(fixture.path())
                .expect_err("special bits cannot disappear from a compaction snapshot");
            assert!(
                error.to_string().contains("special Unix permissions"),
                "{error}"
            );
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                mode
            );
            fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).unwrap();
        }
        super::directory_snapshot(fixture.path()).expect("ordinary permissions remain supported");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compaction_snapshots_refuse_macos_access_control_rules() {
        let fixture = tempdir().unwrap();
        let file = fixture.path().join("file");
        fs::write(&file, "private\n").unwrap();
        for (path, rule) in [
            (file.as_path(), "everyone deny write"),
            (
                fixture.path(),
                "everyone allow read,file_inherit,directory_inherit",
            ),
        ] {
            let before = super::directory_snapshot(fixture.path()).unwrap();
            assert!(
                Command::new("chmod")
                    .args(["+a", rule])
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
            let error = super::verify_snapshot(fixture.path(), &before)
                .expect_err("ACLs must prevent destructive compaction cleanup");
            assert!(error.to_string().contains("ACL"), "{error}");
            assert!(
                Command::new("chmod")
                    .arg("-N")
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
            assert_eq!(
                super::directory_snapshot(fixture.path()).unwrap(),
                before,
                "ordinary snapshot format stays compatible"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compaction_recovery_preserves_new_macos_acls() {
        for phase in [
            CompactWorktreePhase::ReplacementReady,
            CompactWorktreePhase::ReplacementActivated,
            CompactWorktreePhase::AddJournalUpdated,
        ] {
            let fixture = tempdir().unwrap();
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("view");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::write(repository.join("file"), "original\n").unwrap();
            git(&repository, &["add", "."]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::Detached,
                    state_dir: Some(state.clone()),
                    sparse_directories: vec![],
                },
                None,
                true,
            )
            .unwrap();
            compact_worktree_inner(
                CompactWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(phase),
            )
            .expect_err("interrupt compaction");
            let journal = super::CompactJournalStore::open(&state)
                .load_all()
                .unwrap()
                .pop()
                .unwrap();
            let protected = if phase == CompactWorktreePhase::ReplacementReady {
                destination.join("file")
            } else {
                journal.quarantine.join("file")
            };
            assert!(
                Command::new("chmod")
                    .args(["+a", "everyone deny write"])
                    .arg(&protected)
                    .status()
                    .unwrap()
                    .success()
            );
            for _ in 0..2 {
                let report = recover_incomplete_operations(&state).unwrap();
                assert_eq!(report.recovered_compactions, 0, "{phase:?}: {report:?}");
                if phase == CompactWorktreePhase::ReplacementReady {
                    // Before the swap the worktree is untouched, so a change
                    // to it cancels the compaction and leaves it as it is.
                    assert!(report.errors.is_empty(), "{report:?}");
                    assert!(!journal.replacement.exists());
                    let accounting = storage_accounting(&state).unwrap();
                    assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
                } else {
                    assert_eq!(report.errors.len(), 1, "{report:?}");
                    assert!(report.errors[0].contains("ACL"), "{report:?}");
                }
                assert!(riftri_storage::has_macos_acl(&protected).unwrap());
                assert_eq!(fs::read(&protected).unwrap(), b"original\n");
                assert_eq!(fs::read(destination.join("file")).unwrap(), b"original\n");
            }
        }
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
                sparse_directories: Vec::new(),
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

    /// Repository with one managed worktree whose checkout profile changes
    /// after the add: `core.eol lf` is an accepted value, so compaction still
    /// runs, but the profile hash — and therefore the immutable-base bucket —
    /// differs from the one the add staged in.
    fn bucket_retarget_fixture(
        root: &Path,
    ) -> (PathBuf, PathBuf, PathBuf, super::AddWorktreeResult) {
        let repository = root.join("repository");
        let destination = root.join("worktree");
        let state = root.join("state");
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/bucket-retarget")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");
        git(&repository, &["config", "core.eol", "lf"]);
        (repository, destination, state, added)
    }

    fn assert_no_stranded_compaction_quarantine(parent: &Path) {
        let stranded = fs::read_dir(parent)
            .expect("scan worktree parent")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".riftri-compact-old-")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert!(stranded.is_empty(), "stranded quarantines: {stranded:?}");
    }

    #[test]
    fn compaction_retargets_the_add_journal_across_base_buckets() {
        let fixture = tempdir().expect("fixture");
        let (repository, destination, state, added) = bucket_retarget_fixture(fixture.path());

        let compacted = compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("compact across base buckets");

        assert_ne!(
            compacted.base_path.parent(),
            added.base_path.parent(),
            "the profile flip must move the immutable base into a new bucket"
        );
        let journal = JournalStore::open(&state)
            .load_all()
            .expect("load add journals")
            .pop()
            .expect("add journal");
        assert_eq!(journal.base_path, compacted.base_path);
        assert_eq!(
            journal.base_staging.parent(),
            journal.base_path.parent(),
            "base_path and base_staging must move buckets together"
        );
        let accounting = storage_accounting(&state).expect("account retargeted state");
        assert_eq!(accounting.active_views, 1);
        assert!(
            accounting.diagnostic_issues.is_empty(),
            "{:?}",
            accounting.diagnostic_issues
        );

        // The retargeted journal must stay fully operable: compact, repair,
        // move, and remove all validate it again.
        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("compact the retargeted worktree again");
        let repair = recover_incomplete_operations(&state).expect("repair retargeted state");
        assert!(repair.errors.is_empty(), "{repair:?}");
        let moved = fixture.path().join("moved");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: destination,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move the retargeted worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: moved,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the retargeted worktree");
        let accounting = storage_accounting(&state).expect("account retired state");
        assert!(
            accounting.diagnostic_issues.is_empty(),
            "{:?}",
            accounting.diagnostic_issues
        );
    }

    #[test]
    fn compaction_base_bucket_retarget_recovers_from_every_interruption_window() {
        for phase in [
            CompactWorktreePhase::IntentRecorded,
            CompactWorktreePhase::ReplacementReady,
            CompactWorktreePhase::ReplacementActivated,
            CompactWorktreePhase::AddJournalUpdated,
            CompactWorktreePhase::Complete,
        ] {
            let fixture = tempdir().expect("fixture");
            let (repository, destination, state, _added) = bucket_retarget_fixture(fixture.path());

            compact_worktree_inner(
                CompactWorktreeRequest {
                    repository: repository.clone(),
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
            assert_no_stranded_compaction_quarantine(fixture.path());
            let output = Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&destination)
                .output()
                .expect("inspect recovered worktree");
            assert!(output.status.success(), "{phase:?}");
            assert!(output.stdout.is_empty(), "{phase:?}");
            let journal = JournalStore::open(&state)
                .load_all()
                .expect("load add journals")
                .pop()
                .expect("add journal");
            assert_eq!(
                journal.base_staging.parent(),
                journal.base_path.parent(),
                "{phase:?}: no interruption may split the journal across buckets"
            );
            let accounting = storage_accounting(&state).expect("account after repair");
            assert_eq!(accounting.pending_compactions, 0, "{phase:?}");
            assert!(
                accounting.diagnostic_issues.is_empty(),
                "{phase:?}: {:?}",
                accounting.diagnostic_issues
            );
            move_worktree_inner(
                MoveWorktreeRequest {
                    repository,
                    source: destination,
                    destination: fixture.path().join("moved"),
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .expect("move recovered worktree");
        }
    }

    #[test]
    fn compaction_recovers_between_the_base_update_and_the_phase_advance() {
        let fixture = tempdir().expect("fixture");
        let (repository, destination, state, _added) = bucket_retarget_fixture(fixture.path());

        compact_worktree_inner(
            CompactWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(CompactWorktreePhase::ReplacementActivated),
        )
        .expect_err("interrupt after activation");
        let compaction = super::CompactJournalStore::open(&state)
            .load_all()
            .expect("load compaction journals")
            .pop()
            .expect("compaction journal");
        assert_eq!(compaction.phase, CompactWorktreePhase::ReplacementActivated);

        // Simulate the process dying immediately after the add journal's
        // durable base update but before the compaction journal records
        // AddJournalUpdated: perform exactly the update resumption performs.
        let add_store = JournalStore::open(&state);
        let journal_path = add_store
            .load_all()
            .expect("load add journals")
            .pop()
            .expect("add journal")
            .journal_path;
        add_store
            .update_active_base(
                &journal_path,
                &compaction.destination,
                &compaction.old_base_path,
                &compaction.base_path,
                &compaction.expected_commit,
            )
            .expect("durable base update");
        let journal = add_store
            .load_all()
            .expect("reload add journals")
            .pop()
            .expect("updated add journal");
        assert_eq!(journal.base_path, compaction.base_path);
        assert_eq!(
            journal.base_staging.parent(),
            journal.base_path.parent(),
            "the base update must retarget staging in the same durable write"
        );

        let repair = recover_incomplete_operations(&state).expect("resume after the base update");
        assert!(repair.errors.is_empty(), "{repair:?}");
        assert_no_stranded_compaction_quarantine(fixture.path());
        let accounting = storage_accounting(&state).expect("account resumed state");
        assert_eq!(accounting.active_views, 1);
        assert_eq!(accounting.completed_compactions, 1);
        assert_eq!(accounting.pending_compactions, 0);
        assert!(
            accounting.diagnostic_issues.is_empty(),
            "{:?}",
            accounting.diagnostic_issues
        );
    }

    #[cfg(unix)]
    fn rewrite_add_journal_base_staging(journal_path: &Path, base_staging: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(journal_path).expect("read add journal"))
                .expect("parse add journal");
        record["base_staging"] = serde_json::json!({
            "encoding": "unix-bytes",
            "units": base_staging.as_os_str().as_bytes(),
        });
        fs::write(
            journal_path,
            serde_json::to_vec_pretty(&record).expect("encode add journal"),
        )
        .expect("write wedged add journal");
    }

    #[cfg(unix)]
    #[test]
    fn repair_and_lifecycle_recognize_journals_wedged_by_older_compactions() {
        let fixture = tempdir().expect("fixture");
        let (repository, destination, state, added) = bucket_retarget_fixture(fixture.path());

        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            Some(CompactWorktreePhase::AddJournalUpdated),
        )
        .expect_err("interrupt after the add journal update");

        // Recreate the record an older Riftri persisted at this point: the
        // base retargeted into the new bucket while the staging record stayed
        // behind in the bucket the add originally built in.
        let journal = JournalStore::open(&state)
            .load_all()
            .expect("load add journals")
            .pop()
            .expect("add journal");
        let old_bucket = added.base_path.parent().expect("original bucket");
        rewrite_add_journal_base_staging(
            &journal.journal_path,
            &old_bucket.join(format!(".riftri-build-{}", journal.operation_id)),
        );
        let wedged = JournalStore::open(&state)
            .load_all()
            .expect("reload add journals")
            .pop()
            .expect("wedged add journal");
        assert_ne!(
            wedged.base_staging.parent(),
            wedged.base_path.parent(),
            "the forged journal must reproduce the historical wedge"
        );

        let repair = recover_incomplete_operations(&state).expect("repair the wedged compaction");
        assert!(repair.errors.is_empty(), "{repair:?}");
        assert_no_stranded_compaction_quarantine(fixture.path());
        let accounting = storage_accounting(&state).expect("account the recognized journal");
        assert_eq!(accounting.active_views, 1);
        assert_eq!(accounting.pending_compactions, 0);
        assert!(
            accounting.diagnostic_issues.is_empty(),
            "{:?}",
            accounting.diagnostic_issues
        );

        // The next compaction heals the staging record into the base's
        // bucket, and the ordinary lifecycle keeps working throughout.
        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("compact the wedged worktree");
        let healed = JournalStore::open(&state)
            .load_all()
            .expect("load healed journals")
            .pop()
            .expect("healed add journal");
        assert_eq!(healed.base_staging.parent(), healed.base_path.parent());
        let moved = fixture.path().join("moved");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: destination,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move the healed worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: moved,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the healed worktree");
    }

    #[test]
    fn recovery_path_validation_relaxes_only_active_cross_bucket_staging() {
        let fixture = tempdir().expect("fixture");
        let root = fixture.path();
        let state = root.join("state");
        let bases = state.join("bases/v1");
        let journal =
            |phase: AddWorktreePhase, base_staging: PathBuf| crate::journal::DecodedJournal {
                journal_path: state.join("operations/operation.json"),
                operation_id: "operation".to_owned(),
                repository: root.join("repository"),
                destination: root.join("worktree"),
                scratch: root.join(".riftri-view-operation"),
                base_staging,
                base_path: bases.join("r-new/tree"),
                temporary_index: state.join("tmp/index-operation"),
                branch: None,
                branch_created: false,
                expected_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                backend: BackendKind::ApfsClone,
                sparse_directories: Vec::new(),
                overlayfs: None,
                phase,
                last_forward_phase: phase,
            };

        // The shared-bucket invariant is accepted in every phase.
        for phase in [AddWorktreePhase::BaseReady, AddWorktreePhase::Active] {
            assert!(
                super::validate_recovery_paths(
                    &state,
                    &journal(phase, bases.join("r-new/.riftri-build-operation")),
                )
                .is_ok(),
                "{phase:?}"
            );
        }
        // An Active journal wedged by an older cross-bucket compaction stays
        // recognizable while its staging record remains inside a bucket.
        assert!(
            super::validate_recovery_paths(
                &state,
                &journal(
                    AddWorktreePhase::Active,
                    bases.join("r-old/.riftri-build-operation"),
                ),
            )
            .is_ok()
        );
        // Every other phase keeps the strict shared-bucket requirement.
        assert!(
            super::validate_recovery_paths(
                &state,
                &journal(
                    AddWorktreePhase::BaseReady,
                    bases.join("r-old/.riftri-build-operation"),
                ),
            )
            .is_err()
        );
        // Even Active journals may not reference staging outside the
        // immutable-base layout, at the wrong depth, or with a foreign name.
        assert!(
            super::validate_recovery_paths(
                &state,
                &journal(
                    AddWorktreePhase::Active,
                    root.join("elsewhere/.riftri-build-operation"),
                ),
            )
            .is_err()
        );
        assert!(
            super::validate_recovery_paths(
                &state,
                &journal(
                    AddWorktreePhase::Active,
                    bases.join("r-old/deeper/.riftri-build-operation"),
                ),
            )
            .is_err()
        );
        assert!(
            super::validate_recovery_paths(
                &state,
                &journal(
                    AddWorktreePhase::Active,
                    bases.join("r-old/build-operation")
                ),
            )
            .is_err()
        );
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
            let common_git_dir = git
                .inspect_repository(repository)
                .unwrap()
                .identity
                .common_git_dir;
            let analysis = super::analyze_resolved_repository_compatibility(
                &git,
                repository,
                &common_git_dir,
                &resolved,
                &[],
                Some(
                    git.config_values(repository, super::CHECKOUT_CONFIG_KEYS)
                        .unwrap(),
                ),
            )
            .unwrap();
            // Reconstruct the previous, individual-read profile independently.
            let mut profile = Sha256::new();
            profile.update(b"riftri-checkout-profile-v4-sparse\0");
            let version = git.detect().unwrap().version;
            super::hash_profile_input(&mut profile, b"git.version", Some(version.as_bytes()));
            let mut captured = Vec::new();
            for key in keys {
                let value = git.config_value(repository, key).unwrap();
                // The sparse keys are worktree-scoped and describe where the
                // command ran rather than what is materialized, so they are
                // deliberately outside both the profile and the replayed
                // checkout configuration. The canonical cone list carries that
                // information instead.
                if matches!(key, "core.sparsecheckout" | "core.sparsecheckoutcone") {
                    continue;
                }
                super::hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
                if let Some(value) = value {
                    captured.push((key.to_owned(), value));
                }
            }
            assert_eq!(analysis.checkout_profile, profile.finalize().to_vec());
            assert_eq!(analysis.checkout_config, captured);
            assert_eq!(analysis.report.compatible, compatible);
            if !compatible {
                let blocker = analysis
                    .report
                    .blockers
                    .iter()
                    .find(|blocker| blocker.explanation.contains("core.autocrlf=true"))
                    .expect("autocrlf blocker");
                let sources = blocker
                    .explanation
                    .split_once("configuration sources: ")
                    .expect("config source diagnostic")
                    .1;
                assert!(
                    sources.split(", ").any(|source| {
                        source.starts_with("local ") && source.ends_with("=true")
                    }),
                    "{sources}"
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
    fn checkout_component_validation_preserves_preflight_and_normalization() {
        let fixture = tempfile::tempdir().unwrap();
        let destination = fixture.path().join("missing/view");
        for invalid in ["", ".", "..", "../escape", "a/../b", "/absolute"] {
            let paths = [PathBuf::from("café/nested/file"), PathBuf::from(invalid)];
            let error =
                super::validate_destination_path_semantics(&paths, &destination).unwrap_err();
            assert!(
                error.to_string().contains("not a relative checkout path"),
                "{invalid:?}: {error}"
            );
        }
        for valid in ["a", "a/b", "a/./b", "a//b"] {
            super::validate_destination_path_semantics(&[PathBuf::from(valid)], &destination)
                .unwrap();
        }
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
        let paths = [
            PathBuf::from("café/nested/a"),
            PathBuf::from("café/nested/b"),
        ];
        super::validate_destination_path_semantics(&paths, &fixture.path().join("view")).unwrap();
        assert_eq!(
            fs::read_dir(fixture.path()).unwrap().count(),
            0,
            "probe cleanup"
        );
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
            let git = Git::default();
            let common_git_dir = git
                .inspect_repository(fixture.path())
                .expect("inspect fixture repository")
                .identity
                .common_git_dir;
            super::inspect_repository_compatibility(
                &git,
                fixture.path(),
                &common_git_dir,
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

    #[test]
    fn attribute_fast_path_still_rejects_external_attributes() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let attributes = fixture.path().join("global-attributes");
        fs::create_dir(&repository).expect("repository");
        git(&repository, &["init", "--quiet"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("tracked file");
        git(&repository, &["add", "--", "tracked.txt"]);
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
        assert!(
            !repository.join(".gitattributes").exists(),
            "fixture must exercise the no-in-tree-attributes fast path"
        );
        fs::write(&attributes, "*.txt filter=external\n").expect("external attributes");
        git(
            &repository,
            &[
                "config",
                "core.attributesFile",
                attributes.to_str().expect("UTF-8 fixture path"),
            ],
        );

        let git = Git::default();
        let common_git_dir = git
            .inspect_repository(&repository)
            .expect("inspect repository")
            .identity
            .common_git_dir;
        let report = super::inspect_repository_compatibility(
            &git,
            &repository,
            &common_git_dir,
            OsStr::new("HEAD"),
        )
        .expect("compatibility report");

        assert!(!report.compatible);
        assert!(report.blockers.iter().any(|blocker| {
            blocker.kind == super::RepositoryCompatibilityBlockerKind::EffectiveAttributes
        }));
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
                    error.contains(&format!(
                        "riftri status --state-dir {}",
                        crate::shell_quoted_path(&state).expect("representable state directory")
                    )),
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
                assert!(recovery.errors[0].contains(&format!(
                    "riftri status --state-dir {}",
                    crate::shell_quoted_path(&state).expect("representable state directory")
                )));
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
                    sparse_directories: Vec::new(),
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
                sparse_directories: Vec::new(),
            },
            None,
            false,
        )
        .expect("the helper exits from the post-mount test hook");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlayfs_recovery_preserves_a_workdir_the_journaled_namespace_cannot_rule_out() {
        use riftri_storage::{OverlayFsMounter, OverlayFsRecoveryState};

        let fixture = tempdir().expect("fixture");
        if !require_overlayfs_test_namespace(fixture.path()) {
            return;
        }
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        create_overlayfs_repository(&repository);

        // Reproduce the crash window: exit after the mount syscall succeeds
        // but before the mount identity reaches the journal.
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

        let journal = JournalStore::open(&state)
            .load_all()
            .expect("load interrupted journal")
            .pop()
            .expect("one interrupted journal");
        let overlayfs = journal.overlayfs.clone().expect("OverlayFS intent");
        assert!(overlayfs.mount_identity.is_none(), "crash window closed");
        let context = overlayfs.mount_context.clone().expect("mount context");
        let layout = OverlayFsMounter::load(
            &overlayfs.layout_root,
            &journal.base_path,
            &journal.destination,
        )
        .expect("load interrupted layout");
        let identity =
            match OverlayFsMounter::recover_mount(&layout, &context, &overlayfs.recovery_token)
                .expect("adopt interrupted mount")
            {
                OverlayFsRecoveryState::Mounted(identity) => identity,
                state => panic!("expected mounted recovery state, got {state:?}"),
            };
        // Make the mount invisible to this namespace, exactly what recovery
        // sees when the mount lives on in a namespace it cannot inspect.
        OverlayFsMounter::unmount(&layout, &identity).expect("hide interrupted mount");

        // Give the journal the interrupted-remount shape (active, identity
        // never journaled) and a mount context recorded in a different
        // namespace of this boot: from here, the mount may still be live.
        let mut record: serde_json::Value = serde_json::from_slice(
            &fs::read(&journal.journal_path).expect("read interrupted journal"),
        )
        .expect("decode interrupted journal");
        record["phase"] = "active".into();
        record["last_forward_phase"] = "active".into();
        let recorded_inode = record["overlayfs"]["mount_context"]["mount_namespace_inode"]
            .as_u64()
            .expect("journaled namespace inode");
        record["overlayfs"]["mount_context"]["mount_namespace_inode"] = (recorded_inode + 1).into();
        fs::write(
            &journal.journal_path,
            serde_json::to_vec_pretty(&record).expect("encode foreign-namespace journal"),
        )
        .expect("persist foreign-namespace journal");
        let sentinel = layout.work().join("live-mount-sentinel");
        fs::write(&sentinel, b"must survive").expect("plant work-directory sentinel");

        let preserved =
            recover_incomplete_operations(&state).expect("repair with a possibly live mount");
        assert_eq!(preserved.recovered_mounts, 0);
        assert_eq!(preserved.errors.len(), 1, "{:?}", preserved.errors);
        assert!(
            preserved.errors[0].contains("different mount namespace"),
            "{:?}",
            preserved.errors
        );
        assert!(
            sentinel.exists(),
            "a destructive work-directory reset ran before the namespace guard"
        );
        assert!(layout.upper().join(".git").exists());

        // Back in the journaled namespace the absence of the mount is
        // provable, so recovery may reset the disposable work state and
        // remount the view.
        record["overlayfs"]["mount_context"]["mount_namespace_inode"] = recorded_inode.into();
        fs::write(
            &journal.journal_path,
            serde_json::to_vec_pretty(&record).expect("encode restored journal"),
        )
        .expect("persist restored journal");
        let repaired =
            recover_incomplete_operations(&state).expect("repair in the journaled namespace");
        assert!(repaired.errors.is_empty(), "{:?}", repaired.errors);
        assert_eq!(repaired.recovered_mounts, 1);
        assert!(
            Command::new("mountpoint")
                .arg("--quiet")
                .arg(&destination)
                .status()
                .expect("inspect remounted view")
                .success()
        );
        let marker_name = format!(".riftri-overlayfs-recovery-{}", overlayfs.recovery_token);
        assert!(
            !destination.join(&marker_name).exists(),
            "recovery marker still visible in the merged view"
        );

        let remounted = JournalStore::open(&state)
            .load_all()
            .expect("reload remounted journal")
            .pop()
            .expect("one remounted journal");
        let remounted_overlayfs = remounted.overlayfs.expect("remounted OverlayFS intent");
        let identity = remounted_overlayfs
            .mount_identity
            .expect("remounted identity");
        let layout = OverlayFsMounter::load(
            &remounted_overlayfs.layout_root,
            &remounted.base_path,
            &remounted.destination,
        )
        .expect("reload remounted layout");
        OverlayFsMounter::unmount(&layout, &identity).expect("unmount remounted view");
    }

    #[test]
    fn abandoned_probe_scan_names_only_real_probe_directories() {
        let fixture = tempdir().expect("fixture");
        let probe = fixture.path().join(".riftri-overlay-probe-42-1-0");
        fs::create_dir(&probe).expect("create probe directory");
        fs::write(probe.join("leftover"), b"layer").expect("write probe leftover");
        fs::write(
            fixture.path().join(".riftri-overlay-probe-42-1-1"),
            b"not a directory",
        )
        .expect("write probe-named file");
        fs::create_dir(fixture.path().join("user-directory")).expect("create user directory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&probe, fixture.path().join(".riftri-overlay-probe-42-1-2"))
            .expect("create probe-named symlink");

        assert_eq!(super::abandoned_probe_roots(fixture.path()), vec![probe]);
        assert!(super::abandoned_probe_roots(&fixture.path().join("absent")).is_empty());
    }

    #[test]
    fn status_names_an_abandoned_overlayfs_probe_next_to_a_worktree() {
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/probe-diagnostic")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");

        // A failed probe unmount abandons its directory beside the worktree.
        // Diagnostics scan the journaled destination's canonical parent, so
        // compare against the canonical probe path.
        let probe = fixture.path().join(".riftri-overlay-probe-4242-7-0");
        fs::create_dir(&probe).expect("create abandoned probe");
        fs::write(probe.join("leftover"), b"layer").expect("write abandoned layer");
        let probe = probe.canonicalize().expect("resolve abandoned probe");

        let status = storage_accounting(&state).expect("account with abandoned probe");
        assert!(
            status.diagnostic_issues.iter().any(|issue| {
                issue.path == probe && issue.reason.contains("abandoned OverlayFS probe")
            }),
            "abandoned probe not named: {:?}",
            status.diagnostic_issues
        );

        let repaired = recover_incomplete_operations(&state).expect("repair with abandoned probe");
        assert!(repaired.errors.is_empty(), "{:?}", repaired.errors);
        #[cfg(target_os = "linux")]
        {
            assert_eq!(repaired.reaped_probe_roots, vec![probe.clone()]);
            assert!(repaired.preserved_probe_mounts.is_empty());
            assert!(!probe.exists(), "unmounted probe leftover not reaped");
            let clean = storage_accounting(&state).expect("account after probe reap");
            assert!(
                !clean
                    .diagnostic_issues
                    .iter()
                    .any(|issue| issue.reason.contains("abandoned OverlayFS probe")),
                "{:?}",
                clean.diagnostic_issues
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Only the Linux mount inventory can prove the probe unmounted,
            // so other platforms report it and leave it alone.
            assert!(repaired.reaped_probe_roots.is_empty());
            assert!(probe.is_dir(), "probe removed without a liveness proof");
        }

        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state),
            },
            None,
        )
        .expect("remove probe-diagnostic worktree");
        assert!(!destination.exists());
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
                    sparse_directories: Vec::new(),
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
                sparse_directories: Vec::new(),
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

    #[cfg(unix)]
    #[test]
    fn status_reports_unsafe_overlay_roots_without_traversing_or_removing_them() {
        use std::os::unix::fs::symlink;

        for kind in [
            "missing",
            "directory",
            "file",
            "symlink",
            "dangling-symlink",
        ] {
            let fixture = tempdir().expect("fixture");
            let state = fixture.path().join("state");
            fs::create_dir_all(state.join("overlays")).expect("create overlays directory");
            let state = state.canonicalize().expect("resolve state");
            let root = state.join("overlays/v1");
            let outside = fixture.path().join("outside");
            match kind {
                "missing" => {}
                "directory" => fs::create_dir(&root).expect("create layout root"),
                "file" => fs::write(&root, "preserve root\n").expect("write layout root"),
                _ => {
                    if kind == "symlink" {
                        fs::create_dir(&outside).expect("create outside directory");
                        fs::write(outside.join("private.txt"), "preserve outside\n")
                            .expect("write outside file");
                    }
                    symlink(&outside, &root).expect("symlink layout root");
                }
            }

            let report = storage_accounting(&state).expect("diagnose state");
            let paths = report
                .diagnostic_issues
                .iter()
                .map(|issue| issue.path.as_path())
                .collect::<Vec<_>>();
            if matches!(kind, "missing" | "directory") {
                assert!(paths.is_empty(), "{kind}: {paths:?}");
            } else {
                assert_eq!(paths, [root.as_path()], "{kind}");
            }
            match kind {
                "file" => assert_eq!(fs::read_to_string(&root).unwrap(), "preserve root\n"),
                "symlink" | "dangling-symlink" => {
                    assert_eq!(fs::read_link(&root).unwrap(), outside);
                    if kind == "symlink" {
                        assert_eq!(
                            fs::read_to_string(outside.join("private.txt")).unwrap(),
                            "preserve outside\n"
                        );
                    }
                }
                _ => {}
            }
        }
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
                sparse_directories: &[],
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

    /// Reference counts come only from journals that parsed, so an unreadable
    /// one silently under-counts. Two live worktrees sharing a base reported
    /// `refs=0` once their add journals were damaged, and the CLI described
    /// that base as an unreferenced cache — an invitation to delete storage two
    /// working trees still depend on. The accounting must keep reporting the
    /// damage so the renderer can refuse to claim the base is unused.
    #[test]
    fn storage_accounting_reports_damage_alongside_its_under_counted_references() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        // Windows runners default this on, and Riftri refuses a checkout whose
        // bytes Git would rewrite.
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        for (destination, branch) in [("one", "feature/one"), ("two", "feature/two")] {
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: fixture.path().join(destination),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(branch)),
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create managed worktree");
        }

        let healthy = storage_accounting(&state).expect("status");
        assert!(healthy.diagnostic_issues.is_empty());
        assert_eq!(healthy.bases.len(), 1);
        assert_eq!(healthy.bases[0].reference_count, 2);

        for entry in fs::read_dir(state.join("operations")).expect("read operations") {
            let path = entry.expect("entry").path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let contents = fs::read(&path).expect("read journal");
                fs::write(&path, &contents[..contents.len() / 4]).expect("truncate journal");
            }
        }

        let damaged = storage_accounting(&state).expect("status must still report");
        assert_eq!(
            damaged.bases[0].reference_count, 0,
            "the count is derived only from readable journals"
        );
        assert_eq!(
            damaged.diagnostic_issues.len(),
            2,
            "both unreadable journals must be reported so the count is not \
             mistaken for a complete one: {damaged:?}"
        );
        assert!(
            fixture.path().join("one").is_dir() && fixture.path().join("two").is_dir(),
            "both worktrees still exist and still depend on the base"
        );
    }

    /// Git keeps a worktree's registration when its directory disappears and
    /// marks the entry `prunable` rather than dropping it. Riftri treated any
    /// registration as a live worktree, so the add journal was never retired:
    /// `repair` reported nothing to do while `status` flagged the state
    /// forever and `gc` could never reclaim the base. Since `riftri enable`
    /// pins `gc.worktreePruneExpire=never`, Git never clears that entry on its
    /// own either, so the state could not converge without manual surgery.
    #[test]
    fn repair_retires_an_add_journal_whose_worktree_git_marks_prunable() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("vanished");
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/vanished")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");

        // A live worktree is never retired.
        let report = recover_incomplete_operations(&state).expect("repair");
        assert_eq!(report.retired_adds, 0, "{report:?}");
        assert_eq!(report.active, 1, "{report:?}");

        // The directory disappears the way an external `rm -rf` or a cleared
        // `/tmp` would. Git keeps the registration and marks it prunable.
        fs::remove_dir_all(&destination).expect("remove the worktree directory");
        // Git keeps the registration and annotates it instead of dropping it.
        let canonical = fs::canonicalize(fixture.path())
            .expect("canonical fixture")
            .join("vanished");
        assert!(
            riftri_git::Git::default()
                .list_worktrees(&repository)
                .expect("list worktrees")
                .iter()
                .any(|worktree| super::paths_match(&worktree.path, &canonical)
                    && worktree.prunable_reason.is_some()),
            "Git must still register the vanished worktree as prunable"
        );

        let report = recover_incomplete_operations(&state).expect("repair");
        assert_eq!(
            report.retired_adds, 1,
            "a prunable registration must not block retirement: {report:?}"
        );
        assert!(report.errors.is_empty(), "{report:?}");

        // State converges: nothing left to report, and the base is collectable.
        let report = storage_accounting(&state).expect("status");
        assert!(report.diagnostic_issues.is_empty(), "{report:?}");
        assert!(
            report.bases.iter().all(|base| base.reference_count == 0),
            "the retired journal must release its base: {report:?}"
        );
    }

    /// `repair` reaps a temporary only when removal is provably safe: in
    /// `operations/` under the add's lock, elsewhere only once the owning
    /// journal is terminal. `status` once promised "`riftri repair` removes it"
    /// for every directory, so a temporary with no such proof was flagged
    /// forever while repair kept reporting success. That advice could never
    /// be satisfied.
    #[test]
    fn journal_temporary_advice_matches_what_repair_actually_reaps() {
        let fixture = tempdir().expect("fixture");
        let state = fixture.path().join("state");
        for directory in ["operations", "removals"] {
            fs::create_dir_all(state.join(directory)).expect("create journal directory");
        }
        // Diagnostics record canonical paths.
        let state = fs::canonicalize(&state).expect("canonical state directory");
        let reapable = state.join("operations/.add-op-0.Qq34Cd.tmp");
        let preserved = state.join("removals/.remove-op-0.Zz12Ab.tmp");
        fs::write(&reapable, b"{\"partial\"").expect("write temporary");
        fs::write(&preserved, b"{\"partial\"").expect("write temporary");

        let report = storage_accounting(&state).expect("status must diagnose temporaries");
        let advice = |path: &Path| {
            report
                .diagnostic_issues
                .iter()
                .find(|issue| issue.path == path)
                .map(|issue| issue.reason.clone())
                .unwrap_or_else(|| panic!("no diagnostic for {}", path.display()))
        };
        assert!(
            advice(&reapable).contains("`riftri repair` removes it"),
            "a reapable temporary must name repair: {}",
            advice(&reapable)
        );
        assert!(
            !advice(&preserved).contains("`riftri repair` removes it"),
            "repair does not reap this directory, so it must not be promised: {}",
            advice(&preserved)
        );

        // And the advice matches what repair does.
        let reaped = recover_incomplete_operations(&state)
            .expect("repair")
            .reaped_artifacts;
        assert!(reaped.contains(&reapable), "{reaped:?}");
        assert!(!reaped.contains(&preserved), "{reaped:?}");
        assert!(
            preserved.exists(),
            "a preserved temporary must stay in place"
        );
    }

    /// A kill during any lifecycle journal write leaves a temporary beside the
    /// journal. Once that journal is terminal it is never rewritten, so no
    /// writer can still own the temporary: repair reaps it, and status says so.
    /// A temporary whose owner is unknown or still pending stays preserved.
    #[test]
    fn repair_reaps_journal_temporaries_of_finished_operations() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
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
        let add = |destination: &Path| {
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.to_path_buf(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::Detached,
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
        };
        let moved_from = fixture.path().join("moved-from");
        let moved_to = fixture.path().join("moved-to");
        let removed = fixture.path().join("removed");
        let pending = fixture.path().join("pending");
        add(&moved_from);
        add(&removed);
        add(&pending);
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: moved_from,
                destination: moved_to,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: removed,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: pending.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("leave one removal pending");
        // A change after intent keeps that removal pending through repair.
        fs::write(pending.join("tracked.txt"), "changed\n").expect("change pending view");

        // Diagnostics record canonical paths.
        let state = fs::canonicalize(&state).expect("canonical state directory");
        let temporary_for = |directory: &str, operation_id: &str| {
            let path = state
                .join(directory)
                .join(format!(".{operation_id}.Qq34Cd.tmp"));
            fs::write(&path, b"{\"partial\"").expect("write temporary");
            path
        };
        let moves = crate::journal::MoveJournalStore::open(&state)
            .load_all()
            .expect("moves");
        let removals = RemovalJournalStore::open(&state)
            .load_all()
            .expect("removals");
        let finished_removal = removals
            .iter()
            .find(|journal| journal.phase == RemoveWorktreePhase::Complete)
            .expect("a completed removal");
        let pending_removal = removals
            .iter()
            .find(|journal| journal.phase != RemoveWorktreePhase::Complete)
            .expect("a pending removal");
        assert_eq!(moves[0].phase, MoveWorktreePhase::Complete);
        let reapable = [
            temporary_for("moves", &moves[0].operation_id),
            temporary_for("removals", &finished_removal.operation_id),
        ];
        let preserved = [
            temporary_for("removals", &pending_removal.operation_id),
            temporary_for("removals", "remove-op-unknown"),
        ];

        let report = storage_accounting(&state).expect("status");
        let advice = |path: &Path| {
            report
                .diagnostic_issues
                .iter()
                .find(|issue| issue.path == path)
                .map(|issue| issue.reason.clone())
                .unwrap_or_else(|| panic!("no diagnostic for {}", path.display()))
        };
        for path in &reapable {
            assert!(
                advice(path).contains("`riftri repair` removes it"),
                "{}: {}",
                path.display(),
                advice(path)
            );
        }
        for path in &preserved {
            assert!(
                !advice(path).contains("`riftri repair` removes it"),
                "{}: {}",
                path.display(),
                advice(path)
            );
        }

        let reaped = recover_incomplete_operations(&state)
            .expect("repair")
            .reaped_artifacts;
        for path in &reapable {
            assert!(reaped.contains(path), "{reaped:?}");
            assert!(!path.exists(), "{}", path.display());
        }
        for path in &preserved {
            assert!(!reaped.contains(path), "{reaped:?}");
            assert!(path.exists(), "{}", path.display());
        }
    }

    /// A kill during a lifecycle command's first journal write leaves a
    /// complete intent record that was never renamed into place. The writer
    /// held the source add's lock for as long as that temporary could exist,
    /// so once repair holds the lock, nothing can still be writing it.
    #[test]
    fn repair_reaps_intent_writes_that_were_never_published() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
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
        let add = |destination: &Path| {
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.to_path_buf(),
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::Detached,
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
        };
        let moving = fixture.path().join("moving");
        let removing = fixture.path().join("removing");
        add(&moving);
        add(&removing);
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: moving.clone(),
                destination: fixture.path().join("moved"),
                state_dir: Some(state.clone()),
            },
            Some(MoveWorktreePhase::IntentRecorded),
        )
        .expect_err("stop after the move intent");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: removing.clone(),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("stop after the removal intent");

        // Diagnostics record canonical paths.
        let state = fs::canonicalize(&state).expect("canonical state directory");
        // Turn each published intent back into the temporary a kill before
        // its atomic rename leaves behind.
        let unpublish = |journal: &Path, suffix: &str| {
            let name = journal.file_stem().unwrap().to_str().unwrap();
            let temporary = journal.with_file_name(format!(".{name}.{suffix}.tmp"));
            fs::rename(journal, &temporary).expect("unpublish intent");
            temporary
        };
        let moves = crate::journal::MoveJournalStore::open(&state)
            .load_all()
            .expect("moves");
        let removals = RemovalJournalStore::open(&state)
            .load_all()
            .expect("removals");
        let move_intent = unpublish(&moves[0].journal_path, "Qq34Cd");
        let removal_intent = unpublish(&removals[0].journal_path, "Zz12Ab");
        // A torn write proves nothing about its writer and stays preserved.
        let torn = removal_intent.with_file_name(".remove-op-torn.Tt56Ef.tmp");
        let bytes = fs::read(&removal_intent).expect("read intent");
        fs::write(&torn, &bytes[..bytes.len() / 2]).expect("write torn intent");

        let report = storage_accounting(&state).expect("status");
        let advice = |path: &Path| {
            report
                .diagnostic_issues
                .iter()
                .find(|issue| issue.path == path)
                .map(|issue| issue.reason.clone())
                .unwrap_or_else(|| panic!("no diagnostic for {}", path.display()))
        };
        for path in [&move_intent, &removal_intent] {
            assert!(
                advice(path).contains("`riftri repair` removes it"),
                "{}: {}",
                path.display(),
                advice(path)
            );
        }
        assert!(!advice(&torn).contains("`riftri repair` removes it"));

        // While the source add is busy, its intent may still be in flight.
        let busy = {
            let add_journal = JournalStore::open(&state)
                .load_all()
                .expect("adds")
                .into_iter()
                .find(|journal| journal.operation_id == removals[0].source_add_operation_id)
                .expect("removal source add");
            super::try_lock_add_operation(&add_journal.journal_path)
                .expect("lock source add")
                .expect("source add is idle")
        };
        let reaped = recover_incomplete_operations(&state)
            .expect("repair while busy")
            .reaped_artifacts;
        assert!(reaped.contains(&move_intent), "{reaped:?}");
        assert!(!reaped.contains(&removal_intent), "{reaped:?}");
        assert!(removal_intent.exists());
        drop(busy);

        let reaped = recover_incomplete_operations(&state)
            .expect("repair")
            .reaped_artifacts;
        assert!(reaped.contains(&removal_intent), "{reaped:?}");
        assert!(!removal_intent.exists());
        assert!(torn.exists(), "a torn intent must stay preserved");
        // The operations never started, so their views are untouched.
        assert!(moving.is_dir());
        assert!(removing.is_dir());
        let accounting = storage_accounting(&state).expect("status after repair");
        assert_eq!(accounting.active_views, 2);
        assert_eq!(accounting.diagnostic_issues.len(), 1, "{accounting:?}");
    }

    /// A managed worktree removed or moved with plain Git is not an unsafe
    /// journal: status must say what happened and what clears it, and that
    /// advice must actually work.
    #[test]
    fn status_explains_worktrees_changed_outside_riftri() {
        for action in ["removed", "moved"] {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("worktree");
            let elsewhere = fixture.path().join("elsewhere");
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
                    mode: WorktreeMode::NewBranch(OsString::from("feature")),
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
            let from = destination.to_str().unwrap().to_owned();
            let to = elsewhere.to_str().unwrap().to_owned();
            if action == "removed" {
                git(&repository, &["worktree", "remove", &from]);
            } else {
                git(&repository, &["worktree", "move", &from, &to]);
            }

            let report = storage_accounting(&state).expect("status");
            assert_eq!(report.diagnostic_issues.len(), 1, "{action}: {report:?}");
            let reason = &report.diagnostic_issues[0].reason;
            assert!(!reason.contains("unsafe"), "{action}: {reason}");
            assert!(
                reason.contains(&format!("{action} outside Riftri")),
                "{action}: {reason}"
            );
            if action == "removed" {
                assert!(reason.contains("`riftri repair`"), "{reason}");
                let repaired = recover_incomplete_operations(&state).expect("repair");
                assert!(repaired.errors.is_empty(), "{repaired:?}");
            } else {
                assert!(reason.contains("elsewhere"), "{reason}");
                assert!(reason.contains("git worktree move"), "{reason}");
                git(&repository, &["worktree", "move", &to, &from]);
            }
            let report = storage_accounting(&state).expect("status after advice");
            assert!(
                report.diagnostic_issues.is_empty(),
                "{action}: following the advice must clear it: {report:?}"
            );
        }
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
        // A journal that could not be parsed may hold an uncounted base
        // reference, so it must keep the counts conservative.
        assert!(
            report
                .diagnostic_issues
                .iter()
                .filter(|issue| issue.reason.contains("malformed durable operation journal"))
                .all(|issue| issue.base_count_impact == BaseCountImpact::MayHideReference),
            "an unreadable journal must mark base counts unconfirmed: {report:?}"
        );
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
                sparse_directories: Vec::new(),
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
    fn an_unregistered_destination_keeps_its_base_counted_for_status() {
        // `gc` protects a base for any add journal that is neither rolled back
        // nor removal-complete, without consulting Git's worktree list. Status
        // used to drop such a journal into its diagnostics and never count the
        // claim, so it reported `refs=0` for a base `gc` refuses to collect --
        // and only an unrelated blanket hedge stopped it from calling that base
        // an unreferenced cache. The two must agree.
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/forgotten")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");

        // Out-of-band removal: Git stops registering the destination while the
        // add journal stays active, which is what the diagnostic describes.
        git(
            &repository,
            &[
                "worktree",
                "remove",
                "--force",
                destination.to_str().expect("destination path"),
            ],
        );

        let status = storage_accounting(&state).expect("status after Git forgets the destination");
        assert_eq!(status.bases.len(), 1);
        assert_eq!(
            status.bases[0].reference_count, 1,
            "an active add journal still claims this base: {status:?}"
        );
        assert!(
            !status.diagnostic_issues.is_empty(),
            "the unregistered destination must still be reported: {status:?}"
        );
        // The claim is counted above, so this diagnostic must not also mark
        // every base count unconfirmed.
        assert!(
            status
                .diagnostic_issues
                .iter()
                .all(|issue| issue.base_count_impact == BaseCountImpact::CountsUnaffected),
            "a counted claim must not report itself as possibly hidden: {status:?}"
        );

        // The claim status now counts is exactly the one gc refuses to release.
        let collection =
            garbage_collect_inner(&state, false, None).expect("collection plan must succeed");
        assert!(
            collection
                .skipped_protected
                .iter()
                .any(|skipped| skipped.base_path == added.base_path),
            "gc must still protect the base status counts: {collection:?}"
        );
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
                sparse_directories: Vec::new(),
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
                sparse_directories: Vec::new(),
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
    fn interrupted_add_recovery_preserves_staged_index_changes() {
        for sparse in [false, true] {
            for phase in [
                AddWorktreePhase::GitPointerRestored,
                AddWorktreePhase::IndexSynchronized,
            ] {
                for change in ["content", "deletion", "intent-to-add", "conflict"] {
                    let fixture = tempdir().unwrap();
                    let repository = fixture.path().join("repository");
                    let destination = fixture.path().join("view");
                    let state = fixture.path().join("state");
                    fs::create_dir_all(repository.join("selected")).unwrap();
                    git(&repository, &["init", "--quiet"]);
                    git(&repository, &["config", "user.name", "Riftri Tests"]);
                    git(
                        &repository,
                        &["config", "user.email", "riftri@example.invalid"],
                    );
                    git(&repository, &["config", "core.autocrlf", "false"]);
                    fs::write(repository.join("selected/file"), "original\n").unwrap();
                    fs::write(repository.join("empty"), "").unwrap();
                    git(&repository, &["add", "."]);
                    git(&repository, &["commit", "--quiet", "-m", "initial"]);
                    let sparse_directories = if sparse {
                        vec!["selected".to_owned()]
                    } else {
                        vec![]
                    };
                    add_worktree_inner(
                        AddWorktreeRequest {
                            repository: repository.clone(),
                            destination: destination.clone(),
                            revision: OsString::from("HEAD"),
                            mode: WorktreeMode::Detached,
                            state_dir: Some(state.clone()),
                            sparse_directories: sparse_directories.clone(),
                        },
                        Some(phase),
                        false,
                    )
                    .expect_err("interrupt creation");
                    // Also cover the crash gap: Git wrote the index, but the
                    // creator did not yet persist IndexSynchronized.
                    let git_handle = riftri_git::Git::default();
                    if phase == AddWorktreePhase::GitPointerRestored {
                        if sparse {
                            git_handle
                                .synchronize_sparse_worktree_index(
                                    &destination,
                                    &sparse_directories,
                                )
                                .unwrap();
                        } else {
                            git_handle.synchronize_worktree_index(&destination).unwrap();
                        }
                    }
                    match change {
                        "content" => {
                            fs::write(
                                destination.join("selected/file"),
                                "private staged content\n",
                            )
                            .unwrap();
                            git(&destination, &["add", "selected/file"]);
                            fs::write(destination.join("selected/file"), "original\n").unwrap();
                        }
                        "deletion" => git(&destination, &["rm", "--cached", "selected/file"]),
                        "intent-to-add" => {
                            git(&destination, &["rm", "--cached", "empty"]);
                            git(&destination, &["add", "--intent-to-add", "empty"]);
                        }
                        "conflict" => {
                            use std::io::Write;
                            use std::process::Stdio;
                            let oid = Command::new("git")
                                .args(["rev-parse", "HEAD:selected/file"])
                                .current_dir(&destination)
                                .output()
                                .unwrap();
                            let oid = String::from_utf8(oid.stdout).unwrap();
                            git(
                                &destination,
                                &["update-index", "--force-remove", "selected/file"],
                            );
                            let mut child = Command::new("git")
                                .args(["update-index", "--index-info"])
                                .current_dir(&destination)
                                .stdin(Stdio::piped())
                                .spawn()
                                .unwrap();
                            write!(
                                child.stdin.take().unwrap(),
                                "100644 {} 1\tselected/file\n100644 {} 2\tselected/file\n",
                                oid.trim(),
                                oid.trim()
                            )
                            .unwrap();
                            assert!(child.wait().unwrap().success());
                        }
                        _ => unreachable!(),
                    }
                    let before = git_handle.worktree_removal_state(&destination).unwrap();
                    for _ in 0..2 {
                        let report = recover_incomplete_operations(&state).unwrap();
                        assert_eq!(
                            report.recovered, 0,
                            "{change}, sparse={sparse}, {phase:?}: {report:?}"
                        );
                        assert_eq!(report.errors.len(), 1, "{report:?}");
                        assert!(destination.exists());
                        assert_eq!(
                            git_handle.worktree_removal_state(&destination).unwrap(),
                            before
                        );
                        assert_eq!(
                            fs::read(destination.join("selected/file")).unwrap(),
                            b"original\n"
                        );
                    }
                }
            }
        }
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
                    sparse_directories: Vec::new(),
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
    fn sparse_add_recovery_is_idempotent_after_every_transition() {
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
            let branch_name = format!("feature/sparse-recover-{index}");
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            fs::create_dir_all(repository.join("a")).expect("create cone directory");
            fs::create_dir_all(repository.join("b")).expect("create out-of-cone directory");
            fs::write(repository.join("root.txt"), "root\n").expect("write root file");
            fs::write(repository.join("a/file.txt"), "a\n").expect("write cone file");
            fs::write(repository.join("b/file.txt"), "b\n").expect("write out-of-cone file");
            git(&repository, &["add", "-A"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            let request = AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from(&branch_name)),
                state_dir: Some(state.clone()),
                sparse_directories: vec!["a".to_owned()],
            };

            let error = add_worktree_inner(request.clone(), Some(phase), false)
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

            let repeated = recover_incomplete_operations(&state)
                .unwrap_or_else(|error| panic!("repeat recovery after {phase:?}: {error}"));
            assert_eq!(repeated.recovered, 0, "phase {phase:?}");
            assert!(repeated.errors.is_empty(), "phase {phase:?}");

            // The same sparse request succeeds after the rollback and yields a
            // clean, correctly shaped cone view.
            add_worktree_inner(request, None, true)
                .unwrap_or_else(|error| panic!("retry after {phase:?}: {error}"));
            assert!(destination.join("a/file.txt").is_file(), "phase {phase:?}");
            assert!(destination.join("root.txt").is_file(), "phase {phase:?}");
            assert!(!destination.join("b").exists(), "phase {phase:?}");
            let status = Command::new("git")
                .args(["status", "--porcelain=v1", "--untracked-files=all"])
                .current_dir(&destination)
                .output()
                .expect("inspect recreated sparse worktree");
            assert!(status.status.success(), "phase {phase:?}");
            assert!(status.stdout.is_empty(), "phase {phase:?}");
        }
    }

    #[test]
    fn interrupted_sparse_add_preserves_a_later_git_selection_change() {
        for phase in [
            AddWorktreePhase::IndexSynchronized,
            AddWorktreePhase::CleanVerified,
        ] {
            let fixture = tempdir().expect("fixture");
            let repository = fixture.path().join("repository");
            let destination = fixture.path().join("view");
            let state = fixture.path().join("state");
            fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "--quiet"]);
            git(&repository, &["config", "user.name", "Riftri Tests"]);
            git(
                &repository,
                &["config", "user.email", "riftri@example.invalid"],
            );
            git(&repository, &["config", "core.autocrlf", "false"]);
            for dir in ["a", "b"] {
                fs::create_dir(repository.join(dir)).unwrap();
                fs::write(repository.join(dir).join("file.txt"), dir).unwrap();
            }
            git(&repository, &["add", "-A"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            let request = AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: vec!["a".to_owned()],
            };
            let error = add_worktree_inner(request, Some(phase), false).unwrap_err();
            assert!(
                error.to_string().contains("injected failure"),
                "unexpected failure after {phase:?}: {error}"
            );
            git(&destination, &["sparse-checkout", "set", "--cone", "b"]);
            let before = Command::new("git")
                .current_dir(&destination)
                .args(["ls-files", "-t", "-z"])
                .output()
                .unwrap()
                .stdout;
            for _ in 0..2 {
                let report = recover_incomplete_operations(&state).unwrap();
                assert!(!report.errors.is_empty(), "{phase:?}: {report:?}");
                assert!(destination.join("b/file.txt").exists());
                assert!(!destination.join("a").exists());
                let after = Command::new("git")
                    .current_dir(&destination)
                    .args(["ls-files", "-t", "-z"])
                    .output()
                    .unwrap()
                    .stdout;
                assert_eq!(before, after);
            }
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
                sparse_directories: Vec::new(),
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
                    sparse_directories: Vec::new(),
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

            // A late ignored write must also survive: Git's normal clean
            // status cannot serve as the final protection for add rollback.
            fs::write(repository.join(".git/info/exclude"), "raced.txt\n").unwrap();
            let _hook = crate::test_hooks::install(
                crate::test_hooks::FilesystemRacePoint::RollbackGitRemoval,
                |path| {
                    fs::create_dir_all(path).expect("restore raced destination");
                    fs::write(path.join("raced.txt"), "raced write\n").expect("write raced file");
                },
            );

            let error = super::rollback_decoded(&Git::default(), &state, &journal)
                .expect_err("rollback must preserve the concurrent write");

            assert!(
                matches!(
                    &error,
                    super::WorktreeError::Git(GitError::CommandFailed { .. })
                        | super::WorktreeError::InvalidRequest(_)
                ),
                "unexpected rollback error: {error}"
            );
            assert_eq!(
                fs::read_to_string(destination.join("raced.txt")).expect("read preserved write"),
                "raced write\n"
            );
        }
    }

    /// A file written into the destination before the add returned failed the
    /// add, and rollback kept it, but only said "has changes; recovery
    /// preserved it". The add stayed pending, every `repair` failed the same
    /// way, and nothing said how to finish.
    #[cfg(unix)]
    #[test]
    fn a_write_into_an_unfinished_add_is_kept_with_steps_to_finish() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        let destination = fixture.path().join("worktree");
        let state = fixture.path().join("state");
        let saved = fixture.path().join("saved.txt");
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

        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::AddCleanCheck,
            |path| fs::write(path.join("mine.txt"), "user data\n").expect("write into the add"),
        );
        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect_err("the write fails the add's clean check");
        let super::WorktreeError::OperationAndRollback { rollback, .. } = &error else {
            panic!("the add must report its unfinished rollback: {error}");
        };
        assert!(
            matches!(**rollback, super::WorktreeError::RecoveryPending { .. }),
            "{error}"
        );
        let message = error.to_string();
        assert!(
            message.contains(&destination.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("copy anything you need out of"),
            "{message}"
        );
        assert!(message.contains("riftri repair"), "{message}");
        assert_eq!(
            fs::read(destination.join("mine.txt")).unwrap(),
            b"user data\n"
        );

        let report = recover_incomplete_operations(&state).expect("repair report");
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(
            report.errors[0].contains("copy anything you need out of"),
            "{report:?}"
        );

        // Following the steps lets repair finish rolling the add back.
        fs::copy(destination.join("mine.txt"), &saved).expect("copy the write out");
        fs::remove_dir_all(&destination).expect("delete the kept destination");
        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        let registered = Git::default().list_worktrees(&repository).expect("list");
        assert!(
            registered
                .iter()
                .all(|worktree| worktree.path.file_name() != Some(OsStr::new("worktree"))),
            "the rolled-back add must not stay registered"
        );
        assert_eq!(fs::read(&saved).unwrap(), b"user data\n");
    }

    /// The same write, landing before the view was swapped in: the destination
    /// holds only Git's pointer and the caller's file.
    #[cfg(unix)]
    #[test]
    fn repair_explains_a_write_into_an_add_killed_before_its_view() {
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/late-write")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::GitMetadataCreated),
            false,
        )
        .expect_err("simulate process termination");
        fs::write(destination.join("mine.txt"), "user data\n").expect("write into the add");

        for _ in 0..2 {
            let report = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(report.errors.len(), 1, "{report:?}");
            assert!(
                report.errors[0].contains("copy anything you need out of"),
                "{report:?}"
            );
            assert!(report.errors[0].contains("riftri repair"), "{report:?}");
            assert_eq!(
                fs::read(destination.join("mine.txt")).unwrap(),
                b"user data\n"
            );
        }

        fs::remove_dir_all(&destination).expect("delete the kept destination");
        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    fn git_stdout(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[cfg(unix)]
    fn committed_repository(fixture: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let repository = fixture.join("repository");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        (repository, fixture.join("worktree"), fixture.join("state"))
    }

    #[cfg(unix)]
    #[test]
    fn managed_discovery_reads_each_journal_once_across_aliases() {
        let fixture = tempdir().unwrap();
        let (repository, destination, state) = committed_repository(fixture.path());
        git(&repository, &["config", "core.autocrlf", "false"]);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .unwrap();
        let moved = fixture.path().join("zz-moved");
        super::move_worktree(super::MoveWorktreeRequest {
            repository: repository.clone(),
            source: destination,
            destination: moved.clone(),
            state_dir: Some(state.clone()),
        })
        .unwrap();
        let alias = fixture.path().join("00-alias");
        std::os::unix::fs::symlink(&moved, &alias).unwrap();
        let before = crate::test_hooks::journal_open_count();
        assert_eq!(
            super::managed_worktree_state_directory(&repository, &alias).unwrap(),
            Some(state.canonicalize().unwrap())
        );
        assert_eq!(
            crate::test_hooks::journal_open_count() - before,
            2,
            "the add and completed move are each read once across path aliases"
        );
        // No cross-call cache: a newly malformed journal must fail closed.
        fs::write(state.join("moves/corrupt.json"), b"invalid").unwrap();
        assert!(super::managed_worktree_state_directory(&repository, &alias).is_err());
        assert!(super::find_managed_add_journal(&state, &moved).is_err());
    }

    /// Git commands ran in an add's complete view before the add finished: a
    /// commit. Rollback refused to delete that work, so every `repair` failed
    /// forever, and with `-b` the only way out was deleting the branch that
    /// held the commit. The worktree is now released as a plain Git worktree.
    #[cfg(unix)]
    #[test]
    fn repair_releases_an_unfinished_add_that_git_already_used() {
        for branch in [None, Some("feature/early")] {
            let fixture = tempdir().expect("fixture");
            let (repository, destination, state) = committed_repository(fixture.path());
            add_worktree_inner(
                AddWorktreeRequest {
                    repository: repository.clone(),
                    destination: destination.clone(),
                    revision: OsString::from("HEAD"),
                    mode: branch.map_or(WorktreeMode::Detached, |branch| {
                        WorktreeMode::NewBranch(OsString::from(branch))
                    }),
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                Some(AddWorktreePhase::IndexSynchronized),
                false,
            )
            .expect_err("simulate process termination");
            fs::write(destination.join("new.txt"), "new\n").unwrap();
            git(&destination, &["add", "new.txt"]);
            git(&destination, &["commit", "--quiet", "-m", "early work"]);
            let head = git_stdout(&destination, &["rev-parse", "HEAD"]);

            let report = recover_incomplete_operations(&state).expect("repair");

            assert!(report.errors.is_empty(), "{branch:?}: {report:?}");
            assert_eq!(report.released_adds.len(), 1, "{branch:?}: {report:?}");
            assert!(registered(&repository, &destination), "{branch:?}");
            assert_eq!(git_stdout(&destination, &["rev-parse", "HEAD"]), head);
            assert_eq!(git_stdout(&destination, &["status", "--porcelain"]), "");
            if let Some(branch) = branch {
                assert_eq!(
                    git_stdout(&repository, &["rev-parse", branch]),
                    head,
                    "the branch holding the commit is kept"
                );
            }
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
            assert_eq!(accounting.active_views, 0, "{accounting:?}");
            assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
            assert!(
                !super::is_managed_worktree(&repository, &destination).unwrap_or(false),
                "{branch:?}: Riftri no longer manages it"
            );
            let again = recover_incomplete_operations(&state).expect("repair again");
            assert!(
                again.errors.is_empty() && again.released_adds.is_empty(),
                "{again:?}"
            );
        }
    }

    /// The `-b` trap on its own: the worktree is gone, but the branch the add
    /// created moved. Rollback refused forever rather than delete it.
    #[cfg(unix)]
    #[test]
    fn rollback_keeps_a_moved_branch_and_finishes() {
        let fixture = tempdir().expect("fixture");
        let (repository, destination, state) = committed_repository(fixture.path());
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/moved")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::GitMetadataCreated),
            false,
        )
        .expect_err("simulate process termination");
        git(
            &repository,
            &[
                "worktree",
                "remove",
                "--force",
                destination.to_str().unwrap(),
            ],
        );
        git(
            &repository,
            &["commit", "--quiet", "--allow-empty", "-m", "later"],
        );
        git(&repository, &["branch", "--force", "feature/moved", "HEAD"]);
        let moved = git_stdout(&repository, &["rev-parse", "feature/moved"]);

        let report = recover_incomplete_operations(&state).expect("repair");

        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(
            git_stdout(&repository, &["rev-parse", "feature/moved"]),
            moved
        );
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    /// The live command: someone commits in the destination and keeps editing
    /// just before the add's clean check, which then fails.
    #[cfg(unix)]
    #[test]
    fn an_add_whose_view_git_already_used_keeps_it_as_a_plain_worktree() {
        let fixture = tempdir().expect("fixture");
        let (repository, destination, state) = committed_repository(fixture.path());
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::AddCleanCheck,
            |path| {
                fs::write(path.join("new.txt"), "new\n").unwrap();
                git(path, &["add", "new.txt"]);
                git(path, &["commit", "--quiet", "-m", "early work"]);
                fs::write(path.join("draft.txt"), "draft\n").unwrap();
            },
        );

        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect_err("the add did not finish");

        assert!(
            matches!(&error, super::WorktreeError::InvalidRequest(message)
                if message.contains("plain Git worktree")),
            "{error:?}"
        );
        assert!(registered(&repository, &destination));
        assert_eq!(fs::read(destination.join("new.txt")).unwrap(), b"new\n");
        assert_eq!(fs::read(destination.join("draft.txt")).unwrap(), b"draft\n");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    /// Build a committed repository for the empty-destination rollback tests.
    #[cfg(unix)]
    fn empty_destination_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let destination = fixture.path().join("precreated");
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
        fs::create_dir(&destination).unwrap();
        (fixture, repository, state, destination)
    }

    #[cfg(unix)]
    fn assert_no_pending_add(state: &Path) {
        for journal in JournalStore::open(state).load_all().unwrap() {
            assert_eq!(
                journal.phase,
                AddWorktreePhase::RolledBack,
                "a failed add must finish its rollback, not stay pending"
            );
        }
    }

    /// Git accepts an existing empty directory as a worktree destination
    /// (#437). When the add fails before Git has registered anything, that
    /// directory is still the caller's: rollback must leave it in place and
    /// complete. Refusing because an unregistered destination exists left the
    /// journal pending, and `riftri repair` hit the same refusal every time.
    #[cfg(unix)]
    #[test]
    fn a_failed_add_into_a_precreated_empty_directory_keeps_it_and_completes_rollback() {
        let (_fixture, repository, state, destination) = empty_destination_fixture();
        add_worktree_inner(
            AddWorktreeRequest {
                repository,
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::IntentRecorded),
            true,
        )
        .expect_err("injected failure before Git runs");
        assert!(
            destination.is_dir(),
            "the caller's directory must be preserved"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_pending_add(&state);
    }

    /// Once Git has populated a pre-existing empty directory, a failure makes
    /// Git remove the worktree *and* that directory; Riftri's rollback does the
    /// same, and still finishes.
    #[cfg(unix)]
    #[test]
    fn a_failed_add_after_populating_a_precreated_directory_removes_it_like_git() {
        let (_fixture, repository, state, destination) = empty_destination_fixture();
        add_worktree_inner(
            AddWorktreeRequest {
                repository,
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::ViewCreated),
            true,
        )
        .expect_err("injected failure after the view was created");
        assert!(
            !destination.exists(),
            "Git removes the directory in this case too"
        );
        assert_no_pending_add(&state);
    }

    /// `git worktree add` registers a new worktree locked as "initializing"
    /// and unlocks it as its last step. An add killed while that Git call ran
    /// leaves the lock, its branch, and a journal still at `intent-recorded`.
    /// `git worktree remove` refuses a locked worktree, so every later
    /// `repair` failed and the state directory could never recover (#444).
    #[cfg(unix)]
    #[test]
    fn repair_recovers_an_add_killed_inside_its_own_git_worktree_add() {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let destination = fixture.path().join("killed");
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

        // An add whose journal stops at `intent-recorded`, as a kill leaves it.
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("killed-branch")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::IntentRecorded),
            false,
        )
        .expect_err("simulated kill before the journal advanced");

        // What the killed `git worktree add -b` itself left behind: the branch,
        // a registration still locked "initializing", and no directory.
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-checkout",
                "-b",
                "killed-branch",
                destination.to_str().unwrap(),
                "HEAD",
            ],
        );
        let admin = repository.join(".git/worktrees/killed");
        fs::write(admin.join("locked"), "initializing\n").unwrap();
        fs::remove_dir_all(&destination).unwrap();

        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "repair must recover: {report:?}");
        let listed = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&listed.stdout).contains("killed"),
            "the interrupted registration must be removed"
        );
        let branch = Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/killed-branch",
            ])
            .current_dir(&repository)
            .status()
            .unwrap();
        assert!(
            !branch.success(),
            "the branch this add created must not leak"
        );
        for journal in JournalStore::open(&state).load_all().unwrap() {
            assert_eq!(journal.phase, AddWorktreePhase::RolledBack);
        }
    }

    /// The guard on the fix above: only the in-progress lock of this
    /// operation's own interrupted Git call may be removed. A lock placed on a
    /// worktree Git finished registering belongs to someone else, so rollback
    /// must leave the worktree and its lock alone.
    #[cfg(unix)]
    #[test]
    fn repair_never_unlocks_a_worktree_git_finished_registering() {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let destination = fixture.path().join("locked-by-user");
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
                mode: WorktreeMode::NewBranch(OsString::from("user-locked")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::GitMetadataCreated),
            false,
        )
        .expect_err("simulated kill after Git finished registering");
        git(
            &repository,
            &[
                "worktree",
                "lock",
                "--reason",
                "mine",
                destination.to_str().unwrap(),
            ],
        );

        let _ = recover_incomplete_operations(&state);
        let listed = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repository)
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(
            listed.contains("locked-by-user"),
            "the worktree must be kept: {listed}"
        );
        assert!(
            listed.contains("locked mine"),
            "the user's lock must be kept: {listed}"
        );
    }

    #[cfg(unix)]
    fn base_leak_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
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
        (fixture, repository, state)
    }

    #[cfg(unix)]
    fn interrupted_add(
        repository: &Path,
        state: &Path,
        destination: &Path,
        branch: &str,
    ) -> PathBuf {
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.to_path_buf(),
                destination: destination.to_path_buf(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from(branch)),
                state_dir: Some(state.to_path_buf()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::BaseReady),
            false,
        )
        .expect_err("simulated kill once the base exists");
        JournalStore::open(state)
            .load_all()
            .unwrap()
            .into_iter()
            .find(|journal| journal.phase != AddWorktreePhase::Active)
            .expect("interrupted add journal")
            .base_path
    }

    #[cfg(unix)]
    fn completion_marker(base_path: &Path) -> PathBuf {
        let tree = base_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        base_path.with_file_name(format!("{tree}.complete"))
    }

    /// `prepare_base` renames a staged base into place, then digests every
    /// file, then writes the completion marker. A kill during the digest left a
    /// whole base with no marker; rollback removed only the staging path, and —
    /// per D020 — neither repair nor gc deletes an unmarked path on inference,
    /// so the base leaked for good and `status` reported it on every run (#444).
    /// The interrupted add's own journal names that base, which authorizes its
    /// removal.
    #[cfg(unix)]
    #[test]
    fn repair_removes_a_base_an_interrupted_add_left_without_its_marker() {
        let (fixture, repository, state) = base_leak_fixture();
        let base = interrupted_add(
            &repository,
            &state,
            &fixture.path().join("killed"),
            "killed",
        );
        // The state a kill during the digest leaves: the base in place, no marker.
        fs::remove_file(completion_marker(&base)).unwrap();

        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(!base.exists(), "the unfinished base must not leak");
        let accounting = storage_accounting(&state).expect("status");
        assert!(
            accounting.diagnostic_issues.is_empty(),
            "nothing unexplained may remain: {:?}",
            accounting.diagnostic_issues
        );
    }

    /// A finished, marked base is a cache other adds reuse; rolling back one
    /// add that used it must leave it.
    #[cfg(unix)]
    #[test]
    fn repair_keeps_a_complete_base_when_rolling_back() {
        let (fixture, repository, state) = base_leak_fixture();
        let base = interrupted_add(
            &repository,
            &state,
            &fixture.path().join("killed"),
            "killed",
        );
        recover_incomplete_operations(&state).expect("repair");
        assert!(base.is_dir(), "a complete base is kept for reuse");
        assert!(completion_marker(&base).is_file());
    }

    /// An unmarked base that another journal still claims is not this
    /// rollback's to remove.
    #[cfg(unix)]
    #[test]
    fn repair_keeps_an_unmarked_base_another_journal_still_claims() {
        let (fixture, repository, state) = base_leak_fixture();
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: fixture.path().join("live"),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("live")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("an active worktree on the same tree");
        let base = interrupted_add(
            &repository,
            &state,
            &fixture.path().join("killed"),
            "killed",
        );
        fs::remove_file(completion_marker(&base)).unwrap();

        recover_incomplete_operations(&state).expect("repair");
        assert!(
            base.is_dir(),
            "the active worktree's journal still claims this base"
        );
    }

    /// Git creates missing leading directories for a new worktree and, when
    /// the add then fails, removes the worktree but leaves those directories
    /// (#423). A rolled-back Riftri add must do the same: remove exactly the
    /// destination it recorded, never the parents it created.
    #[cfg(unix)]
    #[test]
    fn a_rolled_back_add_removes_the_worktree_but_keeps_created_parents() {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let parents = fixture.path().join("agents").join("task-1");
        let destination = parents.join("worktree");
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
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::ViewCreated),
            true,
        )
        .expect_err("injected failure after the view was created");

        assert!(!destination.exists(), "rollback must remove the worktree");
        assert!(
            parents.is_dir(),
            "the created leading directories stay, as with Git"
        );
        let listed = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repository)
            .output()
            .expect("list worktrees");
        assert!(
            !String::from_utf8_lossy(&listed.stdout).contains("task-1"),
            "Git must no longer list the rolled-back worktree"
        );
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
                    sparse_directories: Vec::new(),
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

    /// Git already used the view of an interrupted add. A complete view is
    /// released as a plain Git worktree; one whose index was never
    /// synchronized is still preserved for manual attention.
    #[test]
    fn recovery_preserves_an_interrupted_view_whose_head_changed() {
        for (detached, change, phase) in [
            (true, "commit", AddWorktreePhase::IndexSynchronized),
            (true, "switch", AddWorktreePhase::IndexSynchronized),
            (false, "switch", AddWorktreePhase::IndexSynchronized),
            (false, "detach", AddWorktreePhase::IndexSynchronized),
            (false, "detach", AddWorktreePhase::GitPointerRestored),
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
                    sparse_directories: Vec::new(),
                },
                Some(phase),
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
            let released = phase == AddWorktreePhase::IndexSynchronized;
            for attempt in 0..2 {
                let report = recover_incomplete_operations(&state).expect("repair report");
                if released {
                    assert!(report.errors.is_empty(), "{detached}/{change}: {report:?}");
                    assert_eq!(
                        report.released_adds.len(),
                        usize::from(attempt == 0),
                        "{detached}/{change}: {report:?}"
                    );
                } else {
                    assert_eq!(report.recovered, 0, "{detached}/{change}: {report:?}");
                    assert_eq!(report.errors.len(), 1, "{report:?}");
                    assert!(report.errors[0].contains("HEAD"), "{report:?}");
                }
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
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::IndexSynchronized),
            false,
        )
        .expect_err("simulate process termination");
        fs::write(destination.join("tracked.txt"), "committed change\n").expect("change view");
        git(&destination, &["add", "--", "tracked.txt"]);
        git(&destination, &["commit", "--quiet", "-m", "preserve me"]);

        let report = recover_incomplete_operations(&state).expect("attempt recovery");

        // The complete view was released with its branch, not refused forever.
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.released_adds.len(), 1, "{report:?}");
        assert!(destination.exists());
        assert!(registered(&repository, &destination));
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved file"),
            "committed change\n"
        );
        assert_eq!(
            git_stdout(&repository, &["rev-parse", "feature/committed"]),
            git_stdout(&destination, &["rev-parse", "HEAD"]),
            "the branch holding the commit is kept"
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
                sparse_directories: Vec::new(),
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

    /// A managed view whose removal stopped right after `clean-verified`, as a
    /// kill does before the view is quarantined, plus that removal's
    /// journal-derived quarantine path.
    fn interrupted_removal_fixture(
        force: bool,
    ) -> (
        crate::test_support::WritableTempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
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
        fs::create_dir(repository.join("nested")).expect("create directory");
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        fs::write(repository.join("nested/deep.txt"), "deep\n").expect("write file");
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        let request = RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: destination.clone(),
            state_dir: Some(state.clone()),
        };
        let interrupted = if force {
            force_remove_worktree_inner(request, Some(RemoveWorktreePhase::CleanVerified))
        } else {
            remove_worktree_inner(request, Some(RemoveWorktreePhase::CleanVerified))
        };
        assert!(
            interrupted
                .expect_err("simulate a kill after the clean check")
                .to_string()
                .contains("injected removal failure")
        );
        let removals = RemovalJournalStore::open(&state)
            .load_all()
            .expect("load removal");
        assert_eq!(removals.len(), 1);
        let quarantine = fixture
            .path()
            .join(format!(".riftri-remove-{}", removals[0].operation_id));
        (fixture, repository, destination, state, quarantine)
    }

    fn registered(repository: &Path, destination: &Path) -> bool {
        // Git reports resolved paths; the destination itself may be gone.
        let destination = fs::canonicalize(destination.parent().unwrap())
            .unwrap()
            .join(destination.file_name().unwrap());
        let destination = destination.as_path();
        riftri_git::Git::default()
            .list_worktrees(repository)
            .expect("list worktrees")
            .iter()
            .any(|worktree| super::paths_match(&worktree.path, destination))
    }

    #[test]
    fn repair_finishes_a_removal_killed_after_quarantining_its_view() {
        for force in [false, true] {
            let (_fixture, repository, destination, state, quarantine) =
                interrupted_removal_fixture(force);
            // The kill lands after the view was renamed aside, before Git
            // dropped its registration.
            fs::rename(&destination, &quarantine).expect("quarantine the view");

            let report = recover_incomplete_operations(&state).expect("repair");
            assert!(report.errors.is_empty(), "force={force}: {report:?}");
            assert_eq!(report.recovered_removals, 1, "force={force}");
            assert!(!destination.exists(), "force={force}");
            assert!(
                !quarantine.exists(),
                "force={force}: repair must not strand the quarantined view"
            );
            assert!(!registered(&repository, &destination), "force={force}");
            assert_eq!(storage_accounting(&state).unwrap().active_views, 0);
        }
    }

    #[test]
    fn repair_finishes_deleting_a_quarantine_git_already_unregistered() {
        for force in [false, true] {
            let (_fixture, repository, destination, state, quarantine) =
                interrupted_removal_fixture(force);
            // The kill lands while the unregistered quarantine is being deleted.
            fs::rename(&destination, &quarantine).expect("quarantine the view");
            git(
                &repository,
                &["worktree", "remove", "--", destination.to_str().unwrap()],
            );
            fs::remove_file(quarantine.join("tracked.txt")).expect("partial delete");

            let report = recover_incomplete_operations(&state).expect("repair");
            assert!(report.errors.is_empty(), "force={force}: {report:?}");
            assert_eq!(report.recovered_removals, 1, "force={force}");
            assert!(!quarantine.exists(), "force={force}");
            assert!(!destination.exists(), "force={force}");
            assert!(!registered(&repository, &destination), "force={force}");
        }
    }

    #[test]
    fn repair_preserves_a_destination_that_reappears_beside_its_quarantine() {
        let (_fixture, repository, destination, state, quarantine) =
            interrupted_removal_fixture(false);
        fs::rename(&destination, &quarantine).expect("quarantine the view");
        fs::create_dir(&destination).expect("recreate the destination");
        fs::write(destination.join("new.txt"), "new\n").expect("write new content");

        for _ in 0..2 {
            let report = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(report.recovered_removals, 0);
            assert_eq!(report.errors.len(), 1, "{report:?}");
            assert_eq!(
                fs::read(destination.join("new.txt")).expect("new content kept"),
                b"new\n"
            );
            assert_eq!(
                fs::read(quarantine.join("tracked.txt")).expect("quarantine kept"),
                b"tracked\n"
            );
            assert!(registered(&repository, &destination));
        }
    }

    #[test]
    fn clean_removal_restores_a_view_written_after_its_quarantine() {
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
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::RemovalQuarantined,
            |quarantine| {
                fs::write(quarantine.join("late.txt"), "late write\n")
                    .expect("write into the quarantined view");
            },
        );

        let error = remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("a write racing a clean removal must stop it");

        assert!(error.to_string().contains("has changes"), "{error}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
        assert_eq!(accounting.cancelled_removals, 1, "{accounting:?}");
        assert_eq!(
            fs::read(destination.join("late.txt")).expect("late write restored"),
            b"late write\n"
        );
        assert!(registered(&repository, &destination));
        let leftovers = fs::read_dir(fixture.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".riftri-remove-"))
            .count();
        assert_eq!(leftovers, 0, "the quarantine must be renamed back");
    }

    /// Forced removal checked its snapshot only before the quarantine rename,
    /// so a write landing between that check and the rename was deleted with
    /// the quarantine and the removal reported success.
    #[test]
    fn forced_removal_restores_a_view_written_after_its_quarantine() {
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
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        // The forced removal authorizes deleting exactly this content.
        fs::write(destination.join("draft.txt"), "draft\n").expect("write forced content");
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::RemovalQuarantined,
            |quarantine| {
                fs::write(quarantine.join("late.txt"), "late write\n")
                    .expect("write into the quarantined view");
            },
        );

        let error = force_remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("a write racing a forced removal must stop it");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
        assert_eq!(accounting.cancelled_removals, 1, "{accounting:?}");

        assert!(
            error
                .to_string()
                .contains("changed after forced removal intent"),
            "{error}"
        );
        assert_eq!(
            fs::read(destination.join("late.txt")).expect("late write restored"),
            b"late write\n"
        );
        assert_eq!(fs::read(destination.join("draft.txt")).unwrap(), b"draft\n");
        assert!(registered(&repository, &destination));
        let leftovers = fs::read_dir(fixture.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".riftri-remove-"))
            .count();
        assert_eq!(leftovers, 0, "the quarantine must be renamed back");
    }

    /// A removal stopped before it removed anything, after which the worktree
    /// changed. Repair refused it forever ("recovery preserved it"), and
    /// would have removed the worktree once the change was undone. It now
    /// cancels the removal, keeps the worktree, and lets a later removal and
    /// journal retirement proceed.
    #[test]
    fn repair_cancels_a_removal_whose_worktree_changed_before_anything_was_removed() {
        for (force, phase) in [
            (false, RemoveWorktreePhase::IntentRecorded),
            (false, RemoveWorktreePhase::CleanVerified),
            (true, RemoveWorktreePhase::IntentRecorded),
            (true, RemoveWorktreePhase::CleanVerified),
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
                    mode: WorktreeMode::Detached,
                    state_dir: Some(state.clone()),
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
            let request = || RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            };
            let stopped = if force {
                force_remove_worktree_inner(request(), Some(phase))
            } else {
                remove_worktree_inner(request(), Some(phase))
            };
            stopped.expect_err("simulate process termination");
            fs::write(destination.join("mine.txt"), "user data\n").expect("write after intent");

            let report = recover_incomplete_operations(&state).expect("repair");

            assert!(report.errors.is_empty(), "{force} {phase:?}: {report:?}");
            assert_eq!(
                report.cancelled_removals, 1,
                "{force} {phase:?}: {report:?}"
            );
            assert_eq!(
                fs::read(destination.join("mine.txt")).unwrap(),
                b"user data\n"
            );
            assert!(registered(&repository, &destination));
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
            assert_eq!(accounting.cancelled_removals, 1, "{accounting:?}");
            assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");

            // Undoing the change no longer lets repair remove the worktree.
            fs::remove_file(destination.join("mine.txt")).expect("undo the change");
            let report = recover_incomplete_operations(&state).expect("repair again");
            assert!(report.errors.is_empty(), "{report:?}");
            assert!(
                destination.join("tracked.txt").exists(),
                "the worktree stays"
            );

            // A new removal works, and gc retires the whole lineage.
            remove_worktree_inner(request(), None).expect("remove the worktree");
            garbage_collect_inner(&state, true, None).expect("gc");
            let remaining = fs::read_dir(state.join("removals"))
                .map(|entries| entries.count())
                .unwrap_or(0);
            assert_eq!(remaining, 0, "{force} {phase:?}: removal journals retired");
        }
    }

    /// A FIFO in a worktree is invisible to `git status`, so compaction's
    /// snapshot met it first and failed as an I/O error to inspect. Now the
    /// check for entries outside the Git tree names it as a policy refusal.
    #[cfg(unix)]
    #[test]
    fn compaction_refuses_a_fifo_as_a_difference_from_its_checkout() {
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
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        fs::write(destination.join("tracked.txt"), "changed\n").unwrap();
        git(
            &destination,
            &["commit", "--quiet", "-am", "worth compacting"],
        );
        assert!(
            Command::new("mkfifo")
                .arg(destination.join("pipe"))
                .status()
                .expect("run mkfifo")
                .success()
        );

        let error = compact_worktree_inner(
            CompactWorktreeRequest {
                repository,
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("a FIFO is not part of the checkout");

        assert!(
            matches!(&error, super::WorktreeError::InvalidRequest(message)
                if message.contains("outside its exact Git tree")),
            "{error:?}"
        );
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
        assert!(destination.join("pipe").exists());
    }

    /// `git status` ignores FIFOs and sockets, so a worktree holding one is
    /// clean, and `git worktree remove` deletes it. Riftri unregistered the
    /// worktree, then failed to delete its quarantine ("unsupported filesystem
    /// entry"), leaving a removal every `repair` failed the same way.
    #[cfg(unix)]
    #[test]
    fn removal_deletes_fifos_and_sockets_like_git() {
        for force in [false, true] {
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
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
            fs::create_dir(destination.join("run")).unwrap();
            assert!(
                Command::new("mkfifo")
                    .arg(destination.join("run/pipe"))
                    .status()
                    .expect("run mkfifo")
                    .success()
            );
            let _socket = std::os::unix::net::UnixListener::bind(destination.join("run/sock"))
                .expect("create a socket");
            let request = RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                state_dir: Some(state.clone()),
            };

            if force {
                force_remove_worktree_inner(request, None)
            } else {
                remove_worktree_inner(request, None)
            }
            .expect("remove a worktree holding special files");

            assert!(!destination.exists(), "force={force}");
            assert!(!registered(&repository, &destination), "force={force}");
            let leftovers = fs::read_dir(fixture.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name.to_string_lossy().starts_with(".riftri-"))
                .count();
            assert_eq!(leftovers, 0, "force={force}: the quarantine is deleted");
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
            assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        }
    }

    #[test]
    fn a_removal_git_refuses_restores_the_quarantined_view() {
        let (fixture, repository, destination, state, _quarantine) =
            interrupted_removal_fixture(false);
        git(
            &repository,
            &["worktree", "lock", "--", destination.to_str().unwrap()],
        );

        let report = recover_incomplete_operations(&state).expect("repair report");
        assert_eq!(report.recovered_removals, 0);
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(report.errors[0].contains("locked"), "{report:?}");
        assert_eq!(
            fs::read(destination.join("tracked.txt")).expect("view restored"),
            b"tracked\n"
        );
        assert!(registered(&repository, &destination));
        let leftovers = fs::read_dir(fixture.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".riftri-remove-"))
            .count();
        assert_eq!(leftovers, 0, "the quarantine must be renamed back");
    }

    /// Git refuses to move or remove a locked worktree unless forced twice,
    /// and Riftri's single `--force` is not that. Refusing only after
    /// recording intent left a pending journal that blocked every later
    /// command until the user found `riftri repair` (#460).
    #[test]
    fn locked_worktrees_are_refused_before_any_journal_is_written() {
        for operation in ["move", "remove", "force-remove"] {
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
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
            let lock_target = destination.to_str().unwrap().to_owned();
            git(
                &repository,
                &["worktree", "lock", "--reason", "on usb", &lock_target],
            );
            let attempt = || match operation {
                "move" => move_worktree_inner(
                    MoveWorktreeRequest {
                        repository: repository.clone(),
                        source: destination.clone(),
                        destination: fixture.path().join("moved"),
                        state_dir: Some(state.clone()),
                    },
                    None,
                )
                .map(|_| ()),
                "remove" => remove_worktree_inner(
                    RemoveWorktreeRequest {
                        repository: repository.clone(),
                        destination: destination.clone(),
                        state_dir: Some(state.clone()),
                    },
                    None,
                )
                .map(|_| ()),
                _ => force_remove_worktree_inner(
                    RemoveWorktreeRequest {
                        repository: repository.clone(),
                        destination: destination.clone(),
                        state_dir: Some(state.clone()),
                    },
                    None,
                )
                .map(|_| ()),
            };

            let error = attempt().expect_err("a locked worktree must be refused");
            let message = error.to_string();
            assert!(
                matches!(error, super::WorktreeError::InvalidRequest(_)),
                "{operation}: {message}"
            );
            assert!(message.contains("is locked"), "{operation}: {message}");
            assert!(message.contains("on usb"), "{operation}: {message}");
            assert!(
                message.contains("git worktree unlock"),
                "{operation}: {message}"
            );
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_moves, 0, "{operation}");
            assert_eq!(accounting.pending_removals, 0, "{operation}");
            assert_eq!(
                fs::read(destination.join("tracked.txt")).expect("view intact"),
                b"tracked\n"
            );

            // Nothing pending blocks the same command once the user unlocks.
            git(&repository, &["worktree", "unlock", &lock_target]);
            attempt().unwrap_or_else(|error| panic!("{operation} after unlock: {error}"));
        }
    }

    #[test]
    fn pending_move_blocks_lifecycle_changes_until_repair() {
        let fixture = tempdir().expect("fixture directory");
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let source = fixture.path().join("source");
        let destination = fixture.path().join("destination");
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
        super::add_worktree(AddWorktreeRequest {
            repository: repository.clone(),
            destination: source.clone(),
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::Detached,
            state_dir: Some(state.clone()),
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");

        let request = MoveWorktreeRequest {
            repository: repository.clone(),
            source: source.clone(),
            destination: destination.clone(),
            state_dir: Some(state.clone()),
        };
        move_worktree_inner(request.clone(), Some(MoveWorktreePhase::IntentRecorded))
            .expect_err("stop after the move intent");
        assert_eq!(super::storage_accounting(&state).unwrap().pending_moves, 1);

        super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: source.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("removal must preserve the pending move source");
        super::force_remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: source.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("forced removal must preserve the pending move source");
        super::compact_worktree(CompactWorktreeRequest {
            repository: repository.clone(),
            destination: source.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("compaction must wait for the pending move");
        super::move_worktree(request).expect_err("a second move must wait for repair");
        assert_eq!(
            fs::read_to_string(source.join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(!destination.exists());

        let recovered = super::recover_incomplete_operations(&state).expect("repair pending move");
        assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
        assert_eq!(recovered.recovered_moves, 1);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(
            riftri_git::Git::default()
                .worktree_is_clean(&destination)
                .unwrap()
        );
        super::compact_worktree(CompactWorktreeRequest {
            repository: repository.clone(),
            destination: destination.clone(),
            state_dir: Some(state.clone()),
        })
        .expect("completed moves do not block compaction");
        super::remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: destination.clone(),
            state_dir: Some(state),
        })
        .expect("completed moves do not block removal");
        assert!(!destination.exists());
    }

    /// Git refuses an add onto a path it still registers, even after the
    /// directory is gone. Letting Git refuse after intent was recorded made
    /// rollback mistake that stale registration for its own and stop with a
    /// pending add (#461); refuse before anything is written instead.
    #[test]
    fn add_refuses_a_destination_git_still_registers() {
        for locked in [false, true] {
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
            let add = |branch: &str| {
                add_worktree_inner(
                    AddWorktreeRequest {
                        repository: repository.clone(),
                        destination: destination.clone(),
                        revision: OsString::from("HEAD"),
                        mode: WorktreeMode::NewBranch(OsString::from(branch)),
                        state_dir: Some(state.clone()),
                        sparse_directories: Vec::new(),
                    },
                    None,
                    true,
                )
            };
            add("first").expect("create worktree");
            let target = destination.to_str().unwrap().to_owned();
            if locked {
                git(&repository, &["worktree", "lock", &target]);
            }
            super::remove_tree_if_present(&destination).expect("delete the worktree directory");

            let error = add("second").expect_err("Git still registers the path");
            let message = error.to_string();
            assert!(
                matches!(error, super::WorktreeError::InvalidRequest(_)),
                "locked={locked}: {message}"
            );
            assert!(message.contains("already registered"), "{message}");
            assert!(message.contains("git worktree prune"), "{message}");
            if locked {
                assert!(message.contains("git worktree unlock"), "{message}");
            }
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(
                accounting.pending_adds, 0,
                "locked={locked}: {accounting:?}"
            );
            assert!(
                riftri_git::Git::default()
                    .local_branch_target(&repository, std::ffi::OsStr::new("second"))
                    .unwrap()
                    .is_none(),
                "locked={locked}: nothing may be created"
            );

            // Following the advice clears the way.
            if locked {
                git(&repository, &["worktree", "unlock", &target]);
            }
            git(&repository, &["worktree", "prune"]);
            add("second").expect("add after pruning the stale registration");
        }
    }

    fn same_destination_fixture() -> (
        crate::test_support::WritableTempDir,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
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
        for index in 0..50 {
            fs::write(repository.join(format!("file-{index}.txt")), "tracked\n")
                .expect("write file");
        }
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        (fixture, repository, destination, state)
    }

    fn detached_add_request(
        repository: &Path,
        destination: &Path,
        state: &Path,
    ) -> AddWorktreeRequest {
        AddWorktreeRequest {
            repository: repository.to_path_buf(),
            destination: destination.to_path_buf(),
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::Detached,
            state_dir: Some(state.to_path_buf()),
            sparse_directories: Vec::new(),
        }
    }

    /// Concurrent adds of one destination: a loser's rollback used to find
    /// the winner's registration, take it for its own, and remove it, even
    /// after the winner had reported success (#469).
    #[test]
    fn concurrent_adds_of_one_destination_keep_the_winner() {
        for round in 0..3 {
            let (_fixture, repository, destination, state) = same_destination_fixture();
            let results = std::thread::scope(|scope| {
                let handles = (0..6)
                    .map(|_| {
                        let request = detached_add_request(&repository, &destination, &state);
                        scope.spawn(move || add_worktree_inner(request, None, true))
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("add thread"))
                    .collect::<Vec<_>>()
            });

            let winners = results.iter().filter(|result| result.is_ok()).count();
            assert_eq!(winners, 1, "round {round}: {results:?}");
            for error in results.iter().filter_map(|result| result.as_ref().err()) {
                assert!(
                    matches!(error, super::WorktreeError::InvalidRequest(_)),
                    "round {round}: a loser must be refused, not rolled back: {error}"
                );
            }
            assert_eq!(
                fs::read(destination.join("file-0.txt")).expect("winner intact"),
                b"tracked\n",
                "round {round}"
            );
            assert!(registered(&repository, &destination), "round {round}");
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_adds, 0, "round {round}: {accounting:?}");
            assert!(
                accounting.diagnostic_issues.is_empty(),
                "round {round}: {accounting:?}"
            );
        }
    }

    /// An add interrupted before Git registered its worktree still claims the
    /// destination, so no later add can register that path before `repair`
    /// has decided what the interrupted one left there.
    #[test]
    fn an_interrupted_add_claims_its_destination_until_repair() {
        let (_fixture, repository, destination, state) = same_destination_fixture();
        add_worktree_inner(
            detached_add_request(&repository, &destination, &state),
            Some(AddWorktreePhase::IntentRecorded),
            false,
        )
        .expect_err("simulate a crash right after intent");

        let error = add_worktree_inner(
            detached_add_request(&repository, &destination, &state),
            None,
            true,
        )
        .expect_err("the interrupted add still claims the destination");
        let message = error.to_string();
        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{message}"
        );
        assert!(message.contains("riftri repair"), "{message}");
        assert!(!destination.exists(), "nothing may be created: {message}");

        let repaired = recover_incomplete_operations(&state).expect("repair");
        assert!(repaired.errors.is_empty(), "{repaired:?}");
        add_worktree_inner(
            detached_add_request(&repository, &destination, &state),
            None,
            true,
        )
        .expect("add after repair");
    }

    /// A base no active worktree references but an unfinished add journal
    /// still claims must be reported by both `gc` and `status`, not silently
    /// dropped from the plan. (Moved from the integration suite: racing adds
    /// no longer leave such a journal behind (#469), so an add interrupted
    /// once its base is ready provides it instead.)
    #[test]
    fn collection_names_the_journal_that_protects_an_unreferenced_base() {
        let (_fixture, repository, destination, state) = same_destination_fixture();
        add_worktree_inner(
            detached_add_request(&repository, &destination, &state),
            Some(AddWorktreePhase::BaseReady),
            false,
        )
        .expect_err("interrupt the add once its base is ready");

        let accounting = storage_accounting(&state).expect("account before collection");
        assert_eq!(accounting.bases.len(), 1);
        assert_eq!(accounting.bases[0].reference_count, 0);
        assert!(
            accounting
                .diagnostic_issues
                .iter()
                .any(|issue| issue.reason.contains("still claims it")),
            "status must explain the retained base: {:?}",
            accounting.diagnostic_issues
        );

        let report = super::garbage_collect(&state, true).expect("collect with a protected base");
        assert!(report.candidates.is_empty());
        assert!(report.collected.is_empty());
        assert!(
            !report.skipped_protected.is_empty(),
            "gc must account for the bases it refuses to collect"
        );
        let base_path = accounting.bases[0].path.clone();
        assert!(
            report
                .skipped_protected
                .iter()
                .all(|protection| protection.base_path == base_path
                    && !protection.operation_id.is_empty()),
            "every skip must name the journal responsible: {:?}",
            report.skipped_protected
        );

        // Once repair rolls the interrupted add back, the base is collectible.
        recover_incomplete_operations(&state).expect("repair before recollecting");
        let report = super::garbage_collect(&state, true).expect("collect after repair");
        assert_eq!(report.skipped_protected.len(), 0);
        assert_eq!(report.collected, [base_path]);
    }

    /// Git warns that an orphaned HEAD points to an invalid reference; the
    /// repository still has commits on other branches, so "no commits yet"
    /// about the whole repository sent people looking for a missing history.
    #[test]
    fn an_orphaned_head_names_its_branch_not_an_empty_repository() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        git(&repository, &["checkout", "--quiet", "--orphan", "fresh"]);

        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: fixture.path().join("worktree"),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(fixture.path().join("state")),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect_err("an unborn HEAD names no commit");

        let message = error.to_string();
        assert!(message.contains("fresh"), "{message}");
        assert!(message.contains("no commits yet"), "{message}");
        assert!(
            !message.contains("the repository has no commits"),
            "{message}"
        );
    }

    fn stable_root_fixture() -> (crate::test_support::WritableTempDir, PathBuf, PathBuf) {
        let fixture = tempdir().expect("fixture");
        let repository = fs::canonicalize(fixture.path())
            .expect("resolve fixture")
            .join("repository");
        let state = fixture.path().join("state");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::create_dir(repository.join("nested")).expect("create directory");
        fs::write(repository.join("nested/tracked.txt"), "tracked\n").expect("write file");
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        (fixture, repository, state)
    }

    fn add_from(invoked_in: &Path, destination: &Path, state: &Path) {
        add_worktree_inner(
            AddWorktreeRequest {
                repository: invoked_in.to_path_buf(),
                destination: destination.to_path_buf(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.to_path_buf()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
    }

    /// Git removes the worktree it is run from; Riftri renamed that view aside
    /// and then could not start Git there to unregister it (#475).
    #[test]
    fn a_worktree_can_be_removed_from_inside_itself() {
        for from_subdirectory in [false, true] {
            let (_fixture, repository, state) = stable_root_fixture();
            let destination = repository.with_file_name("worktree");
            add_from(&repository, &destination, &state);
            let invoked_in = if from_subdirectory {
                destination.join("nested")
            } else {
                destination.clone()
            };

            remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository: invoked_in,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                None,
            )
            .unwrap_or_else(|error| panic!("subdirectory={from_subdirectory}: {error}"));

            assert!(!destination.exists(), "subdirectory={from_subdirectory}");
            assert!(!registered(&repository, &destination));
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
        }
    }

    /// A worktree created from inside another linked worktree must stay
    /// manageable after that worktree is removed: its journal records the
    /// repository's main worktree, not the one that ran the add (#476).
    #[test]
    fn a_task_worktree_outlives_the_worktree_it_was_created_from() {
        let (_fixture, repository, state) = stable_root_fixture();
        let parent = repository.with_file_name("parent");
        let child = repository.with_file_name("child");
        add_from(&repository, &parent, &state);
        git(
            &parent,
            &["commit", "--quiet", "--allow-empty", "-m", "parent only"],
        );
        add_from(&parent, &child, &state);
        let head = |path: &Path| {
            riftri_git::Git::default()
                .resolve_revision(path, std::ffi::OsStr::new("HEAD"))
                .unwrap()
                .commit
        };
        assert_eq!(
            head(&child),
            head(&parent),
            "HEAD means the invoking worktree"
        );
        assert_ne!(head(&child), head(&repository));
        let recorded = JournalStore::open(&state)
            .load_all()
            .expect("journals")
            .into_iter()
            .find(|journal| journal.destination == child)
            .expect("child journal")
            .repository;
        // Git spells paths its own way on Windows; compare as Riftri does.
        assert!(
            super::paths_match(&recorded, &repository),
            "journals record the main worktree: {recorded:?}"
        );

        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: parent.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the parent worktree");
        let moved = repository.with_file_name("child-moved");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: child.clone(),
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move the child after its parent is gone");
        let status = storage_accounting(&state).expect("status");
        assert!(status.diagnostic_issues.is_empty(), "{status:?}");
        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: repository.clone(),
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("compact the child");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the child");
        let repaired = recover_incomplete_operations(&state).expect("repair");
        assert!(repaired.errors.is_empty(), "{repaired:?}");
    }

    /// Rewrite an add journal's recorded repository, reproducing a journal an
    /// older Riftri wrote when the add ran inside a linked worktree (#476).
    fn record_repository(state: &Path, destination: &Path, repository: &Path) {
        let journal = JournalStore::open(state)
            .load_all()
            .expect("journals")
            .into_iter()
            .find(|journal| journal.destination == destination)
            .expect("journal for destination");
        JournalStore::open(state)
            .update_active_repository(
                &journal.journal_path,
                destination,
                &journal.repository,
                repository,
            )
            .expect("rewrite journal");
    }

    /// Journals an older Riftri wrote while running inside a linked worktree
    /// name that worktree; once it is removed every command on the child
    /// failed. Repair re-homes them to the repository's main worktree.
    #[test]
    fn repair_rehomes_a_journal_that_names_a_removed_worktree() {
        let (_fixture, repository, state) = stable_root_fixture();
        let parent = repository.with_file_name("parent");
        let child = repository.with_file_name("child");
        add_from(&repository, &parent, &state);
        add_from(&parent, &child, &state);
        record_repository(&state, &child, &parent);
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: parent.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the parent worktree");

        let status = storage_accounting(&state).expect("status before repair");
        assert_eq!(status.diagnostic_issues.len(), 1, "{status:?}");
        assert!(
            status.diagnostic_issues[0]
                .reason
                .contains("`riftri repair`"),
            "{status:?}"
        );

        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.rehomed_adds, 1, "{report:?}");
        let again = recover_incomplete_operations(&state).expect("repeat repair");
        assert_eq!(again.rehomed_adds, 0, "re-homing is idempotent");
        let status = storage_accounting(&state).expect("status after repair");
        assert!(status.diagnostic_issues.is_empty(), "{status:?}");

        let moved = repository.with_file_name("child-moved");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: child,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move the re-homed child");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: moved,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove the re-homed child");
    }

    /// Re-homing must never attach a journal to a different repository that
    /// happens to have a worktree at the same path.
    #[test]
    fn repair_does_not_rehome_a_journal_onto_another_repository() {
        let (_fixture, repository, state) = stable_root_fixture();
        let child = repository.with_file_name("child");
        add_from(&repository, &child, &state);
        record_repository(&state, &child, &repository.with_file_name("gone"));
        // The destination now belongs to an unrelated repository.
        super::remove_tree_if_present(&child).expect("delete the child view");
        git(&repository, &["worktree", "prune"]);
        let other = repository.with_file_name("other");
        fs::create_dir(&other).expect("create other repository");
        git(&other, &["init", "--quiet"]);
        git(
            &other,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "other",
            ],
        );
        git(
            &other,
            // Relative path: Git cannot take the verbatim `\\?\` form on Windows.
            &["worktree", "add", "--quiet", "--detach", "../child"],
        );

        let report = recover_incomplete_operations(&state).expect("repair");
        assert_eq!(report.rehomed_adds, 0, "{report:?}");
        let recorded = JournalStore::open(&state)
            .load_all()
            .expect("journals")
            .into_iter()
            .find(|journal| journal.destination == child)
            .expect("journal kept")
            .repository;
        assert_eq!(recorded, repository.with_file_name("gone"));
    }

    /// `git clone --bare` followed by `git worktree add` from the bare
    /// directory is a common layout. Git's worktree commands all work there,
    /// and Riftri already managed the same repository from its linked
    /// worktrees, but refused every lifecycle command run from the bare
    /// directory itself.
    #[test]
    fn a_bare_repository_manages_its_worktrees_from_the_bare_directory() {
        let (fixture, source, _state) = stable_root_fixture();
        let root = source.parent().unwrap().to_path_buf();
        let bare = root.join("bare.git");
        let state = bare.join("riftri");
        // Relative paths: Git cannot take the verbatim `\\?\` form on Windows.
        git(
            &root,
            &["clone", "--quiet", "--bare", "repository", "bare.git"],
        );
        // A clone does not copy local config; Windows Git defaults autocrlf on.
        git(&bare, &["config", "core.autocrlf", "false"]);
        let first = root.join("first");
        add_from(&bare, &first, &state);
        assert_eq!(
            fs::read(first.join("nested/tracked.txt")).expect("view materialized"),
            b"tracked\n"
        );
        let recorded = JournalStore::open(&state).load_all().expect("journals")[0]
            .repository
            .clone();
        assert!(super::paths_match(&recorded, &bare), "{recorded:?}");

        let moved = root.join("first-moved");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: bare.clone(),
                source: first,
                destination: moved.clone(),
                state_dir: None,
            },
            None,
        )
        .expect("move from the bare directory");
        compact_worktree_inner(
            CompactWorktreeRequest {
                repository: bare.clone(),
                destination: moved.clone(),
                state_dir: None,
            },
            None,
        )
        .expect("compact from the bare directory");
        // Managed from inside a linked worktree as well.
        let second = root.join("second");
        add_from(&moved, &second, &state);
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: moved.clone(),
                destination: second.clone(),
                state_dir: None,
            },
            None,
        )
        .expect("remove from inside a linked worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: bare.clone(),
                destination: moved.clone(),
                state_dir: None,
            },
            None,
        )
        .expect("remove from the bare directory");

        assert!(!moved.exists() && !second.exists());
        let status = storage_accounting(&state).expect("status");
        assert!(status.diagnostic_issues.is_empty(), "{status:?}");
        assert_eq!(status.active_views, 0);
        drop(fixture);
    }

    /// macOS Git precomposes path arguments to NFC (`core.precomposeunicode`)
    /// and registers that spelling, while the file system keeps whatever
    /// spelling it was given. A worktree added under a decomposed (NFD) name,
    /// as Finder and many apps produce, was then never found in Git's
    /// registry: status reported it unregistered and every move or removal
    /// was refused.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_worktree_named_in_decomposed_unicode_stays_manageable() {
        let (_fixture, repository, state) = stable_root_fixture();
        let decomposed = repository.with_file_name("cafe\u{301}");
        let composed = repository.with_file_name("caf\u{e9}");
        add_from(&repository, &decomposed, &state);

        let status = storage_accounting(&state).expect("status");
        assert!(status.diagnostic_issues.is_empty(), "{status:?}");
        assert_eq!(status.active_views, 1);

        let moved = repository.with_file_name("moved-cafe\u{301}");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: composed,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move by the composed spelling");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: moved.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove by the decomposed spelling");
        assert!(!moved.exists());
        let status = storage_accounting(&state).expect("status after removal");
        assert!(status.diagnostic_issues.is_empty(), "{status:?}");
    }

    fn add_with_state(
        repository: &Path,
        destination: &Path,
        state: &Path,
    ) -> Result<super::AddWorktreeResult, super::WorktreeError> {
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.to_path_buf(),
                destination: destination.to_path_buf(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.to_path_buf()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
    }

    /// `--state-dir some/dir/.` names the same directory as `some/dir`, but
    /// creating it as spelled failed as an operational I/O error.
    #[test]
    fn a_state_directory_spelled_with_dot_components_is_created_once() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        add_with_state(&repository, &root.join("worktree"), &root.join("state/./"))
            .expect("add with a dot-component state directory");
        assert!(root.join("state/operations").is_dir());
        assert!(
            storage_accounting(&root.join("state"))
                .unwrap()
                .diagnostic_issues
                .is_empty()
        );
    }

    /// `..` after a directory that does not exist yet can only be resolved
    /// by creating that directory, which left it behind as litter.
    #[test]
    fn a_state_directory_climbing_out_of_a_missing_directory_is_refused() {
        let (fixture, repository, _state) = stable_root_fixture();
        // Not the canonical root: in a Windows verbatim path (`\\?\`), `..`
        // is an ordinary name, not a step to the parent.
        let root = fixture.path().to_path_buf();
        let error = add_with_state(
            &repository,
            &root.join("worktree"),
            &root.join("missing/../state"),
        )
        .expect_err("`..` out of a missing directory is refused");
        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{error}"
        );
        assert!(!root.join("missing").exists(), "nothing may be created");
        assert!(!root.join("state").exists());
        assert!(!root.join("worktree").exists());
    }

    /// A state directory inside the new worktree (or the reverse) was created
    /// first, which then made the add refuse its own destination as "taken by
    /// another worktree" and left the directories behind.
    #[test]
    fn a_state_directory_and_worktree_cannot_contain_each_other() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let worktree = root.join("worktree");
        let error = add_with_state(&repository, &worktree, &worktree.join("state"))
            .expect_err("state inside the worktree is refused");
        assert!(error.to_string().contains("inside"), "{error}");
        assert!(!worktree.exists(), "nothing may be created");

        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let error = add_with_state(&repository, &state.join("worktree"), &state)
            .expect_err("a worktree inside the state directory is refused");
        assert!(error.to_string().contains("inside"), "{error}");
        assert!(!state.join("worktree").exists());
    }

    /// Riftri materializes a tree under its base location and a staging
    /// sibling, both longer than the destination. A path Git checks out
    /// within the platform limit then failed deep inside the base build as an
    /// operational `checkout-index` error, which invites a pointless retry.
    #[cfg(unix)]
    #[test]
    fn a_tree_path_too_long_for_the_base_location_is_refused_up_front() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let destination = root.join("w");
        let state = root.join("state");
        // Fits under the destination, not under the ~120-byte-longer base path.
        let budget = libc::PATH_MAX as usize - 1 - destination.as_os_str().len() - 1 - 16;
        let mut long = String::new();
        while long.len() + 101 < budget {
            long.push_str(&"d".repeat(100));
            long.push('/');
        }
        long.push_str(&"f".repeat(budget - long.len()));
        let blob = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(&repository)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("hash an empty blob");
        let blob = String::from_utf8(blob.stdout).unwrap().trim().to_owned();
        git(
            &repository,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{long}"),
            ],
        );
        git(&repository, &["commit", "--quiet", "-m", "long path"]);

        let error = add_with_state(&repository, &destination, &state)
            .expect_err("a path that cannot fit under the base is refused");

        let message = error.to_string();
        assert!(
            matches!(error, super::WorktreeError::Unsupported(_)),
            "{message}"
        );
        assert!(message.contains("--state-dir"), "{message}");
        assert!(!destination.exists(), "nothing may be created");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.bases.is_empty(), "{accounting:?}");
        // No empty base bucket may be left behind for status to report.
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    /// The repository bucket was created just before the add's intent was
    /// journaled, so an add killed in between left an empty bucket that no
    /// journal owned and status reported as unexplained forever.
    #[test]
    fn the_base_bucket_is_created_only_after_add_intent_is_journaled() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let observed = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = std::rc::Rc::clone(&observed);
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::AddIntentPersist,
            move |bucket| seen.set(Some(bucket.exists())),
        );

        add_with_state(&repository, &root.join("view"), &state).expect("add");

        assert_eq!(
            observed.get(),
            Some(false),
            "the bucket must not exist before the journal that owns it"
        );
    }

    /// A Git child killed while writing the add's temporary index leaves
    /// `<index>.lock` in Riftri's state. Rollback removed the index but not
    /// the lock, and status reported the lock as unexplained forever.
    #[test]
    fn rollback_removes_the_git_lock_beside_the_temporary_index() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: root.join("view"),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::IntentRecorded),
            false,
        )
        .expect_err("interrupt the add as a kill would");
        let operation = fs::read_dir(state.join("operations"))
            .expect("read journals")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension() == Some(std::ffi::OsStr::new("json")))
            .and_then(|path| path.file_stem().map(std::ffi::OsStr::to_os_string))
            .expect("the interrupted add left its journal");
        let mut lock_name = OsString::from("index-");
        lock_name.push(&operation);
        lock_name.push(".lock");
        let lock = state.join("tmp").join(lock_name);
        fs::create_dir_all(lock.parent().unwrap()).expect("create temporary directory");
        fs::write(&lock, b"").expect("leave Git's index lock behind");

        let report = recover_incomplete_operations(&state).expect("repair");

        assert!(report.errors.is_empty(), "{report:?}");
        assert!(!lock.exists(), "rollback must remove Git's index lock");
        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    /// When Git itself refuses to register the worktree, the add rolls back.
    /// The base bucket it created up front stayed behind, empty, and `status`
    /// reported it as unexplained forever.
    #[cfg(unix)]
    #[test]
    fn a_rolled_back_add_leaves_no_empty_base_bucket() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let admin = repository.join(".git/worktrees");
        let refuse = |destination: &str| {
            fs::create_dir_all(&admin).unwrap();
            fs::set_permissions(&admin, fs::Permissions::from_mode(0o555)).unwrap();
            let refused = add_with_state(&repository, &root.join(destination), &state);
            fs::set_permissions(&admin, fs::Permissions::from_mode(0o755)).unwrap();
            refused.expect_err("Git cannot register the worktree");
        };

        refuse("first");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");

        // A bucket that holds a real base is never removed.
        add_with_state(&repository, &root.join("kept"), &state).expect("a later add works");
        refuse("second");
        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        assert_eq!(accounting.bases.len(), 1, "{accounting:?}");
        assert_eq!(accounting.active_views, 1, "{accounting:?}");
    }

    /// A directory created by a concurrent add between the two probes was
    /// taken for a dangling symbolic link, failing that add at random.
    #[test]
    fn a_parent_created_concurrently_is_not_a_dangling_link() {
        let fixture = tempdir().expect("fixture");
        let root = fs::canonicalize(fixture.path()).unwrap();
        let appearing = root.join("state");
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::DestinationParentProbe,
            |path| {
                fs::create_dir(path).expect("create the parent concurrently");
            },
        );

        let planned = super::planned_destination_parent(&appearing.join("child"))
            .expect("a real directory appearing concurrently is fine");

        assert_eq!(planned, appearing);
    }

    /// A name can be both a branch and a tag (a release branch and its tag,
    /// say). Git checks out the branch; Riftri resolved the bare name, which
    /// prefers the tag, and then refused because "the branch moved".
    #[test]
    fn an_existing_branch_add_resolves_the_branch_not_a_same_named_tag() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        git(&repository, &["branch", "release"]);
        git(
            &repository,
            &["commit", "--quiet", "--allow-empty", "-m", "later"],
        );
        git(&repository, &["tag", "release"]);
        let branch_commit = riftri_git::Git::default()
            .local_branch_target(&repository, std::ffi::OsStr::new("release"))
            .unwrap()
            .expect("branch exists");

        let result = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: root.join("release"),
                revision: OsString::from("release"),
                mode: WorktreeMode::ExistingBranch(OsString::from("release")),
                state_dir: Some(root.join("state")),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("check out the branch, as Git does");

        assert_eq!(result.commit, branch_commit);
        let head = Command::new("git")
            .args(["symbolic-ref", "HEAD"])
            .current_dir(root.join("release"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            "refs/heads/release"
        );
    }

    /// A tree or blob ID names an object, but not a commit: that is the same
    /// caller mistake as an unknown name, not an operational Git failure.
    #[test]
    fn a_detached_add_of_a_non_commit_object_is_an_invalid_request() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let tree = Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .current_dir(&repository)
            .output()
            .unwrap();
        let tree = String::from_utf8(tree.stdout).unwrap().trim().to_owned();

        let error = add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.clone(),
                destination: root.join("tree-view"),
                revision: OsString::from(&tree),
                mode: WorktreeMode::Detached,
                state_dir: Some(root.join("state")),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect_err("a tree is not a commit");

        let message = error.to_string();
        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{message}"
        );
        assert!(message.contains("does not name a commit"), "{message}");
    }

    /// Give `worktree` a submodule store, which makes `git worktree move`
    /// refuse it ("working trees containing submodules cannot be moved")
    /// without cloning a real submodule. Returns the store to delete later.
    fn add_submodule_store(worktree: &Path) -> PathBuf {
        let admin = Command::new("git")
            .args(["rev-parse", "--absolute-git-dir"])
            .current_dir(worktree)
            .output()
            .unwrap();
        let modules =
            PathBuf::from(String::from_utf8(admin.stdout).unwrap().trim()).join("modules");
        fs::create_dir(&modules).unwrap();
        modules
    }

    fn move_request(
        repository: &Path,
        source: &Path,
        destination: &Path,
        state: &Path,
    ) -> MoveWorktreeRequest {
        MoveWorktreeRequest {
            repository: repository.to_path_buf(),
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
            state_dir: Some(state.to_path_buf()),
        }
    }

    /// Git refuses some moves only once asked (a worktree holding submodules).
    /// The move journal stayed at intent, so remove, compact and another move
    /// all refused as "pending", and every repair retried the same refused
    /// move: the worktree was stuck for good.
    #[test]
    fn a_move_git_refuses_is_cancelled_and_leaves_the_worktree_usable() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let source = root.join("source");
        add_with_state(&repository, &source, &state).expect("add");
        let modules = add_submodule_store(&source);

        let error = move_worktree_inner(
            move_request(&repository, &source, &root.join("moved"), &state),
            None,
        )
        .expect_err("Git refuses to move a worktree with submodules");

        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{error}"
        );
        assert!(error.to_string().contains("submodules"), "{error}");
        assert!(source.is_dir() && !root.join("moved").exists());
        let accounting = super::storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_moves, 0, "{accounting:?}");
        assert_eq!(accounting.cancelled_moves, 1, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");

        // The worktree is usable again: once the submodule is gone it moves.
        fs::remove_dir(&modules).unwrap();
        move_worktree_inner(
            move_request(&repository, &source, &root.join("moved"), &state),
            None,
        )
        .expect("move after the cause is fixed");
        assert!(root.join("moved").is_dir());
    }

    /// A journal already stuck at intent (from an older release) is cancelled
    /// by repair instead of being reported for manual attention forever.
    #[test]
    fn repair_cancels_a_pending_move_git_refuses() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let source = root.join("source");
        add_with_state(&repository, &source, &state).expect("add");
        add_submodule_store(&source);
        move_worktree_inner(
            move_request(&repository, &source, &root.join("moved"), &state),
            Some(MoveWorktreePhase::IntentRecorded),
        )
        .expect_err("stop after the move intent");

        let report = recover_incomplete_operations(&state).expect("repair");

        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.cancelled_moves, 1, "{report:?}");
        let accounting = super::storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_moves, 0, "{accounting:?}");
        assert!(source.is_dir() && !root.join("moved").exists());
        let second = recover_incomplete_operations(&state).expect("second repair");
        assert!(second.errors.is_empty(), "{second:?}");
    }

    /// Moving a worktree into itself is refused before anything is recorded.
    #[test]
    fn a_move_into_the_worktree_itself_is_refused_up_front() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let source = root.join("source");
        add_with_state(&repository, &source, &state).expect("add");

        let error = move_worktree_inner(
            move_request(&repository, &source, &source.join("inner"), &state),
            None,
        )
        .expect_err("a worktree cannot move into itself");

        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{error}"
        );
        assert!(error.to_string().contains("into itself"), "{error}");
        let accounting = super::storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_moves, 0, "{accounting:?}");
        assert_eq!(
            accounting.cancelled_moves, 0,
            "nothing was recorded: {accounting:?}"
        );
    }

    /// Git refuses to remove a worktree with submodules without `--force`,
    /// because that deletes the submodule repositories kept in the worktree's
    /// administrative directory. Riftri moved the view aside first, and Git
    /// skips the check for a missing directory, so a clean removal deleted them.
    #[test]
    fn a_clean_removal_refuses_a_worktree_whose_submodule_store_git_protects() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let worktree = root.join("worktree");
        add_with_state(&repository, &worktree, &state).expect("add");
        let modules = add_submodule_store(&worktree);
        fs::write(modules.join("unpushed"), "submodule work\n").unwrap();

        let error = super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: worktree.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("Git refuses this without --force");

        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{error}"
        );
        assert!(error.to_string().contains("submodules"), "{error}");
        assert!(worktree.is_dir() && modules.join("unpushed").is_file());
        let accounting = super::storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
        assert_eq!(accounting.active_views, 1, "{accounting:?}");

        // `--force` removes it, as `git worktree remove --force` does.
        super::force_remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: worktree.clone(),
            state_dir: Some(state),
        })
        .expect("forced removal");
        assert!(!worktree.exists());
    }

    /// The other half of Git's check: a committed gitlink whose submodule is
    /// checked out, with no `modules` store (cloned by hand, say).
    #[test]
    fn a_clean_removal_refuses_a_worktree_with_a_checked_out_submodule() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let worktree = root.join("worktree");
        add_with_state(&repository, &worktree, &state).expect("add");
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .unwrap();
        let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
        git(
            &worktree,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},vendored"),
            ],
        );
        git(&worktree, &["commit", "--quiet", "-m", "vendor"]);
        git(
            &worktree,
            &[
                "clone",
                "--quiet",
                "--no-checkout",
                // Relative: Windows verbatim paths are not valid clone URLs.
                "../repository",
                "vendored",
            ],
        );
        git(
            &worktree.join("vendored"),
            &["checkout", "--quiet", "--detach", &head],
        );
        assert!(
            Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&worktree)
                .output()
                .unwrap()
                .stdout
                .is_empty(),
            "the worktree is clean by Git's status"
        );

        let error = super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: worktree.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("Git refuses this without --force");

        assert!(error.to_string().contains("submodules"), "{error}");
        assert!(worktree.join("vendored/.git").exists());
    }

    /// A base that fails its integrity check is preserved, and every add of
    /// its tree is refused. That refusal named no way out, `status` showed
    /// the base as merely "in use", and `repair` was silent, so the tree could
    /// not be added again until someone found every view built from it.
    #[cfg(unix)]
    #[test]
    fn a_damaged_base_names_its_users_and_is_reported_until_collected() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let first = root.join("first");
        let built = add_with_state(&repository, &first, &state).expect("first add");
        let mut pending = vec![built.base_path.clone()];
        let mut damaged_file = None;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let kind = fs::symlink_metadata(&path).unwrap().file_type();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    damaged_file = Some(path);
                }
            }
        }
        let damaged_file = damaged_file.expect("a regular file in the base");
        fs::set_permissions(
            damaged_file.parent().unwrap(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::set_permissions(&damaged_file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&damaged_file, "damaged\n").unwrap();

        let error = add_with_state(&repository, &root.join("second"), &state)
            .expect_err("a damaged base is never reused");
        let message = error.to_string();
        assert!(message.contains(first.to_str().unwrap()), "{message}");
        assert!(message.contains("riftri gc --apply"), "{message}");

        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.bases.len(), 1, "{accounting:?}");
        assert!(accounting.bases[0].damaged, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");

        // The way out it names works: remove the user, collect, add again.
        super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: first,
            state_dir: Some(state.clone()),
        })
        .expect("remove the view built from the damaged base");
        super::garbage_collect(&state, true).expect("collect the damaged base");
        assert!(!built.base_path.with_extension("damaged").exists());
        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.bases.is_empty(), "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        add_with_state(&repository, &root.join("third"), &state).expect("a rebuilt base");
        let accounting = storage_accounting(&state).expect("status");
        assert!(!accounting.bases[0].damaged, "{accounting:?}");
    }

    /// `git status` hides a modified file marked assume-unchanged or
    /// skip-worktree, so compaction passed its pristine check and only its
    /// snapshot comparison caught the difference. It preserved the view but
    /// left the compaction pending, with its replacement directory beside the
    /// worktree, so remove, move and another compaction refused until repair.
    #[test]
    fn a_compaction_whose_view_differs_from_its_tree_is_cancelled() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let view = root.join("view");
        add_with_state(&repository, &view, &state).expect("add");
        let tracked = Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(&view)
            .output()
            .unwrap();
        let tracked = String::from_utf8(tracked.stdout).unwrap();
        let tracked = tracked
            .split('\0')
            .find(|path| !path.is_empty())
            .unwrap()
            .to_owned();
        git(&view, &["update-index", "--assume-unchanged", &tracked]);
        let file = view.join(&tracked);
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&file, permissions).unwrap();
        fs::write(&file, "hidden local change\n").unwrap();

        let error = super::compact_worktree(CompactWorktreeRequest {
            repository: repository.clone(),
            destination: view.clone(),
            state_dir: Some(state.clone()),
        })
        .expect_err("the view is not a fresh checkout of its tree");

        let message = error.to_string();
        assert!(message.contains("assume-unchanged"), "{message}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "hidden local change\n");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_compactions, 0, "{accounting:?}");
        assert_eq!(accounting.cancelled_compactions, 1, "{accounting:?}");
        let leftovers = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".riftri-compact-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // Nothing is pending, so the worktree's other lifecycle commands work.
        super::force_remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: view.clone(),
            state_dir: Some(state),
        })
        .expect("forced removal without a repair first");
        assert!(!view.exists());
    }

    fn journal_files(state: &Path, directory: &str) -> Vec<String> {
        let mut names = fs::read_dir(state.join(directory))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Finished journal history was never deleted, and every command reads all
    /// of it: after 1,000 add/remove cycles, status and add ran several times
    /// slower. `gc --apply` retires a lineage once its worktree is gone for
    /// good, and never touches a live one.
    #[test]
    fn gc_retires_finished_journal_history_and_keeps_live_history() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        add_with_state(&repository, &root.join("first"), &state).expect("add");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: root.join("first"),
                destination: root.join("moved"),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("move");
        super::compact_worktree(CompactWorktreeRequest {
            repository: repository.clone(),
            destination: root.join("moved"),
            state_dir: Some(state.clone()),
        })
        .expect("compact");
        super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: root.join("moved"),
            state_dir: Some(state.clone()),
        })
        .expect("remove");
        let live = add_with_state(&repository, &root.join("live"), &state).expect("live add");
        let live_journal = live
            .journal_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let plan = super::garbage_collect(&state, false).expect("plan");
        assert_eq!(plan.retirable_journals, 4, "{plan:?}");
        assert_eq!(plan.retired_journals, 0, "{plan:?}");
        assert_eq!(
            journal_files(&state, "removals").len(),
            1,
            "a plan deletes nothing"
        );

        let applied = super::garbage_collect(&state, true).expect("apply");
        assert_eq!(applied.retired_journals, 4, "{applied:?}");
        for directory in ["removals", "moves", "compactions"] {
            assert!(journal_files(&state, directory).is_empty(), "{directory}");
        }
        let operations = journal_files(&state, "operations");
        assert!(operations.contains(&live_journal), "{operations:?}");
        assert_eq!(
            operations.len(),
            2,
            "the live journal and its lock: {operations:?}"
        );
        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        assert_eq!(accounting.active_views, 1, "{accounting:?}");
        assert!(
            recover_incomplete_operations(&state)
                .expect("repair")
                .errors
                .is_empty()
        );

        // The live worktree's own history is retired once it is gone too.
        super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: root.join("live"),
            state_dir: Some(state.clone()),
        })
        .expect("remove the live worktree");
        let applied = super::garbage_collect(&state, true).expect("second apply");
        assert_eq!(applied.collected.len(), 1, "{applied:?}");
        assert!(journal_files(&state, "operations").is_empty());
        assert!(journal_files(&state, "removals").is_empty());
        // This run's own collection journal stays visible until the next run.
        assert_eq!(journal_files(&state, "collections").len(), 1);
        let third = super::garbage_collect(&state, true).expect("third apply");
        assert_eq!(third.retired_journals, 1, "{third:?}");
        assert!(journal_files(&state, "collections").is_empty());
        assert!(
            storage_accounting(&state)
                .unwrap()
                .diagnostic_issues
                .is_empty()
        );
    }

    /// A retirement interrupted after its add journal became `<id>.retired`
    /// leaves a finished removal without its add journal. That state is
    /// explained, not an issue, and the next `gc --apply` finishes it. Without
    /// the marker, the same removal still reports its missing add journal.
    #[test]
    fn an_interrupted_journal_retirement_is_explained_and_finished_later() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let added = add_with_state(&repository, &root.join("view"), &state).expect("add");
        super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: root.join("view"),
            state_dir: Some(state.clone()),
        })
        .expect("remove");
        fs::rename(
            &added.journal_path,
            added.journal_path.with_extension("retired"),
        )
        .expect("simulate the interrupted rename");

        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        let repair = recover_incomplete_operations(&state).expect("repair");
        assert!(repair.errors.is_empty(), "{repair:?}");
        let applied = super::garbage_collect(&state, true).expect("apply");
        assert!(applied.retired_journals >= 1, "{applied:?}");
        assert!(journal_files(&state, "operations").is_empty());
        assert!(journal_files(&state, "removals").is_empty());

        // A missing add journal with no retirement marker is still reported.
        let second = add_with_state(&repository, &root.join("other"), &state).expect("add");
        super::remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: root.join("other"),
            state_dir: Some(state.clone()),
        })
        .expect("remove");
        fs::remove_file(&second.journal_path).expect("lose the add journal");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.diagnostic_issues.len(), 1, "{accounting:?}");
    }

    /// Without `--state-dir`, lifecycle commands read only the default state
    /// directory (D026). A worktree managed in a registered custom state
    /// directory was then reported as "not an active Riftri-managed
    /// worktree", although the repository records exactly where it is managed.
    #[test]
    fn a_refusal_names_the_registered_state_directory_that_manages_the_worktree() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let custom = root.join("custom-state");
        let worktree = root.join("view");
        add_with_state(&repository, &worktree, &custom).expect("add with a custom state");
        let custom = fs::canonicalize(&custom).unwrap();

        let error = super::remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination: worktree.clone(),
            state_dir: None,
        })
        .expect_err("the default state directory does not manage it");
        let message = error.to_string();
        assert!(message.contains(custom.to_str().unwrap()), "{message}");
        assert!(message.contains("--state-dir"), "{message}");

        // An explicit, different state directory keeps the plain refusal.
        let error = super::remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: worktree.clone(),
            state_dir: Some(root.join("other-state")),
        })
        .expect_err("that state directory does not manage it");
        assert!(!error.to_string().contains("registered"), "{error}");
        assert!(worktree.is_dir());
    }

    /// A worktree named `w` inside `.git/worktrees` would be its own Git
    /// metadata directory. The add failed there as `rollback-failed` and stayed
    /// pending, and `riftri repair` could not roll it back. It is refused before
    /// anything is recorded; elsewhere in `.git`, Riftri accepts what Git does.
    #[test]
    fn a_destination_inside_git_worktree_metadata_is_refused_up_front() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        add_with_state(&repository, &root.join("first"), &state).expect("add");
        let inside_admin = repository.join(".git/worktrees/view");

        let error = add_with_state(&repository, &inside_admin, &state)
            .expect_err("Git's worktree metadata directory is refused");

        assert!(
            matches!(error, super::WorktreeError::InvalidRequest(_)),
            "{error}"
        );
        assert!(!inside_admin.exists());
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_adds, 0, "{accounting:?}");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
        let listed = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&listed.stdout).contains("worktrees/view"));

        // Elsewhere in `.git`, as with Git, the add works.
        add_with_state(&repository, &repository.join(".git/elsewhere"), &state)
            .expect("Git accepts this, so Riftri does too");
    }

    /// A view removed, moved, or swapped by another lifecycle operation while
    /// `status` measures it counts as nothing instead of failing the report.
    #[test]
    fn accounting_counts_a_path_that_vanished_mid_scan_as_empty() {
        let fixture = tempdir().expect("fixture");
        assert_eq!(
            super::tree_usage(&fixture.path().join("gone")).unwrap(),
            (0, 0)
        );

        let directory = fixture.path().join("present");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("file"), "12345").unwrap();
        let (logical, _) = super::tree_usage(&directory).unwrap();
        assert_eq!(logical, 5);
    }

    /// A copy of `removal` under another operation ID: what a concurrent
    /// `riftri repair` wrote when it retired an add journal whose removal had
    /// just finished.
    fn write_duplicate_removal(removal: &Path) -> PathBuf {
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(removal).unwrap()).unwrap();
        let id = record["operation_id"].as_str().unwrap().to_owned();
        let duplicate_id = format!("{}9", &id[..id.len() - 1]);
        record["operation_id"] = serde_json::Value::String(duplicate_id.clone());
        let path = removal.with_file_name(format!("{duplicate_id}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        path
    }

    /// `gc --apply` decided what to retire from journals read before it took
    /// the add operation's lock. A removal recorded in between (a concurrent
    /// repair retiring the same worktree) was left with no add journal, and
    /// every lifecycle command then failed on it. The lineage is re-read
    /// under the lock.
    #[test]
    fn journal_retirement_deletes_a_removal_recorded_after_its_snapshot() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        add_with_state(&repository, &root.join("view"), &state).expect("add");
        super::remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: root.join("view"),
            state_dir: Some(state.clone()),
        })
        .expect("remove");
        let removal = fs::read_dir(state.join("removals"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .next()
            .unwrap();
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::JournalRetirementLocked,
            move |_| {
                write_duplicate_removal(&removal);
            },
        );

        super::garbage_collect(&state, true).expect("apply");

        assert!(journal_files(&state, "removals").is_empty());
        assert!(journal_files(&state, "operations").is_empty());
        let accounting = storage_accounting(&state).expect("status");
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    /// Repair retires an add journal whose worktree vanished by recording a
    /// completed removal. When the worktree vanished because a removal had
    /// just finished, a second record is never needed.
    #[test]
    fn retiring_a_vanished_worktree_records_no_second_removal() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let added = add_with_state(&repository, &root.join("view"), &state).expect("add");
        super::remove_worktree(RemoveWorktreeRequest {
            repository,
            destination: root.join("view"),
            state_dir: Some(state.clone()),
        })
        .expect("remove");
        let state = fs::canonicalize(&state).unwrap();
        let add = JournalStore::open(&state)
            .load_all()
            .unwrap()
            .into_iter()
            .find(|journal| journal.journal_path.file_name() == added.journal_path.file_name())
            .unwrap();

        super::retire_vanished_add_journal(&riftri_git::Git::default(), &state, &add)
            .expect("nothing to retire");

        assert_eq!(journal_files(&state, "removals").len(), 1);
    }

    /// A running removal or move holds its worktree's add lock for the whole
    /// transaction. `riftri repair` resumed such journals anyway, so a repair
    /// that ran alongside a live `remove` or `move` drove the same journal
    /// from two processes. It now skips them while the lock is held.
    #[test]
    fn repair_leaves_a_removal_or_move_its_owner_is_running() {
        let (_fixture, repository, _state) = stable_root_fixture();
        let root = repository.parent().unwrap().to_path_buf();
        let state = root.join("state");
        let removed = add_with_state(&repository, &root.join("removed"), &state).expect("add");
        let moved = add_with_state(&repository, &root.join("moved"), &state).expect("add");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: root.join("removed"),
                state_dir: Some(state.clone()),
            },
            Some(RemoveWorktreePhase::IntentRecorded),
        )
        .expect_err("stop after the removal intent");
        move_worktree_inner(
            MoveWorktreeRequest {
                repository: repository.clone(),
                source: root.join("moved"),
                destination: root.join("moved-to"),
                state_dir: Some(state.clone()),
            },
            Some(MoveWorktreePhase::IntentRecorded),
        )
        .expect_err("stop after the move intent");

        {
            // Stand in for the running owners.
            let _removal_owner = super::try_lock_add_operation(&removed.journal_path)
                .unwrap()
                .expect("lock");
            let _move_owner = super::try_lock_add_operation(&moved.journal_path)
                .unwrap()
                .expect("lock");
            let report = recover_incomplete_operations(&state).expect("repair");
            assert!(report.errors.is_empty(), "{report:?}");
            // The add pass counts the same two locked operations as busy too.
            assert!(report.busy_adds >= 2, "{report:?}");
            let accounting = storage_accounting(&state).expect("status");
            assert_eq!(accounting.pending_removals, 1, "{accounting:?}");
            assert_eq!(accounting.pending_moves, 1, "{accounting:?}");
        }

        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "{report:?}");
        let accounting = storage_accounting(&state).expect("status");
        assert_eq!(accounting.pending_removals, 0, "{accounting:?}");
        assert_eq!(accounting.pending_moves, 0, "{accounting:?}");
        assert!(root.join("moved-to").is_dir());
        assert!(!root.join("removed").exists());
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
                sparse_directories: Vec::new(),
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
        // The refusal removed nothing, so the removal was cancelled rather
        // than left for a repair that would refuse it forever.
        for _ in 0..2 {
            let recovery = recover_incomplete_operations(&state).expect("repair report");
            assert_eq!(recovery.recovered_removals, 0);
            assert!(recovery.errors.is_empty(), "{recovery:?}");
            assert!(destination.exists());
        }
        assert_eq!(
            storage_accounting(&state).unwrap().cancelled_removals,
            1,
            "the refused removal is cancelled"
        );
        let accounting = storage_accounting(&state).expect("retained accounting");
        assert_eq!(accounting.active_views, 1);
        assert_eq!(accounting.bases[0].reference_count, 1);
    }

    #[test]
    fn forced_removal_recovery_preserves_later_index_and_head_changes() {
        for change in ["index", "head", "legacy-v1-snapshot", "legacy-v2-snapshot"] {
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
            git(&repository, &["add", "tracked.txt"]);
            git(&repository, &["commit", "--quiet", "-m", "initial"]);
            super::add_worktree(AddWorktreeRequest {
                repository: repository.clone(),
                destination: destination.clone(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::Detached,
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            })
            .unwrap();
            force_remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(RemoveWorktreePhase::IntentRecorded),
            )
            .expect_err("interrupt force intent");
            if change == "index" {
                fs::write(destination.join("tracked.txt"), "later staged work\n").unwrap();
                git(&destination, &["add", "tracked.txt"]);
                fs::write(destination.join("tracked.txt"), "tracked\n").unwrap();
            } else if change == "head" {
                git(
                    &destination,
                    &["commit", "--quiet", "--allow-empty", "-m", "later commit"],
                );
            } else {
                use sha2::{Digest, Sha256};
                let mut digest = Sha256::new();
                let content = crate::base_integrity::marker(&destination).unwrap();
                if change == "legacy-v1-snapshot" {
                    digest.update(b"riftri-forced-removal-snapshot-v1\0");
                    digest.update(&content);
                } else {
                    // The exact digest a v2 binary recorded: the content
                    // marker and Git state, with no metadata pass.
                    digest.update(b"riftri-forced-removal-snapshot-v2\0");
                    digest.update((content.len() as u64).to_le_bytes());
                    digest.update(&content);
                    digest.update(
                        super::Git::default()
                            .worktree_removal_state(&destination)
                            .unwrap(),
                    );
                }
                let store = RemovalJournalStore::open(&state);
                let journal = store.load_all().unwrap().remove(0);
                let mut record = store.reload(&journal).unwrap();
                record.force_snapshot = Some(crate::base_integrity::hex_lower(digest.finalize()));
                store.persist(&record).unwrap();
            }
            // Nothing was removed yet, so repair cancels the removal and
            // keeps the changed worktree.
            for attempt in 0..2 {
                let report = recover_incomplete_operations(&state).unwrap();
                assert_eq!(report.recovered_removals, 0, "{change}: {report:?}");
                assert!(report.errors.is_empty(), "{change}: {report:?}");
                assert_eq!(
                    report.cancelled_removals,
                    usize::from(attempt == 0),
                    "{change}: {report:?}"
                );
                assert!(destination.exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn forced_removal_recovery_preserves_later_metadata_only_changes() {
        use std::os::unix::fs::PermissionsExt;

        #[cfg(target_os = "macos")]
        const ATTRIBUTE: &str = "com.riftri.forced-removal-test";
        #[cfg(not(target_os = "macos"))]
        const ATTRIBUTE: &str = "user.riftri.forced-removal-test";

        for change in ["xattr", "special-bit"] {
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
                    sparse_directories: Vec::new(),
                },
                None,
                true,
            )
            .expect("create worktree");
            fs::create_dir(destination.join("scratch")).expect("create scratch directory");

            force_remove_worktree_inner(
                RemoveWorktreeRequest {
                    repository,
                    destination: destination.clone(),
                    state_dir: Some(state.clone()),
                },
                Some(RemoveWorktreePhase::IntentRecorded),
            )
            .expect_err("simulate interruption after durable force intent");
            assert!(destination.exists());

            // Metadata-only changes: neither edits file contents nor Git
            // state, so only the snapshot's metadata pass can see them.
            if change == "xattr" {
                rustix::fs::setxattr(
                    destination.join("tracked.txt"),
                    ATTRIBUTE,
                    b"added after intent",
                    rustix::fs::XattrFlags::empty(),
                )
                .expect("set xattr after intent");
            } else {
                fs::set_permissions(
                    destination.join("scratch"),
                    fs::Permissions::from_mode(0o1755),
                )
                .expect("set sticky bit after intent");
            }

            for attempt in 0..2 {
                let recovery = recover_incomplete_operations(&state).expect("repair report");
                assert_eq!(recovery.recovered_removals, 0, "{change}: {recovery:?}");
                assert!(recovery.errors.is_empty(), "{change}: {recovery:?}");
                assert_eq!(
                    recovery.cancelled_removals,
                    usize::from(attempt == 0),
                    "{change}: {recovery:?}"
                );
                assert!(destination.exists(), "{change}");
            }

            if change == "xattr" {
                let mut value: Vec<u8> = Vec::with_capacity(64);
                rustix::fs::lgetxattr(
                    destination.join("tracked.txt"),
                    ATTRIBUTE,
                    rustix::buffer::spare_capacity(&mut value),
                )
                .expect("read preserved xattr");
                assert_eq!(value, b"added after intent");
            } else {
                let mode = fs::symlink_metadata(destination.join("scratch"))
                    .expect("inspect preserved directory")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o7777, 0o1755, "{mode:o}");
            }
        }
    }

    #[test]
    fn legacy_v1_marker_base_upgrades_with_one_rebuild() {
        let fixture = tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
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
        let request = |name: &str| AddWorktreeRequest {
            repository: repository.clone(),
            destination: fixture.path().join(name),
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::Detached,
            state_dir: Some(state.clone()),
            sparse_directories: Vec::new(),
        };
        let first = add_worktree_inner(request("first"), None, true).expect("first view");
        let marker_path = first.base_path.with_extension("complete");
        assert!(
            fs::read(&marker_path)
                .expect("fresh marker")
                .starts_with(crate::base_integrity::MARKER_V2_PREFIX)
        );

        // Rewrite the completion marker exactly as a pre-v2 binary recorded
        // it: the v1 content digest of the same base.
        fs::write(
            &marker_path,
            crate::base_integrity::marker(&first.base_path).expect("v1 digest"),
        )
        .expect("write legacy marker");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // Metadata tampering is invisible to a v1 marker. The migration
            // rebuild must discard it rather than clone it into views.
            let file = first.base_path.join("tracked.txt");
            let mode = fs::symlink_metadata(&file)
                .expect("base file metadata")
                .permissions()
                .mode();
            fs::set_permissions(&file, fs::Permissions::from_mode((mode & 0o777) | 0o4000))
                .expect("set setuid bit");
        }

        let second = add_worktree_inner(request("second"), None, true).expect("migrating view");
        assert!(!second.reused_base, "a v1 marker must trigger one rebuild");
        let upgraded = fs::read(&marker_path).expect("upgraded marker");
        assert!(upgraded.starts_with(crate::base_integrity::MARKER_V2_PREFIX));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            for path in [
                first.base_path.join("tracked.txt"),
                second.destination.join("tracked.txt"),
            ] {
                let mode = fs::symlink_metadata(&path)
                    .expect("rebuilt metadata")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o7000, 0, "{}: {mode:o}", path.display());
            }
        }

        let third = add_worktree_inner(request("third"), None, true).expect("cached view");
        assert!(third.reused_base, "the upgraded marker must be reusable");
        assert_eq!(fs::read(&marker_path).expect("stable marker"), upgraded);
        for destination in [&first.destination, &second.destination, &third.destination] {
            git(destination, &["status", "--porcelain=v1"]);
        }
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
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        fs::write(destination.join("untracked.txt"), "explicitly discarded\n")
            .expect("dirty worktree");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // Metadata present before force intent must not cause a false
            // refusal: the snapshot records and re-verifies it unchanged.
            #[cfg(target_os = "macos")]
            let attribute = "com.riftri.forced-removal-test";
            #[cfg(not(target_os = "macos"))]
            let attribute = "user.riftri.forced-removal-test";
            fs::create_dir(destination.join("scratch")).expect("create scratch directory");
            fs::set_permissions(
                destination.join("scratch"),
                fs::Permissions::from_mode(0o1755),
            )
            .expect("set pre-intent sticky bit");
            rustix::fs::setxattr(
                destination.join("untracked.txt"),
                attribute,
                b"recorded before intent",
                rustix::fs::XattrFlags::empty(),
            )
            .expect("set pre-intent xattr");
        }

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

        git(&destination, &["status", "--porcelain"]);
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
                    sparse_directories: Vec::new(),
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
                sparse_directories: Vec::new(),
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
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.cancelled_removals, 1, "{report:?}");
        assert!(destination.is_dir());
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read preserved change"),
            "changed after intent\n"
        );
    }

    #[test]
    fn interrupted_add_recovery_preserves_ignored_and_empty_entries() {
        for (phase, sparse) in [
            (AddWorktreePhase::GitPointerRestored, false),
            (AddWorktreePhase::IndexSynchronized, false),
            (AddWorktreePhase::GitPointerRestored, true),
            (AddWorktreePhase::IndexSynchronized, true),
        ] {
            for private_entry in ["ignored-file", "ignored-directory", "empty-directory"] {
                let fixture = tempdir().unwrap();
                let repository = fixture.path().join("repository");
                let destination = fixture.path().join("view");
                let state = fixture.path().join("state");
                fs::create_dir(&repository).unwrap();
                git(&repository, &["init", "--quiet"]);
                git(&repository, &["config", "user.name", "Riftri Tests"]);
                git(
                    &repository,
                    &["config", "user.email", "riftri@example.invalid"],
                );
                git(&repository, &["config", "core.autocrlf", "false"]);
                fs::write(repository.join("tracked.txt"), "original\n").unwrap();
                fs::write(repository.join(".gitignore"), "secret.txt\nprivate/\n").unwrap();
                fs::create_dir(repository.join("selected")).unwrap();
                fs::write(repository.join("selected/file"), "selected\n").unwrap();
                git(&repository, &["add", "."]);
                git(&repository, &["commit", "--quiet", "-m", "initial"]);
                add_worktree_inner(
                    AddWorktreeRequest {
                        repository: repository.clone(),
                        destination: destination.clone(),
                        revision: OsString::from("HEAD"),
                        mode: WorktreeMode::Detached,
                        state_dir: Some(state.clone()),
                        sparse_directories: if sparse {
                            vec!["selected".to_owned()]
                        } else {
                            vec![]
                        },
                    },
                    Some(phase),
                    false,
                )
                .expect_err("interrupt creation");
                if phase == AddWorktreePhase::GitPointerRestored {
                    if sparse {
                        riftri_git::Git::default()
                            .synchronize_sparse_worktree_index(
                                &destination,
                                &["selected".to_owned()],
                            )
                            .unwrap();
                    } else {
                        riftri_git::Git::default()
                            .synchronize_worktree_index(&destination)
                            .unwrap();
                    }
                }
                let private_path = match private_entry {
                    "ignored-file" => destination.join("secret.txt"),
                    "ignored-directory" => {
                        fs::create_dir(destination.join("private")).unwrap();
                        destination.join("private/notes.txt")
                    }
                    "empty-directory" => destination.join("empty-private-directory"),
                    _ => unreachable!(),
                };
                if private_entry == "empty-directory" {
                    fs::create_dir(&private_path).unwrap();
                } else {
                    fs::write(&private_path, "private data\n").unwrap();
                }
                assert!(
                    riftri_git::Git::default()
                        .worktree_is_clean(&destination)
                        .unwrap()
                );
                for _ in 0..2 {
                    let report = recover_incomplete_operations(&state).unwrap();
                    assert_eq!(
                        report.recovered, 0,
                        "{private_entry}, {phase:?}: {report:?}"
                    );
                    assert_eq!(report.errors.len(), 1, "{report:?}");
                    assert!(private_path.exists());
                    if private_entry != "empty-directory" {
                        assert_eq!(fs::read(&private_path).unwrap(), b"private data\n");
                    }
                }
            }
        }
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
                    sparse_directories: Vec::new(),
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

    /// Build a repository, add one managed worktree, and remove it again so
    /// its immutable base stays on disk with no referencing journal — the
    /// starting state for every collection-cancellation test.
    fn unreferenced_base_fixture(root: &Path, branch: &str) -> (PathBuf, PathBuf, PathBuf) {
        let repository = root.join("repository");
        let destination = root.join("worktree");
        let state = root.join("state");
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
                mode: WorktreeMode::NewBranch(OsString::from(branch)),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination,
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove worktree");
        (repository, state, added.base_path)
    }

    /// Record an add journal for the same base and fail it before
    /// `prepare_base` runs — the racing reference from the issue: it protects
    /// the base while it exists and later rolls back without rebuilding
    /// anything.
    fn race_reference_before_prepare_base(repository: &Path, state: &Path, destination: &Path) {
        add_worktree_inner(
            AddWorktreeRequest {
                repository: repository.to_path_buf(),
                destination: destination.to_path_buf(),
                revision: OsString::from("HEAD"),
                mode: WorktreeMode::NewBranch(OsString::from("feature/gc-racing-reference")),
                state_dir: Some(state.to_path_buf()),
                sparse_directories: Vec::new(),
            },
            Some(AddWorktreePhase::IntentRecorded),
            false,
        )
        .expect_err("record a racing reference before prepare_base");
    }

    #[test]
    fn gc_cancellation_after_marker_removed_restores_the_completion_marker() {
        let fixture = tempdir().expect("fixture");
        let (repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-race-restore");
        let marker = base_path.with_extension("complete");

        garbage_collect_inner(&state, true, Some(GarbageCollectionPhase::MarkerRemoved))
            .expect_err("interrupt collection after marker removal");
        assert!(!marker.exists());
        assert!(base_path.is_dir());

        race_reference_before_prepare_base(&repository, &state, &fixture.path().join("second"));

        let resumed = garbage_collect_inner(&state, true, None).expect("cancel collection");
        assert_eq!(resumed.resumed_collections, 1);
        assert!(resumed.collected.is_empty());
        assert_eq!(
            fs::read(&marker).expect("read restored marker"),
            crate::base_integrity::marker_v2(&base_path).expect("hash restored base"),
        );

        let accounting = storage_accounting(&state).expect("account cancelled collection");
        assert_eq!(accounting.cancelled_collections, 1);
        assert_eq!(accounting.pending_collections, 0);
        assert_eq!(accounting.bases.len(), 1);
        assert_eq!(accounting.bases[0].path, base_path);

        // The racing add rolls back and releases its claim; the restored
        // marker must leave the base enumerable and collectable by a later gc.
        let recovery = recover_incomplete_operations(&state).expect("roll back racing add");
        assert!(recovery.errors.is_empty(), "{recovery:?}");
        let collected = garbage_collect_inner(&state, true, None).expect("collect restored base");
        assert_eq!(collected.collected, vec![base_path.clone()]);
        assert!(!base_path.exists());
        assert!(!marker.exists());
        let accounting = storage_accounting(&state).expect("account final collection");
        assert!(accounting.bases.is_empty());
        assert_eq!(accounting.completed_collections, 1);
    }

    #[test]
    fn marker_restore_recovers_on_either_side_of_the_cancellation_step() {
        // Interruption immediately after the journaled cancellation: the
        // marker was already restored in the same step, so recovery is a
        // no-op and the base stays enumerable.
        let fixture = tempdir().expect("fixture");
        let (repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-cancel-window");
        let marker = base_path.with_extension("complete");
        garbage_collect_inner(&state, true, Some(GarbageCollectionPhase::MarkerRemoved))
            .expect_err("interrupt collection after marker removal");
        race_reference_before_prepare_base(&repository, &state, &fixture.path().join("second"));
        let store = crate::journal::CollectionJournalStore::open(&state);
        let journal = store
            .load_all()
            .expect("load collection journals")
            .into_iter()
            .find(|journal| journal.base_path == base_path)
            .expect("collection journal for the base");
        let resolved_state =
            super::resolve_real_state_directory(&super::absolute_path(&state).expect("state"))
                .expect("resolve state directory");
        let error = super::resume_decoded_collection(
            &resolved_state,
            &store,
            &journal,
            Some(GarbageCollectionPhase::Cancelled),
        )
        .expect_err("interrupt right after the journaled cancellation");
        assert!(
            error
                .to_string()
                .contains("injected garbage-collection failure"),
            "unexpected error: {error}"
        );
        assert!(marker.is_file());
        for pass in 0..2 {
            let recovered = recover_incomplete_operations(&state).expect("recover");
            assert!(recovered.errors.is_empty(), "pass {pass}: {recovered:?}");
            assert_eq!(recovered.recovered_collections, 0, "pass {pass}");
            assert!(marker.is_file(), "pass {pass}");
        }
        let accounting = storage_accounting(&state).expect("account cancelled collection");
        assert_eq!(accounting.cancelled_collections, 1);
        assert_eq!(accounting.bases.len(), 1);

        // Interruption between the marker restore and the journaled
        // cancellation: the journal is still MarkerRemoved but the marker is
        // back, so recovery must settle on the restored marker.
        let fixture = tempdir().expect("fixture");
        let (_repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-restore-window");
        let marker = base_path.with_extension("complete");
        garbage_collect_inner(&state, true, Some(GarbageCollectionPhase::MarkerRemoved))
            .expect_err("interrupt collection after marker removal");
        let content = crate::base_integrity::marker_v2(&base_path).expect("hash base");
        fs::write(&marker, &content).expect("simulate a restore interrupted before cancellation");
        let recovered = recover_incomplete_operations(&state).expect("recover restored marker");
        assert!(recovered.errors.is_empty(), "{recovered:?}");
        assert_eq!(fs::read(&marker).expect("read marker"), content);
        let accounting = storage_accounting(&state).expect("account recovered cancellation");
        assert_eq!(accounting.cancelled_collections, 1);
        assert_eq!(accounting.pending_collections, 0);
        assert_eq!(accounting.bases.len(), 1);
        let collected = garbage_collect_inner(&state, true, None).expect("collect restored base");
        assert_eq!(collected.collected, vec![base_path.clone()]);
        assert!(!base_path.exists());
    }

    #[test]
    fn interrupted_marker_restore_staging_is_reused_and_reaped() {
        // A crash in the middle of staging the restored marker leaves the
        // deterministic staging file behind. A retried cancellation must
        // replace it and still produce a verified marker.
        let fixture = tempdir().expect("fixture");
        let (repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-staging-retry");
        let marker = base_path.with_extension("complete");
        garbage_collect_inner(&state, true, Some(GarbageCollectionPhase::MarkerRemoved))
            .expect_err("interrupt collection after marker removal");
        race_reference_before_prepare_base(&repository, &state, &fixture.path().join("second"));
        let store = crate::journal::CollectionJournalStore::open(&state);
        let journal = store
            .load_all()
            .expect("load collection journals")
            .into_iter()
            .find(|journal| journal.base_path == base_path)
            .expect("collection journal for the base");
        let staging = super::restored_marker_staging_path(&journal).expect("staging path");
        fs::write(&staging, b"stale partial marker").expect("simulate interrupted staging");
        let resumed = garbage_collect_inner(&state, true, None).expect("cancel collection");
        assert_eq!(resumed.resumed_collections, 1);
        assert!(!staging.exists());
        assert_eq!(
            fs::read(&marker).expect("read restored marker"),
            crate::base_integrity::marker_v2(&base_path).expect("hash restored base"),
        );

        // If the racing reference disappears before the retry, the collection
        // finishes instead, and the staging leftover is reaped with it: a
        // genuinely collected base leaves nothing behind.
        let fixture = tempdir().expect("fixture");
        let (_repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-staging-reap");
        let marker = base_path.with_extension("complete");
        garbage_collect_inner(&state, true, Some(GarbageCollectionPhase::MarkerRemoved))
            .expect_err("interrupt collection after marker removal");
        let store = crate::journal::CollectionJournalStore::open(&state);
        let journal = store
            .load_all()
            .expect("load collection journals")
            .into_iter()
            .find(|journal| journal.base_path == base_path)
            .expect("collection journal for the base");
        let staging = super::restored_marker_staging_path(&journal).expect("staging path");
        fs::write(&staging, b"stale partial marker").expect("simulate interrupted staging");
        let resumed = garbage_collect_inner(&state, true, None).expect("finish collection");
        assert_eq!(resumed.resumed_collections, 1);
        assert!(!base_path.exists());
        assert!(!marker.exists());
        assert!(!staging.exists());
        let accounting = storage_accounting(&state).expect("account finished collection");
        assert!(accounting.bases.is_empty());
        assert_eq!(accounting.completed_collections, 1);
        assert!(accounting.diagnostic_issues.is_empty(), "{accounting:?}");
    }

    #[test]
    fn status_diagnoses_a_base_directory_without_a_completion_marker() {
        // The historical leak: a materialized base whose marker vanished with
        // no journal explaining it. Marker-driven enumeration cannot list it,
        // but status must at least surface it as a diagnostic.
        let fixture = tempdir().expect("fixture");
        let (_repository, state, base_path) =
            unreferenced_base_fixture(fixture.path(), "feature/gc-orphan");
        let marker = base_path.with_extension("complete");
        fs::remove_file(&marker).expect("simulate the historical marker leak");

        let plan = garbage_collect_inner(&state, false, None).expect("plan collection");
        assert!(plan.candidates.is_empty());
        let accounting = storage_accounting(&state).expect("account orphan base");
        assert!(accounting.bases.is_empty());
        let issue = accounting
            .diagnostic_issues
            .iter()
            .find(|issue| issue.path == base_path)
            .expect("orphan base directory is surfaced as a diagnostic");
        assert!(
            issue
                .reason
                .contains("not explained by a completion marker or journal"),
            "unexpected diagnostic: {issue:?}"
        );
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
                sparse_directories: Vec::new(),
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
                sparse_directories: Vec::new(),
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
                    sparse_directories: Vec::new(),
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
                    sparse_directories: Vec::new(),
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

    /// One fixture: a repository with one managed worktree, one plain linked
    /// worktree whose HEAD file is overwritten with garbage, and the Riftri
    /// state directory.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    fn corrupt_head_fixture() -> (
        crate::test_support::WritableTempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let fixture = tempdir().expect("fixture");
        let root = fixture
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let repository = root.join("repository");
        let managed = root.join("managed");
        let corrupt = root.join("corrupt");
        let state = root.join("state");
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
                mode: WorktreeMode::NewBranch(OsString::from("feature/corrupt-head-managed")),
                state_dir: Some(state.clone()),
                sparse_directories: Vec::new(),
            },
            None,
            true,
        )
        .expect("create managed worktree");
        // Plain `git` cannot create directories from a Windows verbatim
        // (\\?\) spelling, so hand it the drive-letter form the CLI would.
        let corrupt_text = corrupt.to_string_lossy().into_owned();
        #[cfg(windows)]
        let corrupt_text = corrupt_text
            .strip_prefix("\\\\?\\")
            .map(str::to_owned)
            .unwrap_or(corrupt_text);
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "corrupt",
                corrupt_text.as_str(),
            ],
        );
        fs::write(repository.join(".git/worktrees/corrupt/HEAD"), "garbage\n")
            .expect("corrupt linked worktree HEAD");
        (fixture, repository, managed, corrupt, state)
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn tolerates_an_unrelated_worktree_without_resolvable_head() {
        let (_fixture, repository, managed, corrupt, state) = corrupt_head_fixture();

        // Status keeps working, keeps the managed view, and names the corrupt
        // worktree as a diagnostic instead of failing repo-wide.
        let accounting = storage_accounting(&state).expect("storage accounting");
        assert_eq!(accounting.active_views, 1);
        assert_eq!(accounting.views[0].destination, managed);
        assert!(
            accounting.diagnostic_issues.iter().any(|issue| {
                super::paths_match(&issue.path, &corrupt) && issue.reason.contains("cannot resolve")
            }),
            "diagnostics must name the corrupt worktree: {:?}",
            accounting.diagnostic_issues
        );

        // Repair keeps working and names the corruption it cannot fix.
        let recovered = recover_incomplete_operations(&state).expect("repair");
        assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
        assert_eq!(recovered.unresolvable_worktrees.len(), 1);
        assert!(super::paths_match(
            &recovered.unresolvable_worktrees[0],
            &corrupt
        ));

        // Unrelated lifecycle operations keep working: prune, then removal of
        // the healthy managed worktree.
        prune_worktrees_inner(
            PruneWorktreesRequest {
                repository: repository.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("prune despite corrupt worktree");
        remove_worktree_inner(
            RemoveWorktreeRequest {
                repository,
                destination: managed.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect("remove healthy managed worktree despite corrupt worktree");
        assert!(!managed.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn refuses_lifecycle_operations_on_the_worktree_whose_head_is_unresolvable() {
        let (fixture, repository, managed, _corrupt, state) = corrupt_head_fixture();
        // Corrupt the managed worktree's own HEAD: operations on THE corrupt
        // worktree must fail closed with an actionable error.
        fs::write(repository.join(".git/worktrees/managed/HEAD"), "garbage\n")
            .expect("corrupt managed worktree HEAD");

        let error = remove_worktree_inner(
            RemoveWorktreeRequest {
                repository: repository.clone(),
                destination: managed.clone(),
                state_dir: Some(state.clone()),
            },
            None,
        )
        .expect_err("removal of a corrupt worktree must fail closed");
        assert!(
            error
                .to_string()
                .contains("cannot resolve the worktree HEAD"),
            "unexpected removal error: {error}"
        );
        assert!(
            managed.is_dir(),
            "failed removal must preserve the worktree"
        );

        let error = move_worktree_inner(
            MoveWorktreeRequest {
                repository,
                source: managed.clone(),
                destination: fixture.path().join("moved"),
                state_dir: Some(state),
            },
            None,
        )
        .expect_err("move of a corrupt worktree must fail closed");
        assert!(
            error
                .to_string()
                .contains("cannot resolve the worktree HEAD"),
            "unexpected move error: {error}"
        );
        assert!(managed.is_dir(), "failed move must preserve the worktree");
    }
}
