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
    AddWorktreePhase, GarbageCollectionPhase, MoveWorktreePhase, PruneWorktreesPhase,
    RemoveWorktreePhase,
};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::{MoveJournalTransitionError, PruneJournalTransitionError};

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
    pub expected_commit: String,
    #[serde(default = "legacy_apfs_backend")]
    pub backend: BackendKind,
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
}

#[derive(Debug, Clone)]
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
    pub expected_commit: String,
    pub backend: BackendKind,
    pub overlayfs: Option<DecodedOverlayFsJournal>,
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
}

#[derive(Debug, Clone)]
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
pub(crate) struct CollectionJournalPaths<'a> {
    pub base_path: &'a Path,
    pub quarantine_path: &'a Path,
    pub marker_path: &'a Path,
}

#[derive(Debug, Clone)]
pub(crate) struct DecodedRemovalJournal {
    pub journal_path: PathBuf,
    pub operation_id: String,
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub base_path: PathBuf,
    pub source_add_operation_id: String,
    pub phase: RemoveWorktreePhase,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
            expected_commit,
            backend,
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
            expected_commit: self.expected_commit,
            backend: self.backend,
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
        }
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
        Ok(DecodedRemovalJournal {
            operation_id: self.operation_id,
            repository: PathBuf::from(self.repository.decode(&journal_path)?),
            destination: PathBuf::from(self.destination.decode(&journal_path)?),
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            source_add_operation_id: self.source_add_operation_id,
            phase: self.phase,
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

#[derive(Debug, Clone)]
pub(crate) struct JournalStore {
    directory: PathBuf,
}

impl JournalStore {
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
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.operation_id,
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| io("create temporary journal", &temporary, source))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.clone(),
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
        atomic_replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedJournal>, JournalError> {
        if !self
            .directory
            .try_exists()
            .map_err(|source| io("inspect journal directory", &self.directory, source))?
        {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&self.directory)
            .map_err(|source| io("read journal directory", &self.directory, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| io("read journal entry", &self.directory, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension() == Some(OsStr::new("json")));
        paths.sort_unstable();

        paths
            .into_iter()
            .map(|path| {
                let file = File::open(&path)
                    .map_err(|source| io("open operation journal", &path, source))?;
                let record: JournalRecord =
                    serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                        path: path.clone(),
                        source,
                    })?;
                record.decode(path)
            })
            .collect()
    }

    pub fn update_phase(
        &self,
        journal_path: &Path,
        phase: AddWorktreePhase,
    ) -> Result<(), JournalError> {
        let file = File::open(journal_path)
            .map_err(|source| io("open operation journal", journal_path, source))?;
        let mut record: JournalRecord =
            serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                path: journal_path.to_path_buf(),
                source,
            })?;
        record.phase = phase;
        self.persist(&record)?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn record_overlayfs_mount_identity(
        &self,
        journal_path: &Path,
        identity: OverlayFsMountIdentity,
    ) -> Result<DecodedJournal, JournalError> {
        let file = File::open(journal_path)
            .map_err(|source| io("open operation journal", journal_path, source))?;
        let mut record: JournalRecord =
            serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                path: journal_path.to_path_buf(),
                source,
            })?;
        if self.path_for(&record.operation_id) != journal_path {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "operation ID does not match the journal filename".to_owned(),
            });
        }
        record.record_overlayfs_mount_identity(identity)?;
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
        let file = File::open(journal_path)
            .map_err(|source| io("open operation journal", journal_path, source))?;
        let mut record: JournalRecord =
            serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                path: journal_path.to_path_buf(),
                source,
            })?;
        if self.path_for(&record.operation_id) != journal_path {
            return Err(JournalError::InvalidRecord {
                path: journal_path.to_path_buf(),
                detail: "operation ID does not match the journal filename".to_owned(),
            });
        }
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
}

#[derive(Debug, Clone)]
pub(crate) struct RemovalJournalStore {
    directory: PathBuf,
}

