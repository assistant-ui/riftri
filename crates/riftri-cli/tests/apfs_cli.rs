#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn command(program: &Path, path: &Path, arguments: &[&str]) -> Output {
    Command::new(program)
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run fixture command")
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    command(Path::new("git"), path, arguments)
}

#[test]
fn documented_command_creates_a_clean_real_worktree() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("worktree");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/cli", "HEAD", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run Riftri CLI");

    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created APFS-backed Git worktree"));
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(
        String::from_utf8_lossy(&git(&repository, &["worktree", "list", "--porcelain"]).stdout)
            .contains("refs/heads/feature/cli")
    );

    fs::write(destination.join("tracked.txt"), "private allocation\n").expect("edit view");
    fs::write(destination.join("tracked.txt"), "tracked\n").expect("restore view");
    let compact = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "compact"])
        .arg(&destination)
        .args(["--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("run Riftri compaction CLI");
    assert!(
        compact.status.success(),
        "riftri compact failed: {}",
        String::from_utf8_lossy(&compact.stderr)
    );
    assert!(
        String::from_utf8_lossy(&compact.stdout).contains("Compacted Riftri-backed Git worktree")
    );
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn worktree_add_uses_the_revision_resolved_before_mutation() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("worktree");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "first\n").expect("write first revision");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "first"])
            .status
            .success()
    );
    let first = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("first commit is UTF-8")
        .trim()
        .to_owned();
    fs::write(repository.join("tracked.txt"), "second\n").expect("write second revision");
    assert!(
        git(&repository, &["commit", "-am", "second", "--quiet"])
            .status
            .success()
    );
    let second = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("second commit is UTF-8")
        .trim()
        .to_owned();
    assert!(
        git(&repository, &["branch", "moving", &first])
            .status
            .success()
    );

    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate real Git")
            .stdout,
    )
    .expect("Git path is UTF-8")
    .trim()
    .to_owned();
    let wrapper = fixture.path().join("moving-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = add ]; then\n  \"$RIFTRI_TEST_REAL_GIT\" update-ref refs/heads/moving \"$RIFTRI_TEST_MOVED_COMMIT\" || exit $?\nfi\nexec \"$RIFTRI_TEST_REAL_GIT\" \"$@\"\n",
    )
    .expect("write Git wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/pinned", "moving", "--state-dir"])
        .arg(&state)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_MOVED_COMMIT", &second)
        .current_dir(&repository)
        .output()
        .expect("run Riftri CLI through moving-ref wrapper");

    assert!(
        output.status.success(),
        "riftri failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(git(&destination, &["rev-parse", "HEAD"]).stdout)
            .expect("worktree commit is UTF-8")
            .trim(),
        first
    );
    assert_eq!(
        fs::read_to_string(destination.join("tracked.txt")).expect("read worktree file"),
        "first\n"
    );
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn existing_branch_move_fails_and_rolls_back_without_deleting_the_branch() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("worktree");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "first\n").expect("write first revision");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "first"])
            .status
            .success()
    );
    let first = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("first commit is UTF-8")
        .trim()
        .to_owned();
    assert!(
        git(&repository, &["branch", "moving", &first])
            .status
            .success()
    );
    fs::write(repository.join("tracked.txt"), "second\n").expect("write second revision");
    assert!(
        git(&repository, &["commit", "-am", "second", "--quiet"])
            .status
            .success()
    );
    let second = String::from_utf8(git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("second commit is UTF-8")
        .trim()
        .to_owned();

    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate real Git")
            .stdout,
    )
    .expect("Git path is UTF-8")
    .trim()
    .to_owned();
    let wrapper = fixture.path().join("moving-existing-git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = add ]; then\n  \"$RIFTRI_TEST_REAL_GIT\" update-ref refs/heads/moving \"$RIFTRI_TEST_MOVED_COMMIT\" || exit $?\nfi\nexec \"$RIFTRI_TEST_REAL_GIT\" \"$@\"\n",
    )
    .expect("write Git wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["moving", "--state-dir"])
        .arg(&state)
        .env("RIFTRI_SHIM_ACTIVE", "1")
        .env("RIFTRI_REAL_GIT", &wrapper)
        .env("RIFTRI_TEST_REAL_GIT", real_git)
        .env("RIFTRI_TEST_MOVED_COMMIT", &second)
        .current_dir(&repository)
        .output()
        .expect("run Riftri CLI through moving-ref wrapper");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("existing branch moved"));
    assert!(!destination.exists());
    assert_eq!(
        String::from_utf8(git(&repository, &["rev-parse", "moving"]).stdout)
            .expect("branch target is UTF-8")
            .trim(),
        second
    );
}

