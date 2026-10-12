//! Property tests for what the journal stores write and read back.
//!
//! Journals are the crash-recovery record of every multi-step mutation, so
//! these probe the two things recovery depends on: a persisted record decodes
//! to exactly the native paths it was given, and anything else on disk (a torn
//! write, a flipped byte, another platform's encoding) is refused instead of
//! being read as a different operation.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::BackendKind;
use tempfile::tempdir;

use super::{
    CollectionJournalPaths, CollectionJournalRecord, CollectionJournalStore, CompactJournalPaths,
    CompactJournalRecord, CompactJournalStore, JournalError, JournalPaths, JournalRecord,
    JournalStore, MoveJournalPaths, MoveJournalRecord, MoveJournalStore, PruneJournalRecord,
    PruneJournalStore, RemovalJournalPaths, RemovalJournalRecord, RemovalJournalStore,
};

/// Persisting fsyncs the journal and its directory, so each case costs a few
/// milliseconds of real I/O.
pub(super) const FILESYSTEM_CASES: u64 = 100;

const LOWER_HEX: &str = "0123456789abcdef";

pub(super) fn native_path(tc: &TestCase) -> PathBuf {
    PathBuf::from(OsString::from_vec(tc.draw(gs::binary().max_size(24))))
}

pub(super) fn operation_id(tc: &TestCase) -> String {
    tc.draw(
        gs::text()
            .alphabet("abcdefghijklmnopqrstuvwxyz0123456789-")
            .min_size(1)
            .max_size(16),
    )
}

pub(super) fn sha256_digest(tc: &TestCase) -> String {
    tc.draw(gs::text().alphabet(LOWER_HEX).min_size(64).max_size(64))
}

fn native_cow_backend(tc: &TestCase) -> BackendKind {
    tc.draw(gs::sampled_from(vec![
        BackendKind::ApfsClone,
        BackendKind::Reflink,
        BackendKind::RefsBlockClone,
        BackendKind::FullCopyFallback,
    ]))
}

/// Everything an add journal records, owned so a record can borrow from it.
#[derive(Debug, Clone)]
pub(super) struct AddParts {
    pub repository: PathBuf,
    pub destination: PathBuf,
    pub scratch: PathBuf,
    pub base_staging: PathBuf,
    pub base_path: PathBuf,
    pub temporary_index: PathBuf,
    pub branch: Option<OsString>,
    pub branch_created: bool,
    pub sparse_directories: Vec<String>,
    pub expected_commit: String,
    pub backend: BackendKind,
}

impl AddParts {
    pub(super) fn draw(tc: &TestCase) -> Self {
        Self {
            repository: native_path(tc),
            destination: native_path(tc),
            scratch: native_path(tc),
            base_staging: native_path(tc),
            base_path: native_path(tc),
            temporary_index: native_path(tc),
            branch: tc
                .draw(gs::optional(gs::binary().max_size(16)))
                .map(OsString::from_vec),
            branch_created: tc.draw(gs::booleans()),
            sparse_directories: tc.draw(gs::vecs(gs::text().max_size(8)).max_size(3)),
            expected_commit: tc.draw(gs::text().max_size(40)),
            backend: native_cow_backend(tc),
        }
    }

    pub(super) fn record(&self, operation_id: &str) -> JournalRecord {
        JournalRecord::new(
            operation_id.to_owned(),
            JournalPaths {
                repository: &self.repository,
                destination: &self.destination,
                scratch: &self.scratch,
                base_staging: &self.base_staging,
                base_path: &self.base_path,
                temporary_index: &self.temporary_index,
                branch: self.branch.as_deref(),
                branch_created: self.branch_created,
                sparse_directories: &self.sparse_directories,
            },
            self.expected_commit.clone(),
            self.backend,
        )
    }
}

/// The six journal families. Each has its own record type and store, but all
/// of them promise the same on-disk guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Add,
    Removal,
    Move,
    Compact,
    Prune,
    Collection,
}

