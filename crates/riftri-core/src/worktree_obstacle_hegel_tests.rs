//! Lifecycle verbs and repair against a filesystem that refuses them: a lock
//! path that is not a lockable file, a directory that cannot be written or
//! read. The verb fails or is reported, the view it was aimed at is not
//! half-changed, and once the obstacle is gone repair finishes whatever was
//! started.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, RemoveWorktreeRequest, WorktreeError,
    add_worktree_inner, compact_worktree_inner, completed_base_paths_present,
    force_remove_worktree_inner, garbage_collect, move_worktree_inner,
    recover_incomplete_operations, remove_worktree_inner, storage_accounting, verify_existing_base,
    worktree_inventory_across_states,
};
use super::recovery::Scene;
use super::{Entry, registered, snapshot};
use crate::journal::JournalStore;
use crate::{AddWorktreePhase, CompactWorktreePhase, MoveWorktreePhase, RemoveWorktreePhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Remove,
    ForceRemove,
    Move,
    Compact,
    AddAnother,
    Collect,
    /// Repair with an add, a removal, a move, or a compaction left pending.
    Repair(Pending),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Add,
    Removal,
    Move,
    Compaction,
    Nothing,
}

struct Stage {
    scene: Scene,
    view: PathBuf,
    moved: PathBuf,
    another: PathBuf,
}

impl Stage {
    fn new(verb: Verb) -> Self {
        let scene = Scene::with_view();
        let stage = Self {
            view: scene.view.clone(),
            moved: scene.fixture.path("moved"),
            another: scene.fixture.path("another"),
            scene,
        };
        let stopped = match verb {
            Verb::Repair(Pending::Add) => add_worktree_inner(
                stage
                    .scene
                    .fixture
                    .add_request(&stage.another, &stage.scene.commits[1]),
                Some(AddWorktreePhase::ViewCreated),
                false,
            )
            .map(|_| ()),
            Verb::Repair(Pending::Removal) => {
                remove_worktree_inner(stage.removal(), Some(RemoveWorktreePhase::CleanVerified))
                    .map(|_| ())
            }
            Verb::Repair(Pending::Move) => {
                move_worktree_inner(stage.relocation(), Some(MoveWorktreePhase::IntentRecorded))
                    .map(|_| ())
            }
            Verb::Repair(Pending::Compaction) => {
                assert!(stage.scene.git(
                    &stage.view,
                    &["checkout", "--quiet", "--detach", &stage.scene.commits[1]]
                ));
                compact_worktree_inner(
                    stage.compaction(),
                    Some(CompactWorktreePhase::ReplacementReady),
                )
                .map(|_| ())
            }
            _ => Err(WorktreeError::InvalidRequest("nothing to stop".to_owned())),
        };
        stopped.expect_err("leave an operation pending");
        stage
    }

    fn removal(&self) -> RemoveWorktreeRequest {
        RemoveWorktreeRequest {
            repository: self.scene.fixture.repository.clone(),
            destination: self.view.clone(),
            state_dir: Some(self.scene.fixture.state.clone()),
        }
    }

    fn relocation(&self) -> MoveWorktreeRequest {
        MoveWorktreeRequest {
            repository: self.scene.fixture.repository.clone(),
            source: self.view.clone(),
            destination: self.moved.clone(),
            state_dir: Some(self.scene.fixture.state.clone()),
        }
    }

    fn compaction(&self) -> CompactWorktreeRequest {
        CompactWorktreeRequest {
            repository: self.scene.fixture.repository.clone(),
            destination: self.view.clone(),
            state_dir: Some(self.scene.fixture.state.clone()),
        }
    }

