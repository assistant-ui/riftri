#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

//! Regression coverage for reconciling durable add journals against Git's own
//! worktree registry.
//!
//! Every case here wedged Riftri before: a journal nothing could retire, a
//! live worktree Riftri silently dropped, or storage no command could explain.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, garbage_collect,
    recover_incomplete_operations, remove_worktree, storage_accounting,
};

mod support;
use support::writable_tempdir as tempdir;

fn git(path: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn create_repository(repository: &Path) {
    fs::create_dir_all(repository).expect("create repository");
    git(repository, &["init", "--quiet"]);
    git(repository, &["config", "user.name", "Riftri Tests"]);
    git(
        repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    git(repository, &["add", "--", "tracked.txt"]);
    git(repository, &["commit", "--quiet", "-m", "initial"]);
}

fn add(repository: &Path, state: &Path, destination: &Path, branch: &str) {
    add_worktree(AddWorktreeRequest {
        repository: repository.to_path_buf(),
        destination: destination.to_path_buf(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from(branch)),
        state_dir: Some(state.to_path_buf()),
        sparse_directories: Vec::new(),
    })
    .expect("create Riftri worktree");
}

fn journal_phases(state: &Path) -> Vec<String> {
    let mut phases = Vec::new();
    for entry in fs::read_dir(state.join("operations")).expect("read operations directory") {
        let path = entry.expect("operations entry").path();
        if path.extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read journal"))
                .expect("decode journal");
        phases.push(record["phase"].as_str().expect("journal phase").to_owned());
    }
    phases.sort();
    phases
}

/// Defect 1: a worktree the user deletes by hand leaves an active journal that
/// still claims the path. Repair must retire it, and a second `add` at the
/// same path must never produce a second active journal.
#[test]
fn repair_retires_the_journal_of_a_hand_deleted_worktree() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let worktree = root.join("worktree");
    create_repository(&repository);
    add(&repository, &state, &worktree, "feature/first");

    fs::remove_dir_all(&worktree).expect("delete the worktree by hand");
    git(&repository, &["worktree", "prune"]);

    let report = recover_incomplete_operations(&state).expect("repair after manual deletion");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.retired_adds, 1);
    assert!(report.relocations.is_empty());

    // The retirement releases the base and frees the path for a new add.
    let accounting = storage_accounting(&state).expect("account after repair");
    assert!(accounting.views.is_empty());
    assert_eq!(accounting.bases.len(), 1);
    assert_eq!(accounting.bases[0].reference_count, 0);
    assert!(accounting.diagnostic_issues.is_empty());

    add(&repository, &state, &worktree, "feature/second");
    let accounting = storage_accounting(&state).expect("account after second add");
    assert_eq!(accounting.views.len(), 1);
    assert!(accounting.diagnostic_issues.is_empty());

    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove the second worktree");
}

/// Defect 1, prevention half: even with no repair in between, `add` reclaims
/// the stale journal instead of creating a duplicate claim on one path.
#[test]
fn a_second_add_reclaims_a_stale_journal_instead_of_duplicating_it() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let worktree = root.join("worktree");
    create_repository(&repository);
    add(&repository, &state, &worktree, "feature/first");

    fs::remove_dir_all(&worktree).expect("delete the worktree by hand");
    git(&repository, &["worktree", "prune"]);

    add(&repository, &state, &worktree, "feature/second");

    let accounting = storage_accounting(&state).expect("account after second add");
    assert_eq!(
        accounting.views.len(),
        1,
        "exactly one journal may claim a path"
    );
    assert!(accounting.diagnostic_issues.is_empty());

    // The wedge was that this refused with "multiple active Riftri journals".
    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: worktree,
        state_dir: Some(state),
    })
    .expect("remove the reclaimed worktree");
}

/// Defect 2: `mv` plus the documented `git worktree repair` leaves a live
/// worktree Riftri no longer tracks. Repair must report it by name, must not
/// retire its journal, and must not touch the user's data.
#[test]
fn repair_reports_a_relocated_worktree_without_retiring_or_deleting_it() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let original = root.join("original");
    let relocated = root.join("relocated");
    create_repository(&repository);
    add(&repository, &state, &original, "feature/relocated");
    fs::write(original.join("precious.txt"), "precious\n").expect("write user data");

    fs::rename(&original, &relocated).expect("relocate the worktree");
    git(
        &repository,
        &["worktree", "repair", &relocated.display().to_string()],
    );

    for pass in 0..2 {
        let report = recover_incomplete_operations(&state).expect("repair after relocation");
        assert!(report.errors.is_empty(), "pass {pass}: {:?}", report.errors);
        assert_eq!(report.retired_adds, 0, "pass {pass}");
        assert_eq!(report.relocations.len(), 1, "pass {pass}");
        assert_eq!(report.relocations[0].journal_destination, original);
        assert_eq!(report.relocations[0].registered_path, relocated);
    }

    // The journal stays active, so the base stays protected and the live
    // worktree keeps its contents.
    assert_eq!(journal_phases(&state), ["active"]);
    assert_eq!(
        fs::read_to_string(relocated.join("precious.txt")).expect("read user data"),
        "precious\n"
    );
    assert!(!original.exists());
}

