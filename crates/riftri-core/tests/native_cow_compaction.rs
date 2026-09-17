#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
use riftri_core::recover_incomplete_operations;
use riftri_core::{
    AddWorktreeRequest, CompactWorktreeRequest, WorktreeMode, add_worktree, compact_worktree,
    storage_accounting,
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

fn fixture() -> (support::WritableTempDir, std::path::PathBuf) {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
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
    (fixture, repository)
}

#[test]
fn compaction_replaces_a_pristine_view_without_changing_git_identity() {
    let (fixture, repository) = fixture();
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
    let added = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/compact")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create managed worktree");
    let pointer = fs::read(worktree.join(".git")).expect("read Git pointer");

    fs::write(
        worktree.join("tracked.txt"),
        "a private edit that allocates blocks\n",
    )
    .expect("write private edit");
    fs::write(worktree.join("tracked.txt"), "base\n").expect("restore exact tree bytes");
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());

    let compacted = compact_worktree(CompactWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("compact pristine worktree");

    assert_eq!(compacted.old_base_path, added.base_path);
    assert_eq!(compacted.base_path, added.base_path);
    assert!(compacted.reused_base);
    assert_eq!(
        fs::read(worktree.join(".git")).expect("read pointer"),
        pointer
    );
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "feature/compact"
    );
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        fs::read_to_string(worktree.join("tracked.txt")).unwrap(),
        "base\n"
    );
    let accounting = storage_accounting(&state).expect("inspect compacted state");
    assert_eq!(accounting.active_views, 1);
    assert_eq!(accounting.completed_compactions, 1);
    assert_eq!(accounting.pending_compactions, 0);
    assert!(accounting.diagnostic_issues.is_empty());
}

#[test]
fn compaction_refuses_ignored_files_even_when_git_calls_the_view_clean() {
    let (fixture, repository) = fixture();
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
    add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/ignored")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create managed worktree");
    fs::write(repository.join(".git/info/exclude"), "build-output\n")
        .expect("configure ignored path");
    fs::write(worktree.join("build-output"), "preserve me\n").expect("write ignored output");
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());

    let error = compact_worktree(CompactWorktreeRequest {
        repository,
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("ignored output must be preserved");

    assert!(error.to_string().contains("ignored"), "unexpected: {error}");
    assert_eq!(
        fs::read_to_string(worktree.join("build-output")).unwrap(),
        "preserve me\n"
    );
    assert_eq!(storage_accounting(&state).unwrap().pending_compactions, 0);

    fs::remove_file(worktree.join("build-output")).expect("remove ignored output");
    fs::create_dir(worktree.join("empty-private-directory")).expect("create empty directory");
    let error = compact_worktree(CompactWorktreeRequest {
        repository: worktree.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("empty untracked directory must be preserved");
    assert!(
        error.to_string().contains("exact Git tree"),
        "unexpected: {error}"
    );
    assert!(worktree.join("empty-private-directory").is_dir());
    assert_eq!(storage_accounting(&state).unwrap().pending_compactions, 0);
}

#[test]
fn compaction_rekeys_the_active_view_after_a_clean_commit() {
    let (fixture, repository) = fixture();
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
    let added = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/new-tree")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create managed worktree");
    fs::write(worktree.join("tracked.txt"), "new committed tree\n").expect("change tree");
    git(&worktree, &["add", "--", "tracked.txt"]);
    git(&worktree, &["commit", "--quiet", "-m", "advance tree"]);

    let compacted = compact_worktree(CompactWorktreeRequest {
        repository,
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("compact advanced clean worktree");

    assert_eq!(compacted.old_base_path, added.base_path);
    assert_ne!(compacted.base_path, added.base_path);
    assert!(!compacted.reused_base);
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]).trim(),
        compacted.commit.as_str()
    );
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());
    let accounting = storage_accounting(&state).expect("inspect rekeyed state");
    let view = accounting.views.first().expect("active view");
    assert_eq!(view.base_path, compacted.base_path);
    assert_eq!(
        accounting
            .bases
            .iter()
            .find(|base| base.path == compacted.base_path)
            .map(|base| base.reference_count),
        Some(1)
    );
    assert_eq!(
        accounting
            .bases
            .iter()
            .find(|base| base.path == added.base_path)
            .map(|base| base.reference_count),
        Some(0)
    );
}

#[cfg(unix)]
#[test]
fn compaction_preserves_private_extended_attributes_by_refusing_replacement() {
    let (fixture, repository) = fixture();
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
    add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/xattr")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create managed worktree");
    #[cfg(target_os = "macos")]
    let attribute = "com.riftri.compaction-test";
    #[cfg(target_os = "linux")]
    let attribute = "user.riftri.compaction-test";
    rustix::fs::setxattr(
        worktree.join("tracked.txt"),
        attribute,
        b"private metadata",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("set private xattr");

    let error = compact_worktree(CompactWorktreeRequest {
        repository,
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("replacement without the private xattr must be refused");
    assert!(
        error.to_string().contains("content changed"),
        "unexpected: {error}"
    );
    let repair = recover_incomplete_operations(&state).expect("cancel incomplete compaction");
    assert!(repair.errors.is_empty(), "{repair:?}");
    assert_eq!(
        {
            let mut buffer: Vec<u8> = Vec::with_capacity(256);
            rustix::fs::getxattr(
                worktree.join("tracked.txt"),
                attribute,
                rustix::buffer::spare_capacity(&mut buffer),
            )
            .expect("read preserved xattr");
            buffer
        },
        b"private metadata"
    );
    let accounting = storage_accounting(&state).expect("inspect cancelled compaction");
    assert_eq!(accounting.cancelled_compactions, 1);
    assert_eq!(accounting.pending_compactions, 0);
}