/// Run one expression against whichever store `kind` names. The stores share
/// method names without sharing a trait.
macro_rules! with_store {
    ($kind:expr, $state:expr, |$store:ident| $body:expr) => {
        match $kind {
            Kind::Add => {
                let $store = JournalStore::open($state);
                $body
            }
            Kind::Removal => {
                let $store = RemovalJournalStore::open($state);
                $body
            }
            Kind::Move => {
                let $store = MoveJournalStore::open($state);
                $body
            }
            Kind::Compact => {
                let $store = CompactJournalStore::open($state);
                $body
            }
            Kind::Prune => {
                let $store = PruneJournalStore::open($state);
                $body
            }
            Kind::Collection => {
                let $store = CollectionJournalStore::open($state);
                $body
            }
        }
    };
}

impl Kind {
    pub(super) fn draw(tc: &TestCase) -> Self {
        tc.draw(gs::sampled_from(vec![
            Self::Add,
            Self::Removal,
            Self::Move,
            Self::Compact,
            Self::Prune,
            Self::Collection,
        ]))
    }

    pub(super) fn directory(self) -> &'static str {
        match self {
            Self::Add => "operations",
            Self::Removal => "removals",
            Self::Move => "moves",
            Self::Compact => "compactions",
            Self::Prune => "prunes",
            Self::Collection => "collections",
        }
    }

    /// The fields that hold a native path, by their JSON name.
    fn path_fields(self) -> Vec<&'static str> {
        match self {
            Self::Add => vec![
                "repository",
                "destination",
                "scratch",
                "base_staging",
                "base_path",
                "temporary_index",
            ],
            Self::Removal => vec!["repository", "destination", "base_path"],
            Self::Move => vec!["repository", "source", "destination"],
            Self::Compact => vec![
                "repository",
                "destination",
                "replacement",
                "quarantine",
                "base_staging",
                "base_path",
                "temporary_index",
                "old_base_path",
            ],
            Self::Prune => vec!["repository"],
            Self::Collection => vec!["base_path", "quarantine_path", "marker_path"],
        }
    }

    /// Create the store and persist one generated, valid record.
    pub(super) fn persist_sample(
        self,
        tc: &TestCase,
        state: &Path,
        operation_id: &str,
    ) -> Result<PathBuf, JournalError> {
        let id = operation_id.to_owned();
        let paths = (0..8).map(|_| native_path(tc)).collect::<Vec<_>>();
        match self {
            Self::Add => {
                JournalStore::create(state)?.persist(&AddParts::draw(tc).record(operation_id))
            }
            Self::Removal => {
                RemovalJournalStore::create(state)?.persist(&RemovalJournalRecord::new(
                    id,
                    RemovalJournalPaths {
                        repository: &paths[0],
                        destination: &paths[1],
                        base_path: &paths[2],
                    },
                    self::operation_id(tc),
                ))
            }
            Self::Move => MoveJournalStore::create(state)?.persist(&MoveJournalRecord::new(
                id,
                MoveJournalPaths {
                    repository: &paths[0],
                    source: &paths[1],
                    destination: &paths[2],
                },
                self::operation_id(tc),
            )),
            Self::Compact => {
                CompactJournalStore::create(state)?.persist(&CompactJournalRecord::new(
                    id,
                    CompactJournalPaths {
                        repository: &paths[0],
                        destination: &paths[1],
                        replacement: &paths[2],
                        quarantine: &paths[3],
                        base_staging: &paths[4],
                        base_path: &paths[5],
                        temporary_index: &paths[6],
                        old_base_path: &paths[7],
                    },
                    self::operation_id(tc),
                    tc.draw(gs::text().max_size(40)),
                    sha256_digest(tc),
                    native_cow_backend(tc),
                ))
            }
            Self::Prune => {
                PruneJournalStore::create(state)?.persist(&PruneJournalRecord::new(id, &paths[0]))
            }
            Self::Collection => {
                CollectionJournalStore::create(state)?.persist(&CollectionJournalRecord::new(
                    id,
                    CollectionJournalPaths {
                        base_path: &paths[0],
                        quarantine_path: &paths[1],
                        marker_path: &paths[2],
                    },
                ))
            }
        }
    }

    /// Every loadable journal as its operation ID and full decoded form.
    pub(super) fn load_all(self, state: &Path) -> Result<Vec<(String, String)>, JournalError> {
        with_store!(self, state, |store| store.load_all().map(|journals| {
            journals
                .iter()
                .map(|journal| (journal.operation_id.clone(), format!("{journal:?}")))
                .collect()
        }))
    }

    /// How many journals a status read loaded and how many it reported.
    pub(super) fn status_counts(self, state: &Path) -> (usize, usize) {
        with_store!(self, state, |store| {
            let status = store.load_all_for_status().expect("list journals");
            (status.journals.len(), status.issues.len())
        })
    }

    fn parses(self, bytes: &[u8]) -> bool {
        match self {
            Self::Add => serde_json::from_slice::<JournalRecord>(bytes).is_ok(),
            Self::Removal => serde_json::from_slice::<RemovalJournalRecord>(bytes).is_ok(),
            Self::Move => serde_json::from_slice::<MoveJournalRecord>(bytes).is_ok(),
            Self::Compact => serde_json::from_slice::<CompactJournalRecord>(bytes).is_ok(),
            Self::Prune => serde_json::from_slice::<PruneJournalRecord>(bytes).is_ok(),
            Self::Collection => serde_json::from_slice::<CollectionJournalRecord>(bytes).is_ok(),
        }
    }
}

