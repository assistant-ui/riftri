//! Repair after a kill: each lifecycle verb is stopped after a generated
//! journal phase, something then changes behind Riftri's back (a user write, a
//! plain Git command, a tampered journal), and `repair` runs. Whatever the
//! combination, repair never deletes what a user wrote or a path outside the
//! operation, and a pass that reports no error leaves nothing half done.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::tests::git_stdout;
use super::super::{
    CompactWorktreeRequest, MoveWorktreeRequest, RecoveryReport, RemoveWorktreeRequest,
    WorktreeError, WorktreeMode, add_worktree_inner, compact_worktree_inner,
    force_remove_worktree_inner, move_worktree_inner, recover_incomplete_operations,
    remove_worktree_inner, storage_accounting,
};
use super::{Fixture, TreeFile, registered, snapshot, status};
use crate::journal::CompactJournalStore;
use crate::{AddWorktreePhase, CompactWorktreePhase, MoveWorktreePhase, RemoveWorktreePhase};

pub(super) struct Scene {
    pub fixture: Fixture,
    pub commits: [String; 2],
    pub view: PathBuf,
    /// Paths a user owns, with the bytes repair must leave in one of them.
    pub kept: Vec<(Vec<PathBuf>, Vec<u8>)>,
    /// Set when the user moved a ref under the view, which Git reports as
    /// changes in it.
    pub ref_moved: bool,
}

impl Scene {
    pub fn new() -> Self {
        let fixture = Fixture::new();
        let commits = [
            fixture.commit(
                "main",
                &[
                    TreeFile::regular("tracked.txt", b"first\n"),
                    TreeFile::regular("nested/deep.txt", b"deep\n"),
                ],
            ),
            fixture.commit(
                "other",
                &[
                    TreeFile::regular("tracked.txt", b"second\n"),
                    TreeFile::regular("nested/deep.txt", b"deep\n"),
                ],
            ),
        ];
        let view = fixture.path("view");
        Self {
            fixture,
            commits,
            view,
            kept: Vec::new(),
            ref_moved: false,
        }
    }

    pub fn with_view() -> Self {
        let scene = Self::new();
        add_worktree_inner(
            scene.fixture.add_request(&scene.view, &scene.commits[0]),
            None,
            true,
        )
        .expect("managed view");
        scene
    }

    /// A file the user creates; repair must keep it at one of `homes`.
    pub fn user_writes(&mut self, at: &Path, homes: &[&Path]) {
        let data = b"written by the user\n".to_vec();
        fs::create_dir_all(at).unwrap();
        fs::write(at.join("user-file.txt"), &data).unwrap();
        self.kept.push((
            homes
                .iter()
                .map(|home| home.join("user-file.txt"))
                .collect(),
            data,
        ));
    }

    pub fn git(&self, directory: &Path, arguments: &[&str]) -> bool {
        Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run git")
            .status
            .success()
    }

    /// Git forgets a worktree whose directory is away while it prunes.
    pub fn unregister(&self, view: &Path) {
        let aside = self.fixture.path("set-aside");
        fs::rename(view, &aside).unwrap();
        assert!(self.git(&self.fixture.repository, &["worktree", "prune"]));
        fs::rename(&aside, view).unwrap();
    }

    pub fn only_journal(&self, directory: &str) -> PathBuf {
        let mut journals = fs::read_dir(self.fixture.state.join(directory))
            .expect("journal directory")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        assert_eq!(journals.len(), 1, "{journals:?}");
        journals.remove(0)
    }

