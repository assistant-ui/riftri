//! Property tests that resolve revisions and branches against the real Git
//! as the oracle: Riftri must name the same commit Git does, or none.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use riftri_git::{Git, ObjectId, ResolvedRevision};

#[allow(dead_code)]
mod support;

use support::{Fixture, IDENTITY, isolated_git};

/// A repository with a short history, each commit with its own tree.
fn history(tc: &TestCase) -> (Fixture, Vec<ObjectId>) {
    let fixture = Fixture::new(tc.draw(gs::sampled_from(vec!["sha1", "sha256"])));
    let repository = fixture.repository();
    let commit_count = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    let commits = (0..commit_count)
        .map(|index| {
            std::fs::write(repository.join("file"), index.to_string()).unwrap();
            support::git(&repository, &["add", "file"]);
            fixture.commit(&format!("commit {index}"))
        })
        .collect::<Vec<_>>();
    (fixture, commits)
}

/// What `rev-parse --verify` makes of an expression: the single commit it
/// names once tags are peeled, with that commit's tree.
fn oracle(repository: &Path, expression: &str) -> Option<ResolvedRevision> {
    let verify = |expression: &str| {
        let output = isolated_git(repository)
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(expression)
            .output()
            .unwrap();
        // A negation prints `^<id>`, which is not an object.
        ObjectId::parse(String::from_utf8_lossy(&output.stdout).trim())
            .ok()
            .filter(|_| output.status.success())
    };
    let object = verify(expression)?;
    let commit = verify(&format!("{}^{{commit}}", object.as_str()))?;
    let tree = verify(&format!("{}^{{tree}}", commit.as_str()))?;
    Some(ResolvedRevision { commit, tree })
}

fn expression(tc: &TestCase, commits: &[ObjectId]) -> String {
    let commit = tc.draw(gs::sampled_from(commits.to_vec()));
    let atom = tc.draw(gs::sampled_from(vec![
        "HEAD".to_owned(),
        "@".to_owned(),
        "main".to_owned(),
        "topic".to_owned(),
        "heads/topic".to_owned(),
        "refs/heads/main".to_owned(),
        "light".to_owned(),
        "annotated".to_owned(),
        "tree-tag".to_owned(),
        "blob-tag".to_owned(),
        "missing".to_owned(),
        commit.as_str().to_owned(),
        commit.as_str()[..9].to_owned(),
        String::new(),
    ]));
    let prefix = tc.draw(gs::sampled_from(vec![
        "", "", "", "^", ":/", "-", "--", " ",
    ]));
    let suffixes = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "~1",
            "^",
            "^0",
            "^2",
            "^{}",
            "^{commit}",
            "^{tree}",
            "^{/commit}",
            "^{/c..mit}",
            ":",
            ":file",
            ":missing",
            "^@",
            "^!",
            "^-",
            "@{0}",
            "..",
            "..main",
            "...topic",
            " ",
            "\n",
            "^{",
        ]))
        .max_size(2),
    );
    format!("{prefix}{atom}{}", suffixes.concat())
}

#[hegel::test(test_cases = 8)]
fn requested_revisions_resolve_to_the_commit_git_names_or_to_none(tc: TestCase) {
    let (fixture, commits) = history(&tc);
    let repository = fixture.repository();
    let pick = |tc: &TestCase| tc.draw(gs::sampled_from(commits.clone()));
    let tree = oracle(&repository, "HEAD").expect("HEAD is a commit").tree;
    let blob = fixture.blob(b"tagged blob");
    for (name, target) in [
        ("refs/heads/topic", pick(&tc)),
        ("refs/tags/light", pick(&tc)),
        ("refs/tags/tree-tag", tree),
        ("refs/tags/blob-tag", blob),
    ] {
        support::git(&repository, &["update-ref", name, target.as_str()]);
    }
    let annotated = isolated_git(&repository)
        .args([
            "tag",
            "-a",
            "-m",
            "annotated",
            "annotated",
            pick(&tc).as_str(),
        ])
        .envs(IDENTITY)
        .status()
        .unwrap();
    assert!(annotated.success());
    let head = oracle(&repository, "HEAD").expect("HEAD is a commit");
    let git = Git::default();

    for _ in 0..5 {
        let expression = expression(&tc, &commits);
        let expected = oracle(&repository, &expression);
        let revision = OsStr::new(&expression);

        let requested = git.resolve_requested_revision(&repository, revision);
        assert_eq!(
            requested.as_ref().ok(),
            Some(&expected),
            "{expression:?}: {requested:?}"
        );

        let strict = git.resolve_revision(&repository, revision);
        assert_eq!(strict.ok(), expected, "{expression:?}");

        // The add path may decline to batch, but must never cache a
        // different commit than the strict path would resolve.
        let (info, cached) = git
            .inspect_repository_for_add(&repository, revision)
            .expect("inspect for add");
        assert_eq!(info.head_commit.as_ref(), Some(&head.commit));
        assert_eq!(info.head_tree.as_ref(), Some(&head.tree));
        assert!(
            cached.is_none() || cached == expected,
            "{expression:?}: cached {cached:?}, expected {expected:?}"
        );
    }
}

