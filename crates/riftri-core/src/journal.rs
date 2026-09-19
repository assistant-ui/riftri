use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use riftri_storage::{BackendKind, OverlayFsMountContext, OverlayFsMountIdentity};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::JournalTransitionError;
use crate::RemoveJournalTransitionError;
use crate::{
    AddWorktreePhase, CompactWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase,
    PruneWorktreesPhase, RemoveWorktreePhase,
};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::{
    CompactJournalTransitionError, MoveJournalTransitionError, PruneJournalTransitionError,
};

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("serialize operation journal {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("read operation journal {path}: {source}")]
    Deserialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("journal {path} contains a path encoding for another platform")]
    ForeignPathEncoding { path: PathBuf },

    #[error("journal {path} uses unsupported format version {version}")]
    UnsupportedVersion { path: PathBuf, version: u16 },

    #[error("invalid base-collection journal transition from {current:?} to {requested:?}")]
    InvalidCollectionTransition {
        current: GarbageCollectionPhase,
        requested: GarbageCollectionPhase,
    },

    #[error("invalid operation journal {path}: {detail}")]
    InvalidRecord { path: PathBuf, detail: String },

    #[error("Riftri state path is not a real directory: {path}")]
    InvalidStateDirectory { path: PathBuf },
}

#[derive(Debug)]
pub(crate) struct JournalLoadIssue {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug)]
pub(crate) struct StatusJournalLoad<T> {
    pub journals: Vec<T>,
    pub issues: Vec<JournalLoadIssue>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn require_real_state_directory(directory: &Path) -> Result<(), JournalError> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|source| io("inspect Riftri state directory", directory, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::InvalidStateDirectory {
            path: directory.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn ensure_real_state_directory(
    directory: &Path,
    operation: &'static str,
) -> Result<(), JournalError> {
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(io(operation, directory, source)),
    }
    require_real_state_directory(directory)
}

fn journal_paths(directory: &Path) -> Result<Vec<PathBuf>, JournalError> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io("inspect journal directory", directory, source)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::InvalidStateDirectory {
            path: directory.to_path_buf(),
        });
    }

    let mut paths = fs::read_dir(directory)
        .map_err(|source| io("read journal directory", directory, source))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| io("read journal entry", directory, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.extension() == Some(OsStr::new("json")));
    paths.sort_unstable();
    Ok(paths)
}

fn open_real_journal(path: &Path, operation: &'static str) -> Result<File, JournalError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io("inspect journal entry", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::InvalidRecord {
            path: path.to_path_buf(),
            detail: "journal path is not a real file".to_owned(),
        });
    }
    #[cfg(test)]
    crate::test_hooks::fire(crate::test_hooks::FilesystemRacePoint::JournalOpen, path);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
        // Full sharing, spelled out rather than relying on the standard
        // library's default: a journal read must never block its owner from
        // replacing, writing, or deleting the journal in another process.
        options.share_mode(
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
        );
    }
    let file = options
        .open(path)
        .map_err(|source| io(operation, path, source))?;
    let opened = file
        .metadata()
        .map_err(|source| io("inspect opened journal", path, source))?;
    let current =
        fs::symlink_metadata(path).map_err(|source| io("recheck journal path", path, source))?;
    let mut valid = opened.is_file() && current.is_file() && !current.file_type().is_symlink();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        valid &= opened.dev() == current.dev() && opened.ino() == current.ino();
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        valid &= opened.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            == 0;
    }
    if !valid {
        return Err(JournalError::InvalidRecord {
            path: path.to_path_buf(),
            detail: "journal path changed or is not a real file".to_owned(),
        });
    }
    Ok(file)
}

/// Read a validated journal into memory and close the handle before parsing.
///
/// On Windows a rename-replace of an open destination fails with
/// `ERROR_ACCESS_DENIED` regardless of sharing, so a reader that keeps the
/// journal open while deserializing can make the owner's phase persist fail.
/// Holding the handle only for one buffered read shrinks that window from a
/// parse to a syscall.
fn read_real_journal(path: &Path, operation: &'static str) -> Result<Vec<u8>, JournalError> {
    use std::io::Read;

    let mut file = open_real_journal(path, operation)?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|source| io(operation, path, source))?;
    drop(file);
    Ok(contents)
}

/// Whether a journal read failed only because the journal is being replaced
/// or created right now by its owner.
///
/// Riftri's journal replacement is a rename over the destination. On Windows
/// (`MoveFileExW` without POSIX semantics) that can transiently surface to a
/// concurrent reader as `NotFound` (destination momentarily absent) or
/// `PermissionDenied` (destination in a delete-pending state). Neither says
/// anything about the journal's content, so scans must treat the operation as
/// in flight — never as an error to propagate, and never as evidence the
/// journal can be retired.
pub(crate) fn journal_error_is_in_flight(error: &JournalError) -> bool {
    let JournalError::Io { source, .. } = error else {
        return false;
    };
    source.kind() == std::io::ErrorKind::NotFound
        || (cfg!(windows) && source.kind() == std::io::ErrorKind::PermissionDenied)
}

/// One journal that a reconciling scan could not read because its owner was
/// mid-replacement, even after retries.
#[derive(Debug)]
pub(crate) struct InFlightJournal {
    pub path: PathBuf,
    pub reason: String,
}

/// A journal inventory for reconciliation: everything readable, plus the
/// journals whose owners were actively replacing them.
#[derive(Debug)]
pub(crate) struct ReconcileJournalLoad {
    pub journals: Vec<DecodedJournal>,
    /// Journals still unreadable after retries for a transient-looking reason
    /// other than NotFound. Fail-closed callers must treat these as active
    /// claims of unknown shape; a path whose journal stays NotFound holds no
    /// claim, so persistent NotFound entries are simply skipped.
    pub unreadable: Vec<InFlightJournal>,
    /// Journals that exist but are invalid. Reconciliation must skip them;
    /// `status` reports them.
    pub issues: Vec<JournalLoadIssue>,
}

/// Retry budget for reading a journal that races its owner's atomic replace.
/// The unreadable window is one rename, so a handful of short waits settles
/// it; the cap keeps a genuinely broken state from stalling the caller.
const IN_FLIGHT_READ_ATTEMPTS: u32 = 20;
const IN_FLIGHT_READ_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

fn reload_snapshot<R: serde::de::DeserializeOwned + Clone, D: PartialEq>(
    directory: &Path,
    path: &Path,
    operation_id: &str,
    expected: &D,
    decode: impl FnOnce(R, PathBuf) -> Result<D, JournalError>,
) -> Result<R, JournalError> {
    validate_operation_identity(directory, operation_id, path)?;
    let contents = read_real_journal(path, "reopen operation journal")?;
    let record: R =
        serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
            path: path.to_path_buf(),
            source,
        })?;
    if decode(record.clone(), path.to_path_buf())? != *expected {
        return Err(JournalError::InvalidRecord {
            path: path.to_path_buf(),
            detail: "journal changed after its recovery snapshot was validated".to_owned(),
        });
    }
    Ok(record)
}

fn validate_operation_id(operation_id: &str, journal_path: &Path) -> Result<(), JournalError> {
    let mut components = Path::new(operation_id).components();
    let is_single_normal_component = matches!(
        components.next(),
        Some(std::path::Component::Normal(component)) if component == OsStr::new(operation_id)
    ) && components.next().is_none();
    if operation_id.is_empty()
        || operation_id.contains('/')
        || operation_id.contains('\\')
        || !is_single_normal_component
    {
        return Err(JournalError::InvalidRecord {
            path: journal_path.to_path_buf(),
            detail: "operation ID is not a safe filename component".to_owned(),
        });
    }
    Ok(())
}

fn validate_operation_identity(
    directory: &Path,
    operation_id: &str,
    journal_path: &Path,
) -> Result<(), JournalError> {
    validate_operation_id(operation_id, journal_path)?;
    if directory.join(format!("{operation_id}.json")) != journal_path {
        return Err(JournalError::InvalidRecord {
            path: journal_path.to_path_buf(),
            detail: "operation ID does not match the journal filename".to_owned(),
        });
    }
    Ok(())
}

fn load_status_journals<T>(
    paths: Vec<PathBuf>,
    mut load: impl FnMut(PathBuf) -> Result<T, JournalError>,
) -> StatusJournalLoad<T> {
    let mut journals = Vec::new();
    let mut issues = Vec::new();
    for path in paths {
        match load(path.clone()) {
            Ok(journal) => journals.push(journal),
            Err(error) => issues.push(JournalLoadIssue {
                path,
                reason: error.to_string(),
            }),
        }
    }
    StatusJournalLoad { journals, issues }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "units", rename_all = "kebab-case")]
enum NativeOsString {
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    Utf8(String),
}