struct Persisted {
    _fixture: tempfile::TempDir,
    state: PathBuf,
    journal: PathBuf,
    operation_id: String,
}

fn persisted(tc: &TestCase, kind: Kind) -> Persisted {
    let fixture = tempdir().expect("journal fixture");
    let state = fixture.path().to_path_buf();
    let operation_id = operation_id(tc);
    let journal = kind
        .persist_sample(tc, &state, &operation_id)
        .expect("persist a valid journal");
    Persisted {
        _fixture: fixture,
        state,
        journal,
        operation_id,
    }
}

fn rewrite_json(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value = serde_json::from_slice(&fs::read(path).expect("read journal")).expect("JSON");
    edit(&mut value);
    fs::write(path, serde_json::to_vec(&value).expect("serialize")).expect("rewrite journal");
}

pub(crate) fn entry_names(directory: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(directory)
        .expect("list directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn add_journals_round_trip_native_paths(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let parts = AddParts::draw(&tc);
    let operation_id = operation_id(&tc);
    let store = JournalStore::create(fixture.path()).expect("create store");
    let path = store
        .persist(&parts.record(&operation_id))
        .expect("persist journal");

    let decoded = store.load_operation(&operation_id).expect("load journal");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(decoded.repository, parts.repository);
    assert_eq!(decoded.destination, parts.destination);
    assert_eq!(decoded.scratch, parts.scratch);
    assert_eq!(decoded.base_staging, parts.base_staging);
    assert_eq!(decoded.base_path, parts.base_path);
    assert_eq!(decoded.temporary_index, parts.temporary_index);
    assert_eq!(decoded.branch, parts.branch);
    assert_eq!(decoded.branch_created, parts.branch_created);
    assert_eq!(decoded.sparse_directories, parts.sparse_directories);
    assert_eq!(decoded.expected_commit, parts.expected_commit);
    assert_eq!(decoded.backend, parts.backend);
    assert_eq!(decoded.overlayfs, None);
    assert_eq!(
        store.load_all().expect("load all"),
        std::slice::from_ref(&decoded)
    );
    // Persisting what was read back changes nothing: the format is stable.
    let before = fs::read(&path).expect("read journal");
    store
        .persist(&store.reload(&decoded).expect("reload"))
        .expect("persist again");
    assert_eq!(fs::read(&path).expect("read journal"), before);
}

#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn removal_journals_round_trip_native_paths(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let (repository, destination, base_path) =
        (native_path(&tc), native_path(&tc), native_path(&tc));
    let paths = RemovalJournalPaths {
        repository: &repository,
        destination: &destination,
        base_path: &base_path,
    };
    let (operation_id, source) = (operation_id(&tc), operation_id(&tc));
    let force_snapshot = tc.draw(gs::booleans()).then(|| sha256_digest(&tc));
    let record = match &force_snapshot {
        Some(snapshot) => RemovalJournalRecord::new_forced(
            operation_id.clone(),
            paths,
            source.clone(),
            snapshot.clone(),
        ),
        None => RemovalJournalRecord::new(operation_id.clone(), paths, source.clone()),
    };
    let store = RemovalJournalStore::create(fixture.path()).expect("create store");
    let path = store.persist(&record).expect("persist journal");

    let [decoded] = <[_; 1]>::try_from(store.load_all().expect("load all")).expect("one journal");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(decoded.repository, repository);
    assert_eq!(decoded.destination, destination);
    assert_eq!(decoded.base_path, base_path);
    assert_eq!(decoded.source_add_operation_id, source);
    assert_eq!(decoded.force, force_snapshot.is_some());
    assert_eq!(decoded.force_snapshot, force_snapshot);
    assert_eq!(decoded.overlayfs_clean_snapshot, None);
    assert_eq!(decoded.phase, record.phase);
    assert_eq!(
        store.reload(&decoded).expect("reload").operation_id,
        operation_id
    );
}

