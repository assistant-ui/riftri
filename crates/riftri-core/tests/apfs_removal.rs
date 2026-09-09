#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, remove_worktree,
    storage_accounting,
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

#[test]
fn journaled_removal_refuses_dirty_then_releases_a_clean_view() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
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

    let added = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/removal")),
        state_dir: Some(state.clone()),
    })
    .expect("create Riftri worktree");

    let before = storage_accounting(&state).expect("account before removal");
    assert_eq!(before.active_views, 1);
    assert_eq!(before.bases.len(), 1);
    assert_eq!(before.bases[0].reference_count, 1);
    assert!(before.bases[0].logical_bytes > 0);
    assert!(before.bases[0].allocated_bytes > 0);

    fs::write(worktree.join("tracked.txt"), "dirty\n").expect("make worktree dirty");
    let error = remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("dirty worktree must be preserved");
    assert!(error.to_string().contains("changes"), "unexpected: {error}");
    assert!(worktree.is_dir());
    assert!(git(&repository, &["worktree", "list", "--porcelain"]).contains("feature/removal"));

    fs::write(worktree.join("tracked.txt"), "base\n").expect("restore worktree");
    let removed = remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove clean worktree");

    assert_eq!(
        removed.destination,
        worktree
            .parent()
            .expect("worktree parent")
            .canonicalize()
            .expect("canonical worktree parent")
            .join("worktree")
    );
    assert_eq!(removed.base_path, added.base_path);
    assert!(!removed.destination.exists());
    assert!(
        removed.base_path.is_dir(),
        "zero-reference base is retained"
    );
    assert!(!git(&repository, &["worktree", "list", "--porcelain"]).contains("feature/removal"));

    let after = storage_accounting(&state).expect("account after removal");
    assert_eq!(after.active_views, 0);
    assert_eq!(after.completed_removals, 1);
    assert_eq!(after.bases.len(), 1);
    assert_eq!(after.bases[0].reference_count, 0);
    assert_eq!(after.bases[0].path, removed.base_path);
}

#[test]
fn accounting_tracks_two_views_that_reuse_one_retained_base() {
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

    let first_add = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/first-accounted")),
        state_dir: Some(state.clone()),
    })
    .expect("create first worktree");
    let second_add = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/second-accounted")),
        state_dir: Some(state.clone()),
    })
    .expect("create second worktree");
    assert!(!first_add.reused_base);
    assert!(second_add.reused_base);
    assert_eq!(first_add.base_path, second_add.base_path);

    let both = storage_accounting(&state).expect("account both views");
    assert_eq!(both.active_views, 2);
    assert_eq!(both.bases.len(), 1);
    assert_eq!(both.bases[0].reference_count, 2);

    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: first,
        state_dir: Some(state.clone()),
    })
    .expect("remove first view");
    let one = storage_accounting(&state).expect("account remaining view");
    assert_eq!(one.active_views, 1);
    assert_eq!(one.completed_removals, 1);
    assert_eq!(one.bases[0].reference_count, 1);
    assert_eq!(one.views[0].destination, second.canonicalize().unwrap());
}
