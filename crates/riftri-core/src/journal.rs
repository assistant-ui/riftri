use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use riftri_storage::BackendKind;
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
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
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
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
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
            phase: AddWorktreePhase::IntentRecorded,
            last_forward_phase: AddWorktreePhase::IntentRecorded,
        }
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
            phase: self.phase,
            last_forward_phase: self.last_forward_phase,
            journal_path,
        })
    }
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
        fs::create_dir_all(&directory)
            .map_err(|source| io("create journal directory", &directory, source))?;
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
            .create(true)
            .truncate(true)
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
        fs::create_dir_all(&directory)
            .map_err(|source| io("create removal journal directory", &directory, source))?;
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
            .create(true)
            .truncate(true)
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
        fs::create_dir_all(&directory)
            .map_err(|source| io("create move journal directory", &directory, source))?;
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
            .create(true)
            .truncate(true)
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
        fs::create_dir_all(&directory)
            .map_err(|source| io("create prune journal directory", &directory, source))?;
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
            .create(true)
            .truncate(true)
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
        fs::create_dir_all(&directory)
            .map_err(|source| io("create collection journal directory", &directory, source))?;
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
            .create(true)
            .truncate(true)
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        CollectionJournalPaths, CollectionJournalRecord, CollectionJournalStore, JournalPaths,
        JournalRecord, JournalStore, MoveJournalPaths, MoveJournalRecord, MoveJournalStore,
        PruneJournalRecord, PruneJournalStore, RemovalJournalPaths, RemovalJournalRecord,
        RemovalJournalStore,
    };
    use crate::{GarbageCollectionPhase, MoveWorktreePhase, PruneWorktreesPhase};

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
}