#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn move_prune_and_collection_journals_round_trip_native_paths(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let (first, second, third) = (native_path(&tc), native_path(&tc), native_path(&tc));
    let (operation_id, source) = (operation_id(&tc), operation_id(&tc));

    let moves = MoveJournalStore::create(fixture.path()).expect("create move store");
    let path = moves
        .persist(&MoveJournalRecord::new(
            operation_id.clone(),
            MoveJournalPaths {
                repository: &first,
                source: &second,
                destination: &third,
            },
            source.clone(),
        ))
        .expect("persist move journal");
    let [decoded] = <[_; 1]>::try_from(moves.load_all().expect("load moves")).expect("one move");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(
        (&decoded.repository, &decoded.source, &decoded.destination),
        (&first, &second, &third)
    );
    assert_eq!(decoded.source_add_operation_id, source);
    assert_eq!(
        moves.reload(&decoded).expect("reload").operation_id,
        operation_id
    );

    let prunes = PruneJournalStore::create(fixture.path()).expect("create prune store");
    let path = prunes
        .persist(&PruneJournalRecord::new(operation_id.clone(), &first))
        .expect("persist prune journal");
    let [decoded] = <[_; 1]>::try_from(prunes.load_all().expect("load prunes")).expect("one prune");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(decoded.repository, first);
    assert_eq!(
        prunes.reload(&decoded).expect("reload").operation_id,
        operation_id
    );

    let collections =
        CollectionJournalStore::create(fixture.path()).expect("create collection store");
    let path = collections
        .persist(&CollectionJournalRecord::new(
            operation_id.clone(),
            CollectionJournalPaths {
                base_path: &first,
                quarantine_path: &second,
                marker_path: &third,
            },
        ))
        .expect("persist collection journal");
    let [decoded] = <[_; 1]>::try_from(collections.load_all().expect("load collections"))
        .expect("one collection");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(
        (
            &decoded.base_path,
            &decoded.quarantine_path,
            &decoded.marker_path
        ),
        (&first, &second, &third)
    );
    assert_eq!(
        collections.reload(&decoded).expect("reload").operation_id,
        operation_id
    );
}

