//! Property tests for populating a linked worktree's index: whichever way
//! the index is built, Git must then see exactly the requested sparse cone
//! and no staged change.
#![cfg(unix)]

use std::ffi::OsStr;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_git::{Git, SparseCheckoutState, WorktreeHead};

#[allow(dead_code)]
mod support;

use support::Fixture;

const DIRECTORIES: [&str; 3] = ["alpha", "beta", "gamma/nested"];

#[hegel::test(test_cases = 8)]
fn a_populated_index_selects_exactly_the_requested_cone(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    for directory in DIRECTORIES {
        std::fs::create_dir_all(repository.join(directory)).unwrap();
        std::fs::write(repository.join(directory).join("file"), directory).unwrap();
    }
    std::fs::write(repository.join("root-file"), "root").unwrap();
    support::git(&repository, &["add", "."]);
    fixture.commit("initial");
    let git = Git::default();
    let worktree = fixture.root().join("linked");
    git.add_worktree_no_checkout(
        &repository,
        &worktree,
        OsStr::new("HEAD"),
        WorktreeHead::Detached,
    )
    .expect("add a worktree without a checkout");
    let cone = tc
        .draw(gs::subsequences(DIRECTORIES.to_vec()))
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let index = repository.join(".git/worktrees/linked/index");
    assert!(!index.exists());
    assert!(!git.worktree_index_has_changes(&worktree).unwrap());

    match tc.draw(gs::sampled_from(vec!["recover", "synchronize"])) {
        "recover" => {
            assert!(
                git.initialize_missing_worktree_index(&worktree, &cone)
                    .unwrap()
            );
            // Recovery never replaces an index that already exists, whatever
            // cone a later attempt asks for.
            let installed = std::fs::read(&index).unwrap();
            let other = tc
                .draw(gs::subsequences(DIRECTORIES.to_vec()))
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert!(
                !git.initialize_missing_worktree_index(&worktree, &other)
                    .unwrap()
            );
            assert_eq!(std::fs::read(&index).unwrap(), installed);
        }
        _ if cone.is_empty() => git.synchronize_worktree_index(&worktree).unwrap(),
        _ => git
            .synchronize_sparse_worktree_index(&worktree, &cone)
            .unwrap(),
    }

    assert!(!git.worktree_index_has_changes(&worktree).unwrap());
    git.refresh_worktree_index(&worktree).unwrap();
    let state = git.sparse_checkout_state(&worktree).unwrap();
    let expected = if cone.is_empty() {
        SparseCheckoutState::default()
    } else {
        SparseCheckoutState {
            enabled: true,
            cone: true,
            directories: cone.clone(),
        }
    };
    assert_eq!(state, expected);

    // Git marks every entry outside the cone as skipped; files at the root
    // always belong to a cone.
    let listed = support::git(&worktree, &["ls-files", "-t"]);
    let listed = String::from_utf8(listed.stdout).unwrap();
    for directory in DIRECTORIES {
        let selected = cone.is_empty() || cone.iter().any(|chosen| chosen == directory);
        let tag = if selected { 'H' } else { 'S' };
        assert!(
            listed.contains(&format!("{tag} {directory}/file\n")),
            "{directory}: {listed}"
        );
    }
    assert!(listed.contains("H root-file\n"), "{listed}");
}
