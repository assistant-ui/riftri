#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

use riftri_core::{AddWorktreeRequest, WorktreeMode, add_worktree};
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
fn creates_clean_isolated_linked_worktrees_from_one_base() {
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
    fs::write(repository.join("executable.sh"), "#!/bin/sh\nexit 0\n").expect("write executable");
    fs::set_permissions(
        repository.join("executable.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("set executable mode");
    symlink("tracked.txt", repository.join("tracked-link")).expect("create symlink");
    git(
        &repository,
        &["add", "--", "tracked.txt", "executable.sh", "tracked-link"],
    );
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let first_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/first")),
        state_dir: Some(state.clone()),
    })
    .expect("create first Riftri worktree");
    let second_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/second")),
        state_dir: Some(state),
    })
    .expect("create second Riftri worktree");

    assert!(!first_result.reused_base);
    assert!(second_result.reused_base);
    assert!(git(&repository, &["worktree", "list", "--porcelain"]).contains("feature/first"));
    assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&second, &["status", "--porcelain=v1"]).is_empty());
    assert_ne!(
        fs::metadata(first.join("executable.sh"))
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read_link(first.join("tracked-link")).expect("read symlink"),
        Path::new("tracked.txt")
    );

    fs::write(first.join("tracked.txt"), "first change\n").expect("edit first view");
    assert_eq!(
        fs::read_to_string(second.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(first_result.base_path.join("tracked.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn creates_a_clean_worktree_with_deterministic_in_tree_attributes() {
    let fixture = tempdir().expect("fixture directory");
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
    fs::write(
        repository.join(".gitattributes"),
        "* text=auto\n*.txt text eol=crlf\n*.bin binary\n",
    )
    .expect("write deterministic attributes");
    fs::write(repository.join("tracked.txt"), "first\nsecond\n").expect("write text fixture");
    fs::write(repository.join("payload.bin"), b"binary\0payload\n").expect("write binary fixture");
    git(
        &repository,
        &["add", "--", ".gitattributes", "tracked.txt", "payload.bin"],
    );
    git(&repository, &["commit", "--quiet", "-m", "attributes"]);

    let result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/attributes")),
        state_dir: Some(state),
    })
    .expect("create attributed Riftri worktree");

    assert!(git(&destination, &["status", "--porcelain=v1"]).is_empty());
    assert_eq!(
        fs::read(destination.join("tracked.txt")).expect("read attributed text"),
        b"first\r\nsecond\r\n",
    );
    assert_eq!(
        fs::read(destination.join("payload.bin")).expect("read binary file"),
        b"binary\0payload\n",
    );
    fs::write(destination.join("tracked.txt"), b"changed\r\n").expect("edit worktree");
    assert_eq!(
        fs::read(result.base_path.join("tracked.txt")).expect("read immutable base"),
        b"first\r\nsecond\r\n",
    );
}

#[test]
fn rejects_effective_attributes_before_creating_state_or_git_metadata() {
    let fixture = tempdir().expect("fixture directory");
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
    git(&repository, &["config", "filter.example.clean", "cat"]);
    fs::write(
        repository.join(".git/info/attributes"),
        "*.txt filter=example\n",
    )
    .expect("write active filter attributes");
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/rejected")),
        state_dir: Some(state.clone()),
    })
    .expect_err("active filter must be rejected");

    assert!(error.to_string().contains("attributes"));
    assert!(!destination.exists());
    assert!(!state.exists());
    assert!(
        !git(&repository, &["branch", "--list", "feature/rejected"]).contains("feature/rejected")
    );
}

#[test]
#[ignore = "physical allocation benchmark; run explicitly on an otherwise quiet APFS volume"]
fn cached_view_uses_materially_less_physical_space_than_its_logical_size() {
    const LOGICAL_BYTES: usize = 32 * 1024 * 1024;

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

    let mut file = File::create(repository.join("payload.bin")).expect("create payload");
    let mut state_word = 0x9e37_79b9_7f4a_7c15_u64;
    let mut block = [0_u8; 64 * 1024];
    for _ in 0..(LOGICAL_BYTES / block.len()) {
        for chunk in block.chunks_exact_mut(8) {
            state_word ^= state_word << 13;
            state_word ^= state_word >> 7;
            state_word ^= state_word << 17;
            chunk.copy_from_slice(&state_word.to_le_bytes());
        }
        file.write_all(&block).expect("write payload block");
    }
    file.sync_all().expect("sync payload");
    git(&repository, &["add", "--", "payload.bin"]);
    git(&repository, &["commit", "--quiet", "-m", "payload"]);

    add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first,
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/allocation-base")),
        state_dir: Some(state.clone()),
    })
    .expect("prime immutable base");
    let available_before = fs2::available_space(fixture.path()).expect("space before clone");

    let result = add_worktree(AddWorktreeRequest {
        repository,
        destination: second,
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/allocation-view")),
        state_dir: Some(state),
    })
    .expect("create cached COW view");
    let available_after = fs2::available_space(fixture.path()).expect("space after clone");
    let physical_growth = available_before.saturating_sub(available_after);

    assert!(result.reused_base);
    eprintln!(
        "cached view logical bytes: {LOGICAL_BYTES}; measured APFS volume growth: {physical_growth}"
    );
    assert!(
        physical_growth < (LOGICAL_BYTES as u64 / 4),
        "cached {LOGICAL_BYTES}-byte view consumed {physical_growth} physical bytes"
    );
}