/// A compaction snapshot is accepted only as a 32-byte hexadecimal digest and
/// is compared in lowercase from then on.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn compact_journals_round_trip_and_validate_their_snapshot(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let paths = (0..8).map(|_| native_path(&tc)).collect::<Vec<_>>();
    let (operation_id, source) = (operation_id(&tc), operation_id(&tc));
    let commit = tc.draw(gs::text().max_size(40));
    let snapshot = tc.draw(hegel::one_of!(
        gs::text()
            .alphabet("0123456789abcdefABCDEF")
            .min_size(64)
            .max_size(64),
        gs::text().alphabet("0123456789abcdefABCDEFg ").max_size(70),
    ));
    let backend = native_cow_backend(&tc);
    let store = CompactJournalStore::create(fixture.path()).expect("create store");
    let path = store
        .persist(&CompactJournalRecord::new(
            operation_id.clone(),
            CompactJournalPaths {
                repository: &paths[0],
                destination: &paths[1],
                replacement: &paths[2],
                quarantine: &paths[3],
                base_staging: &paths[4],
                base_path: &paths[5],
                temporary_index: &paths[6],
                old_base_path: &paths[7],
            },
            source.clone(),
            commit.clone(),
            snapshot.clone(),
            backend,
        ))
        .expect("persist journal");

    let is_digest = snapshot.len() == 64 && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit());
    let loaded = store.load_all();
    if !is_digest {
        assert!(
            matches!(loaded, Err(JournalError::InvalidRecord { .. })),
            "snapshot {snapshot:?} was accepted: {loaded:?}"
        );
        return;
    }
    let [decoded] = <[_; 1]>::try_from(loaded.expect("load all")).expect("one journal");
    assert_eq!(decoded.journal_path, path);
    assert_eq!(decoded.operation_id, operation_id);
    assert_eq!(
        [
            &decoded.repository,
            &decoded.destination,
            &decoded.replacement,
            &decoded.quarantine,
            &decoded.base_staging,
            &decoded.base_path,
            &decoded.temporary_index,
            &decoded.old_base_path,
        ],
        [
            &paths[0], &paths[1], &paths[2], &paths[3], &paths[4], &paths[5], &paths[6], &paths[7]
        ]
    );
    assert_eq!(decoded.source_add_operation_id, source);
    assert_eq!(decoded.expected_commit, commit);
    assert_eq!(decoded.expected_snapshot, snapshot.to_ascii_lowercase());
    assert_eq!(decoded.backend, backend);
    assert_eq!(decoded.old_expected_commit, None);
    assert_eq!(
        store.reload(&decoded).expect("reload").operation_id,
        operation_id
    );
}

/// A crash can leave any prefix of a journal behind. Pretty-printed JSON ends
/// in `}` and a newline, so every prefix that stops before the closing brace
/// must be refused by the parser and by every reader built on it.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn a_torn_journal_is_never_read_as_an_operation(tc: TestCase) {
    let kind = Kind::draw(&tc);
    let persisted = persisted(&tc, kind);
    let bytes = fs::read(&persisted.journal).expect("read journal");
    assert!(kind.parses(&bytes));
    let torn_lengths = 0..bytes.len() - 1;
    for length in torn_lengths.clone() {
        assert!(
            !kind.parses(&bytes[..length]),
            "{kind:?} journal torn at byte {length} still parsed"
        );
    }

    let length = tc.draw(
        gs::integers::<usize>()
            .min_value(torn_lengths.start)
            .max_value(torn_lengths.end - 1),
    );
    fs::write(&persisted.journal, &bytes[..length]).expect("tear journal");
    assert!(matches!(
        kind.load_all(&persisted.state),
        Err(JournalError::Deserialize { .. })
    ));
    assert_eq!(kind.status_counts(&persisted.state), (0, 1));
    if kind == Kind::Add {
        let store = JournalStore::open(&persisted.state);
        assert!(matches!(
            store.load_operation(&persisted.operation_id),
            Err(JournalError::Deserialize { .. })
        ));
        let reconciling = store.load_all_reconciling().expect("reconciling scan");
        assert!(reconciling.journals.is_empty() && reconciling.unreadable.is_empty());
        assert_eq!(reconciling.issues.len(), 1);
    }
}

/// Arbitrary bytes are never an operation, and one damaged byte in a real
/// journal either fails closed or still names the operation its filename
/// names. Status and lifecycle readers must agree about which it is.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn corrupted_journal_bytes_fail_closed(tc: TestCase) {
    let kind = Kind::draw(&tc);
    let persisted = persisted(&tc, kind);
    let mut bytes = fs::read(&persisted.journal).expect("read journal");

    fs::write(&persisted.journal, tc.draw(gs::binary().max_size(256))).expect("garbage");
    assert!(kind.load_all(&persisted.state).is_err());
    assert_eq!(kind.status_counts(&persisted.state), (0, 1));

    let offset = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(bytes.len() - 1),
    );
    bytes[offset] = tc.draw(gs::integers::<u8>());
    fs::write(&persisted.journal, &bytes).expect("damage journal");
    match kind.load_all(&persisted.state) {
        Ok(journals) => {
            assert_eq!(journals.len(), 1);
            assert_eq!(journals[0].0, persisted.operation_id);
            assert_eq!(kind.status_counts(&persisted.state), (1, 0));
        }
        Err(_) => assert_eq!(kind.status_counts(&persisted.state), (0, 1)),
    }
}

