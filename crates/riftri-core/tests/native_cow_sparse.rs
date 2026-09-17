#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use riftri_core::{AddWorktreeRequest, WorktreeMode, add_worktree};

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

/// A repository with root files plus `a/`, `b/`, and `crates/riftri-core/`
/// directories, committed once.
fn sparse_fixture_repository(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("root.txt"), "root\n").expect("write root file");
    for directory in ["a/nested", "b", "crates/riftri-core", "crates/riftri-cli"] {
        fs::create_dir_all(repository.join(directory)).expect("create fixture directory");
    }
    fs::write(repository.join("a/file.txt"), "a\n").expect("write a file");
    fs::write(repository.join("a/nested/deep.txt"), "deep\n").expect("write nested file");
    fs::write(repository.join("b/file.txt"), "b\n").expect("write b file");
    fs::write(repository.join("crates/riftri-core/lib.rs"), "// core\n").expect("write core file");
    fs::write(repository.join("crates/riftri-cli/main.rs"), "// cli\n").expect("write cli file");
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    repository
}

fn add(
    repository: &Path,
    destination: &Path,
    state: &Path,
    branch: &str,
    sparse: &[&str],
) -> Result<riftri_core::AddWorktreeResult, riftri_core::WorktreeError> {
    add_worktree(AddWorktreeRequest {
        repository: repository.to_path_buf(),
        destination: destination.to_path_buf(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from(branch)),
        state_dir: Some(state.to_path_buf()),
        sparse_directories: sparse.iter().map(|dir| (*dir).to_owned()).collect(),
    })
}

fn assert_clean(worktree: &Path) {
    assert!(
        git(
            worktree,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        )
        .is_empty(),
        "worktree {} is not clean",
        worktree.display()
    );
}

#[test]
fn creates_a_clean_sparse_cone_view_with_real_git_semantics() {
    let fixture = tempdir().expect("fixture directory");
    let repository = sparse_fixture_repository(fixture.path());
    let state = fixture.path().join("state");
    let destination = fixture.path().join("sparse-a");

    let result = add(
        &repository,
        &destination,
        &state,
        "feature/sparse-a",
        &["a"],
    )
    .expect("create sparse worktree");
    assert!(!result.reused_base);

    // The cone contains all repository-root files plus everything under `a`.
    assert!(destination.join("root.txt").is_file());
    assert!(destination.join("a/file.txt").is_file());
    assert!(destination.join("a/nested/deep.txt").is_file());
    assert!(!destination.join("b").exists());
    assert!(!destination.join("crates").exists());

    // The immutable base holds exactly the same sparse shape.
    assert!(result.base_path.join("root.txt").is_file());
    assert!(result.base_path.join("a/file.txt").is_file());
    assert!(!result.base_path.join("b").exists());
    assert!(!result.base_path.join("crates").exists());

    // Real Git sparse-checkout state: clean status, cone directory list, and
    // skip-worktree bits on every out-of-cone index entry.
    assert_clean(&destination);
    assert_eq!(git(&destination, &["sparse-checkout", "list"]), "a\n");
    let tags = git(&destination, &["ls-files", "-t"]);
    assert!(tags.contains("S b/file.txt"), "{tags}");
    assert!(tags.contains("S crates/riftri-cli/main.rs"), "{tags}");
    assert!(tags.contains("S crates/riftri-core/lib.rs"), "{tags}");
    assert!(tags.contains("H a/file.txt"), "{tags}");
    assert!(tags.contains("H root.txt"), "{tags}");

    // The sparse configuration is scoped to the linked worktree. The main
    // worktree keeps ordinary full-checkout Git configuration and stays clean.
    let main_config = Command::new("git")
        .args(["config", "--local", "core.sparsecheckout"])
        .current_dir(&repository)
        .output()
        .expect("read main worktree config");
    assert!(!main_config.status.success());
    assert_clean(&repository);

    // Compaction of a sparse view is refused until it is supported, and the
    // ordinary clean-removal lifecycle applies unchanged.
    let compaction = riftri_core::compact_worktree(riftri_core::CompactWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        state_dir: Some(state.clone()),
    })
    .expect_err("sparse compaction must be refused");
    assert!(
        compaction
            .to_string()
            .contains("compacting a sparse worktree is not supported yet"),
        "unexpected diagnostic: {compaction}"
    );
    riftri_core::remove_worktree(riftri_core::RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        state_dir: Some(state),
    })
    .expect("cleanly remove sparse worktree");
    assert!(!destination.exists());
}

#[test]
fn sparse_views_share_one_base_and_keep_private_writes_isolated() {
    let fixture = tempdir().expect("fixture directory");
    let repository = sparse_fixture_repository(fixture.path());
    let state = fixture.path().join("state");
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");

    let first_result = add(&repository, &first, &state, "feature/first", &["a"])
        .expect("create first sparse worktree");
    // The same selection written differently canonicalizes to the same
    // profile: trailing slash, duplicate, and a nested cone already covered
    // by its ancestor.
    let second_result = add(
        &repository,
        &second,
        &state,
        "feature/second",
        &["a/", "a", "a/nested"],
    )
    .expect("create second sparse worktree");
    assert!(!first_result.reused_base);
    assert!(second_result.reused_base);
    assert_eq!(first_result.base_path, second_result.base_path);

    fs::write(first.join("a/file.txt"), "private change\n").expect("edit first view");
    assert_eq!(
        fs::read_to_string(second.join("a/file.txt")).expect("read second view"),
        "a\n"
    );
    assert_eq!(
        fs::read_to_string(first_result.base_path.join("a/file.txt")).expect("read base"),
        "a\n"
    );
    assert!(!git(&first, &["status", "--porcelain=v1"]).is_empty());
    assert_clean(&second);
}