const BRANCHES: [&str; 5] = ["topic", "dir/leaf", "x", "release-1.0", "refs/heads/topic"];

struct Branches {
    fixture: Fixture,
    git: Git,
    commits: Vec<ObjectId>,
    branches: BTreeMap<&'static str, ObjectId>,
    attached: bool,
}

impl Branches {
    fn repository(&self) -> PathBuf {
        self.fixture.repository()
    }

    fn commit(&self, tc: &TestCase) -> ObjectId {
        tc.draw(gs::sampled_from(self.commits.clone()))
    }

    fn assert_target(&self, branch: &str) {
        let target = self
            .git
            .local_branch_target(&self.repository(), OsStr::new(branch))
            .expect("look up the branch");
        assert_eq!(target.as_ref(), self.branches.get(branch), "{branch}");
    }
}

#[hegel::state_machine]
impl Branches {
    #[rule]
    fn create_branch(&mut self, tc: TestCase) {
        let branch = tc.draw(gs::sampled_from(BRANCHES.to_vec()));
        let commit = self.commit(&tc);
        let reference = format!("refs/heads/{branch}");
        support::git(
            &self.repository(),
            &["update-ref", &reference, commit.as_str()],
        );
        self.branches.insert(branch, commit);
        self.assert_target(branch);
    }

    /// References that merely end with a branch's full name. `show-ref`
    /// matches by suffix, so each of these is a decoy for the lookup.
    #[rule]
    fn create_decoy(&mut self, tc: TestCase) {
        let branch = tc.draw(gs::sampled_from(BRANCHES[..4].to_vec()));
        let namespace = tc.draw(gs::sampled_from(vec![
            "refs/remotes/origin/refs/heads",
            "refs/tags",
            "refs/remotes/origin",
            "refs/notes/refs/heads",
        ]));
        let reference = format!("{namespace}/{branch}");
        let commit = self.commit(&tc);
        support::git(
            &self.repository(),
            &["update-ref", &reference, commit.as_str()],
        );
    }

    #[rule]
    fn move_branch_if_unchanged(&mut self, tc: TestCase) {
        let branch = tc.draw(gs::sampled_from(BRANCHES.to_vec()));
        let (new, expected) = (self.commit(&tc), self.commit(&tc));

        let moved = self.git.move_branch_if_unchanged(
            &self.repository(),
            OsStr::new(branch),
            &new,
            &expected,
        );

        let unchanged = self.branches.get(branch) == Some(&expected);
        assert_eq!(moved.is_ok(), unchanged, "{branch}: {moved:?}");
        if unchanged {
            self.branches.insert(branch, new);
        }
        self.assert_target(branch);
    }

    #[rule]
    fn delete_branch(&mut self, tc: TestCase) {
        let branch = tc.draw(gs::sampled_from(BRANCHES.to_vec()));

        let deleted = self
            .git
            .delete_branch_force(&self.repository(), OsStr::new(branch));

        let existed = self.branches.remove(branch).is_some();
        assert_eq!(deleted.is_ok(), existed, "{branch}: {deleted:?}");
        self.assert_target(branch);
    }

