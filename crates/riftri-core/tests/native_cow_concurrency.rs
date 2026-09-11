#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use riftri_core::{
    AddWorktreeRequest, AddWorktreeResult, WorktreeMode, add_worktree, storage_accounting,
};
use tempfile::tempdir;

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

fn concurrent_add(
    start: Arc<Barrier>,
    repository: PathBuf,
    state: PathBuf,
    destination: PathBuf,
    branch: String,
) -> thread::JoinHandle<AddWorktreeResult> {
    thread::spawn(move || {
        start.wait();
        add_worktree(AddWorktreeRequest {
            repository,
            destination,
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::NewBranch(OsString::from(branch)),
            state_dir: Some(state),
        })
        .expect("create concurrent Riftri worktree")
    })
}

fn concurrent_commit(
    start: Arc<Barrier>,
    destination: PathBuf,
    contents: String,
    message: String,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        start.wait();
        fs::write(destination.join("tracked.txt"), contents).expect("write isolated view");
        git(&destination, &["add", "--", "tracked.txt"]);
        git(&destination, &["commit", "--quiet", "-m", &message]);
    })
}

#[test]
fn parallel_views_share_one_base_and_keep_commits_isolated() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let add_barrier = Arc::new(Barrier::new(3));
    let first_add = concurrent_add(
        Arc::clone(&add_barrier),
        repository.clone(),
        state.clone(),
        first.clone(),
        "feature/parallel-first".to_owned(),
    );
    let second_add = concurrent_add(
        Arc::clone(&add_barrier),
        repository.clone(),
        state.clone(),
        second.clone(),
        "feature/parallel-second".to_owned(),
    );
    add_barrier.wait();
    let first_result = first_add.join().expect("first add thread");
    let second_result = second_add.join().expect("second add thread");

    assert_eq!(first_result.base_path, second_result.base_path);
    assert_ne!(first_result.reused_base, second_result.reused_base);
    assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&second, &["status", "--porcelain=v1"]).is_empty());
    let worktrees = git(&repository, &["worktree", "list", "--porcelain"]);
    assert!(worktrees.contains(first.to_string_lossy().as_ref()));
    assert!(worktrees.contains(second.to_string_lossy().as_ref()));

    let commit_barrier = Arc::new(Barrier::new(3));
    let first_commit = concurrent_commit(
        Arc::clone(&commit_barrier),
        first.clone(),
        "first agent\n".to_owned(),
        "parallel first".to_owned(),
    );
    let second_commit = concurrent_commit(
        Arc::clone(&commit_barrier),
        second.clone(),
        "second agent\n".to_owned(),
        "parallel second".to_owned(),
    );
    commit_barrier.wait();
    first_commit.join().expect("first commit thread");
    second_commit.join().expect("second commit thread");

    assert_eq!(
        fs::read_to_string(first.join("tracked.txt")).unwrap(),
        "first agent\n"
    );
    assert_eq!(
        fs::read_to_string(second.join("tracked.txt")).unwrap(),
        "second agent\n"
    );
    assert_eq!(
        fs::read_to_string(first_result.base_path.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert!(git(&repository, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&second, &["status", "--porcelain=v1"]).is_empty());
    assert_ne!(
        git(&first, &["rev-parse", "HEAD"]),
        git(&second, &["rev-parse", "HEAD"])
    );

    let journal_count = fs::read_dir(state.join("operations"))
        .expect("read operation journals")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .count();
    assert_eq!(journal_count, 2);
}

#[test]
fn many_parallel_views_reuse_one_base_and_keep_edits_isolated() {
    const WORKERS: usize = 8;

    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let destinations = (0..WORKERS)
        .map(|index| fixture.path().join(format!("view-{index}")))
        .collect::<Vec<_>>();
    let add_barrier = Arc::new(Barrier::new(WORKERS + 1));
    let add_handles = destinations
        .iter()
        .enumerate()
        .map(|(index, destination)| {
            concurrent_add(
                Arc::clone(&add_barrier),
                repository.clone(),
                state.clone(),
                destination.clone(),
                format!("feature/stress-{index}"),
            )
        })
        .collect::<Vec<_>>();
    add_barrier.wait();
    let results = add_handles
        .into_iter()
        .map(|handle| handle.join().expect("add thread"))
        .collect::<Vec<_>>();

    assert_eq!(
        results.iter().filter(|result| result.reused_base).count(),
        WORKERS - 1
    );
    assert!(
        results
            .iter()
            .all(|result| result.base_path == results[0].base_path)
    );
    for destination in &destinations {
        assert!(git(destination, &["status", "--porcelain=v1"]).is_empty());
    }
    let accounting = storage_accounting(&state).expect("account parallel views");
    assert_eq!(accounting.active_views, WORKERS);
    assert_eq!(accounting.bases.len(), 1);
    assert_eq!(accounting.bases[0].reference_count, WORKERS);

    let commit_barrier = Arc::new(Barrier::new(WORKERS + 1));
    let commit_handles = destinations
        .iter()
        .enumerate()
        .map(|(index, destination)| {
            concurrent_commit(
                Arc::clone(&commit_barrier),
                destination.clone(),
                format!("agent {index}\n"),
                format!("parallel agent {index}"),
            )
        })
        .collect::<Vec<_>>();
    commit_barrier.wait();
    for handle in commit_handles {
        handle.join().expect("commit thread");
    }

    let mut heads = HashSet::new();
    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read isolated view"),
            format!("agent {index}\n")
        );
        assert!(git(destination, &["status", "--porcelain=v1"]).is_empty());
        heads.insert(git(destination, &["rev-parse", "HEAD"]));
    }
    assert_eq!(heads.len(), WORKERS);
    assert_eq!(
        fs::read_to_string(results[0].base_path.join("tracked.txt")).expect("read immutable base"),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(repository.join("tracked.txt")).expect("read source repository"),
        "base\n"
    );
    assert!(git(&repository, &["status", "--porcelain=v1"]).is_empty());
}
