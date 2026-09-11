#![cfg(target_os = "windows")]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;

use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, garbage_collect,
    remove_worktree, storage_accounting,
};
use riftri_git::Git;
use riftri_storage::{BackendKind, CapabilityStatus, RefsBlockCloner};
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

fn require_refs(path: &Path) -> bool {
    let capability = RefsBlockCloner::probe(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_REFS").as_deref(),
        Some(OsStr::new("1")),
        "Windows ReFS test volume is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Windows ReFS worktree test on this volume: {}",
        capability.explanation
    );
    false
}

#[test]
fn creates_reuses_and_removes_clean_isolated_git_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_refs(fixture.path()) {
        return;
    }
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
    fs::write(repository.join("payload.bin"), vec![0x6d; 256 * 1024]).expect("write clone payload");
    git(&repository, &["add", "--", "tracked.txt", "payload.bin"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let first_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/windows-first")),
        state_dir: Some(state.clone()),
    })
    .expect("create first ReFS worktree");
    let second_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/windows-second")),
        state_dir: Some(state.clone()),
    })
    .expect("create second ReFS worktree");

    assert_eq!(first_result.backend, BackendKind::RefsBlockClone);
    assert_eq!(second_result.backend, BackendKind::RefsBlockClone);
    assert!(!first_result.reused_base);
    assert!(second_result.reused_base);
    assert_eq!(first_result.base_path, second_result.base_path);
    assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&second, &["status", "--porcelain=v1"]).is_empty());
    let expected_second = second.canonicalize().expect("resolve second worktree");
    assert!(
        Git::default()
            .list_worktrees(&repository)
            .expect("list linked worktrees")
            .iter()
            .any(|worktree| {
                worktree.path.canonicalize().ok().as_ref() == Some(&expected_second)
            })
    );

    fs::write(first.join("tracked.txt"), "private first change\n").expect("edit first worktree");
    assert_eq!(
        fs::read_to_string(second.join("tracked.txt")).expect("read second worktree"),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(first_result.base_path.join("tracked.txt"))
            .expect("read immutable base"),
        "base\n"
    );

    let dirty_error = remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("dirty worktree must be retained");
    assert!(dirty_error.to_string().contains("changes"));
    assert!(first.is_dir());

    fs::write(first.join("tracked.txt"), "base\n").expect("restore first worktree");
    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first,
        state_dir: Some(state.clone()),
    })
    .expect("remove clean first worktree");
    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: second,
        state_dir: Some(state.clone()),
    })
    .expect("remove clean second worktree");

    let accounting = storage_accounting(&state).expect("inspect storage accounting");
    assert_eq!(accounting.active_views, 0);
    assert_eq!(accounting.completed_removals, 2);
    assert_eq!(accounting.bases.len(), 1);
    assert_eq!(accounting.bases[0].reference_count, 0);
    assert!(accounting.diagnostic_issues.is_empty());

    let plan = garbage_collect(&state, false).expect("plan base collection");
    assert_eq!(plan.candidates.len(), 1);
    assert!(first_result.base_path.is_dir());
    let collected = garbage_collect(&state, true).expect("collect retained base");
    assert_eq!(collected.collected, vec![first_result.base_path.clone()]);
    assert!(!first_result.base_path.exists());
}