/// A journal written by a newer format, or on a platform with another native
/// path encoding, is refused whole instead of being decoded lossily.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn other_versions_and_foreign_path_encodings_are_refused(tc: TestCase) {
    let kind = Kind::draw(&tc);
    let persisted = persisted(&tc, kind);
    let original = fs::read(&persisted.journal).expect("read journal");

    let version = tc.draw(gs::integers::<u16>());
    tc.assume(version != JournalRecord::FORMAT_VERSION);
    rewrite_json(&persisted.journal, |journal| {
        journal["format_version"] = version.into();
    });
    match kind.load_all(&persisted.state) {
        Err(JournalError::UnsupportedVersion { path, version: got }) => {
            assert_eq!((path, got), (persisted.journal.clone(), version));
        }
        other => panic!("format version {version} was not refused: {other:?}"),
    }

    fs::write(&persisted.journal, &original).expect("restore journal");
    let field = tc.draw(gs::sampled_from(kind.path_fields()));
    let foreign = if tc.draw(gs::booleans()) {
        let units = tc.draw(gs::vecs(gs::integers::<u16>()).max_size(8));
        serde_json::json!({ "encoding": "windows-wide", "units": units })
    } else {
        let units = tc.draw(gs::text().max_size(8));
        serde_json::json!({ "encoding": "utf8", "units": units })
    };
    rewrite_json(&persisted.journal, |journal| journal[field] = foreign);
    assert!(
        matches!(
            kind.load_all(&persisted.state),
            Err(JournalError::ForeignPathEncoding { .. })
        ),
        "a foreign `{field}` was decoded"
    );
    assert_eq!(kind.status_counts(&persisted.state), (0, 1));
}

/// An operation ID becomes a filename. Whatever the ID is, persisting either
/// refuses it and leaves nothing behind, or writes exactly `<id>.json` inside
/// the store's own directory and reads it back under that ID.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn operation_ids_never_escape_the_journal_directory(tc: TestCase) {
    let kind = Kind::draw(&tc);
    let operation_id = tc.draw(hegel::one_of!(
        gs::text().max_size(12),
        gs::sampled_from(
            [
                "", ".", "..", "../x", "a/b", "a\\b", "/x", "x/..", "x/", "a\0b"
            ]
            .map(str::to_owned)
            .to_vec()
        ),
    ));
    let fixture = tempdir().expect("journal fixture");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("state directory");

    let result = kind.persist_sample(&tc, &state, &operation_id);
    let directory = state.join(kind.directory());
    match result {
        Ok(path) => {
            let name = format!("{operation_id}.json");
            assert_eq!(path, directory.join(&name));
            assert_eq!(entry_names(&directory), [OsString::from(name)]);
            let loaded = kind.load_all(&state).expect("load the persisted journal");
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].0, operation_id);
        }
        Err(error) => assert!(
            entry_names(&directory).is_empty(),
            "refusing {operation_id:?} ({error}) left files behind"
        ),
    }
    assert_eq!(entry_names(fixture.path()), ["state"]);
    assert_eq!(entry_names(&state), [kind.directory()]);
}

