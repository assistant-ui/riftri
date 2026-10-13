//! A state machine over the linked-worktree lifecycle: every operation goes
//! through the real Git, and the inventory Riftri parses must match a model
//! of what Git should now hold.
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};
use riftri_git::{Git, ObjectId, WorktreeHead};

#[allow(dead_code)]
mod support;

use support::Fixture;

#[derive(Debug, Clone)]
struct Linked {
    path: PathBuf,
    branch: Option<String>,
    locked: Option<String>,
    /// The directory is still on disk; Git calls the worktree prunable
    /// once it is gone.
    present: bool,
    dirty: bool,
}

struct Worktrees {
    fixture: Fixture,
    git: Git,
    head: ObjectId,
    linked: Vec<Linked>,
    created: usize,
}

impl Worktrees {
    fn repository(&self) -> PathBuf {
        self.fixture.repository()
    }

    /// A destination no earlier worktree used. Git keeps the path in a
    /// line-oriented file and trims it, so the name avoids line breaks and
    /// ends in a letter; everything else a shell would mangle is fair game.
    fn fresh_path(&mut self, tc: &TestCase) -> PathBuf {
        self.created += 1;
        let odd: String = tc.draw(gs::text().alphabet(" \t\"'\\*?:-#é雪a").max_size(6));
        self.fixture.root().join(format!("w{}{odd}z", self.created))
    }

    /// A rule that needs a linked worktree does nothing while there is
    /// none: a rejected rule would discard the whole sequence so far.
    fn pick(&self, tc: &TestCase) -> Option<usize> {
        let last = self.linked.len().checked_sub(1)?;
        Some(tc.draw(gs::integers::<usize>().max_value(last)))
    }
}

#[hegel::state_machine]
impl Worktrees {
    #[rule(weight = 3.0)]
    fn add(&mut self, tc: TestCase) {
        let path = self.fresh_path(&tc);
        let branch = format!("branch-{}", self.created);
        let head = tc.draw(gs::sampled_from(vec!["new", "existing", "detached"]));
        if head == "existing" {
            support::git(&self.repository(), &["branch", &branch]);
        }
        let head = match head {
            "new" => WorktreeHead::NewBranch(OsStr::new(&branch)),
            "existing" => WorktreeHead::ExistingBranch(OsStr::new(&branch)),
            _ => WorktreeHead::Detached,
        };
        let detached = head == WorktreeHead::Detached;

        self.git
            .add_worktree_no_checkout(&self.repository(), &path, OsStr::new("HEAD"), head)
            .expect("add a linked worktree");

        self.linked.push(Linked {
            path,
            branch: (!detached).then_some(branch),
            locked: None,
            present: true,
            dirty: false,
        });
    }

    /// Git refuses a branch that already exists or is checked out, and
    /// must leave no half-registered worktree behind.
    #[rule]
    fn add_with_a_taken_branch(&mut self, tc: TestCase) {
        let path = self.fresh_path(&tc);
        let head = if tc.draw(gs::booleans()) {
            WorktreeHead::NewBranch(OsStr::new("main"))
        } else {
            WorktreeHead::ExistingBranch(OsStr::new("main"))
        };

        let added =
            self.git
                .add_worktree_no_checkout(&self.repository(), &path, OsStr::new("HEAD"), head);

        assert!(added.is_err());
        assert!(!path.exists());
    }

