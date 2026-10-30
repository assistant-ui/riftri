#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, garbage_collect,
    remove_worktree, storage_accounting,
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
        sparse_directories: Vec::new(),
    })
    .expect("create Riftri worktree");

    let before = storage_accounting(&state).expect("account before removal");
    assert_eq!(before.active_views, 1);
    assert_eq!(before.bases.len(), 1);
    assert_eq!(before.bases[0].reference_count, 1);
    assert!(before.bases[0].logical_bytes > 0);
    assert!(before.bases[0].allocated_bytes > 0);
    // One persistent lock owns the add, and one coordinates its immutable base.
    assert_eq!(before.coordination_locks, 2);
    assert!(
        before.diagnostic_issues.is_empty(),
        "unexpected state issues: {:?}",
        before.diagnostic_issues
    );

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
    assert_eq!(after.coordination_locks, 2, "operation locks are retained");
    assert!(
        after.diagnostic_issues.is_empty(),
        "unexpected state issues: {:?}",
        after.diagnostic_issues
    );
}

#[cfg(unix)]
#[test]
fn base_reuse_rejects_symlinked_completion_markers() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let first = fixture.path().join("first-worktree");
    let second = fixture.path().join("second-worktree");
    let third = fixture.path().join("third-worktree");
    let protected_file = fixture.path().join("protected-marker-target");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked file");
    fs::write(&protected_file, "preserve\n").expect("write protected file");
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    let added = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: first.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/marker-first")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create first worktree");
    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first,
        state_dir: Some(state.clone()),
    })
    .expect("remove first worktree");
    let marker = added.base_path.with_extension("complete");
    fs::remove_file(&marker).expect("remove real completion marker");
    symlink(&protected_file, &marker).expect("replace completion marker with symlink");

    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/marker-second")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect_err("symlinked completion marker must prevent base reuse");

    assert!(error.to_string().contains("completion marker"));
    assert!(error.to_string().contains("not a real file"));
    assert!(
        fs::symlink_metadata(&marker)
            .expect("marker metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(&protected_file).expect("read protected file"),
        "preserve\n"
    );
    assert!(!second.exists());
    assert!(
        !git(&repository, &["worktree", "list", "--porcelain"])
            .contains(second.to_string_lossy().as_ref())
    );
    assert!(
        !git(&repository, &["branch", "--list", "feature/marker-second"])
            .contains("feature/marker-second")
    );

    fs::remove_file(&marker).expect("remove live marker symlink");
    symlink(fixture.path().join("missing-marker-target"), &marker)
        .expect("create broken completion-marker symlink");
    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: third.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/marker-third")),
        state_dir: Some(state),
        sparse_directories: Vec::new(),
    })
    .expect_err("broken completion-marker symlink must prevent base reuse");

    assert!(error.to_string().contains("completion marker"));
    assert!(error.to_string().contains("not a real file"));
    assert!(
        fs::symlink_metadata(&marker)
            .expect("broken marker metadata")
            .file_type()
            .is_symlink()
    );
    assert!(!third.exists());
    assert!(
        !git(&repository, &["branch", "--list", "feature/marker-third"])
            .contains("feature/marker-third")
    );
}

#[test]
fn removal_preserves_untracked_files_when_status_configuration_hides_them() {
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

    add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/untracked-removal")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create Riftri worktree");
    git(&repository, &["config", "status.showUntrackedFiles", "no"]);
    let untracked = worktree.join("private-untracked.txt");
    fs::write(&untracked, "preserve me\n").expect("write untracked file");

    let error = remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("untracked file must prevent removal");

    assert!(error.to_string().contains("changes"), "unexpected: {error}");
    assert_eq!(
        fs::read_to_string(untracked).expect("read preserved untracked file"),
        "preserve me\n"
    );
    assert!(worktree.is_dir());
    let accounting = storage_accounting(&state).expect("account preserved worktree");
    assert_eq!(accounting.active_views, 1);
    assert_eq!(accounting.pending_removals, 0);
}

#[test]
fn accounting_keeps_a_reference_for_a_missing_unreleased_view() {
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
        repository,
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/missing-accounting")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create Riftri worktree");
    fs::remove_dir_all(&worktree).expect("simulate missing managed view");

    let accounting = storage_accounting(&state).expect("account missing managed view");
    assert_eq!(accounting.active_views, 0);
    assert_eq!(accounting.bases.len(), 1);
    assert_eq!(accounting.bases[0].path, added.base_path);
    assert_eq!(accounting.bases[0].reference_count, 1);
    assert!(accounting.diagnostic_issues.iter().any(|issue| {
        issue.path == added.destination && issue.reason.contains("missing worktree")
    }));
    assert!(
        garbage_collect(&state, false)
            .expect("plan collection with missing view")
            .candidates
            .is_empty(),
        "the referenced base must not be advertised as collectible"
    );
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
        sparse_directories: Vec::new(),
    })
    .expect("create first worktree");
    let second_add = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/second-accounted")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
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

