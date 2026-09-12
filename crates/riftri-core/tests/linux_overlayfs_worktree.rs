#![cfg(target_os = "linux")]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;

use riftri_core::{
    AddWorktreeRequest, MoveWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree,
    move_worktree, recover_incomplete_operations, remove_worktree, storage_accounting,
};
use riftri_storage::{BackendKind, CapabilityStatus, OverlayFsMountIdentity, OverlayFsMounter};
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

fn require_overlayfs(path: &Path) -> bool {
    let capability = OverlayFsMounter::probe_current_namespace(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref(),
        Some(OsStr::new("1")),
        "Linux OverlayFS test namespace is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Linux OverlayFS worktree test in this namespace: {}",
        capability.explanation
    );
    false
}

#[test]
fn creates_isolates_and_removes_real_overlayfs_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_overlayfs(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    let moved = fixture.path().join("moved");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    fs::write(repository.join("large.bin"), vec![7_u8; 8 * 1024 * 1024])
        .expect("write allocation fixture");
    git(&repository, &["add", "--", "tracked.txt", "large.bin"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let first_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/overlay-first")),
        state_dir: Some(state.clone()),
    })
    .expect("create first OverlayFS worktree");
    let second_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/overlay-second")),
        state_dir: Some(state.clone()),
    })
    .expect("create second OverlayFS worktree");

    assert_eq!(first_result.backend, BackendKind::OverlayFs);
    assert_eq!(second_result.backend, BackendKind::OverlayFs);
    assert!(!first_result.reused_base);
    assert!(second_result.reused_base);
    assert_eq!(first_result.base_path, second_result.base_path);
    assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&second, &["status", "--porcelain=v1"]).is_empty());
    assert!(
        Command::new("mountpoint")
            .arg("--quiet")
            .arg(&first)
            .status()
            .expect("inspect first mountpoint")
            .success()
    );
    let active_accounting = storage_accounting(&state).expect("account active OverlayFS views");
    assert_eq!(active_accounting.active_views, 2);
    assert!(
        active_accounting
            .views
            .iter()
            .all(|view| view.backend == BackendKind::OverlayFs)
    );
    let base_allocated = active_accounting.bases[0].allocated_bytes;
    assert!(base_allocated >= 8 * 1024 * 1024);
    assert!(
        active_accounting
            .views
            .iter()
            .all(|view| view.allocated_bytes < base_allocated / 8)
    );
    assert!(
        Command::new("mountpoint")
            .arg("--quiet")
            .arg(&second)
            .status()
            .expect("inspect second mountpoint")
            .success()
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

    let move_error = move_worktree(MoveWorktreeRequest {
        repository: repository.clone(),
        source: first.clone(),
        destination: moved,
        state_dir: Some(state.clone()),
    })
    .expect_err("mounted OverlayFS move must fail closed");
    assert!(move_error.to_string().contains("not yet supported"));
    assert!(first.is_dir());

    let dirty_error = remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("dirty OverlayFS worktree must be retained");
    assert!(dirty_error.to_string().contains("changes"));
    assert!(first.is_dir());

    fs::write(first.join("tracked.txt"), "base\n").expect("restore first worktree");
    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove clean first OverlayFS worktree");
    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: second.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove clean second OverlayFS worktree");

    assert!(!first.exists());
    assert!(!second.exists());
    assert_eq!(
        fs::read_dir(state.join("overlays/v1"))
            .expect("read OverlayFS state")
            .count(),
        0
    );
    let accounting = storage_accounting(&state).expect("inspect storage accounting");
    assert_eq!(accounting.active_views, 0);
    assert_eq!(accounting.completed_removals, 2);
    assert!(accounting.diagnostic_issues.is_empty());
}

#[test]
fn repair_remounts_an_active_view_after_a_simulated_reboot() {
    let fixture = tempdir().expect("fixture directory");
    if !require_overlayfs(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let destination = fixture.path().join("worktree");
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
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/reboot-recovery")),
        state_dir: Some(state.clone()),
    })
    .expect("create OverlayFS worktree");
    assert_eq!(added.backend, BackendKind::OverlayFs);
    fs::write(destination.join("tracked.txt"), "private after reboot\n")
        .expect("write private worktree change");

    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(&added.journal_path).expect("read active add journal"))
            .expect("decode active add journal");
    let operation_id = journal["operation_id"]
        .as_str()
        .expect("journal operation ID");
    let layout_root = state.join("overlays/v1").join(operation_id);
    let identity: OverlayFsMountIdentity =
        serde_json::from_value(journal["overlayfs"]["mount_identity"].clone())
            .expect("decode original mount identity");
    let layout = OverlayFsMounter::load(&layout_root, &added.base_path, &destination)
        .expect("load active OverlayFS layout");
    OverlayFsMounter::unmount(&layout, &identity).expect("simulate reboot mount loss");
    assert!(
        !Command::new("mountpoint")
            .arg("--quiet")
            .arg(&destination)
            .status()
            .expect("inspect simulated reboot mountpoint")
            .success()
    );

    let simulated_boot = "00000000-0000-0000-0000-000000000000";
    assert_ne!(identity.boot_id, simulated_boot);
    journal["overlayfs"]["mount_context"]["boot_id"] = simulated_boot.into();
    journal["overlayfs"]["mount_identity"]["boot_id"] = simulated_boot.into();
    fs::write(
        &added.journal_path,
        serde_json::to_vec_pretty(&journal).expect("encode simulated prior-boot journal"),
    )
    .expect("persist simulated prior-boot journal");

    let recovered = recover_incomplete_operations(&state).expect("repair after simulated reboot");
    assert!(
        recovered.errors.is_empty(),
        "reboot repair errors: {:?}",
        recovered.errors
    );
    assert_eq!(recovered.active, 1);
    assert_eq!(recovered.recovered_mounts, 1);
    assert!(
        Command::new("mountpoint")
            .arg("--quiet")
            .arg(&destination)
            .status()
            .expect("inspect recovered mountpoint")
            .success()
    );
    assert_eq!(
        fs::read_to_string(destination.join("tracked.txt")).expect("read recovered private file"),
        "private after reboot\n"
    );
    assert_eq!(
        fs::read_to_string(added.base_path.join("tracked.txt")).expect("read immutable base"),
        "base\n"
    );
    assert!(!git(&destination, &["status", "--porcelain=v1"]).is_empty());

    let repeated = recover_incomplete_operations(&state).expect("repeat reboot repair");
    assert!(repeated.errors.is_empty());
    assert_eq!(repeated.active, 1);
    assert_eq!(repeated.recovered_mounts, 0);
    fs::write(destination.join("tracked.txt"), "base\n").expect("restore tracked file");
    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: destination.clone(),
        state_dir: Some(state),
    })
    .expect("remove recovered worktree");
    assert!(!destination.exists());
}