#[test]
fn worktree_list_reports_only_managed_views_in_human_and_json_output() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let managed = fixture.path().join("managed");
    let unmanaged = fixture.path().join("unmanaged");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );

    let created = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&managed)
        .args(["-b", "feature/managed", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("create managed worktree");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/unmanaged",
                unmanaged.to_str().expect("UTF-8 unmanaged path"),
            ],
        )
        .status
        .success()
    );
    let managed = managed.canonicalize().expect("canonical managed path");

    let human = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "list", "--state-dir"])
        .arg(&state)
        .current_dir(&repository)
        .output()
        .expect("list managed worktrees");
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("human inventory is UTF-8");
    assert!(human.contains("Managed worktrees: 1"));
    assert!(human.contains(&managed.to_string_lossy().to_string()));
    assert!(!human.contains(&unmanaged.to_string_lossy().to_string()));

    let json = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "list", "--state-dir"])
        .arg(&state)
        .arg("--json")
        .current_dir(&repository)
        .output()
        .expect("list managed worktrees as JSON");
    assert!(json.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("parse worktree inventory JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));
    let worktrees = report["worktrees"].as_array().expect("worktree list");
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0]["path"], managed.to_string_lossy().as_ref());
    assert_eq!(
        worktrees[0]["repository"],
        repository
            .canonicalize()
            .expect("canonical repository")
            .to_string_lossy()
            .as_ref()
    );
    assert!(worktrees[0]["head"].is_string());
    assert_eq!(worktrees[0]["branch"], "refs/heads/feature/managed");
    assert_eq!(worktrees[0]["detached"], false);
    assert_eq!(worktrees[0]["locked_reason"], serde_json::Value::Null);
    assert_eq!(worktrees[0]["prunable_reason"], serde_json::Value::Null);
    assert_eq!(worktrees[0]["backend"], "apfs-clone");
    assert!(worktrees[0]["base_path"].is_string());
    assert!(worktrees[0]["logical_bytes"].is_u64());
    assert!(worktrees[0]["allocated_bytes"].is_u64());
}