impl NativeOsString {
    #[cfg(unix)]
    fn encode(value: &OsStr) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self::UnixBytes(value.as_bytes().to_vec())
    }

    #[cfg(target_os = "windows")]
    fn encode(value: &OsStr) -> Self {
        use std::os::windows::ffi::OsStrExt;
        Self::WindowsWide(value.encode_wide().collect())
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    fn encode(value: &OsStr) -> Self {
        Self::Utf8(value.to_string_lossy().into_owned())
    }

    #[cfg(unix)]
    fn decode(&self, journal_path: &Path) -> Result<OsString, JournalError> {
        use std::os::unix::ffi::OsStringExt;
        match self {
            Self::UnixBytes(bytes) => Ok(OsString::from_vec(bytes.clone())),
            _ => Err(JournalError::ForeignPathEncoding {
                path: journal_path.to_path_buf(),
            }),
        }
    }

    #[cfg(target_os = "windows")]
    fn decode(&self, journal_path: &Path) -> Result<OsString, JournalError> {
        use std::os::windows::ffi::OsStringExt;
        match self {
            Self::WindowsWide(units) => Ok(OsString::from_wide(units)),
            _ => Err(JournalError::ForeignPathEncoding {
                path: journal_path.to_path_buf(),
            }),
        }
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    fn decode(&self, journal_path: &Path) -> Result<OsString, JournalError> {
        match self {
            Self::Utf8(value) => Ok(OsString::from(value)),
            _ => Err(JournalError::ForeignPathEncoding {
                path: journal_path.to_path_buf(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    repository: NativeOsString,
    destination: NativeOsString,
    scratch: NativeOsString,
    base_staging: NativeOsString,
    base_path: NativeOsString,
    temporary_index: NativeOsString,
    branch: Option<NativeOsString>,
    #[serde(default = "legacy_created_branch")]
    branch_created: bool,
    pub expected_commit: String,
    #[serde(default = "legacy_apfs_backend")]
    pub backend: BackendKind,
    /// Canonical cone-mode sparse directory list. Empty means the view
    /// materializes the full tree; older journals omit the field entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sparse_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overlayfs: Option<OverlayFsJournalRecord>,
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverlayFsJournalRecord {
    layout_root: NativeOsString,
    recovery_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mount_context: Option<OverlayFsMountContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mount_identity: Option<OverlayFsMountIdentity>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) struct JournalPaths<'a> {
    pub repository: &'a Path,
    pub destination: &'a Path,
    pub scratch: &'a Path,
    pub base_staging: &'a Path,
    pub base_path: &'a Path,
    pub temporary_index: &'a Path,
    pub branch: Option<&'a OsStr>,
    pub branch_created: bool,
    pub sparse_directories: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub scratch: PathBuf,
    pub base_staging: PathBuf,
    pub base_path: PathBuf,
    pub temporary_index: PathBuf,
    pub branch: Option<OsString>,
    pub branch_created: bool,
    pub expected_commit: String,
    pub backend: BackendKind,
    pub sparse_directories: Vec<String>,
    pub overlayfs: Option<DecodedOverlayFsJournal>,
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedOverlayFsJournal {
    pub layout_root: PathBuf,
    pub recovery_token: String,
    pub mount_context: Option<OverlayFsMountContext>,
    pub mount_identity: Option<OverlayFsMountIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemovalJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    repository: NativeOsString,
    destination: NativeOsString,
    base_path: NativeOsString,
    pub source_add_operation_id: String,
    pub phase: RemoveWorktreePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlayfs_clean_snapshot: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MoveJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    repository: NativeOsString,
    source: NativeOsString,
    destination: NativeOsString,
    pub source_add_operation_id: String,
    pub phase: MoveWorktreePhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CompactJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    repository: NativeOsString,
    destination: NativeOsString,
    replacement: NativeOsString,
    quarantine: NativeOsString,
    base_staging: NativeOsString,
    base_path: NativeOsString,
    temporary_index: NativeOsString,
    old_base_path: NativeOsString,
    pub source_add_operation_id: String,
    pub expected_commit: String,
    pub expected_snapshot: String,
    pub backend: BackendKind,
    pub phase: CompactWorktreePhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PruneJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    repository: NativeOsString,
    pub phase: PruneWorktreesPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CollectionJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    base_path: NativeOsString,
    quarantine_path: NativeOsString,
    marker_path: NativeOsString,
    pub phase: GarbageCollectionPhase,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) struct RemovalJournalPaths<'a> {
    pub repository: &'a Path,
    pub destination: &'a Path,
    pub base_path: &'a Path,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) struct MoveJournalPaths<'a> {
    pub repository: &'a Path,
    pub source: &'a Path,
    pub destination: &'a Path,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) struct CompactJournalPaths<'a> {
    pub repository: &'a Path,
    pub destination: &'a Path,
    pub replacement: &'a Path,
    pub quarantine: &'a Path,
    pub base_staging: &'a Path,
    pub base_path: &'a Path,
    pub temporary_index: &'a Path,
    pub old_base_path: &'a Path,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) struct CollectionJournalPaths<'a> {
    pub base_path: &'a Path,
    pub quarantine_path: &'a Path,
    pub marker_path: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedRemovalJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub source_add_operation_id: String,
    pub phase: RemoveWorktreePhase,
    pub overlayfs_clean_snapshot: Option<String>,
    pub force: bool,
    pub force_snapshot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
pub(crate) struct DecodedMoveJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub source_add_operation_id: String,
    pub phase: MoveWorktreePhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
pub(crate) struct DecodedCompactJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub replacement: PathBuf,
    pub quarantine: PathBuf,
    pub base_staging: PathBuf,
    pub base_path: PathBuf,
    pub temporary_index: PathBuf,
    pub old_base_path: PathBuf,
    pub source_add_operation_id: String,
    pub expected_commit: String,
    pub expected_snapshot: String,
    pub backend: BackendKind,
    pub phase: CompactWorktreePhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
pub(crate) struct DecodedPruneJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub phase: PruneWorktreesPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedCollectionJournal {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub journal_path: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub operation_id: String,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub base_path: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub quarantine_path: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub marker_path: PathBuf,
    pub phase: GarbageCollectionPhase,
}

impl JournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(
        operation_id: String,
        paths: JournalPaths<'_>,
        expected_commit: String,
        backend: BackendKind,
    ) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            repository: NativeOsString::encode(paths.repository.as_os_str()),
            destination: NativeOsString::encode(paths.destination.as_os_str()),
            scratch: NativeOsString::encode(paths.scratch.as_os_str()),
            base_staging: NativeOsString::encode(paths.base_staging.as_os_str()),
            base_path: NativeOsString::encode(paths.base_path.as_os_str()),
            temporary_index: NativeOsString::encode(paths.temporary_index.as_os_str()),
            branch: paths.branch.map(NativeOsString::encode),
            branch_created: paths.branch_created,
            expected_commit,
            backend,
            sparse_directories: paths.sparse_directories.to_vec(),
            overlayfs: None,
            phase: AddWorktreePhase::IntentRecorded,
            last_forward_phase: AddWorktreePhase::IntentRecorded,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new_overlayfs(
        operation_id: String,
        paths: JournalPaths<'_>,
        expected_commit: String,
        layout_root: &Path,
        recovery_token: String,
        mount_context: Option<OverlayFsMountContext>,
    ) -> Result<Self, JournalError> {
        validate_recovery_token(Path::new("<new-overlayfs-journal>"), &recovery_token)?;
        let mut record = Self::new(operation_id, paths, expected_commit, BackendKind::OverlayFs);
        record.overlayfs = Some(OverlayFsJournalRecord {
            layout_root: NativeOsString::encode(layout_root.as_os_str()),
            recovery_token,
            mount_context,
            mount_identity: None,
        });
        Ok(record)
    }

    #[cfg(target_os = "linux")]
    pub fn record_overlayfs_mount_identity(
        &mut self,
        identity: OverlayFsMountIdentity,
    ) -> Result<(), JournalError> {
        let overlayfs = self
            .overlayfs
            .as_mut()
            .ok_or_else(|| JournalError::InvalidRecord {
                path: PathBuf::from("<in-memory>"),
                detail: "cannot record an OverlayFS mount identity without durable mount intent"
                    .to_owned(),
            })?;
        if self.backend != BackendKind::OverlayFs {
            return Err(JournalError::InvalidRecord {
                path: PathBuf::from("<in-memory>"),
                detail: "cannot record an OverlayFS mount identity for another backend".to_owned(),
            });
        }
        if overlayfs
            .mount_context
            .as_ref()
            .is_none_or(|context| *context != identity.context())
        {
            return Err(JournalError::InvalidRecord {
                path: PathBuf::from("<in-memory>"),
                detail: "OverlayFS mount identity does not match its durable mount context"
                    .to_owned(),
            });
        }
        if overlayfs
            .mount_identity
            .as_ref()
            .is_some_and(|current| current != &identity)
        {
            return Err(JournalError::InvalidRecord {
                path: PathBuf::from("<in-memory>"),
                detail: "OverlayFS journal already identifies a different mount".to_owned(),
            });
        }
        overlayfs.mount_identity = Some(identity);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn overlayfs_intent(&self) -> Result<DecodedOverlayFsJournal, JournalError> {
        let overlayfs = self
            .overlayfs
            .as_ref()
            .ok_or_else(|| JournalError::InvalidRecord {
                path: PathBuf::from("<in-memory>"),
                detail: "OverlayFS journal is missing its durable mount intent".to_owned(),
            })?;
        Ok(DecodedOverlayFsJournal {
            layout_root: PathBuf::from(overlayfs.layout_root.decode(Path::new("<in-memory>"))?),
            recovery_token: overlayfs.recovery_token.clone(),
            mount_context: overlayfs.mount_context.clone(),
            mount_identity: overlayfs.mount_identity.clone(),
        })
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn transition(&mut self, next: AddWorktreePhase) -> Result<(), JournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(JournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        if !matches!(
            next,
            AddWorktreePhase::RollbackPending | AddWorktreePhase::RolledBack
        ) {
            self.last_forward_phase = next;
        }
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        let overlayfs = match (self.backend, self.overlayfs) {
            (BackendKind::OverlayFs, Some(overlayfs)) => {
                validate_recovery_token(&journal_path, &overlayfs.recovery_token)?;
                if let (Some(context), Some(identity)) =
                    (&overlayfs.mount_context, &overlayfs.mount_identity)
                    && identity.context() != *context
                {
                    return Err(JournalError::InvalidRecord {
                        path: journal_path,
                        detail: "OverlayFS mount identity does not match its durable mount context"
                            .to_owned(),
                    });
                }
                Some(DecodedOverlayFsJournal {
                    layout_root: PathBuf::from(overlayfs.layout_root.decode(&journal_path)?),
                    recovery_token: overlayfs.recovery_token,
                    mount_context: overlayfs.mount_context,
                    mount_identity: overlayfs.mount_identity,
                })
            }
            (BackendKind::OverlayFs, None) => {
                return Err(JournalError::InvalidRecord {
                    path: journal_path,
                    detail: "OverlayFS journal is missing its durable mount intent".to_owned(),
                });
            }
            (_, Some(_)) => {
                return Err(JournalError::InvalidRecord {
                    path: journal_path,
                    detail: "non-OverlayFS journal contains OverlayFS mount intent".to_owned(),
                });
            }
            (_, None) => None,
        };
        Ok(DecodedJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            destination: PathBuf::from(self.destination.decode(&journal_path)?),
            scratch: PathBuf::from(self.scratch.decode(&journal_path)?),
            base_staging: PathBuf::from(self.base_staging.decode(&journal_path)?),
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            temporary_index: PathBuf::from(self.temporary_index.decode(&journal_path)?),
            branch: self
                .branch
                .map(|branch| branch.decode(&journal_path))
                .transpose()?,
            branch_created: self.branch_created,
            expected_commit: self.expected_commit,
            backend: self.backend,
            sparse_directories: self.sparse_directories,
            overlayfs,
            phase: self.phase,
            last_forward_phase: self.last_forward_phase,
            journal_path,
        })
    }
}

fn validate_recovery_token(journal_path: &Path, token: &str) -> Result<(), JournalError> {
    if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(JournalError::InvalidRecord {
        path: journal_path.to_path_buf(),
        detail: "OverlayFS recovery token must be exactly 32 bytes encoded as hexadecimal"
            .to_owned(),
    })
}

const fn legacy_apfs_backend() -> BackendKind {
    BackendKind::ApfsClone
}

const fn legacy_created_branch() -> bool {
    true
}

impl RemovalJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(
        operation_id: String,
        paths: RemovalJournalPaths<'_>,
        source_add_operation_id: String,
    ) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            repository: NativeOsString::encode(paths.repository.as_os_str()),
            destination: NativeOsString::encode(paths.destination.as_os_str()),
            base_path: NativeOsString::encode(paths.base_path.as_os_str()),
            source_add_operation_id,
            phase: RemoveWorktreePhase::IntentRecorded,
            overlayfs_clean_snapshot: None,
            force: false,
            force_snapshot: None,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new_forced(
        operation_id: String,
        paths: RemovalJournalPaths<'_>,
        source_add_operation_id: String,
        force_snapshot: String,
    ) -> Self {
        let mut record = Self::new(operation_id, paths, source_add_operation_id);
        record.force = true;
        record.force_snapshot = Some(force_snapshot);
        record
    }

    pub fn transition(
        &mut self,
        next: RemoveWorktreePhase,
    ) -> Result<(), RemoveJournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(RemoveJournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedRemovalJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        if self.force != self.force_snapshot.is_some()
            || self.force_snapshot.as_deref().is_some_and(|snapshot| {
                snapshot.len() != 64
                    || !snapshot
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(JournalError::InvalidRecord {
                path: journal_path,
                detail: "forced removal must contain one lowercase SHA-256 content snapshot"
                    .to_owned(),
            });
        }
        Ok(DecodedRemovalJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            destination: PathBuf::from(self.destination.decode(&journal_path)?),
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            source_add_operation_id: self.source_add_operation_id,
            phase: self.phase,
            overlayfs_clean_snapshot: self.overlayfs_clean_snapshot,
            force: self.force,
            force_snapshot: self.force_snapshot,
            journal_path,
        })
    }
}

impl MoveJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(
        operation_id: String,
        paths: MoveJournalPaths<'_>,
        source_add_operation_id: String,
    ) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            repository: NativeOsString::encode(paths.repository.as_os_str()),
            source: NativeOsString::encode(paths.source.as_os_str()),
            destination: NativeOsString::encode(paths.destination.as_os_str()),
            source_add_operation_id,
            phase: MoveWorktreePhase::IntentRecorded,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn transition(
        &mut self,
        next: MoveWorktreePhase,
    ) -> Result<(), MoveJournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(MoveJournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedMoveJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        Ok(DecodedMoveJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            source: PathBuf::from(self.source.decode(&journal_path)?),
            destination: PathBuf::from(self.destination.decode(&journal_path)?),
            source_add_operation_id: self.source_add_operation_id,
            phase: self.phase,
            journal_path,
        })
    }
}

impl CompactJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(
        operation_id: String,
        paths: CompactJournalPaths<'_>,
        source_add_operation_id: String,
        expected_commit: String,
        expected_snapshot: String,
        backend: BackendKind,
    ) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            repository: NativeOsString::encode(paths.repository.as_os_str()),
            destination: NativeOsString::encode(paths.destination.as_os_str()),
            replacement: NativeOsString::encode(paths.replacement.as_os_str()),
            quarantine: NativeOsString::encode(paths.quarantine.as_os_str()),
            base_staging: NativeOsString::encode(paths.base_staging.as_os_str()),
            base_path: NativeOsString::encode(paths.base_path.as_os_str()),
            temporary_index: NativeOsString::encode(paths.temporary_index.as_os_str()),
            old_base_path: NativeOsString::encode(paths.old_base_path.as_os_str()),
            source_add_operation_id,
            expected_commit,
            expected_snapshot,
            backend,
            phase: CompactWorktreePhase::IntentRecorded,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn transition(
        &mut self,
        next: CompactWorktreePhase,
    ) -> Result<(), CompactJournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(CompactJournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedCompactJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        if self.expected_snapshot.len() != 64
            || !self
                .expected_snapshot
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(JournalError::InvalidRecord {
                path: journal_path,
                detail: "compaction snapshot must be a 32-byte hexadecimal digest".to_owned(),
            });
        }
        Ok(DecodedCompactJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            destination: PathBuf::from(self.destination.decode(&journal_path)?),
            replacement: PathBuf::from(self.replacement.decode(&journal_path)?),
            quarantine: PathBuf::from(self.quarantine.decode(&journal_path)?),
            base_staging: PathBuf::from(self.base_staging.decode(&journal_path)?),
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            temporary_index: PathBuf::from(self.temporary_index.decode(&journal_path)?),
            old_base_path: PathBuf::from(self.old_base_path.decode(&journal_path)?),
            source_add_operation_id: self.source_add_operation_id,
            expected_commit: self.expected_commit,
            expected_snapshot: self.expected_snapshot.to_ascii_lowercase(),
            backend: self.backend,
            phase: self.phase,
            journal_path,
        })
    }
}

impl PruneJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(operation_id: String, repository: &Path) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            repository: NativeOsString::encode(repository.as_os_str()),
            phase: PruneWorktreesPhase::IntentRecorded,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn transition(
        &mut self,
        next: PruneWorktreesPhase,
    ) -> Result<(), PruneJournalTransitionError> {
        if !self.phase.can_transition_to(next) {
            return Err(PruneJournalTransitionError {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedPruneJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        Ok(DecodedPruneJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            phase: self.phase,
            journal_path,
        })
    }
}

impl CollectionJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn new(operation_id: String, paths: CollectionJournalPaths<'_>) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            operation_id,
            base_path: NativeOsString::encode(paths.base_path.as_os_str()),
            quarantine_path: NativeOsString::encode(paths.quarantine_path.as_os_str()),
            marker_path: NativeOsString::encode(paths.marker_path.as_os_str()),
            phase: GarbageCollectionPhase::IntentRecorded,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn transition(&mut self, next: GarbageCollectionPhase) -> Result<(), JournalError> {
        use GarbageCollectionPhase::{
            BaseQuarantined, Cancelled, Complete, IntentRecorded, MarkerRemoved,
        };
        let valid = matches!(
            (self.phase, next),
            (IntentRecorded, MarkerRemoved)
                | (IntentRecorded, Cancelled)
                | (MarkerRemoved, BaseQuarantined)
                | (MarkerRemoved, Cancelled)
                | (BaseQuarantined, Complete)
        );
        if !valid {
            return Err(JournalError::InvalidCollectionTransition {
                current: self.phase,
                requested: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    pub fn decode(self, journal_path: PathBuf) -> Result<DecodedCollectionJournal, JournalError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(JournalError::UnsupportedVersion {
                path: journal_path,
                version: self.format_version,
            });
        }
        Ok(DecodedCollectionJournal {
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            operation_id: self.operation_id,
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            quarantine_path: PathBuf::from(self.quarantine_path.decode(&journal_path)?),
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            marker_path: PathBuf::from(self.marker_path.decode(&journal_path)?),
            phase: self.phase,
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            journal_path,
        })
    }
}

// Random, exclusively created names let retries proceed past crash leftovers.
// The guard removes this attempt's file if writing or replacement fails.
fn journal_temporary(
    directory: &Path,
    operation_id: &str,
) -> Result<(File, tempfile::TempPath), JournalError> {
    tempfile::Builder::new()
        .prefix(&format!(".{operation_id}."))
        .suffix(".tmp")
        .make_in(directory, |path| {
            OpenOptions::new().create_new(true).write(true).open(path)
        })
        .map(tempfile::NamedTempFile::into_parts)
        .map_err(|source| io("create temporary journal", directory, source))
}

#[derive(Debug, Clone)]
pub(crate) struct JournalStore {
    directory: PathBuf,
}

impl JournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(&self, expected: &DecodedJournal) -> Result<JournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            JournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("operations");
        ensure_real_state_directory(&directory, "create journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("operations"),
        }
    }

    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    pub fn persist(&self, record: &JournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write operation journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush operation journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync operation journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_operation(&self, operation_id: &str) -> Result<DecodedJournal, JournalError> {
        let path = self.path_for(operation_id);
        validate_operation_id(operation_id, &path)?;
        require_real_state_directory(&self.directory)?;
        self.load_path(path)
    }

    pub fn load_all_for_status(&self) -> Result<StatusJournalLoad<DecodedJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    /// Inventory every journal for a reconciling scan that runs concurrently
    /// with other processes' journal writes.
    ///
    /// A read that races its owner's atomic replace is retried briefly. What
    /// still cannot be read afterwards is separated by what it proves: a path
    /// that stays `NotFound` holds no journal and therefore no claim, while
    /// anything else lands in `unreadable` so a fail-closed caller can refuse
    /// to act. Nothing here is ever surfaced as a hard error for a journal
    /// that merely could not be read.
    pub(crate) fn load_all_reconciling(&self) -> Result<ReconcileJournalLoad, JournalError> {
        let mut journals = Vec::new();
        let mut unreadable = Vec::new();
        let mut issues = Vec::new();
        for path in journal_paths(&self.directory)? {
            let mut result = self.load_path(path.clone());
            for _ in 0..IN_FLIGHT_READ_ATTEMPTS {
                match &result {
                    Err(error) if journal_error_is_in_flight(error) => {
                        std::thread::sleep(IN_FLIGHT_READ_DELAY);
                        result = self.load_path(path.clone());
                    }
                    _ => break,
                }
            }
            match result {
                Ok(journal) => journals.push(journal),
                Err(error) if journal_error_is_in_flight(&error) => {
                    // Persistently absent means no journal exists at this
                    // path any more; everything else stays a blocking claim.
                    let vanished = matches!(
                        &error,
                        JournalError::Io { source, .. }
                            if source.kind() == std::io::ErrorKind::NotFound
                    );
                    if !vanished {
                        unreadable.push(InFlightJournal {
                            path,
                            reason: error.to_string(),
                        });
                    }
                }
                Err(error) => issues.push(JournalLoadIssue {
                    path,
                    reason: error.to_string(),
                }),
            }
        }
        Ok(ReconcileJournalLoad {
            journals,
            unreadable,
            issues,
        })
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedJournal, JournalError> {
        let contents = read_real_journal(&path, "open operation journal")?;
        let record: JournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }

    pub fn update_phase(
        &self,
        expected: &DecodedJournal,
        phase: AddWorktreePhase,
    ) -> Result<DecodedJournal, JournalError> {
        let mut record = self.reload(expected)?;
        record.phase = phase;
        self.persist(&record)?;
        record.decode(expected.journal_path.clone())
    }

    #[cfg(target_os = "linux")]
    pub fn record_overlayfs_mount_identity(
        &self,
        expected: &DecodedJournal,
        identity: OverlayFsMountIdentity,
    ) -> Result<DecodedJournal, JournalError> {
        let mut record = self.reload(expected)?;
        record.record_overlayfs_mount_identity(identity)?;
        self.persist(&record)?;
        record.decode(expected.journal_path.clone())
    }

    #[cfg(target_os = "linux")]
    pub fn begin_overlayfs_remount(
        &self,
        expected: &DecodedJournal,
        expected_context: &OverlayFsMountContext,
        expected_identity: Option<&OverlayFsMountIdentity>,
        new_context: OverlayFsMountContext,
    ) -> Result<DecodedJournal, JournalError> {
        let journal_path = &expected.journal_path;
        let mut record = self.reload(expected)?;
        if record.phase != AddWorktreePhase::Active || record.backend != BackendKind::OverlayFs {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "only an active OverlayFS journal can begin a remount".to_owned(),
            });
        }
        let overlayfs = record
            .overlayfs
            .as_mut()
            .ok_or_else(|| JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "OverlayFS journal is missing its durable mount intent".to_owned(),
            })?;
        if overlayfs.mount_context.as_ref() != Some(expected_context)
            || overlayfs.mount_identity.as_ref() != expected_identity
        {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "OverlayFS mount identity changed before remount intent was persisted"
                    .to_owned(),
            });
        }
        overlayfs.mount_context = Some(new_context);
        overlayfs.mount_identity = None;
        self.persist(&record)?;
        record.decode(journal_path.to_path_buf())
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn update_active_destination(
        &self,
        journal_path: &Path,
        expected_source: &Path,
        destination: &Path,
    ) -> Result<(), JournalError> {
        let contents = read_real_journal(journal_path, "open operation journal")?;
        let mut record: JournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: journal_path.to_path_buf(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, journal_path)?;
        let decoded = record.clone().decode(journal_path.to_path_buf())?;
        if decoded.phase != AddWorktreePhase::Active {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "only an active add journal can be relocated".to_owned(),
            });
        }
        if decoded.destination == destination {
            return Ok(());
        }
        if decoded.destination != expected_source {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: format!(
                    "expected source {}, found {}",
                    expected_source.display(),
                    decoded.destination.display()
                ),
            });
        }
        record.destination = NativeOsString::encode(destination.as_os_str());
        self.persist(&record)?;
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn update_active_base(
        &self,
        journal_path: &Path,
        expected_destination: &Path,
        expected_old_base: &Path,
        base_path: &Path,
        expected_commit: &str,
    ) -> Result<(), JournalError> {
        let contents = read_real_journal(journal_path, "open operation journal")?;
        let mut record: JournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: journal_path.to_path_buf(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, journal_path)?;
        let decoded = record.clone().decode(journal_path.to_path_buf())?;
        if decoded.phase != AddWorktreePhase::Active || decoded.destination != expected_destination
        {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "only the expected active add journal can be compacted".to_owned(),
            });
        }
        if decoded.base_path == base_path && decoded.expected_commit == expected_commit {
            return Ok(());
        }
        if decoded.base_path != expected_old_base {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: format!(
                    "expected old base {}, found {}",
                    expected_old_base.display(),
                    decoded.base_path.display()
                ),
            });
        }
        record.base_path = NativeOsString::encode(base_path.as_os_str());
        record.expected_commit = expected_commit.to_owned();
        self.persist(&record)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemovalJournalStore {
    directory: PathBuf,
}

