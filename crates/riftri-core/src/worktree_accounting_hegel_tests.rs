//! One repository with views spread over several registered state
//! directories, some of them damaged. The repository-wide inventory accounts
//! for every view: a view is listed under the one state that owns it, or its
//! state says why it could not be. Ownership lookups that authorize lifecycle
//! work refuse instead of guessing.

use std::fs;
use std::path::{Path, PathBuf};

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::tests::git;
use super::super::{
    MoveWorktreeRequest, RemoveWorktreeRequest, add_worktree, inspect_managed_worktrees,
    managed_worktree_state_directory, move_worktree_inner, remove_worktree_inner,
    storage_accounting, worktree_inventory_across_states,
};
use super::{Fixture, TreeFile, edit_journal, journal_path_value, registered, snapshot};
use crate::MoveWorktreePhase;
use crate::journal::JournalStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Damage {
    None,
    /// A journal in the second state is not a journal at all.
    UnreadableJournal,
    /// Both states hold an active journal for the second view.
    ClaimedTwice,
    /// A move of the second view stopped after recording its intent.
    PendingMove,
    /// Git can no longer resolve the second view's HEAD.
    BrokenHead,
    /// The second view sits on a branch with no commit.
    UnbornHead,
    /// The second view's journal names a directory that is not a repository.
    RepositoryIsPlainDirectory,
    /// The second view's journal names a repository that no longer exists.
    RepositoryGone,
    /// Another repository keeps a view in the second state.
    SharedWithAnotherRepository,
}

