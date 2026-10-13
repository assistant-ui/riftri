//! Property tests for repository inspection: wherever inside a repository
//! Riftri is pointed, it must report the same identity Git does, and a path
//! outside any repository must be named as such.
#![cfg(unix)]

use std::ffi::OsStr;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};
use riftri_git::{Git, GitError, ObjectId};

#[allow(dead_code)]
mod support;

use support::Fixture;

/// Directory names a shell would mangle. The volume decides which bytes a
/// name may hold, so these stay valid UTF-8.
fn directory_name() -> impl PrintableGenerator<String> {
    gs::text()
        .alphabet(" \t\n\"'\\*?:-#é雪a")
        .min_size(1)
        .max_size(6)
}

fn nested(tc: &TestCase, base: PathBuf) -> PathBuf {
    let names = tc.draw(gs::vecs(directory_name()).max_size(2));
    let path = names
        .iter()
        .fold(base, |path, name| path.join(format!("d{name}")));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[hegel::test(test_cases = 15)]
fn every_directory_of_a_repository_reports_the_same_identity(tc: TestCase) {
    let fixture = Fixture::new(tc.draw(gs::sampled_from(vec!["sha1", "sha256"])));
    let repository = fixture.repository();
    let git = Git::default();
    let born = tc.draw(gs::booleans());
    let head = born.then(|| fixture.commit("initial"));
    let inside = nested(&tc, repository.clone());
    let dirty = tc.draw(gs::booleans());
    if dirty {
        std::fs::write(inside.join("untracked"), "private").unwrap();
    }

    let info = git
        .inspect_repository(&inside)
        .expect("inspect from inside");
    assert_eq!(info.root.as_ref(), Some(&repository));
    assert_eq!(info.identity.common_git_dir, repository.join(".git"));
    assert!(!info.is_bare);
    assert_eq!(info.head_commit, head);
    assert_eq!((info.head_tree, info.clean), (None, None));

    let report = git
        .inspect_repository_for_report(&inside)
        .expect("inspect for a report");
    assert_eq!(report.head_commit, head);
    assert_eq!(report.head_tree.is_some(), born);
    assert_eq!(report.clean, Some(!dirty));
    assert_eq!(git.worktree_is_clean(&inside).unwrap(), !dirty);
    assert!(!git.repository_absent(&inside).unwrap());

    if let Some(head) = head {
        let resolved = git.resolve_commit_tree(&inside, head.clone()).unwrap();
        assert_eq!(Some(resolved.tree), report.head_tree);
        assert_eq!(resolved.commit, head);
    }
}

#[hegel::test(test_cases = 15)]
fn a_bare_repository_has_an_identity_but_no_checkout(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let born = tc.draw(gs::booleans());
    let head = born.then(|| fixture.commit("initial"));
    let bare = fixture
        .root()
        .join(format!("b{}.git", tc.draw(directory_name())));
    support::git(
        fixture.root(),
        &[
            "clone",
            "--quiet",
            "--bare",
            "repository",
            bare.to_str().unwrap(),
        ],
    );
    let git = Git::default();

    for report in [
        git.inspect_repository(&bare).unwrap(),
        git.inspect_repository_for_report(&bare).unwrap(),
    ] {
        assert!(report.is_bare);
        assert_eq!(report.root, None);
        assert_eq!(report.identity.common_git_dir, bare);
        assert_eq!(report.head_commit, head);
        // Cleanliness and the tree describe a checkout, which is absent.
        assert_eq!((report.head_tree, report.clean), (None, None));
    }
    assert_eq!(git.worktree_root_from_git_dir(&bare).unwrap(), None);
}

#[hegel::test(test_cases = 15)]
fn a_path_outside_any_repository_is_reported_as_no_repository(tc: TestCase) {
    let scratch = tempfile::tempdir().unwrap();
    let outside = nested(&tc, scratch.path().canonicalize().unwrap());
    let exists = tc.draw(gs::booleans());
    let path = if exists {
        outside
    } else {
        outside.join(tc.draw(directory_name()))
    };
    let git = Git::default();

    assert!(git.repository_absent(&path).unwrap());
    for inspected in [
        git.inspect_repository(&path),
        git.inspect_repository_with_head_tree(&path),
        git.inspect_repository_for_report(&path),
        git.inspect_repository_for_add(&path, OsStr::new("main"))
            .map(|(info, _)| info),
    ] {
        match inspected {
            Err(GitError::RepositoryAbsent { path: reported }) => assert_eq!(reported, path),
            other => panic!("expected an absent repository, got {other:?}"),
        }
    }

    // Operations that need a repository fail, and a missing directory is
    // blamed on the directory, not on the Git executable.
    let tree = ObjectId::parse("1".repeat(40)).unwrap();
    let failures = [
        git.resolve_requested_revision(&path, OsStr::new("main"))
            .map(drop),
        git.list_worktrees(&path).map(drop),
        git.list_tree(&path, &tree).map(drop),
        git.refresh_worktree_index(&path),
        git.materialize_tree(&path, &tree, scratch.path(), &scratch.path().join("index")),
    ];
    for failure in failures {
        match failure {
            Err(GitError::WorkingDirectoryMissing { path: reported }) => {
                assert!(!exists);
                assert_eq!(reported, path);
            }
            Err(GitError::CommandFailed { .. }) => assert!(exists),
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}
