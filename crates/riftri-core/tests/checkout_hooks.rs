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

#[test]
fn checkout_hooks_are_not_silently_skipped_by_optimized_creation() {
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
        fs::write(&hook, "#!/bin/sh\nprintf 'configured\\n' > hook-ran\n").unwrap();
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
        let error = add_worktree(AddWorktreeRequest {
            repository: repository.clone(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: WorktreeMode::NewBranch(OsString::from("hook-test")),
            state_dir: Some(state.clone()),
        })
        .expect_err("must not silently omit checkout setup");
        assert!(error.to_string().contains("post-checkout"), "{error}");
        assert!(!destination.exists());
        assert!(!state.exists());
        assert!(
            !Command::new("git")
                .current_dir(&repository)
                .args(["show-ref", "--verify", "--quiet", "refs/heads/hook-test"])
                .status()
                .unwrap()
                .success()
        );
        let report = doctor(&repository).repository_compatibility.value.unwrap();
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.explanation.contains("post-checkout"))
        );
        #[cfg(unix)]
        if !custom {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                doctor(&repository)
                    .repository_compatibility
                    .value
                    .unwrap()
                    .compatible,
                "Git ignores a non-executable default hook"
            );
        }
    }
}
