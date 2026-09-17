use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
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
    NonCanonicalGitLfs,
    NestedEncoding,
    RepositoryAttributes,
    SparseCheckout,
    SubmoduleGitlink,
}

impl UnsafeCheckoutCase {
    const ALL: [Self; 5] = [
        Self::NonCanonicalGitLfs,
        Self::NestedEncoding,
        Self::RepositoryAttributes,
        Self::SparseCheckout,
        Self::SubmoduleGitlink,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::NonCanonicalGitLfs => "noncanonical-git-lfs",
            Self::NestedEncoding => "nested-encoding",
            Self::RepositoryAttributes => "repository-attributes",
            Self::SparseCheckout => "sparse-checkout",
            Self::SubmoduleGitlink => "submodule-gitlink",
        }
    }

    fn blocker_kind(self) -> &'static str {
        match self {
            Self::NonCanonicalGitLfs => "in-tree-attributes",
            Self::NestedEncoding => "in-tree-attributes",
            Self::RepositoryAttributes => "effective-attributes",
            Self::SparseCheckout => "sparse-checkout",
            Self::SubmoduleGitlink => "submodules",
        }
    }

    fn configure(self, fixture: &RepositoryFixture) {
        match self {
            Self::NonCanonicalGitLfs => {
                fs::write(
                    fixture.repository.join(".gitattributes"),
                    "*.bin filter=lfs -text\n",
                )
                .expect("write incomplete Git LFS attributes");
                fs::write(
                    fixture.repository.join("payload.bin"),
                    "not an LFS pointer\n",
                )
                .expect("write unsafe LFS fixture");
                fixture.commit(
                    &[".gitattributes", "payload.bin"],
                    "incomplete Git LFS attributes",
                );
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
        // The fixture configures its own filters. An inherited Git LFS clean
        // filter can otherwise install hooks while staging pointer blobs.
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

#[cfg(target_os = "macos")]
#[test]
fn canonical_local_git_lfs_object_creates_and_compacts_a_clean_isolated_worktree() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = RepositoryFixture::new();
    let contents = vec![0x5a; 2 * 1024 * 1024];
    let oid = Sha256::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
        contents.len()
    );
    fs::write(
        fixture.repository.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .expect("write canonical LFS attributes");
    fs::write(fixture.repository.join("payload.bin"), &pointer).expect("write LFS pointer");
    fixture.commit(
        &[".gitattributes", "payload.bin"],
        "canonical Git LFS pointer",
    );

    let object = fixture
        .repository
        .join(".git/lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(&oid);
    fs::create_dir_all(object.parent().expect("LFS object parent"))
        .expect("create LFS object store");
    fs::write(&object, &contents).expect("write local LFS object");

    for (key, value) in [
        ("filter.lfs.clean", "git-lfs clean -- %f"),
        ("filter.lfs.smudge", "git-lfs smudge -- %f"),
        ("filter.lfs.required", "true"),
    ] {
        assert_git_success(&fixture.repository, &["config", key, value]);
    }
    let fake_bin = fixture
        .repository
        .parent()
        .expect("repository parent")
        .join("fake-bin");
    fs::create_dir(&fake_bin).expect("create fake executable directory");
    let fake_lfs = fake_bin.join("git-lfs");
    fs::write(
        &fake_lfs,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  version) printf '%s\\n' 'git-lfs/3.7.0 (riftri fixture)' ;;\n  clean) cat >/dev/null; printf '%s' '{pointer}' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .expect("write fake git-lfs");
    fs::set_permissions(&fake_lfs, fs::Permissions::from_mode(0o755))
        .expect("make fake git-lfs executable");
    let inherited_path = std::env::var_os("PATH").expect("PATH");
    let mut child_path = fake_bin.into_os_string();
    child_path.push(":");
    child_path.push(inherited_path);

    let destination = fixture
        .repository
        .parent()
        .expect("repository parent")
        .join("lfs-worktree");
    let state = fixture
        .repository
        .parent()
        .expect("repository parent")
        .join("lfs-state");
    let mut add = Command::new(env!("CARGO_BIN_EXE_riftri"));
    add.args([
        "worktree",
        "add",
        destination.to_str().expect("UTF-8 destination"),
        "--detach",
        "HEAD",
        "--state-dir",
        state.to_str().expect("UTF-8 state"),
    ])
    .current_dir(&fixture.repository)
    .env("PATH", &child_path)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    // Verified LFS objects must not bypass the checkout-hook policy. Install
    // a disposable fixture hook, prove preflight refuses it, then exercise the
    // supported hook-free profile without weakening the production guard.
    let hook = fixture.repository.join(".git/hooks/post-checkout");
    assert!(
        !hook.exists(),
        "host configuration installed a fixture hook"
    );
    fs::write(&hook, "#!/bin/sh\ngit lfs post-checkout \"$@\"\n").expect("write fixture LFS hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
        .expect("make fixture LFS hook executable");
    let refused = add.output().expect("try LFS add with a checkout hook");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("post-checkout"));
    assert!(!destination.exists());
    assert!(!state.exists());
    fs::remove_file(hook).expect("remove only the disposable fixture hook");
    let output = add.output().expect("run Riftri LFS add");
    assert!(
        output.status.success(),
        "Riftri LFS add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(destination.join("payload.bin")).expect("read expanded payload"),
        contents
    );
    let status = Command::new("git")
        .args(["status", "--porcelain=v1", "-z"])
        .current_dir(&destination)
        .env("PATH", &child_path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("inspect LFS worktree");
    assert!(status.status.success());
    assert!(status.stdout.is_empty(), "expanded LFS worktree is dirty");

    let compact = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "worktree",
            "compact",
            destination.to_str().expect("UTF-8 destination"),
            "--state-dir",
            state.to_str().expect("UTF-8 state"),
        ])
        .current_dir(&fixture.repository)
        .env("PATH", &child_path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("compact Riftri LFS worktree");
    assert!(
        compact.status.success(),
        "Riftri LFS compaction failed: {}",
        String::from_utf8_lossy(&compact.stderr)
    );
    assert_eq!(
        fs::read(destination.join("payload.bin")).expect("read compacted expanded payload"),
        contents
    );

    fs::write(destination.join("payload.bin"), b"private edit\n").expect("edit private view");
    assert_eq!(
        fs::read(&object).expect("read retained LFS object"),
        vec![0x5a; 2 * 1024 * 1024],
        "private worktree edit changed the local LFS object"
    );
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