/// An OverlayFS add journal must carry its mount intent, any other backend
/// must not, and a recovery token is exactly 32 hexadecimal bytes.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn overlayfs_intent_is_required_exactly_for_overlayfs_journals(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let parts = AddParts::draw(&tc);
    let operation_id = operation_id(&tc);
    let layout_root = native_path(&tc);
    let token = tc.draw(hegel::one_of!(
        gs::text()
            .alphabet("0123456789abcdefABCDEF")
            .min_size(64)
            .max_size(64),
        gs::text().alphabet("0123456789abcdefg").max_size(70),
    ));
    let paths = JournalPaths {
        repository: &parts.repository,
        destination: &parts.destination,
        scratch: &parts.scratch,
        base_staging: &parts.base_staging,
        base_path: &parts.base_path,
        temporary_index: &parts.temporary_index,
        branch: parts.branch.as_deref(),
        branch_created: parts.branch_created,
        sparse_directories: &parts.sparse_directories,
    };
    let record = JournalRecord::new_overlayfs(
        operation_id.clone(),
        paths,
        parts.expected_commit.clone(),
        &layout_root,
        token.clone(),
        None,
    );
    let is_token = token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit());
    let Ok(record) = record else {
        assert!(!is_token, "recovery token {token:?} was refused");
        return;
    };
    assert!(is_token, "recovery token {token:?} was accepted");

    let store = JournalStore::create(fixture.path()).expect("create store");
    let path = store.persist(&record).expect("persist journal");
    let decoded = store.load_operation(&operation_id).expect("load journal");
    let overlayfs = decoded.overlayfs.expect("mount intent");
    assert_eq!(decoded.backend, BackendKind::OverlayFs);
    assert_eq!(overlayfs.layout_root, layout_root);
    assert_eq!(overlayfs.recovery_token, token);
    assert_eq!(
        (overlayfs.mount_context, overlayfs.mount_identity),
        (None, None)
    );

    // The same intent under any other backend is a contradiction.
    let original = fs::read(&path).expect("read journal");
    let other = serde_json::to_value(parts.backend).expect("backend name");
    rewrite_json(&path, |journal| journal["backend"] = other);
    assert!(matches!(
        store.load_all(),
        Err(JournalError::InvalidRecord { .. })
    ));
    // So is an OverlayFS journal that lost its intent or its token.
    fs::write(&path, &original).expect("restore journal");
    let damage_token = tc.draw(gs::booleans());
    rewrite_json(&path, |journal| {
        if damage_token {
            journal["overlayfs"]["recovery_token"] = "not-a-token".into();
        } else {
            journal.as_object_mut().expect("object").remove("overlayfs");
        }
    });
    assert!(matches!(
        store.load_all(),
        Err(JournalError::InvalidRecord { .. })
    ));
}

/// A recorded mount identity must belong to the recorded mount context.
#[hegel::test(test_cases = FILESYSTEM_CASES)]
fn overlayfs_mount_identity_must_match_its_context(tc: TestCase) {
    let fixture = tempdir().expect("journal fixture");
    let parts = AddParts::draw(&tc);
    let operation_id = operation_id(&tc);
    let small = || gs::integers::<u64>().max_value(3);
    let context = riftri_storage::OverlayFsMountContext {
        profile: riftri_storage::OverlayFsMountProfile::default(),
        boot_id: tc.draw(gs::text().alphabet("ab").max_size(2)),
        mount_namespace_device: tc.draw(small()),
        mount_namespace_inode: tc.draw(small()),
    };
    let identity = riftri_storage::OverlayFsMountIdentity {
        profile: riftri_storage::OverlayFsMountProfile::default(),
        boot_id: tc.draw(gs::text().alphabet("ab").max_size(2)),
        mount_namespace_device: tc.draw(small()),
        mount_namespace_inode: tc.draw(small()),
        mount_id: tc.draw(small()),
    };
    let mut record = parts.record(&operation_id);
    record.backend = BackendKind::OverlayFs;
    record.overlayfs = Some(super::OverlayFsJournalRecord {
        layout_root: super::NativeOsString::encode(parts.scratch.as_os_str()),
        recovery_token: sha256_digest(&tc),
        mount_context: Some(context.clone()),
        mount_identity: Some(identity.clone()),
    });
    let store = JournalStore::create(fixture.path()).expect("create store");
    store.persist(&record).expect("persist journal");

    match store.load_operation(&operation_id) {
        Ok(decoded) => {
            assert_eq!(identity.context(), context);
            let overlayfs = decoded.overlayfs.expect("mount intent");
            assert_eq!(overlayfs.mount_context, Some(context));
            assert_eq!(overlayfs.mount_identity, Some(identity));
        }
        Err(JournalError::InvalidRecord { .. }) => assert_ne!(identity.context(), context),
        Err(error) => panic!("unexpected failure: {error}"),
    }
}