fn inventory_with(tc: TestCase, damage: Damage) {
    let query_from_view = tc.draw(gs::booleans());
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("a.txt", b"text\n")]);
    let states = [fixture.state.clone(), fixture.path("second-state")];
    let views = [fixture.path("first"), fixture.path("second")];
    for (state, view) in states.iter().zip(&views) {
        let mut request = fixture.add_request(view, &commit);
        request.state_dir = Some(state.clone());
        add_worktree(request).expect("managed view");
    }
    let second = JournalStore::open(&states[1])
        .load_all()
        .expect("second journal")
        .remove(0);
    let moved = fixture.path("moved");
    let other = Fixture::new();

    match damage {
        Damage::None => {}
        Damage::UnreadableJournal => {
            fs::write(states[1].join("operations/18aa-1-0.json"), b"not json").unwrap();
        }
        Damage::ClaimedTwice => {
            let copy = states[0]
                .join("operations")
                .join(second.journal_path.file_name().unwrap());
            fs::copy(&second.journal_path, &copy).unwrap();
            let rehomed = |path: &Path| states[0].join(path.strip_prefix(&states[1]).unwrap());
            edit_journal(&copy, |value| {
                value["base_path"] = journal_path_value(&rehomed(&second.base_path));
                value["base_staging"] = journal_path_value(&rehomed(&second.base_staging));
                value["temporary_index"] = journal_path_value(&rehomed(&second.temporary_index));
            });
        }
        Damage::PendingMove => {
            move_worktree_inner(
                MoveWorktreeRequest {
                    repository: fixture.repository.clone(),
                    source: views[1].clone(),
                    destination: moved.clone(),
                    state_dir: Some(states[1].clone()),
                },
                Some(MoveWorktreePhase::IntentRecorded),
            )
            .expect_err("stop the move");
        }
        Damage::BrokenHead => {
            fs::write(
                fixture.repository.join(".git/worktrees/second/HEAD"),
                "garbage\n",
            )
            .unwrap();
        }
        Damage::UnbornHead => git(&views[1], &["checkout", "--quiet", "--orphan", "unborn"]),
        Damage::RepositoryIsPlainDirectory | Damage::RepositoryGone => {
            let elsewhere = fixture.path("not-a-repository");
            if damage == Damage::RepositoryIsPlainDirectory {
                fs::create_dir(&elsewhere).unwrap();
            }
            edit_journal(&second.journal_path, |value| {
                value["repository"] = journal_path_value(&elsewhere);
            });
        }
        Damage::SharedWithAnotherRepository => {
            let foreign = other.commit("main", &[TreeFile::regular("b.txt", b"other\n")]);
            let mut request = other.add_request(&other.path("foreign-view"), &foreign);
            request.state_dir = Some(states[1].clone());
            add_worktree(request).expect("a view of another repository");
        }
    }
    tc.note(&format!(
        "{damage:?}, queried from a view {query_from_view}"
    ));
    let contents = views.iter().map(|view| snapshot(view)).collect::<Vec<_>>();
    let query = if query_from_view {
        &views[0]
    } else {
        &fixture.repository
    };

    let inventory = worktree_inventory_across_states(query).expect("inventory");
    assert!(
        inventory.registration_issues.is_empty(),
        "{:?}",
        inventory.registration_issues
    );
    let listed = inventory
        .states
        .iter()
        .flat_map(|state| {
            state
                .views
                .iter()
                .map(|view| (state.state_directory.clone(), view.destination.clone()))
        })
        .collect::<Vec<(PathBuf, PathBuf)>>();
    for (state, view) in &listed {
        assert!(
            views.contains(view),
            "the inventory lists {}, which is not this repository's view",
            view.display()
        );
        let owner = views
            .iter()
            .position(|candidate| candidate == view)
            .unwrap();
        // Two states that both claim a view are both shown with it.
        if damage != Damage::ClaimedTwice {
            assert_eq!(
                listed.iter().filter(|(_, other)| other == view).count(),
                1,
                "{} is listed twice",
                view.display()
            );
            assert_eq!(
                state, &states[owner],
                "a view is listed under another state"
            );
        }
    }
    assert!(
        listed.contains(&(states[0].clone(), views[0].clone())),
        "damage to one state hid a healthy view of another: {listed:?}"
    );
    let second_state = inventory
        .states
        .iter()
        .find(|state| state.state_directory == states[1])
        .expect("the second state is listed");
    let second_listed = listed.iter().any(|(_, view)| view == &views[1]);
    match damage {
        Damage::None | Damage::SharedWithAnotherRepository | Damage::PendingMove => {
            assert!(second_listed, "{listed:?}");
        }
        _ => assert!(
            second_listed || !second_state.diagnostic_issues.is_empty(),
            "a view vanished from the inventory without a finding"
        ),
    }

    // Lookups that authorize lifecycle work never answer for a view whose
    // ownership is in doubt.
    let owner = managed_worktree_state_directory(&fixture.repository, &views[1]);
    let inspection = inspect_managed_worktrees(
        &fixture.repository,
        &[views[0].clone(), views[1].clone(), moved.clone()],
    );
    match damage {
        Damage::None
        | Damage::BrokenHead
        | Damage::UnbornHead
        | Damage::SharedWithAnotherRepository => {
            assert_eq!(owner.expect("owner"), Some(states[1].clone()));
            let inspection = inspection.expect("inspection");
            assert_eq!(inspection[0].state_directory.as_ref(), Some(&states[0]));
            assert_eq!(inspection[1].state_directory.as_ref(), Some(&states[1]));
            assert_eq!(inspection[2].state_directory, None);
        }
        Damage::UnreadableJournal | Damage::ClaimedTwice | Damage::PendingMove => {
            owner.expect_err("ownership in doubt");
            inspection.expect_err("ownership in doubt");
            if damage == Damage::PendingMove {
                // The path a pending move is headed for is not free either.
                managed_worktree_state_directory(&fixture.repository, &moved)
                    .expect_err("the target of a pending move read as unmanaged");
            }
        }
        Damage::RepositoryIsPlainDirectory | Damage::RepositoryGone => {
            tc.note(&format!("owner {owner:?}"));
        }
    }

    // Status of each state answers on its own, and nothing above wrote to a
    // view.
    storage_accounting(&states[0]).expect("status of the healthy state");
    if damage != Damage::UnreadableJournal {
        storage_accounting(&states[1]).expect("status of the damaged state");
    }
    for (view, content) in views.iter().zip(&contents) {
        assert_eq!(
            &snapshot(view),
            content,
            "a read-only report changed a view"
        );
    }

    // The healthy view is still removable unless its own state is in doubt.
    let removed = remove_worktree_inner(
        RemoveWorktreeRequest {
            repository: fixture.repository.clone(),
            destination: views[0].clone(),
            state_dir: Some(states[0].clone()),
        },
        None,
    );
    assert_eq!(
        removed.is_ok(),
        !views[0].exists() && !registered(&fixture.repository, &views[0]),
        "{removed:?}"
    );
    if damage != Damage::ClaimedTwice {
        removed.expect("remove the healthy view");
    }
    assert_eq!(snapshot(&views[1]), contents[1]);
    assert!(other.root().exists());
}

macro_rules! per_damage {
    ($($name:ident => $damage:expr),+ $(,)?) => {
        $(
            #[hegel::test(
                test_cases = 2,
                derandomize = true,
                suppress_health_check = [HealthCheck::TooSlow]
            )]
            fn $name(tc: TestCase) {
                inventory_with(tc, $damage);
            }
        )+
    };
}

per_damage!(
    healthy_states_list_each_view_once => Damage::None,
    an_unreadable_journal_is_reported_for_its_state => Damage::UnreadableJournal,
    a_view_claimed_by_two_states_has_no_owner => Damage::ClaimedTwice,
    a_view_with_a_pending_move_has_no_owner => Damage::PendingMove,
    a_view_with_a_broken_head_is_reported => Damage::BrokenHead,
    a_view_on_an_unborn_branch_is_reported => Damage::UnbornHead,
    a_journal_naming_a_plain_directory_is_reported => Damage::RepositoryIsPlainDirectory,
    a_journal_naming_a_missing_repository_is_reported => Damage::RepositoryGone,
    another_repository_sharing_a_state_is_filtered_out => Damage::SharedWithAnotherRepository,
);