    /// Point one path of a journal at a directory the user owns.
    fn retarget(&mut self, journal: &Path, field: &str) {
        let elsewhere = self.fixture.path("elsewhere");
        self.user_writes(&elsewhere.clone(), &[&elsewhere]);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
        assert!(value[field]["units"].is_array(), "{field} is a path field");
        value[field]["units"] = serde_json::json!(elsewhere.as_os_str().as_bytes());
        fs::write(journal, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    /// Run repair until a pass finds nothing new, and check what every
    /// scenario shares.
    pub fn settle(&self, tc: &TestCase, places: &[&Path]) -> RecoveryReport {
        let report = recover_incomplete_operations(&self.fixture.state).expect("repair");
        tc.note(&format!("repair: {report:?}"));
        let again = recover_incomplete_operations(&self.fixture.state).expect("repair again");
        tc.note(&format!("repair again: {:?}", again.errors));
        assert_eq!(
            (
                again.recovered,
                again.recovered_removals,
                again.recovered_moves,
                again.recovered_compactions,
                again.retired_adds,
                again.errors.len(),
            ),
            (0, 0, 0, 0, 0, report.errors.len()),
            "a second repair found different work: {again:?}"
        );
        for (homes, data) in &self.kept {
            assert!(
                homes
                    .iter()
                    .any(|home| fs::read(home).is_ok_and(|found| &found == data)),
                "repair lost the user's {}",
                homes[0].display()
            );
        }
        let present = places
            .iter()
            .filter(|place| place.join(".git").exists())
            .collect::<Vec<_>>();
        assert!(present.len() <= 1, "the view exists twice: {present:?}");
        if !report.errors.is_empty() {
            return report;
        }
        let accounting = storage_accounting(&self.fixture.state).expect("status after repair");
        assert_eq!(
            (
                accounting.pending_adds,
                accounting.pending_removals,
                accounting.pending_moves,
                accounting.pending_compactions,
            ),
            (0, 0, 0, 0),
            "an error-free repair left work pending: {accounting:?}"
        );
        for view in &accounting.views {
            assert!(
                registered(&self.fixture.repository, &view.destination)
                    && view.destination.join(".git").exists(),
                "status lists a view that is gone: {}",
                view.destination.display()
            );
            assert!(view.base_path.is_dir(), "an active view lost its base");
        }
        for place in places {
            if place.join(".git").exists() && self.kept.is_empty() && !self.ref_moved {
                assert_eq!(status(place), "", "an untouched view is dirty after repair");
            }
        }
        report
    }
}

pub(super) fn injected(error: &WorktreeError) -> bool {
    error.to_string().contains("injected")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalInterference {
    None,
    UserWrites,
    UserDeletes,
    GitRemoves,
    Unregistered,
    Quarantined,
    QuarantinedAndPruned,
    QuarantineBeside,
    Reappears,
    AddJournalGone,
    Retargeted(&'static str),
}

fn removal_scenario(tc: TestCase, phase: RemoveWorktreePhase) {
    let force = tc.draw(gs::booleans());
    let interference = tc.draw(gs::sampled_from(vec![
        RemovalInterference::None,
        RemovalInterference::UserWrites,
        RemovalInterference::UserDeletes,
        RemovalInterference::GitRemoves,
        RemovalInterference::Unregistered,
        RemovalInterference::Quarantined,
        RemovalInterference::QuarantinedAndPruned,
        RemovalInterference::QuarantineBeside,
        RemovalInterference::Reappears,
        RemovalInterference::AddJournalGone,
        RemovalInterference::Retargeted("destination"),
        RemovalInterference::Retargeted("base_path"),
        RemovalInterference::Retargeted("repository"),
    ]));
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    let request = RemoveWorktreeRequest {
        repository: scene.fixture.repository.clone(),
        destination: view.clone(),
        state_dir: Some(scene.fixture.state.clone()),
    };
    let stopped = if force {
        force_remove_worktree_inner(request, Some(phase))
    } else {
        remove_worktree_inner(request, Some(phase))
    }
    .expect_err("stop the removal");
    assert!(injected(&stopped), "{stopped}");
    let journal = scene.only_journal("removals");
    let operation = journal.file_stem().unwrap().to_str().unwrap().to_owned();
    let quarantine = scene.fixture.path(format!(".riftri-remove-{operation}"));
    let present = view.is_dir();
    assert_eq!(present, phase <= RemoveWorktreePhase::CleanVerified);

    match interference {
        RemovalInterference::None => {}
        RemovalInterference::UserWrites if present => scene.user_writes(&view.clone(), &[&view]),
        RemovalInterference::UserDeletes if present => fs::remove_dir_all(&view).unwrap(),
        RemovalInterference::GitRemoves if present => {
            assert!(scene.git(
                &scene.fixture.repository,
                &["worktree", "remove", "--force", view.to_str().unwrap()]
            ));
        }
        RemovalInterference::Unregistered if present => {
            scene.unregister(&view);
            scene
                .kept
                .push((vec![view.join("tracked.txt")], b"first\n".to_vec()));
        }
        RemovalInterference::Quarantined if present => fs::rename(&view, &quarantine).unwrap(),
        RemovalInterference::QuarantinedAndPruned if present => {
            fs::rename(&view, &quarantine).unwrap();
            assert!(scene.git(&scene.fixture.repository, &["worktree", "prune"]));
        }
        RemovalInterference::QuarantineBeside if present => {
            scene.user_writes(&quarantine.clone(), &[&quarantine]);
            scene
                .kept
                .push((vec![view.join("tracked.txt")], b"first\n".to_vec()));
        }
        RemovalInterference::Reappears if !present => scene.user_writes(&view.clone(), &[&view]),
        RemovalInterference::AddJournalGone => {
            let add = scene.only_journal("operations");
            fs::remove_file(add).unwrap();
            if present {
                scene
                    .kept
                    .push((vec![view.join("tracked.txt")], b"first\n".to_vec()));
            }
        }
        RemovalInterference::Retargeted(field) => scene.retarget(&journal, field),
        _ => {}
    }
    tc.note(&format!("force {force}, {phase:?}, {interference:?}"));

    let report = scene.settle(&tc, &[&view]);
    let tampered = matches!(
        interference,
        RemovalInterference::AddJournalGone | RemovalInterference::Retargeted(_)
    );
    if tampered {
        assert!(
            !report.errors.is_empty(),
            "repair trusted a tampered journal"
        );
        assert_eq!(view.is_dir(), present, "repair acted on a tampered journal");
    }
    if interference == RemovalInterference::None {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }
    if report.errors.is_empty() {
        // Finished or cancelled, never in between.
        assert_eq!(view.is_dir(), registered(&scene.fixture.repository, &view));
        assert!(
            !quarantine.exists(),
            "a finished removal left its quarantine"
        );
        if view.is_dir() {
            assert_eq!(fs::read(view.join("tracked.txt")).unwrap(), b"first\n");
        }
    }
}

macro_rules! per_phase {
    ($scenario:ident: $($name:ident => $phase:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = 12,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                $scenario(tc, $phase);
            }
        )+
    };
}

per_phase!(removal_scenario:
    a_removal_stopped_at_its_intent_is_finished_or_cancelled => RemoveWorktreePhase::IntentRecorded,
    a_removal_stopped_after_its_clean_check_is_finished_or_cancelled => RemoveWorktreePhase::CleanVerified,
    a_removal_stopped_after_deleting_the_view_is_finished => RemoveWorktreePhase::WorktreeRemoved,
    a_removal_stopped_after_releasing_its_base_is_finished => RemoveWorktreePhase::BaseReleased,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveInterference {
    None,
    UserWrites,
    GitMovesAhead,
    GitMovesBack,
    DestinationAppears,
    SourceLocked,
    SourceDeleted,
    AddJournalGone,
    Retargeted(&'static str),
    JournalRenamed,
}

fn move_scenario(tc: TestCase, phase: MoveWorktreePhase) {
    let interference = tc.draw(gs::sampled_from(vec![
        MoveInterference::None,
        MoveInterference::UserWrites,
        MoveInterference::GitMovesAhead,
        MoveInterference::GitMovesBack,
        MoveInterference::DestinationAppears,
        MoveInterference::SourceLocked,
        MoveInterference::SourceDeleted,
        MoveInterference::AddJournalGone,
        MoveInterference::Retargeted("destination"),
        MoveInterference::Retargeted("source"),
        MoveInterference::JournalRenamed,
    ]));
    let mut scene = Scene::with_view();
    let source = scene.view.clone();
    let destination = scene.fixture.path("moved");
    let stopped = move_worktree_inner(
        MoveWorktreeRequest {
            repository: scene.fixture.repository.clone(),
            source: source.clone(),
            destination: destination.clone(),
            state_dir: Some(scene.fixture.state.clone()),
        },
        Some(phase),
    )
    .expect_err("stop the move");
    assert!(injected(&stopped), "{stopped}");
    let journal = scene.only_journal("moves");
    let moved = phase >= MoveWorktreePhase::WorktreeMoved;
    assert_eq!(destination.is_dir(), moved);
    let current = if moved { &destination } else { &source };
    let source_text = source.to_str().unwrap();
    let destination_text = destination.to_str().unwrap();

    match interference {
        MoveInterference::None => {}
        MoveInterference::UserWrites => {
            scene.user_writes(&current.clone(), &[&source, &destination]);
        }
        MoveInterference::GitMovesAhead if !moved => {
            assert!(scene.git(
                &scene.fixture.repository,
                &["worktree", "move", source_text, destination_text]
            ));
        }
        MoveInterference::GitMovesBack if moved => {
            assert!(scene.git(
                &scene.fixture.repository,
                &["worktree", "move", destination_text, source_text]
            ));
            scene
                .kept
                .push((vec![source.join("tracked.txt")], b"first\n".to_vec()));
        }
        MoveInterference::DestinationAppears if !moved => {
            scene.user_writes(&destination.clone(), &[&destination]);
            scene
                .kept
                .push((vec![source.join("tracked.txt")], b"first\n".to_vec()));
        }
        MoveInterference::SourceLocked if !moved => {
            assert!(scene.git(
                &scene.fixture.repository,
                &["worktree", "lock", source_text]
            ));
            scene
                .kept
                .push((vec![source.join("tracked.txt")], b"first\n".to_vec()));
        }
        MoveInterference::SourceDeleted if !moved => fs::remove_dir_all(&source).unwrap(),
        MoveInterference::AddJournalGone => {
            fs::remove_file(scene.only_journal("operations")).unwrap()
        }
        MoveInterference::Retargeted(field) => scene.retarget(&journal, field),
        MoveInterference::JournalRenamed => {
            fs::rename(&journal, journal.with_file_name("move-0-0-0.json")).unwrap();
        }
        _ => {}
    }
    tc.note(&format!("{phase:?}, {interference:?}"));
    let before = current.is_dir().then(|| snapshot(current));
    if interference == MoveInterference::JournalRenamed {
        // A journal that is not where its own ID says is not interpreted at
        // all, so the whole pass stops before touching anything.
        recover_incomplete_operations(&scene.fixture.state)
            .expect_err("a journal under another operation's name");
        assert_eq!(current.is_dir().then(|| snapshot(current)), before);
        return;
    }

    let report = scene.settle(&tc, &[&source, &destination]);
    let tampered = matches!(
        interference,
        MoveInterference::AddJournalGone | MoveInterference::Retargeted(_)
    );
    if tampered {
        assert!(
            !report.errors.is_empty(),
            "repair trusted a tampered journal"
        );
        assert_eq!(
            current.is_dir().then(|| snapshot(current)),
            before,
            "repair acted on a tampered journal"
        );
    }
    if interference == MoveInterference::None {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(
            destination.is_dir() && !source.exists(),
            "the move was not finished"
        );
    }
    if report.errors.is_empty() && interference != MoveInterference::SourceDeleted {
        let accounting = storage_accounting(&scene.fixture.state).expect("status");
        assert_eq!(accounting.active_views, 1, "{accounting:?}");
        let listed = &accounting.views[0].destination;
        assert!(
            listed.join("tracked.txt").is_file(),
            "status names {}, where the view is not",
            listed.display()
        );
    }
}

per_phase!(move_scenario:
    a_move_stopped_at_its_intent_is_finished_or_cancelled => MoveWorktreePhase::IntentRecorded,
    a_move_stopped_after_git_moved_the_view_is_finished => MoveWorktreePhase::WorktreeMoved,
    a_move_stopped_after_updating_the_add_journal_is_finished => MoveWorktreePhase::AddJournalUpdated,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionInterference {
    None,
    UserWrites,
    ReplacementDeleted,
    OldViewDeleted,
    DestinationDeleted,
    GitRemoves,
    SwapUndone,
    HeadMoves,
    AddJournalGone,
    Retargeted(&'static str),
}

fn compaction_scenario(tc: TestCase, phase: CompactWorktreePhase) {
    let interference = tc.draw(gs::sampled_from(vec![
        CompactionInterference::None,
        CompactionInterference::UserWrites,
        CompactionInterference::ReplacementDeleted,
        CompactionInterference::OldViewDeleted,
        CompactionInterference::DestinationDeleted,
        CompactionInterference::GitRemoves,
        CompactionInterference::SwapUndone,
        CompactionInterference::HeadMoves,
        CompactionInterference::AddJournalGone,
        CompactionInterference::Retargeted("replacement"),
        CompactionInterference::Retargeted("quarantine"),
        CompactionInterference::Retargeted("base_path"),
        CompactionInterference::Retargeted("old_base_path"),
    ]));
    let mut scene = Scene::with_view();
    let view = scene.view.clone();
    // Compaction rebuilds the view on the base of the commit it now holds.
    assert!(scene.git(
        &view,
        &["checkout", "--quiet", "--detach", &scene.commits[1]]
    ));
    let stopped = compact_worktree_inner(
        CompactWorktreeRequest {
            repository: scene.fixture.repository.clone(),
            destination: view.clone(),
            state_dir: Some(scene.fixture.state.clone()),
        },
        Some(phase),
    )
    .expect_err("stop the compaction");
    assert!(injected(&stopped), "{stopped}");
    let journal = CompactJournalStore::open(&scene.fixture.state)
        .load_all()
        .expect("compaction journal")
        .remove(0);
    let (replacement, quarantine) = (journal.replacement.clone(), journal.quarantine.clone());
    let swapped = phase >= CompactWorktreePhase::ReplacementActivated;
    assert_eq!(quarantine.is_dir(), swapped);

    match interference {
        CompactionInterference::None => {}
        CompactionInterference::UserWrites => {
            scene.user_writes(&view.clone(), &[&view, &quarantine, &replacement]);
        }
        CompactionInterference::ReplacementDeleted if replacement.is_dir() => {
            fs::remove_dir_all(&replacement).unwrap();
        }
        CompactionInterference::OldViewDeleted if swapped => {
            fs::remove_dir_all(&quarantine).unwrap();
        }
        CompactionInterference::DestinationDeleted => fs::remove_dir_all(&view).unwrap(),
        CompactionInterference::GitRemoves => {
            scene.git(
                &scene.fixture.repository,
                &["worktree", "remove", "--force", view.to_str().unwrap()],
            );
        }
        CompactionInterference::SwapUndone if swapped => {
            let aside = scene.fixture.path("swap-aside");
            fs::rename(&view, &aside).unwrap();
            fs::rename(&quarantine, &view).unwrap();
            fs::rename(&aside, &quarantine).unwrap();
        }
        CompactionInterference::HeadMoves => {
            scene.git(
                &view,
                &["checkout", "--quiet", "--detach", &scene.commits[0]],
            );
        }
        CompactionInterference::AddJournalGone => {
            fs::remove_file(scene.only_journal("operations")).unwrap();
        }
        CompactionInterference::Retargeted(field) => {
            scene.retarget(&journal.journal_path, field);
        }
        _ => {}
    }
    tc.note(&format!("{phase:?}, {interference:?}"));
    let head_before = view
        .join(".git")
        .exists()
        .then(|| git_stdout(&view, &["rev-parse", "HEAD"]));

    let report = scene.settle(&tc, &[&view]);
    let tampered = matches!(
        interference,
        CompactionInterference::AddJournalGone | CompactionInterference::Retargeted(_)
    );
    if tampered {
        assert!(
            !report.errors.is_empty(),
            "repair trusted a tampered journal"
        );
    }
    if interference == CompactionInterference::None {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }
    if view.join(".git").exists() && registered(&scene.fixture.repository, &view) {
        assert_eq!(
            Some(git_stdout(&view, &["rev-parse", "HEAD"])),
            head_before,
            "repair moved the view's HEAD"
        );
    }
    if report.errors.is_empty() {
        for leftover in [&replacement, &quarantine] {
            assert!(
                !leftover.exists(),
                "a settled compaction left {}",
                leftover.display()
            );
        }
    }
}

per_phase!(compaction_scenario:
    a_compaction_stopped_at_its_intent_is_cancelled_or_finished => CompactWorktreePhase::IntentRecorded,
    a_compaction_stopped_with_its_replacement_ready_settles => CompactWorktreePhase::ReplacementReady,
    a_compaction_stopped_after_the_swap_settles => CompactWorktreePhase::ReplacementActivated,
    a_compaction_stopped_after_updating_the_add_journal_settles => CompactWorktreePhase::AddJournalUpdated,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddInterference {
    None,
    UserWrites,
    DestinationDeleted,
    GitRemoves,
    Unregistered,
    HeadMoves,
    BranchMoves,
    BranchDeleted,
    SecondAdd,
    Retargeted(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branching {
    Detached,
    New,
    Existing,
}

fn add_scenario(tc: TestCase, phases: &[AddWorktreePhase]) {
    let phase = tc.draw(gs::sampled_from(phases.to_vec()));
    let branching = tc.draw(gs::sampled_from(vec![
        Branching::Detached,
        Branching::New,
        Branching::Existing,
    ]));
    let interference = tc.draw(gs::sampled_from(vec![
        AddInterference::None,
        AddInterference::UserWrites,
        AddInterference::DestinationDeleted,
        AddInterference::GitRemoves,
        AddInterference::Unregistered,
        AddInterference::HeadMoves,
        AddInterference::BranchMoves,
        AddInterference::BranchDeleted,
        AddInterference::SecondAdd,
        AddInterference::Retargeted("scratch"),
        AddInterference::Retargeted("base_staging"),
        AddInterference::Retargeted("base_path"),
        AddInterference::Retargeted("temporary_index"),
    ]));
    let mut scene = Scene::new();
    let view = scene.view.clone();
    let repository = scene.fixture.repository.clone();
    let branch = "topic";
    let reference = format!("refs/heads/{branch}");
    if branching == Branching::Existing {
        assert!(scene.git(&repository, &["branch", branch, &scene.commits[0]]));
    }
    let mut request = scene.fixture.add_request(&view, &scene.commits[0]);
    request.mode = match branching {
        Branching::Detached => WorktreeMode::Detached,
        Branching::New => WorktreeMode::NewBranch(OsString::from(branch)),
        Branching::Existing => {
            request.revision = OsString::from(branch);
            WorktreeMode::ExistingBranch(OsString::from(branch))
        }
    };
    let stopped =
        add_worktree_inner(request.clone(), Some(phase), false).expect_err("stop the add");
    assert!(injected(&stopped), "{stopped}");
    let journal = scene.only_journal("operations");
    let materialized = view.join("tracked.txt").is_file();
    let has_pointer = view.join(".git").exists();

    match interference {
        AddInterference::None => {}
        AddInterference::UserWrites if view.is_dir() => scene.user_writes(&view.clone(), &[&view]),
        AddInterference::DestinationDeleted if view.is_dir() => fs::remove_dir_all(&view).unwrap(),
        AddInterference::GitRemoves => {
            scene.git(
                &repository,
                &["worktree", "remove", "--force", view.to_str().unwrap()],
            );
        }
        AddInterference::Unregistered if materialized => {
            scene.unregister(&view);
            scene
                .kept
                .push((vec![view.join("tracked.txt")], b"first\n".to_vec()));
        }
        AddInterference::HeadMoves if materialized && has_pointer => {
            if scene.git(
                &view,
                &["checkout", "--quiet", "--detach", &scene.commits[1]],
            ) {
                scene
                    .kept
                    .push((vec![view.join("tracked.txt")], b"second\n".to_vec()));
            }
        }
        AddInterference::BranchMoves if branching != Branching::Detached => {
            scene.ref_moved = true;
            scene.git(&repository, &["update-ref", &reference, &scene.commits[1]]);
        }
        AddInterference::BranchDeleted if branching != Branching::Detached => {
            scene.ref_moved = true;
            scene.git(&repository, &["update-ref", "-d", &reference]);
        }
        AddInterference::SecondAdd => {
            let mut second = request.clone();
            second.mode = WorktreeMode::Detached;
            second.revision = OsString::from(&scene.commits[0]);
            let _ = add_worktree_inner(second, None, true);
        }
        AddInterference::Retargeted(field) => scene.retarget(&journal, field),
        _ => {}
    }
    tc.note(&format!("{phase:?}, {branching:?}, {interference:?}"));
    let branch_target = || {
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &reference])
            .current_dir(&repository)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let branch_before = branch_target();

    let report = scene.settle(&tc, &[&view]);
    if matches!(interference, AddInterference::Retargeted(_)) {
        assert!(
            !report.errors.is_empty(),
            "repair trusted a tampered journal"
        );
    }
    if interference == AddInterference::None {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!view.exists(), "an unfinished add was not rolled back");
        assert!(!registered(&repository, &view));
    }
    // A branch that existed before the add, or that moved since, is never
    // deleted by rolling the add back.
    let user_owns_branch = branching == Branching::Existing
        || (branching == Branching::New && interference == AddInterference::BranchMoves);
    if user_owns_branch && branch_before.is_some() {
        assert_eq!(
            branch_target(),
            branch_before,
            "rollback changed the user's branch"
        );
    }
    if report.errors.is_empty() && view.exists() && interference != AddInterference::SecondAdd {
        // Kept only as a plain worktree handed back to the user.
        assert!(
            report.released_adds.contains(&view) || !view.join(".git").exists(),
            "a rolled-back add left its view registered"
        );
    }
}

macro_rules! add_phases {
    ($($name:ident => $phases:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = 12,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                add_scenario(tc, &$phases);
            }
        )+
    };
}

add_phases!(
    an_add_stopped_before_its_base_is_rolled_back =>
        [AddWorktreePhase::IntentRecorded, AddWorktreePhase::GitMetadataCreated],
    an_add_stopped_while_building_its_view_is_rolled_back =>
        [AddWorktreePhase::BaseReady, AddWorktreePhase::ViewCreated, AddWorktreePhase::GitPointerRestored],
    an_add_stopped_before_activation_is_rolled_back_or_released =>
        [AddWorktreePhase::IndexSynchronized, AddWorktreePhase::CleanVerified],
);
