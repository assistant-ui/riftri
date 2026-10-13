//! The state directory as untrusted input: immutable-base integrity, garbage
//! collection over arbitrary layouts with interruption, status diagnostics
//! for paths Riftri did not write, and state-directory registrations.
//! Riftri reports what it does not recognize and deletes only what a journal
//! and a real completion marker authorize.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};
use riftri_storage::BackendKind;

use super::super::tests::git;
use super::super::{
    StateDirectorySource, WorktreeError, add_worktree, forget_missing_state_directory,
    garbage_collect, garbage_collect_inner, is_managed_worktree, recover_incomplete_operations,
    repository_state_directories, storage_accounting, verify_existing_base,
    worktree_inventory_across_states,
};
use super::{Entry, Fixture, TreeFile, snapshot};
use crate::journal::{CollectionJournalStore, JournalPaths, JournalRecord, JournalStore};
use crate::{AddWorktreePhase, GarbageCollectionPhase, base_integrity};

const TREE: &str = "1111111111111111111111111111111111111111";

fn write_tree(root: &Path, tc: &TestCase) {
    fs::create_dir_all(root.join("nested")).unwrap();
    for name in ["a.txt", "nested/b.bin", "run.sh"] {
        fs::write(root.join(name), tc.draw(gs::binary().max_size(24))).unwrap();
    }
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("a.txt", root.join("link")).unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Current,
    Legacy,
    Empty,
    Garbage,
    Missing,
    Symlink,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseChange {
    None,
    Content,
    AddedFile,
    RemovedFile,
    ExecutableBit,
    LinkTarget,
    BaseRemoved,
    BaseIsFile,
}

/// A base is reused only when its current-format marker still matches every
/// byte, mode, and link under it; anything else is rebuilt or refused.
#[hegel::test(test_cases = 200)]
fn a_base_is_reused_only_while_its_marker_matches(tc: TestCase) {
    let state = tempfile::tempdir().unwrap();
    let bucket = state.path().join("bases/v1/r-bucket");
    let base = bucket.join(TREE);
    let complete = base.with_extension("complete");
    write_tree(&base, &tc);
    let marker = tc.draw(gs::sampled_from(vec![
        Marker::Current,
        Marker::Current,
        Marker::Legacy,
        Marker::Empty,
        Marker::Garbage,
        Marker::Missing,
        Marker::Symlink,
        Marker::Directory,
    ]));
    match marker {
        Marker::Current => fs::write(&complete, base_integrity::marker_v2(&base).unwrap()).unwrap(),
        Marker::Legacy => fs::write(&complete, base_integrity::marker(&base).unwrap()).unwrap(),
        Marker::Empty => fs::write(&complete, b"").unwrap(),
        Marker::Garbage => fs::write(&complete, tc.draw(gs::binary().min_size(1))).unwrap(),
        Marker::Missing => {}
        Marker::Symlink => symlink(bucket.join("elsewhere"), &complete).unwrap(),
        Marker::Directory => fs::create_dir(&complete).unwrap(),
    }
    let change = tc.draw(gs::sampled_from(vec![
        BaseChange::None,
        BaseChange::None,
        BaseChange::Content,
        BaseChange::AddedFile,
        BaseChange::RemovedFile,
        BaseChange::ExecutableBit,
        BaseChange::LinkTarget,
        BaseChange::BaseRemoved,
        BaseChange::BaseIsFile,
    ]));
    match change {
        BaseChange::None => {}
        BaseChange::Content => {
            let mut data = fs::read(base.join("nested/b.bin")).unwrap();
            data.push(b'!');
            fs::write(base.join("nested/b.bin"), data).unwrap();
        }
        BaseChange::AddedFile => fs::write(base.join("nested/new"), b"").unwrap(),
        BaseChange::RemovedFile => fs::remove_file(base.join("a.txt")).unwrap(),
        BaseChange::ExecutableBit => {
            fs::set_permissions(base.join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        BaseChange::LinkTarget => {
            fs::remove_file(base.join("link")).unwrap();
            symlink("run.sh", base.join("link")).unwrap();
        }
        BaseChange::BaseRemoved => fs::remove_dir_all(&base).unwrap(),
        BaseChange::BaseIsFile => {
            fs::remove_dir_all(&base).unwrap();
            fs::write(&base, b"not a directory").unwrap();
        }
    }
    let before = base.is_dir().then(|| snapshot(&base));

    let verdict = verify_existing_base(&base, &complete);
    tc.note(&format!("{marker:?} {change:?} -> {verdict:?}"));
    let unchanged = change == BaseChange::None;
    match (marker, change) {
        (Marker::Symlink | Marker::Directory, _) => {
            let message = verdict.expect_err("an unsafe marker").to_string();
            assert!(message.contains("is not a real file"), "{message}");
        }
        (Marker::Missing, _) | (_, BaseChange::BaseRemoved) => {
            assert!(!verdict.expect("nothing complete to verify"));
        }
        (_, BaseChange::BaseIsFile) => {
            let message = verdict.expect_err("a base that is a file").to_string();
            assert!(message.contains("is not a real directory"), "{message}");
        }
        (Marker::Current, _) if unchanged => assert!(verdict.expect("an intact base")),
        // A legacy marker cannot vouch for metadata, so the base is rebuilt.
        (Marker::Legacy, _) if unchanged => assert!(!verdict.expect("a legacy base")),
        // The legacy digest never covered the executable bit of a file.
        (Marker::Legacy, BaseChange::ExecutableBit) => {
            tc.note("legacy markers do not attest modes");
        }
        _ => {
            let message = verdict.expect_err("a damaged base").to_string();
            assert!(message.contains("integrity check failed"), "{message}");
            assert!(
                message.contains("riftri gc --apply --state-dir"),
                "{message}"
            );
            assert!(
                base.with_extension("damaged").is_file(),
                "a damaged base is marked for status"
            );
        }
    }
    assert_eq!(
        base.is_dir().then(|| snapshot(&base)),
        before,
        "verification changed the base"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BaseShape {
    Directory,
    ReadOnlyDirectory,
    File,
    Symlink,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkerShape {
    File,
    Symlink,
    Directory,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentShape {
    Real,
    BasesSymlink,
    VersionSymlink,
    VersionFile,
}

pub(super) struct SyntheticState {
    root: tempfile::TempDir,
    pub state: PathBuf,
    pub outside: PathBuf,
}

impl SyntheticState {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let resolved = fs::canonicalize(root.path()).unwrap();
        let outside = resolved.join("outside");
        fs::create_dir_all(outside.join("kept")).unwrap();
        fs::write(outside.join("kept/data"), b"user data\n").unwrap();
        Self {
            state: resolved.join("state"),
            outside,
            root,
        }
    }

    pub fn bucket(&self) -> PathBuf {
        self.state.join("bases/v1/r-bucket")
    }

    pub fn base(&self, index: usize) -> PathBuf {
        self.bucket().join(format!("{index:040x}"))
    }

    pub fn plant(&self, index: usize, base: BaseShape, marker: MarkerShape, tc: &TestCase) {
        let path = self.base(index);
        fs::create_dir_all(self.bucket()).unwrap();
        match base {
            BaseShape::Directory | BaseShape::ReadOnlyDirectory => write_tree(&path, tc),
            BaseShape::File => fs::write(&path, b"a file where a base belongs").unwrap(),
            BaseShape::Symlink => symlink(self.outside.join("kept"), &path).unwrap(),
            BaseShape::Missing => {}
        }
        if base == BaseShape::ReadOnlyDirectory {
            // Riftri seals real bases this way, and the marker attests modes.
            for directory in [path.join("nested"), path.clone()] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
        let complete = path.with_extension("complete");
        match marker {
            MarkerShape::File if path.is_dir() => {
                fs::write(&complete, base_integrity::marker_v2(&path).unwrap()).unwrap();
            }
            MarkerShape::File => fs::write(&complete, b"").unwrap(),
            MarkerShape::Symlink => symlink(self.outside.join("kept/data"), &complete).unwrap(),
            MarkerShape::Directory => fs::create_dir(&complete).unwrap(),
            MarkerShape::Missing => {}
        }
    }

    /// An active add journal that names `base`, as a view built on it leaves.
    pub fn reference(&self, base: &Path) {
        let mut record = JournalRecord::new(
            "18ddd0000000000-1-0".to_owned(),
            JournalPaths {
                repository: &self.outside,
                destination: &self.outside.join("view"),
                scratch: &self.outside.join(".view.scratch"),
                base_staging: &self.bucket().join(".staging"),
                base_path: base,
                temporary_index: &self.state.join("tmp/index"),
                branch: None,
                branch_created: false,
                sparse_directories: &[],
            },
            "2".repeat(40),
            BackendKind::ApfsClone,
        );
        for next in [
            AddWorktreePhase::GitMetadataCreated,
            AddWorktreePhase::BaseReady,
            AddWorktreePhase::ViewCreated,
            AddWorktreePhase::GitPointerRestored,
            AddWorktreePhase::IndexSynchronized,
            AddWorktreePhase::CleanVerified,
            AddWorktreePhase::Active,
        ] {
            record.transition(next).unwrap();
        }
        JournalStore::create(&self.state)
            .unwrap()
            .persist(&record)
            .unwrap();
    }

    /// Everything under the fixture, so any deletion or write is visible.
    pub fn everything(&self) -> BTreeMap<Vec<u8>, Entry> {
        snapshot(self.root.path())
    }
}

impl Drop for SyntheticState {
    fn drop(&mut self) {
        // Sealed bases would otherwise keep the temporary directory alive.
        let mut pending = vec![self.root.path().to_path_buf()];
        while let Some(directory) = pending.pop() {
            let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o755));
            for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(entry.path());
                }
            }
        }
    }
}

/// Garbage collection deletes a path only when it is a real directory with a
/// real completion marker directly under a real `bases/v1/<bucket>`. Symlinks
/// are never followed, a plan never writes, and unmarked paths are kept.
#[hegel::test(test_cases = 150)]
fn collection_deletes_only_real_marked_bases(tc: TestCase) {
    let fixture = SyntheticState::new();
    let shapes = (0..tc.draw(gs::integers::<usize>().min_value(1).max_value(4)))
        .map(|index| {
            let base = tc.draw(gs::sampled_from(vec![
                BaseShape::Directory,
                BaseShape::Directory,
                BaseShape::ReadOnlyDirectory,
                BaseShape::File,
                BaseShape::Symlink,
                BaseShape::Missing,
            ]));
            let marker = tc.draw(gs::sampled_from(vec![
                MarkerShape::File,
                MarkerShape::File,
                MarkerShape::File,
                MarkerShape::Symlink,
                MarkerShape::Directory,
                MarkerShape::Missing,
            ]));
            fixture.plant(index, base, marker, &tc);
            (base, marker)
        })
        .collect::<Vec<_>>();
    let parent = tc.draw(gs::sampled_from(vec![
        ParentShape::Real,
        ParentShape::Real,
        ParentShape::Real,
        ParentShape::BasesSymlink,
        ParentShape::VersionSymlink,
        ParentShape::VersionFile,
    ]));
    let relocated = fixture.outside.join("relocated");
    match parent {
        ParentShape::Real => {}
        ParentShape::BasesSymlink => {
            fs::rename(fixture.state.join("bases"), &relocated).unwrap();
            symlink(&relocated, fixture.state.join("bases")).unwrap();
        }
        ParentShape::VersionSymlink => {
            fs::rename(fixture.state.join("bases/v1"), &relocated).unwrap();
            symlink(&relocated, fixture.state.join("bases/v1")).unwrap();
        }
        ParentShape::VersionFile => {
            fs::rename(fixture.state.join("bases/v1"), &relocated).unwrap();
            fs::write(fixture.state.join("bases/v1"), b"").unwrap();
        }
    }
    let before = fixture.everything();

    let plan = garbage_collect(&fixture.state, false);
    assert_eq!(
        fixture.everything(),
        before,
        "a collection plan wrote to disk"
    );
    let applied = garbage_collect(&fixture.state, true);
    tc.note(&format!("{shapes:?} under {parent:?}: {applied:?}"));

    if parent != ParentShape::Real {
        let error = applied.expect_err("a redirected or replaced base root");
        match parent {
            ParentShape::VersionFile => {
                assert!(
                    error.to_string().contains("is not a real directory"),
                    "{error}"
                );
            }
            _ => assert!(
                matches!(error, WorktreeError::SymlinkedBaseParent { .. }),
                "{error}"
            ),
        }
        assert!(plan.is_err(), "a plan trusted a redirected base root");
        assert_eq!(
            fixture.everything(),
            before,
            "collection wrote through a link"
        );
        return;
    }

    let unsafe_marker = shapes
        .iter()
        .any(|(_, marker)| matches!(marker, MarkerShape::Symlink | MarkerShape::Directory));
    let marked = |shape: &(BaseShape, MarkerShape)| shape.1 == MarkerShape::File;
    // A marker whose base is already gone is collected too, with nothing
    // to reclaim.
    let real = |shape: &(BaseShape, MarkerShape)| {
        matches!(
            shape.0,
            BaseShape::Directory | BaseShape::ReadOnlyDirectory | BaseShape::Missing
        )
    };
    let collectible = shapes.iter().filter(|shape| marked(shape)).all(real);
    match &applied {
        Ok(report) => {
            assert!(
                !unsafe_marker && collectible,
                "collected past an unsafe entry"
            );
            let expected = shapes
                .iter()
                .enumerate()
                .filter(|(_, shape)| marked(shape))
                .map(|(index, _)| fixture.base(index))
                .collect::<Vec<_>>();
            assert_eq!(report.collected, expected);
            assert_eq!(
                plan.expect("plan").candidates.len(),
                expected.len(),
                "the plan and the run disagree"
            );
        }
        Err(_) => assert!(unsafe_marker || !collectible, "refused a clean layout"),
    }
    if unsafe_marker {
        assert_eq!(
            fixture.everything(),
            before,
            "an unsafe marker was acted on"
        );
    }

    let after = fixture.everything();
    for (path, entry) in &before {
        let text = String::from_utf8_lossy(path);
        let base_index = shapes.iter().enumerate().find(|(index, shape)| {
            marked(shape) && real(shape) && text.contains(&format!("{index:040x}"))
        });
        if base_index.is_none() {
            assert_eq!(
                after.get(path),
                Some(entry),
                "collection changed {text}, which no real marked base covers"
            );
        }
    }
    let accounting = storage_accounting(&fixture.state).expect("status after collection");
    assert_eq!(accounting.pending_collections, 0);
}

/// Whatever a plan lists as collectible, the run collects. A completion
/// marker whose base directory is gone is listed, with nothing to reclaim.
#[hegel::test(test_cases = 50)]
fn a_collection_run_collects_every_planned_candidate(tc: TestCase) {
    let fixture = SyntheticState::new();
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
    for index in 0..count {
        let base = tc.draw(gs::sampled_from(vec![
            BaseShape::Directory,
            BaseShape::ReadOnlyDirectory,
            BaseShape::Missing,
        ]));
        fixture.plant(index, base, MarkerShape::File, &tc);
    }
    let plan = garbage_collect(&fixture.state, false).expect("plan");
    let planned = plan
        .candidates
        .iter()
        .map(|candidate| candidate.base_path.clone())
        .collect::<Vec<_>>();
    assert_eq!(planned.len(), count);
    let applied = garbage_collect(&fixture.state, true).expect("collect what was planned");
    assert_eq!(applied.collected, planned);
    for index in 0..count {
        assert!(!fixture.base(index).exists());
        assert!(!fixture.base(index).with_extension("complete").exists());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interference {
    None,
    MarkerRecreated,
    ReferenceAppears,
    BaseDeleted,
    BaseBecomesFile,
    MarkerBecomesDirectory,
}

/// A collection stopped after any phase is finished or cancelled by the next
/// run. A reference that appears before the base is quarantined wins: the
/// base stays, with a marker that again vouches for its content.
#[hegel::test(test_cases = 150)]
fn an_interrupted_collection_resumes_or_yields_to_a_new_reference(tc: TestCase) {
    let fixture = SyntheticState::new();
    fixture.plant(0, BaseShape::ReadOnlyDirectory, MarkerShape::File, &tc);
    let base = fixture.base(0);
    let marker = base.with_extension("complete");
    let content = snapshot(&base);
    let phase = tc.draw(gs::sampled_from(vec![
        GarbageCollectionPhase::IntentRecorded,
        GarbageCollectionPhase::MarkerRemoved,
        GarbageCollectionPhase::BaseQuarantined,
    ]));
    let interference = tc.draw(gs::sampled_from(vec![
        Interference::None,
        Interference::MarkerRecreated,
        Interference::ReferenceAppears,
        Interference::BaseDeleted,
        Interference::BaseBecomesFile,
        Interference::MarkerBecomesDirectory,
    ]));
    let with_repair = tc.draw(gs::booleans());

    let stopped = garbage_collect_inner(&fixture.state, true, Some(phase));
    assert!(matches!(
        stopped,
        Err(WorktreeError::InjectedCollectionFailure(_))
    ));
    let quarantined = phase == GarbageCollectionPhase::BaseQuarantined;
    assert_eq!(base.exists(), !quarantined);
    assert_eq!(
        marker.exists(),
        phase == GarbageCollectionPhase::IntentRecorded
    );
    let pending = storage_accounting(&fixture.state).expect("status while pending");
    assert_eq!(pending.pending_collections, 1);

    match interference {
        Interference::None => {}
        // Once the base is quarantined a new marker would describe nothing.
        Interference::MarkerRecreated if !quarantined => fs::write(&marker, b"").unwrap(),
        Interference::ReferenceAppears => {
            fixture.reference(&base);
        }
        Interference::BaseDeleted if !quarantined => {
            fs::set_permissions(base.join("nested"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
            fs::remove_dir_all(&base).unwrap();
        }
        Interference::BaseBecomesFile if !quarantined => {
            fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
            fs::rename(&base, fixture.outside.join("moved-base")).unwrap();
            fs::write(&base, b"a file where the base was").unwrap();
        }
        Interference::MarkerBecomesDirectory if !quarantined => {
            let _ = fs::remove_file(&marker);
            fs::create_dir(&marker).unwrap();
        }
        Interference::MarkerRecreated
        | Interference::MarkerBecomesDirectory
        | Interference::BaseDeleted
        | Interference::BaseBecomesFile => {}
    }

    // The synthetic reference names no real repository for repair to query.
    let with_repair = with_repair && interference != Interference::ReferenceAppears;
    let errors = if with_repair {
        recover_incomplete_operations(&fixture.state)
            .expect("repair")
            .errors
    } else {
        garbage_collect(&fixture.state, true)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default()
    };
    tc.note(&format!("{phase:?} {interference:?}: {errors:?}"));
    let journals = CollectionJournalStore::open(&fixture.state)
        .load_all()
        .expect("collection journals");
    let phases = journals
        .iter()
        .map(|journal| journal.phase)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fs::read(fixture.outside.join("kept/data")).unwrap(),
        b"user data\n"
    );

    let reference_wins = !quarantined
        && (interference == Interference::ReferenceAppears
            // Repair only resumes; a collection run would go on to collect
            // the still unreferenced base under a fresh journal.
            || (interference == Interference::MarkerRecreated
                && phase == GarbageCollectionPhase::MarkerRemoved
                && with_repair));
    if reference_wins {
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(snapshot(&base), content, "the referenced base changed");
        assert!(phases.contains(&GarbageCollectionPhase::Cancelled));
        if interference == Interference::ReferenceAppears {
            assert!(
                verify_existing_base(&base, &marker).expect("verify the kept base"),
                "the restored marker does not vouch for the base"
            );
        }
        return;
    }
    match interference {
        Interference::BaseBecomesFile if !quarantined => {
            assert!(!errors.is_empty(), "a file was accepted as a base");
            assert_eq!(fs::read(&base).unwrap(), b"a file where the base was");
        }
        Interference::MarkerBecomesDirectory if !quarantined => {
            assert!(!errors.is_empty(), "a directory was accepted as a marker");
            assert!(marker.is_dir(), "an unrecognized marker was deleted");
            assert_eq!(
                snapshot(&base),
                content,
                "the base changed under a bad marker"
            );
        }
        _ => {
            assert!(errors.is_empty(), "{errors:?}");
            assert!(!base.exists(), "the base survived its collection");
            assert!(phases.contains(&GarbageCollectionPhase::Complete));
            let leftovers = fs::read_dir(fixture.bucket())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name.to_string_lossy().starts_with(".riftri-gc-"))
                .collect::<Vec<_>>();
            assert!(
                leftovers.is_empty(),
                "quarantine left behind: {leftovers:?}"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    File,
    Directory,
    Symlink,
}

/// Where a path is planted in a live state directory, the words status must
/// use to explain it (`None` when Riftri recognizes the name as its own and
/// stays silent), and whether the name is one Riftri itself writes and may
/// therefore clean up.
const PLANTS: &[(&str, &[Shape], Option<&str>, bool)] = &[
    (
        "notes.txt",
        &[Shape::File, Shape::Directory, Shape::Symlink],
        Some("not part of the versioned Riftri state layout"),
        false,
    ),
    (
        "tmp",
        &[Shape::File, Shape::Symlink],
        Some("expected a real Riftri state directory"),
        false,
    ),
    (
        "moves",
        &[Shape::File, Shape::Symlink],
        Some("expected a real Riftri state directory"),
        false,
    ),
    (
        "overlays",
        &[Shape::File],
        Some("expected a real Riftri state directory"),
        false,
    ),
    (
        "operations/stray.json.bak",
        &[Shape::File, Shape::Directory, Shape::Symlink],
        Some("not a recognized durable operation journal"),
        false,
    ),
    ("operations/18aa-1-0.lock", &[Shape::File], None, true),
    ("operations/18aa-1-0.retired", &[Shape::File], None, true),
    (
        "operations/.18aa-1-0.Ab12Cd.tmp",
        &[Shape::File],
        Some("`riftri repair` removes it"),
        true,
    ),
    (
        "removals/.remove-18aa-1-0.Ab12Cd.tmp",
        &[Shape::File],
        Some("it holds no operation and Riftri preserves it"),
        false,
    ),
    (
        "removals/unknown",
        &[Shape::File, Shape::Directory],
        Some("not a recognized durable operation journal"),
        false,
    ),
    (
        "tmp/leftover.index",
        &[Shape::File, Shape::Directory],
        Some("temporary artifact is not referenced"),
        false,
    ),
    (
        "bases/v0",
        &[Shape::File, Shape::Directory, Shape::Symlink],
        Some("not part of the supported immutable-base layout version"),
        false,
    ),
    (
        "bases/v1/stray-bucket",
        &[Shape::File, Shape::Symlink],
        Some("bucket must be a real directory"),
        false,
    ),
    (
        "bases/v1/empty-bucket",
        &[Shape::Directory],
        Some("empty immutable-base repository bucket"),
        false,
    ),
    (
        "bases/v1/r-other/unexplained",
        &[Shape::File, Shape::Directory, Shape::Symlink],
        Some("not explained by a completion marker or journal"),
        false,
    ),
    (
        "overlays/v2",
        &[Shape::File, Shape::Directory],
        Some("not part of the supported OverlayFS state layout version"),
        false,
    ),
    (
        "overlays/v1",
        &[Shape::File, Shape::Symlink],
        Some("OverlayFS layout root must be a real directory"),
        false,
    ),
    (
        "overlays/v1/orphan",
        &[Shape::File, Shape::Directory],
        Some("not referenced by a live add journal"),
        false,
    ),
];

/// Status names every path it cannot explain, and neither status, repair, nor
/// collection deletes one.
#[hegel::test(
    test_cases = 10,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn status_reports_foreign_state_paths_and_nothing_deletes_them(tc: TestCase) {
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("a.txt", b"text\n")]);
    let view = fixture.path("view");
    let added = add_worktree(fixture.add_request(&view, &commit)).expect("managed view");
    let clean = storage_accounting(&fixture.state).expect("status of a fresh state");
    assert!(clean.diagnostic_issues.is_empty(), "{clean:?}");

    let outside = fixture.path("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("data"), b"user data\n").unwrap();
    let plants = tc.draw(gs::subsequences(PLANTS).min_size(1).max_size(6));
    let mut planted = Vec::new();
    for (relative, shapes, explanation, owned) in plants {
        let path = fixture.state.join(relative);
        // A plant may sit under another one that is not a directory.
        let parent = path.parent().expect("a planted path has a parent");
        if path.exists() || fs::create_dir_all(parent).is_err() {
            continue;
        }
        match tc.draw(gs::sampled_from(shapes.to_vec())) {
            Shape::File => fs::write(&path, b"foreign").unwrap(),
            Shape::Directory => {
                fs::create_dir(&path).unwrap();
                if !relative.ends_with("empty-bucket") {
                    fs::write(path.join("inner"), b"foreign").unwrap();
                }
            }
            Shape::Symlink => symlink(&outside, &path).unwrap(),
        }
        planted.push((path, explanation, owned));
    }
    tc.note(&format!("planted {planted:?}"));
    let before = snapshot(&fixture.state);

    let accounting = storage_accounting(&fixture.state).expect("status with foreign paths");
    assert_eq!(accounting.active_views, 1, "{accounting:?}");
    assert_eq!(accounting.views[0].destination, view);
    for (path, explanation, _) in &planted {
        let reasons = accounting
            .diagnostic_issues
            .iter()
            .filter(|issue| &issue.path == path)
            .map(|issue| issue.reason.as_str())
            .collect::<Vec<_>>();
        match explanation {
            Some(expected) => assert!(
                reasons.iter().any(|reason| reason.contains(expected)),
                "{} is explained as {reasons:?}, not {expected:?}",
                path.display()
            ),
            None => assert!(reasons.is_empty(), "{reasons:?}"),
        }
    }
    let planted_paths = planted.iter().map(|(path, _, _)| path).collect::<Vec<_>>();
    for issue in &accounting.diagnostic_issues {
        assert!(
            planted_paths
                .iter()
                .any(|path| issue.path.starts_with(path)),
            "status blamed a path nobody planted: {issue:?}"
        );
    }

    let repair = recover_incomplete_operations(&fixture.state).expect("repair");
    assert!(repair.errors.is_empty(), "{:?}", repair.errors);
    for path in &repair.reaped_artifacts {
        assert!(
            planted
                .iter()
                .any(|(planted, _, owned)| *owned && planted == path),
            "repair reaped {}, which is not an interrupted journal write",
            path.display()
        );
    }
    let _ = garbage_collect(&fixture.state, true);
    let after = snapshot(&fixture.state);
    for (path, _, owned) in &planted {
        if *owned {
            continue;
        }
        let relative = path.strip_prefix(&fixture.state).unwrap().as_os_str();
        let key = relative.as_encoded_bytes().to_vec();
        assert_eq!(
            after.get(&key),
            before.get(&key),
            "{} was changed or deleted",
            path.display()
        );
    }
    assert_eq!(fs::read(outside.join("data")).unwrap(), b"user data\n");
    assert!(added.base_path.is_dir(), "the active view lost its base");
    assert_eq!(fs::read(view.join("a.txt")).unwrap(), b"text\n");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registration {
    Relative,
    Missing,
    File,
    Symlink,
    DanglingSymlink,
    EmptyDirectory,
    SameAgain,
    ThroughFile,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultState {
    Absent,
    Directory,
    File,
    Symlink,
}

/// The read-only inventory reports every registration it will not traverse
/// and still lists each managed view once. Lookups that authorize lifecycle
/// work fail closed on the same registrations instead of answering
/// "unmanaged", and only a registration whose directory is gone can be
/// forgotten.
#[hegel::test(
    test_cases = 16,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn registrations_are_reported_and_unsafe_ones_fail_closed(tc: TestCase) {
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("a.txt", b"text\n")]);
    let view = fixture.path("view");
    add_worktree(fixture.add_request(&view, &commit)).expect("managed view");

    let registrations = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            Registration::Relative,
            Registration::Missing,
            Registration::File,
            Registration::Symlink,
            Registration::DanglingSymlink,
            Registration::EmptyDirectory,
            Registration::SameAgain,
            Registration::ThroughFile,
            Registration::Unreadable,
        ]))
        .max_size(3)
        .unique(true),
    );
    let default = tc.draw(gs::sampled_from(vec![
        DefaultState::Absent,
        DefaultState::Absent,
        DefaultState::Directory,
        DefaultState::File,
        DefaultState::Symlink,
    ]));
    let default_path = fixture.repository.join(".git/riftri");
    match default {
        DefaultState::Absent => {}
        DefaultState::Directory => fs::create_dir(&default_path).unwrap(),
        DefaultState::File => fs::write(&default_path, b"").unwrap(),
        DefaultState::Symlink => symlink(&fixture.state, &default_path).unwrap(),
    }
    let mut unsafe_paths = Vec::new();
    let mut extra_states = 0;
    let mut missing = None;
    let mut unreadable = None;
    for registration in &registrations {
        let path = fixture.path(format!("registered-{registration:?}"));
        let value = match registration {
            Registration::Relative => PathBuf::from("relative/state"),
            Registration::Missing => {
                missing = Some(path.clone());
                path
            }
            Registration::File => {
                fs::write(&path, b"not a directory").unwrap();
                path
            }
            Registration::Symlink => {
                symlink(&fixture.state, &path).unwrap();
                path
            }
            Registration::DanglingSymlink => {
                symlink(fixture.path("nowhere"), &path).unwrap();
                path
            }
            Registration::EmptyDirectory => {
                fs::create_dir(&path).unwrap();
                extra_states += 1;
                path
            }
            // The same directory spelled another way is one state.
            Registration::SameAgain => fixture.state.join("."),
            Registration::ThroughFile => {
                fs::write(&path, b"not a directory").unwrap();
                path.join("state")
            }
            // A state whose journals cannot be read is listed, with the
            // failure as its one finding, rather than skipped.
            Registration::Unreadable => {
                fs::create_dir_all(path.join("operations")).unwrap();
                fs::write(path.join("operations/18aa-1-0.json"), b"not json").unwrap();
                unreadable = Some(path.clone());
                extra_states += 1;
                path
            }
        };
        if !matches!(
            registration,
            Registration::EmptyDirectory | Registration::SameAgain | Registration::Unreadable
        ) {
            unsafe_paths.push(value.clone());
        }
        git(
            &fixture.repository,
            &[
                "config",
                "--add",
                "riftri.stateDirectory",
                value.to_str().unwrap(),
            ],
        );
    }
    let default_unsafe = matches!(default, DefaultState::File | DefaultState::Symlink);
    tc.note(&format!("{registrations:?}, default {default:?}"));

    let inventory = worktree_inventory_across_states(&fixture.repository).expect("inventory");
    for path in &unsafe_paths {
        assert!(
            inventory
                .registration_issues
                .iter()
                .any(|issue| &issue.path == path),
            "{} was not reported: {:?}",
            path.display(),
            inventory.registration_issues
        );
    }
    assert_eq!(
        inventory.registration_issues.len(),
        unsafe_paths.len() + usize::from(default_unsafe)
    );
    let views = inventory
        .states
        .iter()
        .flat_map(|state| &state.views)
        .map(|view| view.destination.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        views,
        vec![view.clone()],
        "each managed view is listed once"
    );
    let expected_states = 1 + extra_states + usize::from(default == DefaultState::Directory);
    assert_eq!(
        inventory.states.len(),
        expected_states,
        "{:?}",
        inventory.states
    );
    assert_eq!(
        inventory
            .states
            .iter()
            .filter(|state| state.source == StateDirectorySource::Default)
            .count(),
        usize::from(default == DefaultState::Directory)
    );

    if let Some(unreadable) = &unreadable {
        let state = inventory
            .states
            .iter()
            .find(|state| &state.state_directory == unreadable)
            .expect("the unreadable state is listed");
        assert!(state.views.is_empty());
        assert_eq!(
            state.diagnostic_issues.len(),
            1,
            "{:?}",
            state.diagnostic_issues
        );
        assert!(
            state.diagnostic_issues[0]
                .reason
                .contains("could not be inventoried")
        );
    }

    let strict = repository_state_directories(&fixture.repository);
    let managed = is_managed_worktree(&fixture.repository, &view);
    if unsafe_paths.is_empty() && !default_unsafe {
        assert_eq!(strict.expect("state directories").len(), expected_states);
        // Ownership cannot be decided while one state's journals are unreadable.
        assert_eq!(managed.ok(), unreadable.is_none().then_some(true));
    } else {
        strict.expect_err("an unsafe registration must stop discovery");
        managed.expect_err("an unsafe registration must not read as unmanaged");
    }

    forget_missing_state_directory(&fixture.repository, &fixture.state)
        .expect_err("an existing state directory is never unregistered");
    forget_missing_state_directory(&fixture.repository, &fixture.path("never-registered"))
        .expect_err("an unregistered path");
    if let Some(missing) = missing {
        assert_eq!(
            forget_missing_state_directory(&fixture.repository, &missing).expect("forget"),
            missing
        );
        forget_missing_state_directory(&fixture.repository, &missing)
            .expect_err("a registration is forgotten once");
        let after = worktree_inventory_across_states(&fixture.repository).expect("inventory");
        assert!(
            !after
                .registration_issues
                .iter()
                .any(|issue| issue.path == missing)
        );
        assert_eq!(
            after.registration_issues.len(),
            inventory.registration_issues.len() - 1
        );
    }
    assert!(fixture.root().exists());
}