/// Defect 3: the losers of a racing add are stuck in `rollback-pending` and
/// correctly refuse to delete the winner's tree. Repair must be able to retire
/// them anyway, without touching the winner.
#[test]
fn repair_retires_racing_add_losers_without_touching_the_winner() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let destination = root.join("contested");
    create_repository(&repository);

    let racers = 3;
    let barrier = Arc::new(Barrier::new(racers));
    let handles = (0..racers)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            let repository = repository.clone();
            let state = state.clone();
            let destination = destination.clone();
            thread::spawn(move || {
                barrier.wait();
                add_worktree(AddWorktreeRequest {
                    repository,
                    destination,
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!("feature/race-{index}"))),
                    state_dir: Some(state),
                    sparse_directories: Vec::new(),
                })
                .is_ok()
            })
        })
        .collect::<Vec<_>>();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().expect("racing add thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "exactly one racer may create the worktree");

    let report = recover_incomplete_operations(&state).expect("repair after the race");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.recovered, racers - 1);

    // Idempotent: a second pass finds nothing left to do.
    let repeated = recover_incomplete_operations(&state).expect("repeat repair after the race");
    assert!(repeated.errors.is_empty(), "{:?}", repeated.errors);
    assert_eq!(repeated.recovered, 0);

    // The winner survives untouched and `status` agrees with `repair`.
    let accounting = storage_accounting(&state).expect("account after the race");
    assert_eq!(accounting.views.len(), 1);
    assert_eq!(accounting.views[0].destination, destination);
    assert!(
        accounting.diagnostic_issues.is_empty(),
        "{:?}",
        accounting.diagnostic_issues
    );
    assert!(destination.join("tracked.txt").is_file());
    // Git prints its own path spelling (forward slashes, no verbatim prefix
    // on Windows), so compare canonicalized paths rather than display text.
    let registered = git(&repository, &["worktree", "list", "--porcelain"]);
    let winner_listed = registered
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter_map(|path| PathBuf::from(path).canonicalize().ok())
        .any(|path| path == destination);
    assert!(winner_listed, "{registered}");
}

/// Defect 4: a base no active worktree references but an unfinished journal
/// still claims must be reported by both `gc` and `status`, not silently
/// dropped from the plan.
#[test]
fn collection_names_the_journal_that_protects_an_unreferenced_base() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let destination = root.join("contested");
    create_repository(&repository);

    let racers = 2;
    let barrier = Arc::new(Barrier::new(racers));
    let handles = (0..racers)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            let repository = repository.clone();
            let state = state.clone();
            let destination = destination.clone();
            thread::spawn(move || {
                barrier.wait();
                add_worktree(AddWorktreeRequest {
                    repository,
                    destination,
                    revision: OsString::from("HEAD"),
                    mode: WorktreeMode::NewBranch(OsString::from(format!("feature/gc-{index}"))),
                    state_dir: Some(state),
                    sparse_directories: Vec::new(),
                })
                .is_ok()
            })
        })
        .collect::<Vec<_>>();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().expect("racing add thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);

    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove the winning worktree");

    // No active view references the base now, but the losing journal does.
    let accounting = storage_accounting(&state).expect("account before collection");
    assert_eq!(accounting.bases.len(), 1);
    assert_eq!(accounting.bases[0].reference_count, 0);
    let explained = accounting
        .diagnostic_issues
        .iter()
        .any(|issue| issue.reason.contains("still claims it"));
    assert!(
        explained,
        "status must explain the retained base: {:?}",
        accounting.diagnostic_issues
    );

    let report = garbage_collect(&state, true).expect("collect with a protected base");
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
            .all(|protection| protection.base_path == base_path),
        "{:?}",
        report.skipped_protected
    );
    assert!(
        report
            .skipped_protected
            .iter()
            .all(|protection| !protection.operation_id.is_empty()),
        "every skip must name the journal responsible"
    );

    // Once repair retires the loser, the same base becomes collectible.
    recover_incomplete_operations(&state).expect("repair before recollecting");
    let report = garbage_collect(&state, true).expect("collect after repair");
    assert_eq!(report.skipped_protected.len(), 0);
    assert_eq!(report.collected, [base_path]);
}

