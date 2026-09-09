use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use fs2::FileExt;
#[cfg(target_os = "macos")]
use riftri_git::WorktreeHead;
use riftri_git::{Git, GitError, ObjectId};
use riftri_storage::{ApfsCloner, StorageError};
#[cfg(target_os = "macos")]
use riftri_storage::{BackendKind, CapabilityStatus, DestinationVolume, probe_backends};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::journal::{
    DecodedJournal, DecodedRemovalJournal, JournalError, JournalStore, RemovalJournalRecord,
    RemovalJournalStore,
};
#[cfg(target_os = "macos")]
use crate::journal::{JournalPaths, JournalRecord, RemovalJournalPaths};
use crate::{
    AddWorktreePhase, JournalTransitionError, RemoveJournalTransitionError, RemoveWorktreePhase,
};

#[cfg(target_os = "macos")]
static OPERATION_NONCE: AtomicU64 = AtomicU64::new(0);

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
    /// same APFS volume as the destination.
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
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageAccountingReport {
    pub active_views: usize,
    pub completed_removals: usize,
    pub pending_removals: usize,
    pub bases: Vec<BaseStorageAccounting>,
    pub views: Vec<ViewStorageAccounting>,
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
}

pub fn add_worktree(request: AddWorktreeRequest) -> Result<AddWorktreeResult, WorktreeError> {
    add_worktree_inner(request, None, true)
}

pub fn remove_worktree(
    request: RemoveWorktreeRequest,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    remove_worktree_inner(request, None)
}

/// Return whether `destination` is an active Riftri-managed worktree in the
/// repository's default state directory.
pub fn is_managed_worktree(repository: &Path, destination: &Path) -> Result<bool, WorktreeError> {
    let git = Git::default();
    let repository_info = git.inspect_repository(repository)?;
    let state_directory = repository_info.identity.common_git_dir.join("riftri");
    if !state_directory.exists() || !destination.exists() {
        return Ok(false);
    }
    let destination = fs::canonicalize(destination)
        .map_err(|source| io("resolve worktree destination", destination, source))?;
    Ok(find_managed_add_journal(&state_directory, &destination)?.is_some())
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
    let completed = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.clone())
        .collect::<HashSet<_>>();

    let mut references = BTreeMap::<PathBuf, usize>::new();
    let mut views = Vec::new();
    for journal in add_journals.iter().filter(|journal| {
        journal.phase == AddWorktreePhase::Active
            && !completed.contains(&journal.operation_id)
            && journal.destination.is_dir()
    }) {
        let (logical_bytes, allocated_bytes) = tree_usage(&journal.destination)?;
        *references.entry(journal.base_path.clone()).or_default() += 1;
        views.push(ViewStorageAccounting {
            destination: journal.destination.clone(),
            base_path: journal.base_path.clone(),
            logical_bytes,
            allocated_bytes,
        });
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

    Ok(StorageAccountingReport {
        active_views: views.len(),
        completed_removals: removal_journals
            .iter()
            .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
            .count(),
        pending_removals: removal_journals
            .iter()
            .filter(|journal| journal.phase != RemoveWorktreePhase::Complete)
            .count(),
        bases,
        views,
        total_logical_bytes,
        total_allocated_bytes,
    })
}

#[cfg(not(target_os = "macos"))]
fn remove_worktree_inner(
    _request: RemoveWorktreeRequest,
    _fail_after: Option<RemoveWorktreePhase>,
) -> Result<RemoveWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "journaled Riftri removal currently requires macOS".to_owned(),
    ))
}

#[cfg(target_os = "macos")]
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
    if !git
        .list_worktrees(&repository_root)?
        .into_iter()
        .any(|worktree| worktree.path == destination)
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

#[cfg(not(target_os = "macos"))]
fn add_worktree_inner(
    _request: AddWorktreeRequest,
    _fail_after: Option<AddWorktreePhase>,
    _rollback_on_error: bool,
) -> Result<AddWorktreeResult, WorktreeError> {
    Err(WorktreeError::Unsupported(
        "the explicit prototype currently requires a writable APFS volume on macOS".to_owned(),
    ))
}