    #[rule]
    fn switch_head(&mut self, _: TestCase) {
        self.attached = !self.attached;
        let target = if self.attached { "main" } else { "--detach" };
        support::git(&self.repository(), &["checkout", "--quiet", target]);
    }

    #[invariant]
    fn lookups_see_exactly_the_branches_that_exist(&mut self, _: TestCase) {
        for branch in BRANCHES {
            self.assert_target(branch);
        }
        let head = self.git.symbolic_head_branch(&self.repository()).unwrap();
        assert_eq!(head, self.attached.then(|| b"main".to_vec()));
        assert!(self.git.has_any_reference(&self.repository()).unwrap());
    }
}

#[hegel::test(test_cases = 6)]
fn branch_lookups_and_updates_follow_the_references_git_holds(tc: TestCase) {
    let (fixture, commits) = history(&tc);
    let machine = Branches {
        fixture,
        git: Git::default(),
        commits,
        branches: BTreeMap::new(),
        attached: true,
    };
    hegel::stateful::machine(machine).steps(10).run(tc);
}

#[hegel::test(test_cases = 20)]
fn a_repository_without_commits_resolves_nothing(tc: TestCase) {
    let fixture = Fixture::new(tc.draw(gs::sampled_from(vec!["sha1", "sha256"])));
    let repository = fixture.repository();
    let git = Git::default();
    assert!(!git.has_any_reference(&repository).unwrap());
    assert_eq!(
        git.symbolic_head_branch(&repository).unwrap(),
        Some(b"main".to_vec())
    );
    let missing = ObjectId::parse("1".repeat(40)).unwrap();
    let expression = expression(&tc, &[missing]);
    tc.assume(oracle(&repository, &expression).is_none());
    // A bare full-length ID is the subject of the ignored property below.
    tc.assume(ObjectId::parse(expression.as_str()).is_err());

    let revision = OsStr::new(&expression);
    assert_eq!(
        git.resolve_requested_revision(&repository, revision)
            .unwrap(),
        None,
        "{expression:?}"
    );
    assert!(git.resolve_revision(&repository, revision).is_err());
    let (info, cached) = git
        .inspect_repository_for_add(&repository, revision)
        .expect("an unborn HEAD is valid repository state");
    assert_eq!(
        (info.head_commit, info.head_tree, cached),
        (None, None, None)
    );
    let branch = tc.draw(gs::sampled_from(BRANCHES.to_vec()));
    assert_eq!(
        git.local_branch_target(&repository, OsStr::new(branch))
            .unwrap(),
        None
    );
}

#[hegel::test(test_cases = 20)]
#[ignore = "bug: resolve_requested_revision reports an absent full-length ID as a Git failure"]
fn a_full_length_id_of_an_absent_object_names_nothing(tc: TestCase) {
    let (fixture, commits) = history(&tc);
    let repository = fixture.repository();
    let length = commits[0].as_str().len();
    let digit = tc.draw(gs::sampled_from(
        "123456789abcdef".chars().collect::<Vec<_>>(),
    ));
    let absent = digit.to_string().repeat(length);
    tc.assume(oracle(&repository, &absent).is_none());

    let requested = Git::default().resolve_requested_revision(&repository, OsStr::new(&absent));

    assert_eq!(requested.ok(), Some(None), "{absent}");
}

#[hegel::test(test_cases = 6)]
fn a_message_search_holding_two_dots_names_one_commit_not_a_range(tc: TestCase) {
    let (fixture, commits) = history(&tc);
    let repository = fixture.repository();
    let index = tc.draw(gs::integers::<usize>().max_value(commits.len() - 1));
    let pattern = tc.draw(gs::sampled_from(vec![
        format!("c..mit {index}"),
        format!("comm.. {index}"),
        format!("commit..{index}"),
        "comm..".to_owned(),
    ]));
    let expression = format!("HEAD^{{/{pattern}}}");
    let expected = oracle(&repository, &expression);

    let git = Git::default();
    let revision = OsStr::new(&expression);
    assert_eq!(git.resolve_revision(&repository, revision).ok(), expected);
    assert_eq!(
        git.resolve_requested_revision(&repository, revision).ok(),
        Some(expected)
    );
}