impl RemovalJournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(
        &self,
        expected: &DecodedRemovalJournal,
    ) -> Result<RemovalJournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            RemovalJournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("removals");
        ensure_real_state_directory(&directory, "create removal journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("removals"),
        }
    }

    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    pub fn persist(&self, record: &RemovalJournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write removal journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush removal journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync removal journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedRemovalJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_all_for_status(
        &self,
    ) -> Result<StatusJournalLoad<DecodedRemovalJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedRemovalJournal, JournalError> {
        let contents = read_real_journal(&path, "open removal journal")?;
        let record: RemovalJournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MoveJournalStore {
    directory: PathBuf,
}

impl MoveJournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(&self, expected: &DecodedMoveJournal) -> Result<MoveJournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            MoveJournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("moves");
        ensure_real_state_directory(&directory, "create move journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("moves"),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn persist(&self, record: &MoveJournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write move journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush move journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync move journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedMoveJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_all_for_status(
        &self,
    ) -> Result<StatusJournalLoad<DecodedMoveJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedMoveJournal, JournalError> {
        let contents = read_real_journal(&path, "open move journal")?;
        let record: MoveJournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompactJournalStore {
    directory: PathBuf,
}

impl CompactJournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(
        &self,
        expected: &DecodedCompactJournal,
    ) -> Result<CompactJournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            CompactJournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("compactions");
        ensure_real_state_directory(&directory, "create compaction journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("compactions"),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn persist(&self, record: &CompactJournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write compaction journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush compaction journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync compaction journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedCompactJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_all_for_status(
        &self,
    ) -> Result<StatusJournalLoad<DecodedCompactJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedCompactJournal, JournalError> {
        let contents = read_real_journal(&path, "open compaction journal")?;
        let record: CompactJournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PruneJournalStore {
    directory: PathBuf,
}

impl PruneJournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(
        &self,
        expected: &DecodedPruneJournal,
    ) -> Result<PruneJournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            PruneJournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("prunes");
        ensure_real_state_directory(&directory, "create prune journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("prunes"),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn persist(&self, record: &PruneJournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write prune journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush prune journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync prune journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedPruneJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_all_for_status(
        &self,
    ) -> Result<StatusJournalLoad<DecodedPruneJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedPruneJournal, JournalError> {
        let contents = read_real_journal(&path, "open prune journal")?;
        let record: PruneJournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CollectionJournalStore {
    directory: PathBuf,
}

impl CollectionJournalStore {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn reload(
        &self,
        expected: &DecodedCollectionJournal,
    ) -> Result<CollectionJournalRecord, JournalError> {
        reload_snapshot(
            &self.directory,
            &expected.journal_path,
            &expected.operation_id,
            expected,
            CollectionJournalRecord::decode,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn create(state_directory: &Path) -> Result<Self, JournalError> {
        let directory = state_directory.join("collections");
        ensure_real_state_directory(&directory, "create collection journal directory")?;
        sync_parent(&directory)?;
        Ok(Self { directory })
    }

    pub fn open(state_directory: &Path) -> Self {
        Self {
            directory: state_directory.join("collections"),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub fn persist(&self, record: &CollectionJournalRecord) -> Result<PathBuf, JournalError> {
        let path = self.path_for(&record.operation_id);
        validate_operation_id(&record.operation_id, &path)?;
        let (file, mut temporary) = journal_temporary(&self.directory, &record.operation_id)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.to_path_buf(),
                source,
            }
        })?;
        writer
            .write_all(b"\n")
            .map_err(|source| io("write collection journal", &temporary, source))?;
        writer
            .flush()
            .map_err(|source| io("flush collection journal", &temporary, source))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|source| io("sync collection journal", &temporary, source))?;
        drop(writer);
        atomic_replace(&temporary, &path)?;
        temporary.disable_cleanup(true);
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedCollectionJournal>, JournalError> {
        journal_paths(&self.directory)?
            .into_iter()
            .map(|path| self.load_path(path))
            .collect()
    }

    pub fn load_all_for_status(
        &self,
    ) -> Result<StatusJournalLoad<DecodedCollectionJournal>, JournalError> {
        Ok(load_status_journals(
            journal_paths(&self.directory)?,
            |path| self.load_path(path),
        ))
    }

    fn load_path(&self, path: PathBuf) -> Result<DecodedCollectionJournal, JournalError> {
        let contents = read_real_journal(&path, "open collection journal")?;
        let record: CollectionJournalRecord =
            serde_json::from_slice(&contents).map_err(|source| JournalError::Deserialize {
                path: path.clone(),
                source,
            })?;
        validate_operation_identity(&self.directory, &record.operation_id, &path)?;
        record.decode(path)
    }
}

#[cfg(not(target_os = "windows"))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), JournalError> {
    fs::rename(source, destination)
        .map_err(|source_error| io("replace operation journal", destination, source_error))
}

#[cfg(target_os = "windows")]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), JournalError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source_wide.push(0);
    let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination_wide.push(0);
    // MOVEFILE_REPLACE_EXISTING fails with ERROR_ACCESS_DENIED while any other
    // handle is open on the destination, no matter what sharing that handle
    // was opened with. Riftri's own scans (status, repair, reconciliation in
    // a concurrent add) read journals for only a syscall-sized window, so a
    // short bounded retry outlasts every legitimate reader without changing
    // semantics: on success the replacement is exactly as atomic as before,
    // and a persistent denial still surfaces as the same error.
    let attempts = 50_u32;
    for attempt in 0.. {
        // SAFETY: both paths are NUL-terminated UTF-16 buffers.
        // REPLACE_EXISTING gives journal updates Windows' replacement
        // semantics, while WRITE_THROUGH waits for the move to reach the
        // filesystem before returning.
        let succeeded = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        let transient = matches!(
            error.raw_os_error(),
            Some(code) if code == ERROR_ACCESS_DENIED as i32
                || code == ERROR_SHARING_VIOLATION as i32
        );
        if !transient || attempt >= attempts {
            return Err(io("replace operation journal", destination, error));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unreachable!("the replace loop always returns");
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), JournalError> {
    let parent = path.parent().unwrap_or(path);
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io("sync journal directory", parent, source))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), JournalError> {
    Ok(())
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> JournalError {
    JournalError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        CollectionJournalPaths, CollectionJournalRecord, CollectionJournalStore,
        CompactJournalPaths, CompactJournalRecord, CompactJournalStore, JournalError, JournalPaths,
        JournalRecord, JournalStore, MoveJournalPaths, MoveJournalRecord, MoveJournalStore,
        PruneJournalRecord, PruneJournalStore, RemovalJournalPaths, RemovalJournalRecord,
        RemovalJournalStore,
    };
    #[cfg(target_os = "linux")]
    use crate::AddWorktreePhase;
    use crate::{
        CompactWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase, PruneWorktreesPhase,
    };

    #[test]
    fn journal_store_rejects_a_symlinked_state_directory() {
        let directory = tempdir().expect("journal fixture");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&outside).expect("create outside directory");
        symlink(&outside, directory.path().join("operations")).expect("symlink journal directory");

        let error = JournalStore::create(directory.path())
            .expect_err("symlinked journal directory must be rejected");

        assert!(error.to_string().contains("not a real directory"));
        assert!(
            std::fs::read_dir(outside)
                .expect("read outside directory")
                .next()
                .is_none()
        );
    }

    #[test]
    fn journal_readers_reject_symlinked_directories() {
        let directory = tempdir().expect("journal fixture");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&outside).expect("create outside directory");

        for name in [
            "operations",
            "removals",
            "moves",
            "compactions",
            "prunes",
            "collections",
        ] {
            let journal_directory = directory.path().join(name);
            symlink(&outside, &journal_directory).expect("symlink journal directory");

            let result = match name {
                "operations" => JournalStore::open(directory.path()).load_all().map(|_| ()),
                "removals" => RemovalJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "moves" => MoveJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "compactions" => CompactJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "prunes" => PruneJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "collections" => CollectionJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                _ => unreachable!(),
            };

            assert!(
                matches!(result, Err(JournalError::InvalidStateDirectory { path }) if path == journal_directory),
                "{name} reader followed a symlinked journal directory"
            );
            std::fs::remove_file(journal_directory).expect("remove journal symlink");
        }
    }

    #[test]
    fn journal_readers_reject_symlinked_json_entries() {
        let directory = tempdir().expect("journal fixture");
        let outside = directory.path().join("outside.json");
        std::fs::write(&outside, b"{}\n").expect("write outside journal");

        for name in [
            "operations",
            "removals",
            "moves",
            "compactions",
            "prunes",
            "collections",
        ] {
            let journal_directory = directory.path().join(name);
            std::fs::create_dir(&journal_directory).expect("create journal directory");
            let journal_path = journal_directory.join("linked.json");
            symlink(&outside, &journal_path).expect("symlink journal entry");

            let result = match name {
                "operations" => JournalStore::open(directory.path()).load_all().map(|_| ()),
                "removals" => RemovalJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "moves" => MoveJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "compactions" => CompactJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "prunes" => PruneJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                "collections" => CollectionJournalStore::open(directory.path())
                    .load_all()
                    .map(|_| ()),
                _ => unreachable!(),
            };

            assert!(
                matches!(result, Err(JournalError::InvalidRecord { path, .. }) if path == journal_path),
                "{name} reader followed a symlinked journal entry"
            );
            std::fs::remove_file(journal_path).expect("remove journal symlink");
            std::fs::remove_dir(journal_directory).expect("remove journal directory");
        }
    }

    #[test]
    fn journal_open_rejects_a_symlink_swap_after_initial_inspection() {
        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let record = JournalRecord::new(
            "raced-open".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );
        let journal_path = store.persist(&record).expect("persist journal");
        let outside = directory.path().join("outside.json");
        std::fs::write(&outside, b"outside\n").expect("write outside file");
        let outside_for_hook = outside.clone();
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::JournalOpen,
            move |path| {
                std::fs::remove_file(path).expect("remove inspected journal");
                symlink(&outside_for_hook, path).expect("replace journal with symlink");
            },
        );

        let error = store
            .load_operation("raced-open")
            .expect_err("raced journal open must fail closed");

        assert!(matches!(error, JournalError::Io { .. }));
        assert_eq!(
            std::fs::read(&outside).expect("outside file remains readable"),
            b"outside\n"
        );
        assert!(
            std::fs::symlink_metadata(&journal_path)
                .expect("raced path remains for inspection")
                .file_type()
                .is_symlink()
        );
    }

    fn assert_invalid_journal<T>(result: Result<T, JournalError>, expected_path: &Path) {
        assert!(
            matches!(result, Err(JournalError::InvalidRecord { path, .. }) if path == expected_path),
            "journal identity mismatch must fail closed"
        );
    }

    #[test]
    fn journal_phase_updates_bind_the_operation_id_to_the_filename() {
        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let record = JournalRecord::new(
            "original".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );
        let journal_path = store.persist(&record).expect("persist journal");
        let snapshot = record
            .clone()
            .decode(journal_path.clone())
            .expect("snapshot");
        let mut tampered = record;
        tampered.operation_id = "replacement".to_owned();
        std::fs::write(
            &journal_path,
            serde_json::to_vec_pretty(&tampered).expect("serialize tampered journal"),
        )
        .expect("tamper operation ID");

        let result = store.update_phase(&snapshot, crate::AddWorktreePhase::BaseReady);

        assert_invalid_journal(result, &journal_path);
        assert!(
            !store.path_for("replacement").exists(),
            "mismatched update created a second journal"
        );
    }

    #[test]
    fn forced_removal_journal_requires_a_lowercase_sha256_snapshot() {
        let paths = RemovalJournalPaths {
            repository: Path::new("/repository"),
            destination: Path::new("/destination"),
            base_path: Path::new("/base"),
        };
        let valid = RemovalJournalRecord::new_forced(
            "remove-operation".to_owned(),
            paths,
            "add-operation".to_owned(),
            "ab".repeat(32),
        );
        let journal_path = Path::new("/state/removals/remove-operation.json").to_path_buf();
        assert!(valid.clone().decode(journal_path.clone()).is_ok());

        for snapshot in [None, Some("AB".repeat(32)), Some("ab".repeat(31))] {
            let mut invalid = valid.clone();
            invalid.force_snapshot = snapshot;
            assert!(matches!(
                invalid.decode(journal_path.clone()),
                Err(JournalError::InvalidRecord { .. })
            ));
        }
    }

    #[test]
    fn every_journal_reader_binds_operation_ids_to_filenames() {
        let directory = tempdir().expect("journal fixture");

        let add_store = JournalStore::create(directory.path()).expect("create add store");
        let add = JournalRecord::new(
            "add-operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );
        let path = add_store.persist(&add).expect("persist add journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename add journal");
        assert_invalid_journal(add_store.load_all(), &renamed);

        let removal_store =
            RemovalJournalStore::create(directory.path()).expect("create removal store");
        let removal = RemovalJournalRecord::new(
            "remove-operation".to_owned(),
            RemovalJournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                base_path: Path::new("/base"),
            },
            "add-operation".to_owned(),
        );
        let path = removal_store
            .persist(&removal)
            .expect("persist removal journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename removal journal");
        assert_invalid_journal(removal_store.load_all(), &renamed);

        let move_store = MoveJournalStore::create(directory.path()).expect("create move store");
        let move_record = MoveJournalRecord::new(
            "move-operation".to_owned(),
            MoveJournalPaths {
                repository: Path::new("/repository"),
                source: Path::new("/source"),
                destination: Path::new("/destination"),
            },
            "add-operation".to_owned(),
        );
        let path = move_store
            .persist(&move_record)
            .expect("persist move journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename move journal");
        assert_invalid_journal(move_store.load_all(), &renamed);

        let compact_store =
            CompactJournalStore::create(directory.path()).expect("create compaction store");
        let compact = CompactJournalRecord::new(
            "compact-operation".to_owned(),
            CompactJournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                replacement: Path::new("/replacement"),
                quarantine: Path::new("/quarantine"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                old_base_path: Path::new("/old-base"),
            },
            "add-operation".to_owned(),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            "00".repeat(32),
            riftri_storage::BackendKind::ApfsClone,
        );
        let path = compact_store
            .persist(&compact)
            .expect("persist compaction journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename compaction journal");
        assert_invalid_journal(compact_store.load_all(), &renamed);

        let prune_store = PruneJournalStore::create(directory.path()).expect("create prune store");
        let prune = PruneJournalRecord::new("prune-operation".to_owned(), Path::new("/repository"));
        let path = prune_store.persist(&prune).expect("persist prune journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename prune journal");
        assert_invalid_journal(prune_store.load_all(), &renamed);

        let collection_store =
            CollectionJournalStore::create(directory.path()).expect("create collection store");
        let collection = CollectionJournalRecord::new(
            "collection-operation".to_owned(),
            CollectionJournalPaths {
                base_path: Path::new("/base"),
                quarantine_path: Path::new("/quarantine"),
                marker_path: Path::new("/marker"),
            },
        );
        let path = collection_store
            .persist(&collection)
            .expect("persist collection journal");
        let renamed = path.with_file_name("renamed.json");
        std::fs::rename(path, &renamed).expect("rename collection journal");
        assert_invalid_journal(collection_store.load_all(), &renamed);
    }

    #[cfg(unix)]
    #[test]
    fn journal_round_trips_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let destination = directory
            .path()
            .join(OsString::from_vec(b"destination-\xff".to_vec()));
        let record = JournalRecord::new(
            "operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: &destination,
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: Some(OsString::from_vec(b"branch-\xfe".to_vec()).as_os_str()),
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );

        store.persist(&record).expect("persist journal");
        let loaded = store.load_all().expect("load journal");

        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].destination.as_os_str().as_bytes(),
            destination.as_os_str().as_bytes()
        );
        assert_eq!(
            loaded[0].branch.as_deref().map(OsStrExt::as_bytes),
            Some(b"branch-\xfe".as_slice())
        );
        assert_eq!(loaded[0].backend, riftri_storage::BackendKind::ApfsClone);
        assert!(!loaded[0].branch_created);

        let mut legacy = serde_json::to_value(record).expect("serialize legacy fixture");
        let legacy_object = legacy.as_object_mut().expect("journal object");
        legacy_object.remove("backend");
        legacy_object.remove("branch_created");
        let legacy: JournalRecord =
            serde_json::from_value(legacy).expect("read journal without backend field");
        assert_eq!(legacy.backend, riftri_storage::BackendKind::ApfsClone);
        assert!(legacy.branch_created);
    }

    #[cfg(unix)]
    #[test]
    fn overlayfs_journal_round_trips_mount_intent_and_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        use riftri_storage::{
            BackendKind, OverlayFsMountContext, OverlayFsMountIdentity, OverlayFsMountProfile,
        };

        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let layout_root = directory
            .path()
            .join(OsString::from_vec(b"overlay-\xff".to_vec()));
        let context = OverlayFsMountContext {
            profile: OverlayFsMountProfile::RootlessUserXattr,
            boot_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
            mount_namespace_device: 4,
            mount_namespace_inode: 5,
        };
        let record = JournalRecord::new_overlayfs(
            "operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            &layout_root,
            "ab".repeat(32),
            Some(context.clone()),
        )
        .expect("create OverlayFS journal");
        let mut value = serde_json::to_value(record).expect("serialize OverlayFS journal");
        value["overlayfs"]["mount_identity"] = serde_json::to_value(OverlayFsMountIdentity {
            profile: context.profile,
            boot_id: context.boot_id.clone(),
            mount_namespace_device: context.mount_namespace_device,
            mount_namespace_inode: context.mount_namespace_inode,
            mount_id: 6,
        })
        .expect("serialize mount identity");
        let record: JournalRecord =
            serde_json::from_value(value).expect("decode OverlayFS journal fixture");
        store.persist(&record).expect("persist OverlayFS journal");

        let loaded = store.load_all().expect("load OverlayFS journal");
        let overlayfs = loaded[0]
            .overlayfs
            .as_ref()
            .expect("decoded OverlayFS intent");
        assert_eq!(loaded[0].backend, BackendKind::OverlayFs);
        assert_eq!(
            overlayfs.layout_root.as_os_str().as_bytes(),
            layout_root.as_os_str().as_bytes()
        );
        assert_eq!(overlayfs.recovery_token, "ab".repeat(32));
        assert_eq!(overlayfs.mount_context.as_ref(), Some(&context));
        assert_eq!(overlayfs.mount_identity.as_ref().unwrap().mount_id, 6);

        let mut pre_profile =
            serde_json::to_value(&record).expect("serialize pre-profile OverlayFS journal");
        pre_profile["overlayfs"]["mount_context"]
            .as_object_mut()
            .expect("mount context object")
            .remove("profile");
        pre_profile["overlayfs"]["mount_identity"]
            .as_object_mut()
            .expect("mount identity object")
            .remove("profile");
        let pre_profile: JournalRecord =
            serde_json::from_value(pre_profile).expect("decode journal without mount profiles");
        let pre_profile = pre_profile
            .decode(Path::new("/pre-profile-overlayfs.json").to_path_buf())
            .expect("accept prior OverlayFS profile shape");
        let pre_profile = pre_profile.overlayfs.expect("OverlayFS intent");
        assert_eq!(
            pre_profile.mount_context.unwrap().profile,
            OverlayFsMountProfile::RootlessUserXattr
        );
        assert_eq!(
            pre_profile.mount_identity.unwrap().profile,
            OverlayFsMountProfile::RootlessUserXattr
        );

        let mut legacy = serde_json::to_value(record).expect("serialize legacy OverlayFS journal");
        legacy["overlayfs"]
            .as_object_mut()
            .expect("OverlayFS record object")
            .remove("mount_context");
        let legacy: JournalRecord =
            serde_json::from_value(legacy).expect("decode journal without mount context");
        let legacy = legacy
            .decode(Path::new("/legacy-overlayfs.json").to_path_buf())
            .expect("accept prior OverlayFS journal shape");
        assert!(legacy.overlayfs.unwrap().mount_context.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn overlayfs_journal_requires_valid_durable_mount_intent() {
        let record = JournalRecord::new(
            "operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::OverlayFs,
        );
        let error = record
            .decode(Path::new("/journal.json").to_path_buf())
            .expect_err("OverlayFS record without intent must fail closed");
        assert!(error.to_string().contains("mount intent"));

        let error = JournalRecord::new_overlayfs(
            "operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            Path::new("/layout"),
            "not-a-256-bit-token".to_owned(),
            None,
        )
        .expect_err("short recovery token must fail closed");
        assert!(error.to_string().contains("32 bytes"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn active_overlayfs_remount_atomically_replaces_mount_context() {
        use riftri_storage::{
            BackendKind, OverlayFsMountContext, OverlayFsMountIdentity, OverlayFsMountProfile,
        };

        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let previous_context = OverlayFsMountContext {
            profile: OverlayFsMountProfile::RootlessUserXattr,
            boot_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
            mount_namespace_device: 4,
            mount_namespace_inode: 5,
        };
        let previous_identity = OverlayFsMountIdentity {
            profile: previous_context.profile,
            boot_id: previous_context.boot_id.clone(),
            mount_namespace_device: previous_context.mount_namespace_device,
            mount_namespace_inode: previous_context.mount_namespace_inode,
            mount_id: 6,
        };
        let mut record = JournalRecord::new_overlayfs(
            "operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            Path::new("/state/overlays/v1/operation"),
            "ab".repeat(32),
            Some(previous_context.clone()),
        )
        .expect("create OverlayFS journal");
        record.backend = BackendKind::OverlayFs;
        record.phase = AddWorktreePhase::Active;
        record.last_forward_phase = AddWorktreePhase::Active;
        record
            .record_overlayfs_mount_identity(previous_identity.clone())
            .expect("record original mount identity");
        let path = store.persist(&record).expect("persist active journal");
        let new_context = OverlayFsMountContext {
            profile: OverlayFsMountProfile::RootlessUserXattr,
            boot_id: "fedcba98-7654-3210-fedc-ba9876543210".to_owned(),
            mount_namespace_device: 7,
            mount_namespace_inode: 8,
        };

        let remount = store
            .begin_overlayfs_remount(
                &record.decode(path).expect("snapshot"),
                &previous_context,
                Some(&previous_identity),
                new_context.clone(),
            )
            .expect("persist remount intent");
        let overlayfs = remount.overlayfs.expect("decoded OverlayFS remount intent");
        assert_eq!(overlayfs.mount_context, Some(new_context));
        assert!(overlayfs.mount_identity.is_none());
        assert_eq!(remount.phase, AddWorktreePhase::Active);
    }

    #[cfg(unix)]
    #[test]
    fn removal_journal_round_trips_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let directory = tempdir().expect("journal fixture");
        let store =
            RemovalJournalStore::create(directory.path()).expect("create removal journal store");
        let destination = directory
            .path()
            .join(OsString::from_vec(b"removed-\xff".to_vec()));
        let record = RemovalJournalRecord::new(
            "removal".to_owned(),
            RemovalJournalPaths {
                repository: Path::new("/repository"),
                destination: &destination,
                base_path: Path::new("/state/bases/v1/repository/tree"),
            },
            "add-operation".to_owned(),
        );

        store.persist(&record).expect("persist removal journal");
        let loaded = store.load_all().expect("load removal journal");

        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].destination.as_os_str().as_bytes(),
            destination.as_os_str().as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn collection_journal_round_trips_non_utf8_paths_and_phases() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let directory = tempdir().expect("journal fixture");
        let store =
            CollectionJournalStore::create(directory.path()).expect("create collection store");
        let base_path = directory
            .path()
            .join(OsString::from_vec(b"base-\xff".to_vec()));
        let quarantine_path = directory.path().join(".riftri-gc-operation");
        let marker_path = directory.path().join("base.complete");
        let mut record = CollectionJournalRecord::new(
            "operation".to_owned(),
            CollectionJournalPaths {
                base_path: &base_path,
                quarantine_path: &quarantine_path,
                marker_path: &marker_path,
            },
        );
        record
            .transition(GarbageCollectionPhase::MarkerRemoved)
            .expect("advance collection journal");
        store.persist(&record).expect("persist collection journal");

        let loaded = store.load_all().expect("load collection journal");
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].base_path.as_os_str().as_bytes(),
            base_path.as_os_str().as_bytes()
        );
        assert_eq!(loaded[0].phase, GarbageCollectionPhase::MarkerRemoved);
    }

    #[cfg(unix)]
    #[test]
    fn move_compact_and_prune_journals_round_trip_native_paths_and_phases() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let directory = tempdir().expect("journal fixture");
        let source = directory
            .path()
            .join(OsString::from_vec(b"source-\xff".to_vec()));
        let destination = directory
            .path()
            .join(OsString::from_vec(b"destination-\xfe".to_vec()));
        let move_store = MoveJournalStore::create(directory.path()).expect("create move store");
        let mut move_record = MoveJournalRecord::new(
            "move-operation".to_owned(),
            MoveJournalPaths {
                repository: Path::new("/repository"),
                source: &source,
                destination: &destination,
            },
            "add-operation".to_owned(),
        );
        move_record
            .transition(MoveWorktreePhase::WorktreeMoved)
            .expect("advance move journal");
        move_store
            .persist(&move_record)
            .expect("persist move journal");
        let moves = move_store.load_all().expect("load move journal");
        assert_eq!(moves.len(), 1);
        assert_eq!(
            moves[0].source.as_os_str().as_bytes(),
            source.as_os_str().as_bytes()
        );
        assert_eq!(
            moves[0].destination.as_os_str().as_bytes(),
            destination.as_os_str().as_bytes()
        );
        assert_eq!(moves[0].phase, MoveWorktreePhase::WorktreeMoved);

        let compact_store =
            CompactJournalStore::create(directory.path()).expect("create compaction store");
        let replacement = directory
            .path()
            .join(OsString::from_vec(b"replacement-\xfd".to_vec()));
        let quarantine = directory
            .path()
            .join(OsString::from_vec(b"quarantine-\xfc".to_vec()));
        let mut compact_record = CompactJournalRecord::new(
            "compact-operation".to_owned(),
            CompactJournalPaths {
                repository: Path::new("/repository"),
                destination: &destination,
                replacement: &replacement,
                quarantine: &quarantine,
                base_staging: Path::new("/state/bases/v1/repository/.riftri-build-compact"),
                base_path: Path::new("/state/bases/v1/repository/tree"),
                temporary_index: Path::new("/state/tmp/compact-index"),
                old_base_path: Path::new("/state/bases/v1/repository/old-tree"),
            },
            "add-operation".to_owned(),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            "ab".repeat(32),
            riftri_storage::BackendKind::ApfsClone,
        );
        compact_record
            .transition(CompactWorktreePhase::ReplacementReady)
            .expect("advance compaction journal");
        compact_store
            .persist(&compact_record)
            .expect("persist compaction journal");
        let compactions = compact_store.load_all().expect("load compaction journal");
        assert_eq!(compactions.len(), 1);
        assert_eq!(
            compactions[0].replacement.as_os_str().as_bytes(),
            replacement.as_os_str().as_bytes()
        );
        assert_eq!(
            compactions[0].quarantine.as_os_str().as_bytes(),
            quarantine.as_os_str().as_bytes()
        );
        assert_eq!(compactions[0].phase, CompactWorktreePhase::ReplacementReady);

        let prune_store = PruneJournalStore::create(directory.path()).expect("create prune store");
        let mut prune_record =
            PruneJournalRecord::new("prune-operation".to_owned(), Path::new("/repository"));
        prune_record
            .transition(PruneWorktreesPhase::GitMetadataPruned)
            .expect("advance prune journal");
        prune_store
            .persist(&prune_record)
            .expect("persist prune journal");
        let prunes = prune_store.load_all().expect("load prune journal");
        assert_eq!(prunes.len(), 1);
        assert_eq!(prunes[0].repository, Path::new("/repository"));
        assert_eq!(prunes[0].phase, PruneWorktreesPhase::GitMetadataPruned);
    }

    #[test]
    fn failed_journal_replacement_cleans_its_temporary_and_can_retry() {
        let directory = tempdir().expect("fixture");
        let store = PruneJournalStore::create(directory.path()).expect("store");
        let record = PruneJournalRecord::new("retry".to_owned(), Path::new("/repository"));
        let path = store.path_for("retry");
        std::fs::create_dir(&path).expect("block replacement");
        store.persist(&record).expect_err("replacement fails");
        assert_eq!(
            std::fs::read_dir(&store.directory)
                .expect("entries")
                .count(),
            1
        );
        std::fs::remove_dir(&path).expect("remove fixture blocker");
        store.persist(&record).expect("retry succeeds");
        assert_eq!(store.load_all().expect("load retried record").len(), 1);
        assert_eq!(
            std::fs::read_dir(&store.directory)
                .expect("entries")
                .count(),
            1
        );
    }

    #[test]
    fn journal_stores_do_not_write_through_temporary_file_symlinks() {
        fn plant_symlink(state: &Path, directory: &str, operation_id: &str, target: &Path) {
            let temporary = state
                .join(directory)
                .join(format!(".{operation_id}.{}.tmp", std::process::id()));
            symlink(target, temporary).expect("plant temporary-file symlink");
        }

        let directory = tempdir().expect("journal fixture");
        let protected = directory.path().join("protected");
        std::fs::write(&protected, "must remain unchanged\n").expect("write protected file");

        let add_store = JournalStore::create(directory.path()).expect("create add store");
        let add = JournalRecord::new(
            "add-operation".to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );
        plant_symlink(directory.path(), "operations", "add-operation", &protected);
        add_store
            .persist(&add)
            .expect("add journal must ignore the stale temporary symlink");

        let removal_store =
            RemovalJournalStore::create(directory.path()).expect("create removal store");
        let removal = RemovalJournalRecord::new(
            "removal-operation".to_owned(),
            RemovalJournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                base_path: Path::new("/base"),
            },
            "add-operation".to_owned(),
        );
        plant_symlink(
            directory.path(),
            "removals",
            "removal-operation",
            &protected,
        );
        removal_store
            .persist(&removal)
            .expect("removal journal must ignore the stale temporary symlink");

        let move_store = MoveJournalStore::create(directory.path()).expect("create move store");
        let move_record = MoveJournalRecord::new(
            "move-operation".to_owned(),
            MoveJournalPaths {
                repository: Path::new("/repository"),
                source: Path::new("/source"),
                destination: Path::new("/destination"),
            },
            "add-operation".to_owned(),
        );
        plant_symlink(directory.path(), "moves", "move-operation", &protected);
        move_store
            .persist(&move_record)
            .expect("move journal must ignore the stale temporary symlink");

        let compact_store =
            CompactJournalStore::create(directory.path()).expect("create compaction store");
        let compact = CompactJournalRecord::new(
            "compact-operation".to_owned(),
            CompactJournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                replacement: Path::new("/replacement"),
                quarantine: Path::new("/quarantine"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                old_base_path: Path::new("/old-base"),
            },
            "add-operation".to_owned(),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            "00".repeat(32),
            riftri_storage::BackendKind::ApfsClone,
        );
        plant_symlink(
            directory.path(),
            "compactions",
            "compact-operation",
            &protected,
        );
        compact_store
            .persist(&compact)
            .expect("compaction journal must ignore the stale temporary symlink");

        let prune_store = PruneJournalStore::create(directory.path()).expect("create prune store");
        let prune = PruneJournalRecord::new("prune-operation".to_owned(), Path::new("/repository"));
        plant_symlink(directory.path(), "prunes", "prune-operation", &protected);
        prune_store
            .persist(&prune)
            .expect("prune journal must ignore the stale temporary symlink");

        let collection_store =
            CollectionJournalStore::create(directory.path()).expect("create collection store");
        let collection = CollectionJournalRecord::new(
            "collection-operation".to_owned(),
            CollectionJournalPaths {
                base_path: Path::new("/base"),
                quarantine_path: Path::new("/quarantine"),
                marker_path: Path::new("/marker"),
            },
        );
        plant_symlink(
            directory.path(),
            "collections",
            "collection-operation",
            &protected,
        );
        collection_store
            .persist(&collection)
            .expect("collection journal must ignore the stale temporary symlink");

        assert_eq!(
            std::fs::read_to_string(protected).expect("read protected file"),
            "must remain unchanged\n"
        );
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux", target_os = "windows")
))]
mod reconcile_read_tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::{JournalPaths, JournalRecord, JournalStore};

    fn sample_record(operation_id: &str) -> JournalRecord {
        JournalRecord::new(
            operation_id.to_owned(),
            JournalPaths {
                repository: Path::new("/repository"),
                destination: Path::new("/destination"),
                scratch: Path::new("/scratch"),
                base_staging: Path::new("/base-staging"),
                base_path: Path::new("/base"),
                temporary_index: Path::new("/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        )
    }

    /// A journal file that disappears between the directory listing and the
    /// open — the reader-visible window of a non-POSIX rename replacement —
    /// must be treated as in flight and skipped, never surfaced as an error
    /// and never mistaken for a retirable journal.
    #[test]
    fn a_journal_vanishing_mid_scan_is_in_flight_not_an_error() {
        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let record = sample_record("vanishing-mid-scan");
        let journal_path = store.persist(&record).expect("persist journal");

        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::JournalOpen,
            move |path: &Path| {
                std::fs::remove_file(path).expect("simulate the replacement window");
            },
        );

        let load = store.load_all_reconciling().expect("reconciling load");
        assert!(load.journals.is_empty(), "{:?}", load.journals);
        assert!(load.unreadable.is_empty(), "{:?}", load.unreadable);
        assert!(load.issues.is_empty(), "{:?}", load.issues);
        assert!(!journal_path.exists());
    }

    /// A concurrent reader holding a journal open without delete sharing must
    /// not make the owner's phase persist fail: the replacement retries until
    /// the reader's handle closes. This is the exact shape of the regression
    /// seen with parallel adds on ReFS.
    #[cfg(target_os = "windows")]
    #[test]
    fn persisting_retries_past_a_reader_without_delete_sharing() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::sync::Arc;
        use std::sync::Barrier;

        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let mut record = sample_record("contended-persist");
        let journal_path = store.persist(&record).expect("persist journal");

        let barrier = Arc::new(Barrier::new(2));
        let holder = std::thread::spawn({
            let barrier = Arc::clone(&barrier);
            let journal_path = journal_path.clone();
            move || {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(&journal_path)
                    .expect("hold the journal open without delete sharing");
                barrier.wait();
                std::thread::sleep(std::time::Duration::from_millis(150));
                drop(file);
            }
        });

        barrier.wait();
        record
            .transition(crate::AddWorktreePhase::GitMetadataCreated)
            .expect("advance the journal in memory");
        store
            .persist(&record)
            .expect("journal replacement must outlast a transient reader");
        holder.join().expect("reader thread");

        let reloaded = store
            .load_operation("contended-persist")
            .expect("reload the replaced journal");
        assert_eq!(reloaded.phase, crate::AddWorktreePhase::GitMetadataCreated);
    }
}