    /// Run the verb; `Ok` carries how many problems a repair reported.
    fn run(&self, verb: Verb) -> Result<usize, WorktreeError> {
        match verb {
            Verb::Remove => remove_worktree_inner(self.removal(), None).map(|_| 0),
            Verb::ForceRemove => force_remove_worktree_inner(self.removal(), None).map(|_| 0),
            Verb::Move => move_worktree_inner(self.relocation(), None).map(|_| 0),
            Verb::Compact => compact_worktree_inner(self.compaction(), None).map(|_| 0),
            Verb::AddAnother => add_worktree_inner(
                self.scene
                    .fixture
                    .add_request(&self.another, &self.scene.commits[1]),
                None,
                true,
            )
            .map(|_| 0),
            Verb::Collect => garbage_collect(&self.scene.fixture.state, true).map(|_| 0),
            Verb::Repair(_) => recover_incomplete_operations(&self.scene.fixture.state)
                .map(|report| report.errors.len()),
        }
    }

    /// The view's files, wherever a move may have put them.
    fn view_content(&self) -> Option<BTreeMap<Vec<u8>, Entry>> {
        [&self.view, &self.moved]
            .into_iter()
            .find(|path| path.join("tracked.txt").is_file())
            .map(|path| snapshot(path))
    }

    /// With the obstacle gone, repair finishes and the view is whole or gone.
    fn recovers(&self, tc: &TestCase, verb: Verb, content: &Option<BTreeMap<Vec<u8>, Entry>>) {
        let report = self.scene.settle(tc, &[&self.view, &self.moved]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let removes = matches!(
            verb,
            Verb::Remove | Verb::ForceRemove | Verb::Repair(Pending::Removal)
        );
        match self.view_content() {
            Some(now) => {
                if let Some(content) = content {
                    assert_eq!(content, &now, "the view changed across a refused verb");
                }
            }
            None => assert!(removes, "{verb:?} lost the view"),
        }
        for place in [&self.view, &self.moved, &self.another] {
            assert_eq!(
                place.join(".git").exists(),
                registered(&self.scene.fixture.repository, place),
                "{} is half registered",
                place.display()
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lock {
    /// The per-view lock beside the add journal.
    View,
    /// The repository-wide lock over Git's worktree registry.
    WorktreeMetadata,
    /// The lock over the immutable base the view was cloned from.
    Base,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotALock {
    Directory,
    Fifo,
    DanglingSymlink,
    SymlinkToFile,
}

fn lock_obstacle(tc: TestCase, verb: Verb) {
    let lock = tc.draw(gs::sampled_from(vec![
        Lock::View,
        Lock::WorktreeMetadata,
        Lock::Base,
    ]));
    let obstacle = tc.draw(gs::sampled_from(vec![
        NotALock::Directory,
        NotALock::Fifo,
        NotALock::DanglingSymlink,
        NotALock::SymlinkToFile,
    ]));
    let stage = Stage::new(verb);
    let state = stage.scene.fixture.state.clone();
    let managed = JournalStore::open(&state)
        .load_all()
        .expect("add journals")
        .into_iter()
        .find(|journal| journal.destination == stage.view)
        .expect("the view's journal");
    let path = match lock {
        Lock::View => managed.journal_path.with_extension("lock"),
        Lock::WorktreeMetadata => stage
            .scene
            .fixture
            .repository
            .join(".git/riftri-worktree-metadata.lock"),
        Lock::Base => managed.base_path.with_extension("lock"),
    };
    let _ = fs::remove_file(&path);
    let target = stage.scene.fixture.path("lock-target");
    match obstacle {
        NotALock::Directory => fs::create_dir(&path).unwrap(),
        NotALock::Fifo => {
            assert!(
                Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        NotALock::DanglingSymlink => symlink(&target, &path).unwrap(),
        NotALock::SymlinkToFile => {
            fs::write(&target, b"").unwrap();
            symlink(&target, &path).unwrap();
        }
    }
    tc.note(&format!("{lock:?} is {obstacle:?}, {verb:?}"));
    let content = stage.view_content();
    let journals = |directory: &str| {
        fs::read_dir(state.join(directory))
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    let removals = journals("removals");

    // Read-only reports take no locks, so they still answer.
    storage_accounting(&state).expect("status beside an unusable lock");
    worktree_inventory_across_states(&stage.scene.fixture.repository).expect("inventory");

    let outcome = stage.run(verb);
    tc.note(&format!("outcome: {outcome:?}"));
    // Nothing follows a link planted where a lock belongs.
    assert!(
        !target.exists() || fs::read(&target).unwrap().is_empty(),
        "a lock was written through a symbolic link"
    );
    if outcome.is_err() && matches!(verb, Verb::Remove | Verb::ForceRemove) {
        assert_eq!(
            stage.view_content(),
            content,
            "a refused removal changed the view"
        );
        assert!(registered(&stage.scene.fixture.repository, &stage.view));
        assert_eq!(
            journals("removals"),
            removals,
            "a refused removal left a journal"
        );
    }

    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir(&path).unwrap();
    } else {
        fs::remove_file(&path).unwrap();
    }
    stage.recovers(&tc, verb, &content);
}

macro_rules! per_verb {
    ($scenario:ident, $cases:expr; $($name:ident => $verb:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = $cases,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                $scenario(tc, $verb);
            }
        )+
    };
}

per_verb!(lock_obstacle, 12;
    removing_without_a_usable_lock_fails_cleanly => Verb::Remove,
    force_removing_without_a_usable_lock_fails_cleanly => Verb::ForceRemove,
    moving_without_a_usable_lock_fails_cleanly => Verb::Move,
    compacting_without_a_usable_lock_fails_cleanly => Verb::Compact,
    adding_without_a_usable_lock_fails_cleanly => Verb::AddAnother,
    collecting_without_a_usable_lock_fails_cleanly => Verb::Collect,
    repairing_an_add_without_a_usable_lock_fails_cleanly => Verb::Repair(Pending::Add),
    repairing_a_removal_without_a_usable_lock_fails_cleanly => Verb::Repair(Pending::Removal),
    repairing_a_move_without_a_usable_lock_fails_cleanly => Verb::Repair(Pending::Move),
    repairing_a_compaction_without_a_usable_lock_fails_cleanly => Verb::Repair(Pending::Compaction),
    repairing_nothing_without_a_usable_lock_fails_cleanly => Verb::Repair(Pending::Nothing),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sealed {
    /// A journal directory, the temporary directory, or the base bucket
    /// cannot be written.
    StateSubdirectory(&'static str),
    BaseBucket,
    /// The directory that holds the view and its siblings cannot be written.
    ViewParent,
    /// A directory inside the view cannot be read.
    InsideView,
}

/// Puts permissions back even when an assertion fails first, so the fixture
/// can always be deleted.
struct Restore(Vec<(PathBuf, u32)>);

impl Restore {
    fn seal(&mut self, path: &Path, mode: u32) {
        let current = fs::metadata(path).unwrap().permissions().mode();
        self.0.push((path.to_path_buf(), current));
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn undo(&mut self) {
        for (path, mode) in self.0.drain(..).rev() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
        }
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        self.undo();
    }
}

fn permission_obstacle(tc: TestCase, verb: Verb) {
    let sealed = tc.draw(gs::sampled_from(vec![
        Sealed::StateSubdirectory("operations"),
        Sealed::StateSubdirectory("removals"),
        Sealed::StateSubdirectory("moves"),
        Sealed::StateSubdirectory("compactions"),
        Sealed::StateSubdirectory("collections"),
        Sealed::StateSubdirectory("tmp"),
        Sealed::BaseBucket,
        Sealed::ViewParent,
        Sealed::InsideView,
    ]));
    let stage = Stage::new(verb);
    let state = stage.scene.fixture.state.clone();
    let managed = JournalStore::open(&state)
        .load_all()
        .expect("add journals")
        .into_iter()
        .find(|journal| journal.destination == stage.view)
        .expect("the view's journal");
    let content = stage.view_content();
    let mut restore = Restore(Vec::new());
    match sealed {
        Sealed::StateSubdirectory(name) => {
            let directory = state.join(name);
            fs::create_dir_all(&directory).unwrap();
            restore.seal(&directory, 0o555);
        }
        Sealed::BaseBucket => restore.seal(managed.base_path.parent().unwrap(), 0o555),
        Sealed::ViewParent => restore.seal(stage.view.parent().unwrap(), 0o555),
        Sealed::InsideView => {
            restore.seal(&stage.view.join("nested"), 0o000);
            // A move takes the sealed directory with it.
            restore.0.push((stage.moved.join("nested"), 0o755));
        }
    }
    tc.note(&format!("{sealed:?}, {verb:?}"));

    // A report either answers or fails; it never writes.
    let status = storage_accounting(&state).map(|_| ());
    let inventory = worktree_inventory_across_states(&stage.scene.fixture.repository)
        .expect("the inventory reports a state it cannot read instead of failing");
    if status.is_err() {
        assert!(
            inventory
                .states
                .iter()
                .any(|state| !state.diagnostic_issues.is_empty()),
            "the inventory hid a state that status cannot read"
        );
    }
    let outcome = stage.run(verb);
    tc.note(&format!("status {status:?}, outcome {outcome:?}"));
    restore.undo();

    if outcome.is_err() && matches!(verb, Verb::Remove | Verb::ForceRemove) {
        assert_eq!(
            stage.view_content(),
            content,
            "a refused removal changed the view"
        );
    }
    stage.recovers(&tc, verb, &content);
}

per_verb!(permission_obstacle, 9;
    removing_under_sealed_directories_fails_cleanly => Verb::Remove,
    force_removing_under_sealed_directories_fails_cleanly => Verb::ForceRemove,
    moving_under_sealed_directories_fails_cleanly => Verb::Move,
    compacting_under_sealed_directories_fails_cleanly => Verb::Compact,
    adding_under_sealed_directories_fails_cleanly => Verb::AddAnother,
    collecting_under_sealed_directories_fails_cleanly => Verb::Collect,
    repairing_an_add_under_sealed_directories_fails_cleanly => Verb::Repair(Pending::Add),
    repairing_a_removal_under_sealed_directories_fails_cleanly => Verb::Repair(Pending::Removal),
    repairing_a_move_under_sealed_directories_fails_cleanly => Verb::Repair(Pending::Move),
    repairing_a_compaction_under_sealed_directories_fails_cleanly => Verb::Repair(Pending::Compaction),
    repairing_nothing_under_sealed_directories_fails_cleanly => Verb::Repair(Pending::Nothing),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unreadable {
    Marker,
    /// The bucket cannot be searched, so neither base nor marker can be seen.
    Bucket,
    BaseDirectory,
    NestedDirectory,
    File,
}

/// A base whose marker or content cannot be read is never reported as
/// reusable, and checking it changes nothing.
#[hegel::test(test_cases = 60, derandomize = true)]
fn an_unreadable_base_or_marker_is_never_trusted(tc: TestCase) {
    let fixture = tempfile::tempdir().unwrap();
    let bucket = fs::canonicalize(fixture.path())
        .unwrap()
        .join("state/bases/v1/r-bucket");
    let base = bucket.join("1".repeat(40));
    let marker = base.with_extension("complete");
    fs::create_dir_all(base.join("nested")).unwrap();
    for name in ["a.txt", "nested/b.bin"] {
        fs::write(base.join(name), tc.draw(gs::binary().max_size(24))).unwrap();
    }
    fs::write(&marker, crate::base_integrity::marker_v2(&base).unwrap()).unwrap();
    assert!(verify_existing_base(&base, &marker).expect("an intact base"));
    let before = snapshot(&bucket);

    let unreadable = tc.draw(gs::sampled_from(vec![
        Unreadable::Marker,
        Unreadable::Bucket,
        Unreadable::BaseDirectory,
        Unreadable::NestedDirectory,
        Unreadable::File,
    ]));
    let mut restore = Restore(Vec::new());
    match unreadable {
        Unreadable::Marker => restore.seal(&marker, 0o000),
        Unreadable::Bucket => restore.seal(&bucket, 0o000),
        Unreadable::BaseDirectory => restore.seal(&base, 0o000),
        Unreadable::NestedDirectory => restore.seal(&base.join("nested"), 0o000),
        Unreadable::File => restore.seal(&base.join("a.txt"), 0o000),
    }
    let verdict = verify_existing_base(&base, &marker);
    let present = completed_base_paths_present(&base, &marker);
    restore.undo();
    tc.note(&format!("{unreadable:?}: {verdict:?}, present {present:?}"));

    assert!(
        !matches!(verdict, Ok(true)),
        "a base that could not be read was reused"
    );
    if unreadable == Unreadable::Bucket {
        present.expect_err("an unsearchable bucket was reported as holding nothing");
    }
    // A failed read is not evidence of damage.
    let _ = fs::remove_file(base.with_extension("damaged"));
    assert_eq!(snapshot(&bucket), before, "checking a base changed it");
    assert!(verify_existing_base(&base, &marker).expect("the base is intact again"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfoDirectory {
    File,
    SymlinkToDirectory,
    DanglingSymlink,
    Unsearchable,
}

/// Attributes outside the tree change what a checkout holds, so a repository
/// whose `info` directory cannot be inspected safely is refused before
/// anything is created, and accepted again once it is ordinary.
#[hegel::test(
    test_cases = 8,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn an_uninspectable_info_directory_refuses_the_add(tc: TestCase) {
    let shape = tc.draw(gs::sampled_from(vec![
        InfoDirectory::File,
        InfoDirectory::SymlinkToDirectory,
        InfoDirectory::DanglingSymlink,
        InfoDirectory::Unsearchable,
    ]));
    let with_attributes = tc.draw(gs::booleans());
    let scene = Scene::new();
    let info = scene.fixture.repository.join(".git/info");
    let aside = scene.fixture.path("info-aside");
    fs::create_dir_all(&info).unwrap();
    if with_attributes {
        fs::write(info.join("attributes"), b"").unwrap();
    }
    let mut restore = Restore(Vec::new());
    match shape {
        InfoDirectory::File => {
            fs::rename(&info, &aside).unwrap();
            fs::write(&info, b"").unwrap();
        }
        InfoDirectory::SymlinkToDirectory => {
            fs::rename(&info, &aside).unwrap();
            symlink(&aside, &info).unwrap();
        }
        InfoDirectory::DanglingSymlink => {
            fs::rename(&info, &aside).unwrap();
            symlink(scene.fixture.path("nowhere"), &info).unwrap();
        }
        InfoDirectory::Unsearchable => restore.seal(&info, 0o000),
    }
    tc.note(&format!("{shape:?}, attributes file {with_attributes}"));
    let request = scene.fixture.add_request(&scene.view, &scene.commits[0]);

    let refused = add_worktree_inner(request.clone(), None, true);
    restore.undo();
    let error = refused.expect_err("an add trusted attributes it could not inspect");
    assert!(matches!(error, WorktreeError::Unsupported(_)), "{error:?}");
    assert!(!scene.view.exists() && !scene.fixture.state.exists());
    assert!(!registered(&scene.fixture.repository, &scene.view));

    if shape != InfoDirectory::Unsearchable {
        fs::remove_file(&info).unwrap();
        fs::rename(&aside, &info).unwrap();
    }
    add_worktree_inner(request, None, true).expect("an ordinary info directory");
    assert_eq!(
        fs::read(scene.view.join("tracked.txt")).unwrap(),
        b"first\n"
    );
}
