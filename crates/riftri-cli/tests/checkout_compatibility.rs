use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

struct RepositoryFixture {
    _directory: TempDir,
    repository: PathBuf,
}

impl RepositoryFixture {
    fn new() -> Self {
        let directory = tempdir().expect("fixture directory");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("create repository");
        for arguments in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Riftri Tests"][..],
            &["config", "user.email", "riftri@example.invalid"][..],
            &["config", "core.autocrlf", "false"][..],
        ] {
            assert_git_success(&repository, arguments);
        }
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
        assert_git_success(&repository, &["add", "--", "tracked.txt"]);
        assert_git_success(&repository, &["commit", "--quiet", "-m", "initial"]);
        Self {
            _directory: directory,
            repository,
        }
    }

    fn commit(&self, paths: &[&str], message: &str) {
        let mut add = vec!["add", "--"];
        add.extend(paths);
        assert_git_success(&self.repository, &add);
        assert_git_success(&self.repository, &["commit", "--quiet", "-m", message]);
    }
}

#[derive(Clone, Copy, Debug)]
enum UnsafeCheckoutCase {
    GitLfs,
    NestedEncoding,
    RepositoryAttributes,
    SparseCheckout,
    SubmoduleGitlink,
}

impl UnsafeCheckoutCase {
    const ALL: [Self; 5] = [
        Self::GitLfs,
        Self::NestedEncoding,
        Self::RepositoryAttributes,
        Self::SparseCheckout,
        Self::SubmoduleGitlink,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::GitLfs => "git-lfs",
            Self::NestedEncoding => "nested-encoding",
            Self::RepositoryAttributes => "repository-attributes",
            Self::SparseCheckout => "sparse-checkout",
            Self::SubmoduleGitlink => "submodule-gitlink",
        }
    }

    fn blocker_kind(self) -> &'static str {
        match self {
            Self::GitLfs | Self::NestedEncoding => "in-tree-attributes",
            Self::RepositoryAttributes => "effective-attributes",
            Self::SparseCheckout => "sparse-checkout",
            Self::SubmoduleGitlink => "submodules",
        }
    }

    fn configure(self, fixture: &RepositoryFixture) {
        match self {
            Self::GitLfs => {
                fs::write(
                    fixture.repository.join(".gitattributes"),
                    "*.bin filter=lfs diff=lfs merge=lfs -text\n",
                )
                .expect("write Git LFS attributes");
                fs::write(fixture.repository.join("payload.bin"), "lfs pointer\n")
                    .expect("write LFS fixture");
                fixture.commit(&[".gitattributes", "payload.bin"], "Git LFS attributes");
            }
            Self::NestedEncoding => {
                fs::create_dir(fixture.repository.join("nested")).expect("create nested directory");
                fs::write(
                    fixture.repository.join("nested/.gitattributes"),
                    "*.txt working-tree-encoding=UTF-16\n",
                )
                .expect("write nested attributes");
                fs::write(
                    fixture.repository.join("nested/encoded.txt"),
                    b"\xff\xfee\0n\0c\0o\0d\0e\0d\0\r\0\n\0",
                )
                .expect("write UTF-16 fixture");
                fixture.commit(
                    &["nested/.gitattributes", "nested/encoded.txt"],
                    "nested encoding",
                );
            }
            Self::RepositoryAttributes => {
                fs::write(
                    fixture.repository.join(".git/info/attributes"),
                    "*.txt filter=local\n",
                )
                .expect("write repository attributes");
            }
            Self::SparseCheckout => {
                assert_git_success(
                    &fixture.repository,
                    &["config", "core.sparseCheckout", "true"],
                );
            }
            Self::SubmoduleGitlink => {
                let head =
                    String::from_utf8(git(&fixture.repository, &["rev-parse", "HEAD"]).stdout)
                        .expect("UTF-8 object ID");
                let cache_info = format!("160000,{},vendor/dependency", head.trim());
                assert_git_success(
                    &fixture.repository,
                    &["update-index", "--add", "--cacheinfo", &cache_info],
                );
                fs::write(
                    fixture.repository.join(".gitmodules"),
                    "[submodule \"dependency\"]\n\tpath = vendor/dependency\n\turl = ../dependency\n",
                )
                .expect("write submodule metadata");
                fixture.commit(&[".gitmodules"], "submodule gitlink");
            }
        }
    }
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn assert_git_success(path: &Path, arguments: &[&str]) {
    let output = git(path, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

fn riftri(path: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Riftri CLI")
}

#[test]
fn unsafe_checkout_inputs_are_diagnosed_and_rejected_without_mutation() {
    for case in UnsafeCheckoutCase::ALL {
        let fixture = RepositoryFixture::new();
        case.configure(&fixture);
        let destination = fixture
            .repository
            .parent()
            .expect("repository parent")
            .join(format!("{} worktree", case.name()));
        let state = fixture
            .repository
            .parent()
            .expect("repository parent")
            .join(format!("{} state", case.name()));
        let worktrees_before = git(
            &fixture.repository,
            &["worktree", "list", "--porcelain", "-z"],
        );
        assert!(worktrees_before.status.success());

        let doctor = riftri(&fixture.repository, &["doctor", "--json"]);
        assert!(
            doctor.status.success(),
            "doctor failed for {case:?}: {}",
            String::from_utf8_lossy(&doctor.stderr),
        );
        let report: serde_json::Value =
            serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
        let compatibility = &report["repository_compatibility"]["value"];
        assert_eq!(
            compatibility["compatible"], false,
            "{case:?} was unexpectedly compatible",
        );
        let blocker_kinds = compatibility["blockers"]
            .as_array()
            .expect("compatibility blockers")
            .iter()
            .filter_map(|blocker| blocker["kind"].as_str())
            .collect::<Vec<_>>();
        assert!(
            blocker_kinds.contains(&case.blocker_kind()),
            "{case:?} blockers were {blocker_kinds:?}",
        );
        assert!(!state.exists(), "doctor created state for {case:?}");

        let add = riftri(
            &fixture.repository,
            &[
                "worktree",
                "add",
                destination.to_str().expect("UTF-8 destination"),
                "--detach",
                "HEAD",
                "--state-dir",
                state.to_str().expect("UTF-8 state path"),
            ],
        );
        assert!(
            !add.status.success(),
            "unsafe add succeeded for {case:?}: {}",
            String::from_utf8_lossy(&add.stdout),
        );
        assert!(
            !destination.exists(),
            "add created destination for {case:?}"
        );
        assert!(!state.exists(), "add created state for {case:?}");
        let worktrees_after = git(
            &fixture.repository,
            &["worktree", "list", "--porcelain", "-z"],
        );
        assert!(worktrees_after.status.success());
        assert_eq!(
            worktrees_after.stdout, worktrees_before.stdout,
            "add changed Git metadata for {case:?}",
        );
    }
}
