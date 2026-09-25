use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use riftri_core::{AddWorktreeRequest, WorktreeMode, add_worktree, doctor};

fn git(repository: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// Riftri clones a base instead of checking out, so Git never fires
/// `post-checkout` for it. Riftri runs the hook itself afterwards, with the
/// arguments and working directory Git uses, so an optimized worktree matches
/// what `git worktree add` produces. Before this, any repository with such a
/// hook was refused outright, which excluded every repository using a hook
/// manager: husky sets `core.hooksPath` and generates a `post-checkout` entry
/// whether or not the project defines one.
#[test]
fn optimized_creation_runs_the_post_checkout_hook_git_would_run() {
    for configuration in ["default", "relative", "absolute", "empty-custom", "linked"] {
        let custom = !matches!(configuration, "default" | "linked");
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "base\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let hooks = if custom {
            repository.join("custom-hooks")
        } else {
            repository.join(".git/hooks")
        };
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        // Record the arguments and working directory Git's contract specifies.
        fs::write(
            &hook,
            "#!/bin/sh\nprintf '%s %s %s\\n' \"$1\" \"$2\" \"$3\" > \"$(pwd)/hook-ran\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        if custom {
            let path = if configuration == "absolute" {
                hooks.as_path()
            } else {
                Path::new("custom-hooks")
            };
            let output = Command::new("git")
                .current_dir(&repository)
                .args(["config", "core.hooksPath"])
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        if configuration == "empty-custom" {
            fs::remove_file(&hook).unwrap();
        }
        let repository = if configuration == "linked" {
            let linked = fixture.path().join("caller");
            let output = Command::new("git")
                .current_dir(&repository)
                .args(["worktree", "add", "--no-checkout", "--detach"])
                .arg(&linked)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            linked
        } else {
            repository
        };

        let destination = fixture.path().join("view");
        let state = fixture.path().join("state");
        let result = add_worktree(AddWorktreeRequest {
            repository: repository.clone(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::NewBranch(OsString::from("hook-test")),
            state_dir: Some(state.clone()),
            sparse_directories: Vec::new(),
        })
        .unwrap_or_else(|error| panic!("{configuration}: {error}"));
        assert!(destination.exists(), "{configuration}");

        // A hook is no longer a compatibility blocker.
        let report = doctor(&repository).repository_compatibility.value.unwrap();
        assert!(
            !report
                .blockers
                .iter()
                .any(|blocker| blocker.explanation.contains("hook")),
            "{configuration}: {:?}",
            report.blockers
        );

        let marker = destination.join("hook-ran");
        if configuration == "empty-custom" {
            // No hook exists, so nothing runs and nothing is reported.
            assert!(!marker.exists(), "{configuration}");
            assert!(result.post_checkout.is_none(), "{configuration}");
            continue;
        }

        // Git's contract: null object id, the new HEAD, and 1 for a branch
        // checkout, run from inside the new worktree.
        let recorded = fs::read_to_string(&marker)
            .unwrap_or_else(|error| panic!("{configuration}: hook did not run: {error}"));
        let fields = recorded.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 3, "{configuration}: {recorded:?}");
        assert!(
            fields[0].bytes().all(|byte| byte == b'0'),
            "{configuration}: old HEAD must be the null object id, got {}",
            fields[0]
        );
        assert_eq!(fields[1], result.commit.as_str(), "{configuration}");
        assert_eq!(fields[2], "1", "{configuration}");

        let outcome = result.post_checkout.expect(configuration);
        assert!(outcome.succeeded(), "{configuration}: {outcome:?}");
    }
}

/// A hook that fails is reported, not rolled back. `git worktree add` exits
/// non-zero and leaves the worktree in place; Riftri matches that rather than
/// discarding a worktree Git would have kept.
#[test]
fn a_failing_post_checkout_hook_is_reported_without_discarding_the_worktree() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    fs::write(repository.join("tracked.txt"), "base\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    let hook = repository.join(".git/hooks/post-checkout");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\nexit 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let destination = fixture.path().join("view");
    let result = add_worktree(AddWorktreeRequest {
        repository,
        destination: destination.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("hook-fail")),
        state_dir: Some(fixture.path().join("state")),
        sparse_directories: Vec::new(),
    })
    .expect("a failing hook must not fail the creation itself");

    assert!(destination.join("tracked.txt").exists());
    let outcome = result.post_checkout.expect("hook outcome is reported");
    assert!(outcome.started);
    assert_eq!(outcome.exit_code, Some(3));
    assert!(!outcome.succeeded());
}