#[test]
fn lifecycle_commands_emit_stable_json_reports() {
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let destination = fixture.path().join("worktree");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(&repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    assert!(
        git(&repository, &["add", "--", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        git(&repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );

    let riftri = |arguments: &[&str]| -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
        command
            .args(arguments)
            .args(["--state-dir"])
            .arg(&state)
            .arg("--json")
            .current_dir(&repository);
        command.output().expect("run Riftri CLI")
    };
    let parse = |output: &Output, operation: &str| -> serde_json::Value {
        assert!(
            output.status.success(),
            "riftri {operation} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("parse {operation} JSON: {error}"))
    };

    let destination_argument = destination.to_str().expect("UTF-8 destination");
    let added = parse(
        &riftri(&[
            "worktree",
            "add",
            destination_argument,
            "-b",
            "feature/json",
        ]),
        "worktree add",
    );
    assert_eq!(added["schema_version"], 1);
    assert_eq!(added["backend"], "apfs-clone");
    assert!(added["commit"].is_string());
    assert!(added["tree"].is_string());
    assert_eq!(added["reused_base"], false);
    assert!(added["base_path"].is_string());
    assert!(added["journal_path"].is_string());

    let status = parse(&riftri(&["status", "."]), "status");
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["operations"]["active_views"], 1);
    assert_eq!(status["bases"].as_array().expect("bases").len(), 1);
    assert_eq!(status["bases"][0]["reference_count"], 1);
    assert_eq!(status["bases"][0]["in_use"], true);
    assert_eq!(status["worktrees"].as_array().expect("views").len(), 1);
    assert_eq!(status["worktrees"][0]["backend"], "apfs-clone");
    assert!(status["total_logical_bytes"].is_u64());

    let compacted = parse(
        &riftri(&["worktree", "compact", destination_argument]),
        "worktree compact",
    );
    assert_eq!(compacted["schema_version"], 1);
    assert_eq!(compacted["destination"], added["destination"]);
    assert!(compacted["old_base_path"].is_string());
    assert!(compacted["base_path"].is_string());

    let repaired = parse(&riftri(&["repair", "."]), "repair");
    assert_eq!(repaired["schema_version"], 1);
    assert_eq!(repaired["errors"], serde_json::json!([]));
    assert_eq!(repaired["active"], 1);

    let removed = parse(
        &riftri(&["worktree", "remove", destination_argument]),
        "worktree remove",
    );
    assert_eq!(removed["schema_version"], 1);
    assert_eq!(removed["forced"], false);
    assert!(removed["base_path"].is_string());

    let pruned = parse(&riftri(&["worktree", "prune"]), "worktree prune");
    assert_eq!(pruned["schema_version"], 1);
    assert!(pruned["journal_path"].is_string());

    let plan = parse(&riftri(&["gc", "."]), "gc plan");
    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["applied"], false);
    assert!(
        !plan["candidates"]
            .as_array()
            .expect("candidates")
            .is_empty()
    );
    assert_eq!(plan["collected"], serde_json::json!([]));

    let collected = parse(&riftri(&["gc", ".", "--apply"]), "gc apply");
    assert_eq!(collected["applied"], true);
    assert!(
        !collected["collected"]
            .as_array()
            .expect("collected")
            .is_empty()
    );
    assert!(collected["removed_logical_bytes"].is_u64());
}

#[test]
fn all_states_inventory_discovers_worktrees_across_registered_states() {
    let fixture = tempdir().expect("fixture directory");
    let shared_state = fixture.path().join("shared-state");
    let mut repositories = Vec::new();
    for (name, branch) in [("first", "feature/first"), ("second", "feature/second")] {
        let repository = fixture.path().join(name);
        fs::create_dir(&repository).expect("create repository");
        for arguments in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Riftri Tests"][..],
            &["config", "user.email", "riftri@example.invalid"][..],
            &["config", "core.autocrlf", "false"][..],
        ] {
            assert!(git(&repository, arguments).status.success());
        }
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
        assert!(
            git(&repository, &["add", "--", "tracked.txt"])
                .status
                .success()
        );
        assert!(
            git(&repository, &["commit", "--quiet", "-m", "initial"])
                .status
                .success()
        );

        let destination = fixture.path().join(format!("{name}-worktree"));
        let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["worktree", "add"])
            .arg(&destination)
            .args(["-b", branch, "HEAD", "--state-dir"])
            .arg(&shared_state)
            .current_dir(&repository)
            .output()
            .expect("run Riftri CLI");
        assert!(
            added.status.success(),
            "riftri worktree add failed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        repositories.push((repository, destination));
    }
    let (first_repository, first_worktree) = &repositories[0];
    let (_, second_worktree) = &repositories[1];

    // The documented default scope stays unchanged: it still reads only the
    // default state location and therefore reports no managed worktrees.
    let default_scope = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "list", "--json"])
        .current_dir(first_repository)
        .output()
        .expect("list default scope");
    assert!(default_scope.status.success());
    let default_report: serde_json::Value =
        serde_json::from_slice(&default_scope.stdout).expect("parse default inventory JSON");
    assert_eq!(default_report["schema_version"], 1);
    assert_eq!(default_report["worktrees"], serde_json::json!([]));

    // The all-states scope discovers the registered shared state directory and
    // filters out the other repository's worktree.
    let all_states = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "list", "--all-states", "--json"])
        .current_dir(first_repository)
        .output()
        .expect("list all registered states");
    assert!(
        all_states.status.success(),
        "riftri worktree list --all-states failed: {}",
        String::from_utf8_lossy(&all_states.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&all_states.stdout).expect("parse all-states inventory JSON");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["scope"], "all-registered-states");
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));

    let canonical_state = shared_state
        .canonicalize()
        .expect("canonical shared state")
        .to_string_lossy()
        .into_owned();
    let states = report["state_directories"]
        .as_array()
        .expect("state_directories array");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["path"], canonical_state);
    assert_eq!(states[0]["source"], "registered");

    let worktrees = report["worktrees"].as_array().expect("worktree list");
    assert_eq!(
        worktrees.len(),
        1,
        "shared-state inventory must only list the queried repository's worktree"
    );
    assert_eq!(
        worktrees[0]["path"],
        first_worktree
            .canonicalize()
            .expect("canonical managed path")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(worktrees[0]["state_directory"], canonical_state);
    assert_eq!(worktrees[0]["branch"], "refs/heads/feature/first");
    assert!(
        !String::from_utf8_lossy(&all_states.stdout)
            .contains(&second_worktree.to_string_lossy().into_owned()),
        "the other repository's worktree must be filtered out"
    );
}