    #[rule]
    fn lock(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };
        if self.linked[index].locked.is_some() {
            return;
        }
        let reason = tc.draw(gs::sampled_from(vec![
            "",
            "why",
            "two words",
            "quote\"d",
            "é雪",
        ]));
        let path = self.linked[index].path.to_str().unwrap().to_owned();
        let mut arguments = vec!["worktree", "lock"];
        if !reason.is_empty() {
            arguments.extend(["--reason", reason]);
        }
        arguments.extend(["--", &path]);
        support::git(&self.repository(), &arguments);
        self.linked[index].locked = Some(reason.to_owned());
    }

    #[rule]
    fn unlock(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };

        let unlocked = self
            .git
            .unlock_worktree(&self.repository(), &self.linked[index].path);

        let was_locked = self.linked[index].locked.take().is_some();
        assert_eq!(unlocked.is_ok(), was_locked, "{unlocked:?}");
    }

    #[rule]
    fn make_dirty(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };
        if !self.linked[index].present {
            return;
        }
        std::fs::write(self.linked[index].path.join("untracked"), "private").unwrap();
        self.linked[index].dirty = true;
    }

    #[rule]
    fn delete_directory(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };
        if !self.linked[index].present {
            return;
        }
        std::fs::remove_dir_all(&self.linked[index].path).unwrap();
        self.linked[index].present = false;
        self.linked[index].dirty = false;
    }

    #[rule]
    fn move_worktree(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };
        let destination = self.fresh_path(&tc);
        let linked = &self.linked[index];

        let moved = self
            .git
            .move_worktree(&self.repository(), &linked.path, &destination);

        let movable = linked.locked.is_none() && linked.present;
        assert_eq!(moved.is_ok(), movable, "{linked:?}: {moved:?}");
        if movable {
            self.linked[index].path = destination;
        }
    }

    #[rule]
    fn remove(&mut self, tc: TestCase) {
        let Some(index) = self.pick(&tc) else {
            return;
        };
        let force = tc.draw(gs::booleans());
        let linked = &self.linked[index];

        // A plain removal comes first, as it does for a caller that only
        // escalates once Git has refused.
        let plain = self.git.remove_worktree(&self.repository(), &linked.path);
        let mut removable = linked.locked.is_none() && !linked.dirty;
        assert_eq!(plain.is_ok(), removable, "{linked:?}: {plain:?}");
        if !removable && force {
            let forced = self
                .git
                .remove_worktree_force(&self.repository(), &linked.path);
            // A lock outranks force; unsaved files do not.
            removable = linked.locked.is_none();
            assert_eq!(forced.is_ok(), removable, "{linked:?}: {forced:?}");
        }
        if removable {
            assert!(!linked.path.exists());
            self.linked.remove(index);
        }
    }

    #[rule]
    fn prune(&mut self, _: TestCase) {
        self.git
            .prune_worktrees(&self.repository())
            .expect("prune stale registrations");
        self.linked
            .retain(|linked| linked.present || linked.locked.is_some());
    }

    #[invariant(always_run)]
    fn the_inventory_matches_the_model(&mut self, _: TestCase) {
        let repository = self.repository();
        let mut listed = self
            .git
            .list_worktrees(&repository)
            .expect("list worktrees");
        let main = listed.remove(0);
        assert_eq!(main.path, repository);
        assert_eq!(main.branch.as_deref(), Some(&b"refs/heads/main"[..]));
        assert_eq!(main.locked_reason, None);
        assert_eq!(main.prunable_reason, None);

        listed.sort_by(|left, right| left.path.cmp(&right.path));
        let mut expected = self.linked.clone();
        expected.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(listed.len(), expected.len(), "{listed:?}\n{expected:?}");
        for (listed, expected) in listed.iter().zip(&expected) {
            assert_eq!(listed.path, expected.path);
            assert_eq!(listed.head.as_ref(), Some(&self.head));
            assert_eq!(
                listed.branch,
                expected
                    .branch
                    .as_ref()
                    .map(|branch| format!("refs/heads/{branch}").into_bytes())
            );
            assert_eq!(listed.detached, expected.branch.is_none());
            assert!(!listed.bare && !listed.head_unresolvable);
            assert_eq!(
                listed.locked_reason,
                expected
                    .locked
                    .as_ref()
                    .map(|reason| reason.clone().into_bytes())
            );
            // A lock is what protects a registration from being pruned.
            assert_eq!(
                listed.prunable_reason.is_some(),
                !expected.present && expected.locked.is_none(),
                "{listed:?}"
            );
        }
    }

    #[invariant]
    fn every_present_worktree_answers_for_itself(&mut self, tc: TestCase) {
        let repository = self.repository();
        assert_eq!(
            self.git
                .worktree_root_from_git_dir(&repository.join(".git"))
                .unwrap(),
            Some(repository.clone())
        );
        // A selector no registered path ends with is a plain path, however
        // short or long it is.
        for unknown in ["", "no-such-worktree", &"long/".repeat(200)] {
            let unknown = OsString::from(unknown);
            assert_eq!(
                self.git
                    .resolve_worktree_path(&repository, &repository, &unknown)
                    .unwrap(),
                repository.join(&unknown)
            );
        }
        let present = self
            .linked
            .iter()
            .filter(|linked| linked.present)
            .collect::<Vec<_>>();
        if present.is_empty() {
            return;
        }
        let linked = tc.draw(gs::sampled_from(present));

        // Git accepts any unique trailing part of a worktree's path.
        let name = linked.path.file_name().unwrap();
        assert_eq!(
            self.git
                .resolve_worktree_path(&repository, &repository, name)
                .unwrap(),
            linked.path
        );
        let info = self.git.inspect_repository(&linked.path).unwrap();
        assert_eq!(info.root.as_ref(), Some(&linked.path));
        assert_eq!(info.identity.common_git_dir, repository.join(".git"));
        assert_eq!(info.head_commit.as_ref(), Some(&self.head));
        assert_eq!(
            self.git.symbolic_head_branch(&linked.path).unwrap(),
            linked
                .branch
                .as_ref()
                .map(|branch| branch.clone().into_bytes())
        );
        assert_eq!(
            self.git.worktree_is_clean(&linked.path).unwrap(),
            !linked.dirty
        );
        assert_eq!(
            self.git.worktree_is_pristine(&linked.path).unwrap(),
            !linked.dirty
        );
        assert!(!self.git.worktree_has_submodules(&linked.path).unwrap());
    }
}

// Each step spawns Git several times, which is slow enough on a busy
// machine to trip the generation-speed check without anything being wrong.
#[hegel::test(test_cases = 5, suppress_health_check = [HealthCheck::TooSlow])]
fn the_worktree_inventory_follows_every_lifecycle_operation(tc: TestCase) {
    let fixture = Fixture::committed();
    let head = support::object_id(&support::git(&fixture.repository(), &["rev-parse", "HEAD"]));
    let machine = Worktrees {
        fixture,
        git: Git::default(),
        head,
        linked: Vec::new(),
        created: 0,
    };
    hegel::stateful::machine(machine).steps(12).run(tc);
}