#[cfg(target_os = "macos")]
fn add_worktree_inner(
    request: AddWorktreeRequest,
    fail_after: Option<AddWorktreePhase>,
    rollback_on_error: bool,
) -> Result<AddWorktreeResult, WorktreeError> {
    let git = Git::default();
    let repository = git.inspect_repository(&request.repository)?;
    if repository.is_bare {
        return Err(WorktreeError::Unsupported(
            "bare repositories are not supported by the APFS prototype".to_owned(),
        ));
    }
    let repository_root = repository.root.ok_or_else(|| {
        WorktreeError::InvalidRequest("Git did not report a working-tree root".to_owned())
    })?;
    let destination = normalize_new_destination(&request.destination)?;
    let checkout_profile = validate_compatibility(&git, &repository_root, &request.revision)?;
    let resolved = git.resolve_revision(&repository_root, &request.revision)?;
    let destination_volume = supported_apfs_volume(&destination)?;

    let requested_state = request
        .state_dir
        .unwrap_or_else(|| repository.identity.common_git_dir.join("riftri"));
    let state_directory = absolute_path(&requested_state)?;
    let state_volume = supported_apfs_volume(&state_directory)?;
    if destination_volume.identity != state_volume.identity {
        return Err(WorktreeError::Unsupported(format!(
            "state directory {} and destination {} are on different volumes; pass --state-dir on the destination APFS volume",
            state_directory.display(),
            destination.display()
        )));
    }

    create_state_layout(&state_directory)?;
    let state_directory = fs::canonicalize(&state_directory)
        .map_err(|source| io("resolve state directory", &state_directory, source))?;
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
    let mut journal = JournalRecord::new(
        operation_id,
        JournalPaths {
            repository: &repository_root,
            destination: &destination,
            scratch: &scratch,
            base_staging: &base_staging,
            base_path: &base_path,
            temporary_index: &temporary_index,
            branch,
        },
        resolved.commit.as_str().to_owned(),
    );
    let journal_path = store.persist(&journal)?;

    let operation = perform_add(
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
        fail_after,
    );

    match operation {
        Ok(reused_base) => Ok(AddWorktreeResult {
            destination,
            commit: resolved.commit,
            tree: resolved.tree,
            base_path,
            journal_path,
            reused_base,
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
#[cfg(target_os = "macos")]
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
    fail_after: Option<AddWorktreePhase>,
) -> Result<bool, WorktreeError> {
    let head = match mode {
        WorktreeMode::NewBranch(branch) => WorktreeHead::NewBranch(branch),
        WorktreeMode::Detached => WorktreeHead::Detached,
    };
    git.add_worktree_no_checkout(repository, destination, revision, head)?;
    advance(
        store,
        journal,
        AddWorktreePhase::GitMetadataCreated,
        fail_after,
    )?;

    let reused_base = prepare_base(
        git,
        repository,
        tree,
        base_path,
        base_staging,
        temporary_index,
    )?;
    advance(store, journal, AddWorktreePhase::BaseReady, fail_after)?;

    ApfsCloner::clone_tree(base_path, scratch)?;
    ApfsCloner::make_tree_owner_writable(scratch)?;
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
        .map_err(|source| io("activate APFS worktree view", destination, source))?;
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

#[cfg(target_os = "macos")]
fn advance(
    store: &JournalStore,
    journal: &mut JournalRecord,
    phase: AddWorktreePhase,
    fail_after: Option<AddWorktreePhase>,
) -> Result<(), WorktreeError> {
    journal.transition(phase)?;
    store.persist(journal)?;
    if fail_after == Some(phase) {
        return Err(WorktreeError::InjectedFailure(phase));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
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
    let complete_exists = complete_path
        .try_exists()
        .map_err(|source| io("inspect immutable-base marker", &complete_path, source))?;
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
    ApfsCloner::make_tree_read_only(base_path)?;
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

#[cfg(target_os = "macos")]
fn validate_compatibility(
    git: &Git,
    repository: &Path,
    revision: &OsStr,
) -> Result<Vec<u8>, WorktreeError> {
    let resolved = git.resolve_revision(repository, revision)?;
    let entries = git.list_tree(repository, &resolved.tree)?;
    let paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    for entry in &entries {
        if entry.path.file_name() == Some(OsStr::new(".gitattributes")) {
            return Err(WorktreeError::Unsupported(format!(
                "tree contains {}; in-tree attributes are deferred until Riftri can prove checkout equivalence",
                entry.path.display()
            )));
        }
        if entry.path == Path::new(".gitmodules") || entry.object_kind == b"commit" {
            return Err(WorktreeError::Unsupported(
                "submodules are deferred to the compatibility milestone".to_owned(),
            ));
        }
    }
    if git.paths_have_effective_attributes(repository, &paths)? {
        return Err(WorktreeError::Unsupported(
            "Git attributes resolve for at least one tracked path; external and system attributes are not supported yet"
                .to_owned(),
        ));
    }
    let mut profile = Sha256::new();
    profile.update(b"riftri-checkout-profile-v1\0");
    let git_version = git.detect()?.version;
    hash_profile_input(&mut profile, b"git.version", Some(git_version.as_bytes()));

    for (key, accepted) in [
        ("core.attributesfile", &[][..]),
        ("core.sparsecheckout", &[b"false".as_slice()][..]),
        ("core.sparsecheckoutcone", &[b"false".as_slice()][..]),
        ("core.autocrlf", &[b"false".as_slice()][..]),
        ("core.eol", &[b"native".as_slice(), b"lf".as_slice()][..]),
        ("core.symlinks", &[b"true".as_slice()][..]),
    ] {
        let value = checked_config_value(git, repository, key, accepted)?;
        hash_profile_input(&mut profile, key.as_bytes(), value.as_deref());
    }
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
    Ok(profile.finalize().to_vec())
}

#[cfg(target_os = "macos")]
fn checked_config_value(
    git: &Git,
    repository: &Path,
    key: &str,
    accepted: &[&[u8]],
) -> Result<Option<Vec<u8>>, WorktreeError> {
    let Some(value) = git.config_value(repository, key)? else {
        return Ok(None);
    };
    if accepted
        .iter()
        .any(|accepted| value.eq_ignore_ascii_case(accepted))
    {
        return Ok(Some(value));
    }
    Err(WorktreeError::Unsupported(format!(
        "Git configuration {key}={} can change checkout bytes and is not supported by the APFS prototype",
        String::from_utf8_lossy(&value)
    )))
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn supported_apfs_volume(path: &Path) -> Result<DestinationVolume, WorktreeError> {
    let capability = probe_backends(path)
        .into_iter()
        .find(|capability| capability.kind == BackendKind::ApfsClone)
        .ok_or_else(|| {
            WorktreeError::Unsupported(
                "this build does not provide the APFS clone backend".to_owned(),
            )
        })?;
    if capability.status != CapabilityStatus::Supported {
        return Err(WorktreeError::Unsupported(capability.explanation));
    }
    capability.volume.ok_or_else(|| {
        WorktreeError::Unsupported("APFS capability did not include a volume identity".to_owned())
    })
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn create_state_layout(state_directory: &Path) -> Result<(), WorktreeError> {
    for directory in [
        state_directory.to_path_buf(),
        state_directory.join("bases/v1"),
        state_directory.join("tmp"),
    ] {
        fs::create_dir_all(&directory)
            .map_err(|source| io("create Riftri state directory", &directory, source))?;
    }
    sync_parent(state_directory)?;
    Ok(())
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn repository_cache_id(common_git_directory: &Path, checkout_profile: &[u8]) -> String {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(common_git_directory.as_os_str().as_bytes());
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn next_operation_id(timestamp: u128) -> String {
    let nonce = OPERATION_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:x}-{:x}-{nonce:x}", std::process::id())
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn fail_removal_if_requested(
    phase: RemoveWorktreePhase,
    fail_after: Option<RemoveWorktreePhase>,
) -> Result<(), WorktreeError> {
    if fail_after == Some(phase) {
        return Err(WorktreeError::InjectedRemovalFailure(phase));
    }
    Ok(())
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
    let mut allocated_bytes = allocated_bytes(&metadata);
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
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
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
    let completed_adds = removal_journals
        .iter()
        .filter(|journal| journal.phase == RemoveWorktreePhase::Complete)
        .map(|journal| journal.source_add_operation_id.as_str())
        .collect::<HashSet<_>>();
    let mut report = RecoveryReport {
        scanned: journals.len().saturating_add(removal_journals.len()),
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
        .any(|worktree| worktree.path == journal.destination);
    Ok((registered, journal.destination.exists()))
}

fn validate_recovery_paths(
    state_directory: &Path,
    journal: &DecodedJournal,
) -> Result<(), WorktreeError> {
    let bases = state_directory.join("bases/v1");
    let temporary = state_directory.join("tmp");
    let base_repository = journal.base_path.parent();
    if base_repository != journal.base_staging.parent()
        || base_repository.and_then(Path::parent) != Some(bases.as_path())
        || !journal
            .base_staging
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".riftri-build-"))
        || journal.temporary_index.parent() != Some(temporary.as_path())
        || journal.scratch.parent() != journal.destination.parent()
        || !journal
            .scratch
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".riftri-view-"))
        || !journal.repository.is_absolute()
        || !journal.destination.is_absolute()
    {
        return Err(WorktreeError::InvalidRequest(format!(
            "journal {} contains paths outside its operation scope",
            journal.journal_path.display()
        )));
    }
    Ok(())
}

fn rollback_decoded(git: &Git, journal: &DecodedJournal) -> Result<(), WorktreeError> {
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
        .any(|worktree| worktree.path == journal.destination);

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
    ApfsCloner::make_tree_owner_writable(path)?;
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

#[cfg(target_os = "macos")]
fn sync_parent(path: &Path) -> Result<(), WorktreeError> {
    let parent = path.expect_parent()?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io("sync parent directory", parent, source))
}

#[cfg(target_os = "macos")]
trait PathExt {
    fn expect_parent(&self) -> Result<&Path, WorktreeError>;
}

#[cfg(target_os = "macos")]
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::tempdir;

    use super::{
        AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree_inner,
        next_operation_id, recover_incomplete_operations, remove_worktree_inner,
        storage_accounting,
    };
    use crate::{AddWorktreePhase, RemoveWorktreePhase};

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
}
