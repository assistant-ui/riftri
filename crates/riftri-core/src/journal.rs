use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
use crate::JournalTransitionError;
use crate::RemoveJournalTransitionError;
use crate::{AddWorktreePhase, GarbageCollectionPhase, RemoveWorktreePhase};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "units", rename_all = "kebab-case")]
enum NativeOsString {
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    Utf8(String),
}

impl NativeOsString {
    #[cfg(target_os = "macos")]
    fn encode(value: &OsStr) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self::UnixBytes(value.as_bytes().to_vec())
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
    pub phase: AddWorktreePhase,
    pub last_forward_phase: AddWorktreePhase,
}

#[cfg(target_os = "macos")]
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
pub(crate) struct CollectionJournalRecord {
    pub format_version: u16,
    pub operation_id: String,
    base_path: NativeOsString,
    quarantine_path: NativeOsString,
    marker_path: NativeOsString,
    pub phase: GarbageCollectionPhase,
}

#[cfg(target_os = "macos")]
pub(crate) struct RemovalJournalPaths<'a> {
    pub repository: &'a Path,
    pub destination: &'a Path,
    pub base_path: &'a Path,
}

#[cfg(target_os = "macos")]
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
pub(crate) struct DecodedCollectionJournal {
    #[cfg(target_os = "macos")]
    pub journal_path: PathBuf,
    #[cfg(target_os = "macos")]
    pub operation_id: String,
    #[cfg(target_os = "macos")]
    pub base_path: PathBuf,
    #[cfg(target_os = "macos")]
    pub quarantine_path: PathBuf,
    #[cfg(target_os = "macos")]
    pub marker_path: PathBuf,
    pub phase: GarbageCollectionPhase,
}

impl JournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(target_os = "macos")]
    pub fn new(operation_id: String, paths: JournalPaths<'_>, expected_commit: String) -> Self {
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
            phase: AddWorktreePhase::IntentRecorded,
            last_forward_phase: AddWorktreePhase::IntentRecorded,
        }
    }

    #[cfg(target_os = "macos")]
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
            phase: self.phase,
            last_forward_phase: self.last_forward_phase,
            journal_path,
        })
    }
}

impl RemovalJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(target_os = "macos")]
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

impl CollectionJournalRecord {
    pub const FORMAT_VERSION: u16 = 1;

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
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
            #[cfg(target_os = "macos")]
            operation_id: self.operation_id,
            #[cfg(target_os = "macos")]
            base_path: PathBuf::from(self.base_path.decode(&journal_path)?),
            #[cfg(target_os = "macos")]
            quarantine_path: PathBuf::from(self.quarantine_path.decode(&journal_path)?),
            #[cfg(target_os = "macos")]
            marker_path: PathBuf::from(self.marker_path.decode(&journal_path)?),
            phase: self.phase,
            #[cfg(target_os = "macos")]
            journal_path,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JournalStore {
    directory: PathBuf,
}

impl JournalStore {
    #[cfg(target_os = "macos")]
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
        fs::rename(&temporary, &path)
            .map_err(|source| io("replace operation journal", &path, source))?;
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
}

#[derive(Debug, Clone)]
pub(crate) struct RemovalJournalStore {
    directory: PathBuf,
}

impl RemovalJournalStore {
    #[cfg(target_os = "macos")]
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
        fs::rename(&temporary, &path)
            .map_err(|source| io("replace removal journal", &path, source))?;
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
pub(crate) struct CollectionJournalStore {
    directory: PathBuf,
}

impl CollectionJournalStore {
    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    pub fn path_for(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    #[cfg(target_os = "macos")]
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
        fs::rename(&temporary, &path)
            .map_err(|source| io("replace collection journal", &path, source))?;
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
        JournalRecord, JournalStore, RemovalJournalPaths, RemovalJournalRecord,
        RemovalJournalStore,
    };
    use crate::GarbageCollectionPhase;

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
}