#[test]
fn distinct_sparse_selections_at_one_commit_never_share_a_base() {
    let fixture = tempdir().expect("fixture directory");
    let repository = sparse_fixture_repository(fixture.path());
    let state = fixture.path().join("state");

    let full_before = add(
        &repository,
        &fixture.path().join("full-before"),
        &state,
        "feature/full-before",
        &[],
    )
    .expect("create full worktree before sparse views");
    let sparse_a = add(
        &repository,
        &fixture.path().join("sparse-a"),
        &state,
        "feature/sparse-a",
        &["a"],
    )
    .expect("create sparse view of a");
    let sparse_b = add(
        &repository,
        &fixture.path().join("sparse-b"),
        &state,
        "feature/sparse-b",
        &["b"],
    )
    .expect("create sparse view of b");
    let full_after = add(
        &repository,
        &fixture.path().join("full-after"),
        &state,
        "feature/full-after",
        &[],
    )
    .expect("create full worktree after sparse views");

    // Same commit, three distinct materialization profiles, three bases.
    assert_eq!(sparse_a.tree, sparse_b.tree);
    assert_eq!(sparse_a.tree, full_before.tree);
    assert_ne!(sparse_a.base_path, sparse_b.base_path);
    assert_ne!(sparse_a.base_path, full_before.base_path);
    assert_ne!(sparse_b.base_path, full_before.base_path);

    // Full behavior is unchanged: the second full view reuses the original
    // full base even though sparse views were created in between.
    assert!(!full_before.reused_base);
    assert!(full_after.reused_base);
    assert_eq!(full_before.base_path, full_after.base_path);

    let sparse_a_path = fixture.path().join("sparse-a");
    let sparse_b_path = fixture.path().join("sparse-b");
    let full_path = fixture.path().join("full-after");
    assert!(sparse_a_path.join("a/file.txt").is_file());
    assert!(!sparse_a_path.join("b").exists());
    assert!(sparse_b_path.join("b/file.txt").is_file());
    assert!(!sparse_b_path.join("a").exists());
    assert!(full_path.join("a/file.txt").is_file());
    assert!(full_path.join("b/file.txt").is_file());
    assert!(full_path.join("crates/riftri-cli/main.rs").is_file());
    for worktree in [&sparse_a_path, &sparse_b_path, &full_path] {
        assert_clean(worktree);
    }
}

#[test]
fn refuses_unsupported_sparse_requests_before_creating_state() {
    let fixture = tempdir().expect("fixture directory");
    let repository = sparse_fixture_repository(fixture.path());

    let unsupported = [
        ("/a", "absolute"),
        ("a/*", "pattern"),
        ("a?", "pattern"),
        ("!a", "negated"),
        ("", "empty"),
        ("a//nested", "component"),
        ("./a", "component"),
        ("../repository", "component"),
        (".git", "administrative"),
        ("b\\c", "pattern"),
        ("missing-directory", "does not exist"),
        ("root.txt", "is a file"),
    ];
    for (index, (directory, expectation)) in unsupported.into_iter().enumerate() {
        let destination = fixture.path().join(format!("refused-{index}"));
        let state = fixture.path().join(format!("state-{index}"));
        let error = add(
            &repository,
            &destination,
            &state,
            &format!("feature/refused-{index}"),
            &[directory],
        )
        .expect_err("unsupported sparse request must fail closed");
        // Refusals happen before any durable state, Git metadata, or
        // destination directory exists, and never fall back to a full tree.
        assert!(
            !destination.exists(),
            "{directory:?} ({expectation}) created a destination"
        );
        assert!(
            !state.exists(),
            "{directory:?} ({expectation}) created lifecycle state: {error}"
        );
        let branch = Command::new("git")
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/feature/refused-{index}"))
            .current_dir(&repository)
            .status()
            .expect("check branch");
        assert!(
            !branch.success(),
            "{directory:?} ({expectation}) created a branch"
        );
    }
}

#[test]
fn refuses_repository_configured_sparse_checkout_before_creating_state() {
    let fixture = tempdir().expect("fixture directory");
    let repository = sparse_fixture_repository(fixture.path());
    git(&repository, &["config", "core.sparseCheckout", "true"]);

    for sparse in [&["a"][..], &[][..]] {
        let destination = fixture.path().join("configured");
        let state = fixture.path().join("state");
        let error = add(
            &repository,
            &destination,
            &state,
            "feature/configured",
            sparse,
        )
        .expect_err("repository-configured sparse checkout must be refused");
        assert!(
            error.to_string().contains("core.sparsecheckout"),
            "unexpected diagnostic: {error}"
        );
        assert!(!destination.exists());
        assert!(!state.exists());
    }
}