#[test]
fn garbage_collection_requires_apply_and_never_collects_an_in_use_base() {
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
        mode: WorktreeMode::NewBranch(OsString::from("feature/gc-first")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create first worktree");
    add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/gc-second")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create second worktree");

    assert!(
        garbage_collect(&state, false)
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(garbage_collect(&state, true).unwrap().collected.is_empty());
    assert!(first_add.base_path.is_dir());

    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: first,
        state_dir: Some(state.clone()),
    })
    .expect("remove first view");
    assert!(
        garbage_collect(&state, false)
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(first_add.base_path.is_dir());

    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: second,
        state_dir: Some(state.clone()),
    })
    .expect("remove second view");
    let plan = garbage_collect(&state, false).expect("plan collection");
    assert!(!plan.applied);
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].base_path, first_add.base_path);
    assert!(
        first_add.base_path.is_dir(),
        "plan must not remove the base"
    );

    let applied = garbage_collect(&state, true).expect("apply collection");
    assert!(applied.applied);
    assert_eq!(
        applied.collected.as_slice(),
        std::slice::from_ref(&first_add.base_path)
    );
    assert!(applied.skipped_in_use.is_empty());
    assert!(!first_add.base_path.exists());
    assert!(!first_add.base_path.with_extension("complete").exists());

    let accounting = storage_accounting(&state).expect("account collected state");
    assert!(accounting.bases.is_empty());
    assert_eq!(accounting.completed_collections, 1);
    assert_eq!(accounting.pending_collections, 0);

    let repeated = garbage_collect(&state, true).expect("repeat collection");
    assert!(repeated.candidates.is_empty());
    assert!(repeated.collected.is_empty());

    let rebuilt_path = fixture.path().join("rebuilt");
    let rebuilt = add_worktree(AddWorktreeRequest {
        repository: fixture.path().join("repository"),
        destination: rebuilt_path.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/gc-rebuilt")),
        state_dir: Some(state),
        sparse_directories: Vec::new(),
    })
    .expect("rebuild collected base");
    assert!(!rebuilt.reused_base);
    assert_eq!(rebuilt.base_path, first_add.base_path);
    assert!(git(&rebuilt_path, &["status", "--porcelain=v1"]).is_empty());
}

/// Deletion grants permission only on directories. A clean removal must still
/// delete ignored read-only files under read-only directories, without
/// following a symlink out of the view, and collection must still delete an
/// immutable base whose nested directories and files are all read-only.
#[cfg(unix)]
#[test]
fn removal_and_collection_delete_read_only_trees_without_following_links() {
    use std::os::unix::fs::PermissionsExt;

    let mode = |path: &Path| fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777;
    let set_mode = |path: &Path, mode: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    };
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let worktree = fixture.path().join("worktree");
    let outside = fixture.path().join("outside");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::create_dir_all(repository.join("src/nested/deeper")).unwrap();
    fs::write(repository.join("src/nested/deeper/leaf.txt"), "leaf\n").unwrap();
    fs::write(repository.join("src/tool.sh"), "#!/bin/sh\n").unwrap();
    set_mode(&repository.join("src/tool.sh"), 0o755);
    fs::write(repository.join(".gitignore"), "node_modules/\n").unwrap();
    git(&repository, &["add", "--all"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);

    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("target.txt"), "outside\n").unwrap();
    set_mode(&outside.join("target.txt"), 0o444);
    set_mode(&outside, 0o555);
    let outside_modes = (mode(&outside), mode(&outside.join("target.txt")));

    let added = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/read-only-trees")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create Riftri worktree");
    // The immutable base is read-only all the way down.
    for relative in ["src", "src/nested", "src/nested/deeper"] {
        assert_eq!(
            mode(&added.base_path.join(relative)) & 0o222,
            0,
            "{relative}"
        );
    }
    assert_eq!(
        mode(&added.base_path.join("src/nested/deeper/leaf.txt")) & 0o222,
        0
    );

    let package = worktree.join("node_modules/package/lib");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("index.js"), "module.exports = 1;\n").unwrap();
    set_mode(&package.join("index.js"), 0o444);
    symlink(&outside, worktree.join("node_modules/outside-dir")).unwrap();
    symlink(outside.join("target.txt"), package.join("outside-file")).unwrap();
    symlink("index.js", package.join("inside")).unwrap();
    set_mode(&package, 0o555);
    set_mode(&worktree.join("node_modules/package"), 0o500);
    assert!(git(&worktree, &["status", "--porcelain=v1"]).is_empty());

    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: worktree.clone(),
        state_dir: Some(state.clone()),
    })
    .expect("remove a clean view holding read-only ignored files");
    assert!(!worktree.exists());
    assert_eq!(
        (mode(&outside), mode(&outside.join("target.txt"))),
        outside_modes
    );
    assert_eq!(
        fs::read_to_string(outside.join("target.txt")).unwrap(),
        "outside\n"
    );

    let applied = garbage_collect(&state, true).expect("collect the read-only base");
    assert_eq!(
        applied.collected.as_slice(),
        std::slice::from_ref(&added.base_path)
    );
    assert!(!added.base_path.exists());
    assert_eq!(
        (mode(&outside), mode(&outside.join("target.txt"))),
        outside_modes
    );
    set_mode(&outside, 0o755);
}