/// Defect 5: Riftri's own interrupted atomic-write temporaries must be reaped
/// by repair, never reported as foreign files, and must not shield anything
/// that could be a user's file.
#[test]
fn repair_reaps_interrupted_journal_writes_and_preserves_foreign_files() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let worktree = root.join("worktree");
    create_repository(&repository);
    add(&repository, &state, &worktree, "feature/temporaries");

    let operations = state.join("operations");
    let operation_id = fs::read_dir(&operations)
        .expect("read operations directory")
        .filter_map(|entry| {
            let path = entry.expect("operations entry").path();
            (path.extension() == Some(std::ffi::OsStr::new("json")))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .next()
        .expect("one add journal");

    let live_temporary = operations.join(format!(".{operation_id}.aB3dEf.tmp"));
    let orphan_temporary = operations.join(".remove-1234abcd-0001-0.zZ9yXw.tmp");
    let orphan_lock = operations.join("remove-1234abcd-0001-0.lock");
    let foreign_file = operations.join("notes.txt");
    fs::write(&live_temporary, b"{}").expect("stage interrupted journal write");
    fs::write(&orphan_temporary, b"{}").expect("stage orphan journal write");
    fs::write(&orphan_lock, b"").expect("stage orphan coordination lock");
    fs::write(&foreign_file, b"keep me\n").expect("stage foreign file");

    let accounting = storage_accounting(&state).expect("account with leftovers");
    let reported = accounting
        .diagnostic_issues
        .iter()
        .map(|issue| (issue.path.clone(), issue.reason.clone()))
        .collect::<Vec<_>>();
    assert!(
        reported.iter().any(|(path, reason)| path == &live_temporary
            && reason.contains("interrupted Riftri journal write")),
        "{reported:?}"
    );
    assert!(
        reported
            .iter()
            .any(|(path, reason)| path == &foreign_file && reason.contains("Riftri will preserve")),
        "{reported:?}"
    );
    assert!(
        !reported.iter().any(|(path, _)| path == &orphan_lock),
        "a coordination lock is Riftri's own artifact: {reported:?}"
    );

    let report = recover_incomplete_operations(&state).expect("repair with leftovers");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let mut reaped = report.reaped_artifacts.clone();
    reaped.sort();
    let mut expected = vec![live_temporary.clone(), orphan_temporary.clone()];
    expected.sort();
    assert_eq!(reaped, expected);
    assert!(!live_temporary.exists());
    assert!(!orphan_temporary.exists());
    assert_eq!(
        fs::read_to_string(&foreign_file).expect("foreign file survives"),
        "keep me\n"
    );
    assert!(
        orphan_lock.exists(),
        "coordination locks are never unlinked"
    );

    // The health check now only reports the file that really is foreign.
    let accounting = storage_accounting(&state).expect("account after reaping");
    assert_eq!(
        accounting
            .diagnostic_issues
            .iter()
            .map(|issue| issue.path.clone())
            .collect::<Vec<PathBuf>>(),
        [foreign_file]
    );

    // Repair stays idempotent with nothing left to reap.
    let repeated = recover_incomplete_operations(&state).expect("repeat repair");
    assert!(repeated.errors.is_empty(), "{:?}", repeated.errors);
    assert!(repeated.reaped_artifacts.is_empty());
    assert_eq!(journal_phases(&state), ["active"]);
}

/// `status` and `repair` must agree: whatever repair leaves behind, status
/// reports as healthy, and repair is safe to run twice.
#[test]
fn status_and_repair_agree_after_reconciliation() {
    let fixture = tempdir().expect("fixture directory");
    let root = fixture.path().canonicalize().expect("resolve fixture");
    let repository = root.join("repository");
    let state = root.join("state");
    let kept = root.join("kept");
    let deleted = root.join("deleted");
    create_repository(&repository);
    add(&repository, &state, &kept, "feature/kept");
    add(&repository, &state, &deleted, "feature/deleted");

    fs::remove_dir_all(&deleted).expect("delete one worktree by hand");
    git(&repository, &["worktree", "prune"]);

    // status notices the inconsistency before repair runs.
    let before = storage_accounting(&state).expect("account before repair");
    assert!(!before.diagnostic_issues.is_empty());

    for pass in 0..2 {
        let report = recover_incomplete_operations(&state).expect("repair");
        assert!(report.errors.is_empty(), "pass {pass}: {:?}", report.errors);
        assert_eq!(report.retired_adds, usize::from(pass == 0), "pass {pass}");
        let after = storage_accounting(&state).expect("account after repair");
        assert!(
            after.diagnostic_issues.is_empty(),
            "pass {pass}: {:?}",
            after.diagnostic_issues
        );
        assert_eq!(after.views.len(), 1, "pass {pass}");
        assert_eq!(after.views[0].destination, kept, "pass {pass}");
    }
}
