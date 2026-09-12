#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::ffi::OsString;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Stdio};

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

fn git_with_input(path: &Path, arguments: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Git fixture command");
    child
        .stdin
        .take()
        .expect("Git stdin")
        .write_all(input)
        .expect("write Git fixture input");
    let output = child
        .wait_with_output()
        .expect("finish Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn destination_is_case_sensitive(parent: &Path) -> bool {
    let probe = tempfile::Builder::new()
        .prefix(".riftri-test-case-")
        .tempdir_in(parent)
        .expect("create case-sensitivity probe");
    fs::write(probe.path().join("Case"), b"upper").expect("create first case probe");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(probe.path().join("case"))
        .is_ok()
}

#[cfg(target_os = "macos")]
const PRIVATE_XATTR: &str = "com.assistantui.riftri.private";
#[cfg(target_os = "linux")]
const PRIVATE_XATTR: &str = "user.riftri.private";

#[cfg(unix)]
fn read_private_xattr(path: &Path) -> rustix::io::Result<Vec<u8>> {
    let mut value = Vec::with_capacity(64);
    rustix::fs::getxattr(
        path,
        PRIVATE_XATTR,
        rustix::buffer::spare_capacity(&mut value),
    )?;
    Ok(value)
}

#[test]
fn creates_clean_isolated_linked_worktrees_from_one_base() {
    let fixture = tempdir().expect("fixture directory");
    let fixture_path = fixture.path().to_path_buf();
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
    #[cfg(unix)]
    fs::set_permissions(
        repository.join("executable.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("set executable mode");
    #[cfg(unix)]
    symlink("tracked.txt", repository.join("tracked-link")).expect("create symlink");
    git(&repository, &["add", "--", "tracked.txt", "executable.sh"]);
    #[cfg(unix)]
    git(&repository, &["add", "--", "tracked-link"]);
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
    #[cfg(unix)]
    assert_ne!(
        fs::metadata(first.join("executable.sh"))
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(first.join("tracked-link")).expect("read symlink"),
        Path::new("tracked.txt")
    );

    #[cfg(unix)]
    {
        fs::set_permissions(
            first.join("executable.sh"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("change executable mode in first view");
        assert_eq!(
            fs::metadata(first.join("executable.sh"))
                .expect("first executable metadata")
                .permissions()
                .mode()
                & 0o111,
            0
        );
        for path in [&second, &first_result.base_path] {
            assert_ne!(
                fs::metadata(path.join("executable.sh"))
                    .expect("unchanged executable metadata")
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        fs::set_permissions(
            first.join("executable.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("restore executable mode in first view");

        fs::remove_file(first.join("tracked-link")).expect("replace first-view symlink");
        symlink("executable.sh", first.join("tracked-link"))
            .expect("create private first-view symlink");
        assert_eq!(
            fs::read_link(second.join("tracked-link")).expect("read second-view symlink"),
            Path::new("tracked.txt")
        );
        assert_eq!(
            fs::read_link(first_result.base_path.join("tracked-link"))
                .expect("read immutable-base symlink"),
            Path::new("tracked.txt")
        );
        fs::remove_file(first.join("tracked-link")).expect("remove private symlink");
        symlink("tracked.txt", first.join("tracked-link")).expect("restore tracked symlink");

        rustix::fs::setxattr(
            first.join("tracked.txt"),
            PRIVATE_XATTR,
            b"first-view",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("set private worktree xattr");
        assert_eq!(
            read_private_xattr(&first.join("tracked.txt")).expect("read first-view xattr"),
            b"first-view"
        );
        assert!(read_private_xattr(&second.join("tracked.txt")).is_err());
        assert!(read_private_xattr(&first_result.base_path.join("tracked.txt")).is_err());
        assert!(git(&first, &["status", "--porcelain=v1"]).is_empty());
    }

    fs::write(first.join("tracked.txt"), "first change\n").expect("edit first view");
    assert_eq!(
        fs::read_to_string(second.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(first_result.base_path.join("tracked.txt")).unwrap(),
        "base\n"
    );

    drop(fixture);
    assert!(
        !fixture_path.exists(),
        "read-only immutable base prevented fixture cleanup"
    );
}

#[test]
fn validates_unicode_aliases_before_durable_mutation() {
    for (first_name, second_name) in [("e\u{301}.txt", "é.txt"), ("Ü.txt", "ü.txt")] {
        let fixture = tempdir().unwrap();
        let repository = fixture.path().join("repository");
        let state = fixture.path().join("state");
        let destination = fixture.path().join("worktree");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Test"]);
        git(
            &repository,
            &["config", "user.email", "test@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        let first = git_with_input(&repository, &["hash-object", "-w", "--stdin"], b"first\n");
        let second = git_with_input(&repository, &["hash-object", "-w", "--stdin"], b"second\n");
        let input = format!(
            "100644 blob {}\t{first_name}\0100644 blob {}\t{second_name}\0",
            first.trim(),
            second.trim()
        );
        let tree = git_with_input(&repository, &["mktree", "-z"], input.as_bytes());
        let commit = git(
            &repository,
            &["commit-tree", tree.trim(), "-m", "unicode fixture"],
        );
        let probe = tempfile::tempdir_in(fixture.path()).unwrap();
        fs::write(probe.path().join(first_name), b"first").unwrap();
        let distinct = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe.path().join(second_name))
        {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => panic!("inspect filesystem names: {error}"),
        };
        probe.close().unwrap();
        let before = git(&repository, &["worktree", "list", "--porcelain", "-z"]);
        let result = add_worktree(AddWorktreeRequest {
            repository: repository.clone(),
            destination: destination.clone(),
            revision: OsString::from(commit.trim()),
            mode: WorktreeMode::Detached,
            state_dir: Some(state.clone()),
        });
        if distinct {
            result.expect("filesystem represents both Unicode names");
            assert_eq!(fs::read(destination.join(first_name)).unwrap(), b"first\n");
            assert_eq!(
                fs::read(destination.join(second_name)).unwrap(),
                b"second\n"
            );
            assert!(git(&destination, &["status", "--porcelain=v1"]).is_empty());
        } else {
            let error = result.expect_err("colliding Unicode names must fail preflight");
            assert!(error.to_string().contains("cannot coexist"), "{error}");
            assert!(!state.exists());
            assert!(!destination.exists());
            assert_eq!(
                git(&repository, &["worktree", "list", "--porcelain", "-z"]),
                before
            );
            assert!(fs::read_dir(fixture.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".riftri-path-probe-")
            }));
        }
    }
}

#[test]
fn validates_case_colliding_tree_paths_before_durable_mutation() {
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

    let upper_blob = git_with_input(&repository, &["hash-object", "-w", "--stdin"], b"upper\n");
    let lower_blob = git_with_input(&repository, &["hash-object", "-w", "--stdin"], b"lower\n");
    let tree_input = format!(
        "100644 blob {}\tCase.txt\0100644 blob {}\tcase.txt\0",
        upper_blob.trim(),
        lower_blob.trim()
    );
    let tree = git_with_input(&repository, &["mktree", "-z"], tree_input.as_bytes());
    let commit = git(
        &repository,
        &["commit-tree", tree.trim(), "-m", "case-collision fixture"],
    );
    let worktrees_before = git(&repository, &["worktree", "list", "--porcelain", "-z"]);

    let result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        revision: OsString::from(commit.trim()),
        mode: WorktreeMode::Detached,
        state_dir: Some(state.clone()),
    });

    if destination_is_case_sensitive(fixture.path()) {
        result.expect("case-sensitive destination supports distinct paths");
        assert_eq!(
            fs::read_to_string(destination.join("Case.txt")).expect("read upper-case path"),
            "upper\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("case.txt")).expect("read lower-case path"),
            "lower\n"
        );
        assert!(git(&destination, &["status", "--porcelain=v1"]).is_empty());
    } else {
        let error = result.expect_err("case-insensitive destination must reject colliding paths");
        assert!(
            error.to_string().contains("cannot coexist"),
            "unexpected error: {error}"
        );
        assert!(!destination.exists());
        assert!(!state.exists());
        assert!(
            fs::read_dir(fixture.path())
                .expect("read fixture after rejected path probe")
                .all(|entry| {
                    !entry
                        .expect("read fixture entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".riftri-path-probe-")
                }),
            "destination path-semantics probe was not cleaned up"
        );
        assert_eq!(
            git(&repository, &["worktree", "list", "--porcelain", "-z"]),
            worktrees_before,
        );
    }
}

#[test]
fn refuses_reuse_of_a_base_with_an_injected_ignored_file() {
    let fixture = tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").expect("tracked file");
    fs::write(repository.join(".gitignore"), "ignored.txt\n").expect("ignore rule");
    git(&repository, &["add", "."]);
    git(
        &repository,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
    );
    let state = fixture.path().join("state");
    let request = |name: &str| AddWorktreeRequest {
        repository: repository.clone(),
        destination: fixture.path().join(name),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::Detached,
        state_dir: Some(state.clone()),
    };
    let first = add_worktree(request("first")).expect("first view");
    let original = fs::metadata(&first.base_path)
        .expect("base metadata")
        .permissions();
    let mut writable = original.clone();
    #[cfg(unix)]
    writable.set_mode(original.mode() | 0o200);
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    // Windows clears an attribute, not Unix mode bits.
    {
        writable.set_readonly(false);
    }
    fs::set_permissions(&first.base_path, writable).expect("make fixture base writable");
    fs::write(first.base_path.join("ignored.txt"), "injected\n").expect("inject ignored file");
    fs::set_permissions(&first.base_path, original).expect("restore base permissions");
    let error = add_worktree(request("second")).expect_err("corrupt base must not be reused");
    assert!(error.to_string().contains("integrity"), "{error}");
    assert!(!fixture.path().join("second").exists());
    assert!(!first.destination.join("ignored.txt").exists());
    assert!(git(&first.destination, &["status", "--porcelain=v1"]).is_empty());
    assert_eq!(
        fs::read(first.base_path.join("ignored.txt")).expect("preserve evidence"),
        b"injected\n"
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

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_state_layout_directory_without_writing_through_it() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let outside_bases = fixture.path().join("outside-bases");
    let destination = fixture.path().join("worktree");
    fs::create_dir(&repository).expect("create repository");
    fs::create_dir(&state).expect("create state directory");
    fs::create_dir(&outside_bases).expect("create outside base directory");
    symlink(&outside_bases, state.join("bases")).expect("symlink bases directory");
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

    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/symlinked-state-layout")),
        state_dir: Some(state.clone()),
    })
    .expect_err("symlinked state layout must be rejected");

    assert!(error.to_string().contains("real directory"));
    assert!(error.to_string().contains("bases"));
    assert!(!destination.exists());
    assert!(
        fs::read_dir(&outside_bases)
            .expect("read outside base directory")
            .next()
            .is_none(),
        "Riftri wrote through the state-layout symlink"
    );
    assert!(
        !git(
            &repository,
            &["branch", "--list", "feature/symlinked-state-layout"]
        )
        .contains("feature/symlinked-state-layout")
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_repository_base_bucket_without_writing_through_it() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    let outside = fixture.path().join("outside");
    fs::create_dir(&repository).expect("create repository");
    fs::create_dir(&outside).expect("create outside directory");
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
        destination: first,
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/base-bucket-first")),
        state_dir: Some(state.clone()),
    })
    .expect("create initial managed worktree");
    let bucket = added.base_path.parent().expect("repository base bucket");
    let original_bucket = bucket.with_extension("original");
    fs::rename(bucket, &original_bucket).expect("move original base bucket");
    symlink(&outside, bucket).expect("symlink repository base bucket");

    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: second.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/base-bucket-second")),
        state_dir: Some(state),
    })
    .expect_err("symlinked repository base bucket must be rejected");

    assert!(error.to_string().contains("real directory"));
    assert!(!second.exists());
    assert!(
        fs::read_dir(&outside)
            .expect("read outside directory")
            .next()
            .is_none(),
        "Riftri wrote through the repository base-bucket symlink"
    );
    assert!(
        !git(
            &repository,
            &["branch", "--list", "feature/base-bucket-second"]
        )
        .contains("feature/base-bucket-second")
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_lifecycle_lock_before_mutation() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let destination = fixture.path().join("worktree");
    let protected = fixture.path().join("protected-lock-target");
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
    fs::write(&protected, "protected\n").expect("write protected lock target");
    symlink(
        &protected,
        repository.join(".git/riftri-state-directory.lock"),
    )
    .expect("symlink lifecycle lock");

    let error = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("feature/symlinked-lock")),
        state_dir: Some(state),
    })
    .expect_err("symlinked lifecycle lock must be rejected");

    assert!(error.to_string().contains("lock"));
    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(protected).expect("read protected lock target"),
        "protected\n"
    );
    assert!(
        !git(&repository, &["branch", "--list", "feature/symlinked-lock"])
            .contains("feature/symlinked-lock")
    );
}

#[test]
#[ignore = "physical allocation benchmark; run explicitly on an otherwise quiet native COW volume"]
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
        "cached view logical bytes: {LOGICAL_BYTES}; measured native COW volume growth: {physical_growth}"
    );
    assert!(
        physical_growth < (LOGICAL_BYTES as u64 / 4),
        "cached {LOGICAL_BYTES}-byte view consumed {physical_growth} physical bytes"
    );
}