impl RemovalJournalStore {
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
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.operation_id,
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| io("create temporary removal journal", &temporary, source))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.clone(),
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
        atomic_replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedRemovalJournal>, JournalError> {
        if !self
            .directory
            .try_exists()
            .map_err(|source| io("inspect removal journal directory", &self.directory, source))?
        {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&self.directory)
            .map_err(|source| io("read removal journal directory", &self.directory, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| io("read removal journal entry", &self.directory, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension() == Some(OsStr::new("json")));
        paths.sort_unstable();

        paths
            .into_iter()
            .map(|path| {
                let file = File::open(&path)
                    .map_err(|source| io("open removal journal", &path, source))?;
                let record: RemovalJournalRecord =
                    serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                        path: path.clone(),
                        source,
                    })?;
                record.decode(path)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MoveJournalStore {
    directory: PathBuf,
}

impl MoveJournalStore {
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
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.operation_id,
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| io("create temporary move journal", &temporary, source))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.clone(),
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
        atomic_replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedMoveJournal>, JournalError> {
        if !self
            .directory
            .try_exists()
            .map_err(|source| io("inspect move journal directory", &self.directory, source))?
        {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&self.directory)
            .map_err(|source| io("read move journal directory", &self.directory, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| io("read move journal entry", &self.directory, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension() == Some(OsStr::new("json")));
        paths.sort_unstable();

        paths
            .into_iter()
            .map(|path| {
                let file =
                    File::open(&path).map_err(|source| io("open move journal", &path, source))?;
                let record: MoveJournalRecord =
                    serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                        path: path.clone(),
                        source,
                    })?;
                record.decode(path)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PruneJournalStore {
    directory: PathBuf,
}

impl PruneJournalStore {
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
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.operation_id,
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| io("create temporary prune journal", &temporary, source))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.clone(),
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
        atomic_replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedPruneJournal>, JournalError> {
        if !self
            .directory
            .try_exists()
            .map_err(|source| io("inspect prune journal directory", &self.directory, source))?
        {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&self.directory)
            .map_err(|source| io("read prune journal directory", &self.directory, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| io("read prune journal entry", &self.directory, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension() == Some(OsStr::new("json")));
        paths.sort_unstable();

        paths
            .into_iter()
            .map(|path| {
                let file =
                    File::open(&path).map_err(|source| io("open prune journal", &path, source))?;
                let record: PruneJournalRecord =
                    serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                        path: path.clone(),
                        source,
                    })?;
                record.decode(path)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CollectionJournalStore {
    directory: PathBuf,
}

impl CollectionJournalStore {
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
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.operation_id,
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| io("create temporary collection journal", &temporary, source))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, record).map_err(|source| {
            JournalError::Serialize {
                path: temporary.clone(),
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
        atomic_replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(path)
    }

    pub fn load_all(&self) -> Result<Vec<DecodedCollectionJournal>, JournalError> {
        if !self.directory.try_exists().map_err(|source| {
            io(
                "inspect collection journal directory",
                &self.directory,
                source,
            )
        })? {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&self.directory)
            .map_err(|source| io("read collection journal directory", &self.directory, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| io("read collection journal entry", &self.directory, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension() == Some(OsStr::new("json")));
        paths.sort_unstable();

        paths
            .into_iter()
            .map(|path| {
                let file = File::open(&path)
                    .map_err(|source| io("open collection journal", &path, source))?;
                let record: CollectionJournalRecord =
                    serde_json::from_reader(file).map_err(|source| JournalError::Deserialize {
                        path: path.clone(),
                        source,
                    })?;
                record.decode(path)
            })
            .collect()
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

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source_wide.push(0);
    let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination_wide.push(0);
    // SAFETY: both paths are NUL-terminated UTF-16 buffers. REPLACE_EXISTING
    // gives journal updates Windows' replacement semantics, while WRITE_THROUGH
    // waits for the move to reach the filesystem before returning.
    let succeeded = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(io(
            "replace operation journal",
            destination,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
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
        CollectionJournalPaths, CollectionJournalRecord, CollectionJournalStore, JournalPaths,
        JournalRecord, JournalStore, MoveJournalPaths, MoveJournalRecord, MoveJournalStore,
        PruneJournalRecord, PruneJournalStore, RemovalJournalPaths, RemovalJournalRecord,
        RemovalJournalStore,
    };
    use crate::{GarbageCollectionPhase, MoveWorktreePhase, PruneWorktreesPhase};

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

        let mut legacy = serde_json::to_value(record).expect("serialize legacy fixture");
        legacy
            .as_object_mut()
            .expect("journal object")
            .remove("backend");
        let legacy: JournalRecord =
            serde_json::from_value(legacy).expect("read journal without backend field");
        assert_eq!(legacy.backend, riftri_storage::BackendKind::ApfsClone);
    }

    #[cfg(unix)]
    #[test]
    fn overlayfs_journal_round_trips_mount_intent_and_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        use riftri_storage::{BackendKind, OverlayFsMountContext, OverlayFsMountIdentity};

        let directory = tempdir().expect("journal fixture");
        let store = JournalStore::create(directory.path()).expect("create journal store");
        let layout_root = directory
            .path()
            .join(OsString::from_vec(b"overlay-\xff".to_vec()));
        let context = OverlayFsMountContext {
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
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            &layout_root,
            "ab".repeat(32),
            Some(context.clone()),
        )
        .expect("create OverlayFS journal");
        let mut value = serde_json::to_value(record).expect("serialize OverlayFS journal");
        value["overlayfs"]["mount_identity"] = serde_json::to_value(OverlayFsMountIdentity {
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
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            Path::new("/layout"),
            "not-a-256-bit-token".to_owned(),
            None,
        )
        .expect_err("short recovery token must fail closed");
        assert!(error.to_string().contains("32 bytes"));
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
    fn move_and_prune_journals_round_trip_native_paths_and_phases() {
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
            },
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            riftri_storage::BackendKind::ApfsClone,
        );
        plant_symlink(directory.path(), "operations", "add-operation", &protected);
        add_store
            .persist(&add)
            .expect_err("add journal must reject the symlink");

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
            .expect_err("removal journal must reject the symlink");

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
            .expect_err("move journal must reject the symlink");

        let prune_store = PruneJournalStore::create(directory.path()).expect("create prune store");
        let prune = PruneJournalRecord::new("prune-operation".to_owned(), Path::new("/repository"));
        plant_symlink(directory.path(), "prunes", "prune-operation", &protected);
        prune_store
            .persist(&prune)
            .expect_err("prune journal must reject the symlink");

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
            .expect_err("collection journal must reject the symlink");

        assert_eq!(
            std::fs::read_to_string(protected).expect("read protected file"),
            "must remain unchanged\n"
        );
    }
}
