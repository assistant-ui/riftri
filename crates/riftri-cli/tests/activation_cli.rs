use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

struct RepositoryFixture {
    directory: TempDir,
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
        Self {
            directory,
            repository,
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

fn riftri(path: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Riftri CLI")
}

#[test]
fn enable_and_disable_change_only_repository_local_config() {
    let fixture = RepositoryFixture::new();

    let enabled = riftri(&fixture.repository, &["enable"]);
    assert!(
        enabled.status.success(),
        "enable failed: {}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        git(
            &fixture.repository,
            &["config", "--local", "--get", "riftri.enabled"]
        )
        .stdout,
        b"true\n"
    );
    let doctor = riftri(&fixture.repository, &["doctor"]);
    assert!(doctor.status.success());
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("Repository enabled: yes"));

    let disabled = riftri(&fixture.repository, &["disable"]);
    assert!(
        disabled.status.success(),
        "disable failed: {}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert_eq!(
        git(
            &fixture.repository,
            &["config", "--local", "--get", "riftri.enabled"]
        )
        .status
        .code(),
        Some(1)
    );
}

#[test]
fn doctor_reports_checkout_compatibility_before_mutation() {
    let fixture = RepositoryFixture::new();

    let doctor = riftri(&fixture.repository, &["doctor"]);
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(
        String::from_utf8_lossy(&doctor.stdout)
            .contains("Repository checkout compatibility (HEAD): supported")
    );
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[test]
fn doctor_accepts_deterministic_in_tree_attributes() {
    let fixture = RepositoryFixture::new();
    fs::write(
        fixture.repository.join(".gitattributes"),
        "* text=auto\n*.txt text eol=lf\n*.bin binary\n",
    )
    .expect("write deterministic attributes");
    fs::write(fixture.repository.join("payload.bin"), b"binary\0payload\n")
        .expect("write binary fixture");
    assert!(
        git(
            &fixture.repository,
            &["add", "--", ".gitattributes", "payload.bin"]
        )
        .status
        .success()
    );
    assert!(
        git(
            &fixture.repository,
            &["commit", "--quiet", "-m", "add deterministic attributes"]
        )
        .status
        .success()
    );

    let doctor = riftri(&fixture.repository, &["doctor", "--json"]);
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
    let compatibility = &report["repository_compatibility"]["value"];
    assert_eq!(compatibility["compatible"], true);
    assert_eq!(compatibility["blockers"], serde_json::json!([]));
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[test]
fn doctor_json_lists_every_detected_checkout_blocker() {
    let fixture = RepositoryFixture::new();
    fs::write(
        fixture.repository.join(".gitattributes"),
        "*.txt filter=lfs\n",
    )
    .expect("write attributes");
    fs::write(
        fixture.repository.join(".gitmodules"),
        "[submodule \"dependency\"]\n\tpath = dependency\n\turl = ../dependency\n",
    )
    .expect("write submodule metadata");
    assert!(
        git(
            &fixture.repository,
            &["add", "--", ".gitattributes", ".gitmodules"]
        )
        .status
        .success()
    );
    assert!(
        git(
            &fixture.repository,
            &["commit", "--quiet", "-m", "add checkout inputs"]
        )
        .status
        .success()
    );
    assert!(
        git(
            &fixture.repository,
            &["config", "core.sparseCheckout", "true"]
        )
        .status
        .success()
    );

    let doctor = riftri(&fixture.repository, &["doctor", "--json"]);
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
    let compatibility = &report["repository_compatibility"]["value"];
    assert_eq!(compatibility["compatible"], false);
    let blocker_kinds = compatibility["blockers"]
        .as_array()
        .expect("compatibility blockers")
        .iter()
        .map(|blocker| blocker["kind"].as_str().expect("blocker kind"))
        .collect::<Vec<_>>();
    assert!(blocker_kinds.contains(&"in-tree-attributes"));
    assert!(blocker_kinds.contains(&"submodules"));
    assert!(blocker_kinds.contains(&"sparse-checkout"));
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[test]
fn status_and_repair_explain_an_empty_lifecycle() {
    let fixture = RepositoryFixture::new();

    let status = riftri(&fixture.repository, &["status"]);
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_output = String::from_utf8_lossy(&status.stdout);
    assert!(status_output.contains("Active views: 0"));
    assert!(status_output.contains("Pending adds: 0"));
    assert!(status_output.contains("Pending removals: 0"));
    assert!(status_output.contains("Retained bases: 0"));
    assert!(status_output.contains("State issues: 0"));
    assert!(status_output.contains("Total filesystem-accounted allocated: 0 bytes"));
    assert!(status_output.contains(
        "may count shared COW blocks more than once; it is not exclusive physical disk use"
    ));

    #[cfg(target_os = "macos")]
    {
        let gc = riftri(&fixture.repository, &["gc"]);
        assert!(
            gc.status.success(),
            "gc plan failed: {}",
            String::from_utf8_lossy(&gc.stderr)
        );
        let gc_output = String::from_utf8_lossy(&gc.stdout);
        assert!(gc_output.contains("Removed filesystem-accounted allocated bytes: 0"));
        assert!(gc_output.contains("Physical-sharing proof: use the platform volume-delta"));
    }

    let repair = riftri(&fixture.repository, &["repair"]);
    assert!(
        repair.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    let repair_output = String::from_utf8_lossy(&repair.stdout);
    assert!(repair_output.contains("Scanned operations: 0"));
    assert!(repair_output.contains("No journaled operation needs manual attention"));
}

#[test]
fn exec_delegates_normal_git_to_the_real_executable() {
    let fixture = RepositoryFixture::new();
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let direct = git(&fixture.repository, &["rev-parse", "--show-toplevel"]);
    let output = riftri(
        &fixture.repository,
        &["exec", "--", "git", "rev-parse", "--show-toplevel"],
    );
    assert!(
        output.status.success(),
        "scoped Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, direct.stdout);
    assert_eq!(output.stderr, direct.stderr);

    let direct_failure = git(
        &fixture.repository,
        &["rev-parse", "--verify", "refs/heads/does-not-exist"],
    );
    let scoped_failure = riftri(
        &fixture.repository,
        &[
            "exec",
            "--",
            "git",
            "rev-parse",
            "--verify",
            "refs/heads/does-not-exist",
        ],
    );
    assert_eq!(scoped_failure.status.code(), direct_failure.status.code());
    assert_eq!(scoped_failure.stdout, direct_failure.stdout);
    assert_eq!(scoped_failure.stderr, direct_failure.stderr);
}

#[test]
fn exec_binds_any_command_to_an_exact_git_worktree() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("bound-view");
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/bound-command",
                destination.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--worktree"])
        .arg(&destination)
        .args(["--", "git", "rev-parse", "--show-toplevel"])
        .current_dir(fixture.directory.path())
        .output()
        .expect("run a generic command in a bound worktree");

    assert!(
        output.status.success(),
        "worktree-bound command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert_eq!(
        reported.canonicalize().expect("canonical reported root"),
        destination
            .canonicalize()
            .expect("canonical bound worktree")
    );
}

#[test]
fn exec_rejects_a_binding_below_the_worktree_root() {
    let fixture = RepositoryFixture::new();
    let nested = fixture.repository.join("nested");
    fs::create_dir(&nested).expect("create nested directory");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--worktree"])
        .arg(&nested)
        .args(["--", "git", "rev-parse", "--show-toplevel"])
        .current_dir(fixture.directory.path())
        .output()
        .expect("reject an inexact worktree binding");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--worktree must name the exact Git worktree root")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn bound_exec_keeps_optimized_git_interception_active() {
    let fixture = RepositoryFixture::new();
    let bound = fixture.directory.path().join("bound-agent");
    let optimized = fixture.directory.path().join("bound-created-view");
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/bound-agent",
                bound.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--worktree"])
        .arg(&bound)
        .args(["--", "git", "worktree", "add", "-b", "feature/from-bound"])
        .arg(&optimized)
        .arg("HEAD")
        .current_dir(fixture.directory.path())
        .output()
        .expect("create optimized worktree from a bound process");

    assert!(
        output.status.success(),
        "bound optimized add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("optimized APFS worktree"));
    assert!(
        git(&optimized, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(fixture.repository.join(".git/riftri/operations").is_dir());
}

#[test]
fn exec_leaves_worktree_add_untouched_for_a_disabled_repository() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("ordinary-view");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/ordinary",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run ordinary Git worktree add");

    assert!(
        output.status.success(),
        "ordinary add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.repository.join(".git/riftri").exists());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn shell_hook_places_a_durable_riftri_git_shim_first_on_path() {
    let cache = tempdir().expect("shell hook cache");
    let cache_root = cache.path().join("cache with ' quote");
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["shell", "hook", "sh"])
        .env("RIFTRI_CACHE_DIR", &cache_root)
        .output()
        .expect("render shell hook");

    assert!(
        output.status.success(),
        "shell hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let hook = String::from_utf8(output.stdout).expect("UTF-8 shell hook");
    let repeated = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["shell", "hook", "sh"])
        .env("RIFTRI_CACHE_DIR", &cache_root)
        .output()
        .expect("render shell hook again");
    assert!(
        repeated.status.success(),
        "repeated shell hook failed: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let shell = Command::new("sh")
        .args(["-c", &format!("{hook}\ncommand -v git")])
        .output()
        .expect("evaluate shell hook");
    assert!(
        shell.status.success(),
        "shell hook evaluation failed: {}",
        String::from_utf8_lossy(&shell.stderr)
    );
    let shim = cache_root.join("shims/v1/git");
    assert_eq!(
        PathBuf::from(String::from_utf8(shell.stdout).unwrap().trim()),
        shim
    );
    assert!(
        fs::symlink_metadata(shim)
            .expect("installed Git shim")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(cache_root.join("shims/v1"))
            .expect("private shim directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn shell_status_explains_global_scope_and_deactivation_restores_git() {
    let fixture = RepositoryFixture::new();
    let cache = tempdir().expect("shell hook cache");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let binary = env!("CARGO_BIN_EXE_riftri");
    let output = Command::new("sh")
        .args([
            "-c",
            "eval \"$(\"$RIFTRI_TEST_BIN\" shell hook sh)\"\n\
             \"$RIFTRI_TEST_BIN\" shell status \"$RIFTRI_TEST_REPOSITORY\"\n\
             PATH=$PATH:$RIFTRI_CACHE_DIR/shims/v1; export PATH\n\
             eval \"$(\"$RIFTRI_TEST_BIN\" shell deactivate sh)\"\n\
             case :$PATH: in *:$RIFTRI_CACHE_DIR/shims/v1:*) exit 43 ;; esac\n\
             printf 'marker=%s\\n' \"${RIFTRI_SHIM_ACTIVE-unset}\"\n\
             printf 'real_git=%s\\n' \"${RIFTRI_REAL_GIT-unset}\"\n\
             command -v git",
        ])
        .env("RIFTRI_TEST_BIN", binary)
        .env("RIFTRI_TEST_REPOSITORY", &fixture.repository)
        .env("RIFTRI_CACHE_DIR", cache.path())
        .output()
        .expect("activate, inspect, and deactivate shell hook");

    assert!(
        output.status.success(),
        "shell lifecycle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 shell output");
    assert!(stdout.contains("Shell interception: active"));
    assert!(stdout.contains("every new shell too only if you added the hook to your profile"));
    assert!(stdout.contains("Repository optimization: enabled"));
    assert!(stdout.contains("Effective optimized interception: active"));
    assert!(stdout.contains("marker=unset"));
    assert!(stdout.contains("real_git=unset"));
    assert!(!stdout.lines().last().expect("git path").contains("riftri"));
}

#[cfg(unix)]
#[test]
fn shell_hook_leaves_disabled_repository_adds_with_real_git() {
    let fixture = RepositoryFixture::new();
    let cache = tempdir().expect("shell hook cache");
    let destination = fixture.directory.path().join("ordinary-shell-view");
    let output = Command::new("sh")
        .args([
            "-c",
            "eval \"$(\"$RIFTRI_TEST_BIN\" shell hook sh)\"\n\
             git worktree add -b feature/ordinary-shell \"$RIFTRI_TEST_DESTINATION\" HEAD",
        ])
        .current_dir(&fixture.repository)
        .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
        .env("RIFTRI_TEST_DESTINATION", &destination)
        .env("RIFTRI_CACHE_DIR", cache.path())
        .output()
        .expect("run normal Git through shell hook");

    assert!(
        output.status.success(),
        "ordinary add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.repository.join(".git/riftri").exists());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn enabled_unsupported_add_fails_without_falling_back() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("unsupported-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "add"])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run unsupported enabled add");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires `-b <branch>`"));
    assert!(!destination.exists());
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn exec_routes_enabled_git_worktree_add_through_apfs() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("optimized-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/enabled",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("run enabled Git worktree add");

    assert!(
        output.status.success(),
        "enabled add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("optimized APFS worktree"));
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(fixture.repository.join(".git/riftri/operations").is_dir());
}

#[cfg(target_os = "macos")]
#[test]
fn exec_routes_clean_managed_git_worktree_removal_through_riftri() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("removed-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/removed",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add enabled worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let removed = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("remove enabled worktree");

    assert!(
        removed.status.success(),
        "enabled removal failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(String::from_utf8_lossy(&removed.stderr).contains("safely removed worktree"));
    assert!(!destination.exists());
    assert!(fixture.repository.join(".git/riftri/removals").is_dir());

    let status = riftri(&fixture.repository, &["status"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(status.contains("Active views: 0"));
    assert!(status.contains("refs=0"));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_forced_removal_of_a_managed_view_fails_closed() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("guarded-remove-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/guarded-remove",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove", "--force"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("guard forced managed removal");

    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
    assert!(destination.is_dir());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_forced_removal_of_a_missing_managed_view_fails_closed() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("missing-guarded-remove-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/missing-guarded-remove",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    fs::remove_dir_all(&destination).expect("simulate a missing managed worktree view");

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove", "--force"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("guard forced removal of missing managed worktree");

    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
    let inventory = git(&fixture.repository, &["worktree", "list", "--porcelain"]);
    assert!(inventory.status.success());
    assert!(
        String::from_utf8_lossy(&inventory.stdout).contains(destination.to_string_lossy().as_ref()),
        "managed worktree registration was removed outside the Riftri journal"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_git_directory_options_cannot_bypass_managed_removal_guard() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("global-option-guarded-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/global-option-guard",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "--git-dir=.git",
            "--work-tree=.",
            "worktree",
            "remove",
            "--force",
        ])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("guard globally configured managed removal");

    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
    assert!(destination.is_dir());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_pathspec_options_cannot_bypass_managed_removal_guard() {
    for (index, option) in [
        "--literal-pathspecs",
        "--glob-pathspecs",
        "--noglob-pathspecs",
        "--icase-pathspecs",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = RepositoryFixture::new();
        let destination = fixture
            .directory
            .path()
            .join(format!("pathspec-option-guarded-view-{index}"));
        assert!(riftri(&fixture.repository, &["enable"]).status.success());
        let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args([
                "exec",
                "--",
                "git",
                "worktree",
                "add",
                "-b",
                &format!("feature/pathspec-option-guard-{index}"),
            ])
            .arg(&destination)
            .arg("HEAD")
            .current_dir(&fixture.repository)
            .output()
            .expect("add managed worktree");
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );

        let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git", option, "worktree", "remove", "--force"])
            .arg(&destination)
            .current_dir(&fixture.repository)
            .output()
            .expect("guard pathspec-configured managed removal");

        assert!(
            !removal.status.success(),
            "{option} bypassed the managed removal guard"
        );
        assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
        assert!(destination.is_dir());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn linked_worktree_git_directory_cannot_bypass_managed_removal_guard() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("linked-git-dir-guarded-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/linked-git-dir-guard",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let git_directory = git(
        &destination,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    );
    assert!(git_directory.status.success());
    let git_directory = PathBuf::from(
        String::from_utf8(git_directory.stdout)
            .expect("UTF-8 fixture Git directory")
            .trim_end(),
    );

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "--git-dir"])
        .arg(&git_directory)
        .args(["worktree", "remove", "--force"])
        .arg(&destination)
        .current_dir(fixture.directory.path())
        .output()
        .expect("guard linked Git directory removal");

    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
    assert!(destination.is_dir());
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_move_of_a_managed_view_is_journaled() {
    let fixture = RepositoryFixture::new();
    let source = fixture.directory.path().join("guarded-move-source");
    let destination = fixture.directory.path().join("guarded-move-destination");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/guarded-move",
        ])
        .arg(&source)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    fs::write(source.join("private.txt"), "private move data\n")
        .expect("write private worktree data");

    let moved = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "move"])
        .arg(&source)
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("move managed worktree");

    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(String::from_utf8_lossy(&moved.stderr).contains("moved managed Riftri worktree"));
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("private.txt")).expect("read private worktree data"),
        "private move data\n"
    );
    assert!(fixture.repository.join(".git/riftri/moves").is_dir());

    let status = riftri(&fixture.repository, &["status"]);
    assert!(status.status.success());
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(status.contains("Active views: 1"));
    assert!(status.contains(destination.to_string_lossy().as_ref()));
    assert!(!status.contains(source.to_string_lossy().as_ref()));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_lifecycle_uses_the_registered_custom_state_directory() {
    let fixture = RepositoryFixture::new();
    let state = fixture.directory.path().join("custom-state");
    let source = fixture.directory.path().join("custom-state-source");
    let destination = fixture.directory.path().join("custom-state-destination");
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&source)
        .args(["-b", "feature/custom-state", "HEAD", "--repository"])
        .arg(&fixture.repository)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("add worktree with custom state");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let moved = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "move"])
        .arg(&source)
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("move custom-state worktree through shim");
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(String::from_utf8_lossy(&moved.stderr).contains("moved managed Riftri worktree"));
    assert!(state.join("moves").is_dir());

    let removed = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("remove custom-state worktree through shim");
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(String::from_utf8_lossy(&removed.stderr).contains("safely removed worktree"));
    assert!(state.join("removals").is_dir());
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_prune_checks_and_journals_registered_custom_state() {
    let fixture = RepositoryFixture::new();
    let state = fixture.directory.path().join("custom-prune-state");
    let destination = fixture.directory.path().join("custom-prune-view");
    let stale = fixture.directory.path().join("custom-prune-stale");
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&destination)
        .args(["-b", "feature/custom-prune", "HEAD", "--repository"])
        .arg(&fixture.repository)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("add worktree with custom prune state");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/custom-prune-stale",
                stale.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    fs::remove_dir_all(&stale).expect("remove ordinary stale worktree");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let pruned = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "prune"])
        .current_dir(&fixture.repository)
        .output()
        .expect("prune with registered custom state");

    assert!(
        pruned.status.success(),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    assert!(String::from_utf8_lossy(&pruned.stderr).contains("pruned stale Git worktree metadata"));
    assert!(destination.is_dir());
    assert!(state.join("prunes").is_dir());
    let inventory = git(&fixture.repository, &["worktree", "list", "--porcelain"]);
    assert!(!String::from_utf8_lossy(&inventory.stdout).contains(stale.to_string_lossy().as_ref()));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_prune_with_managed_state_is_journaled() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("guarded-prune-view");
    let stale = fixture.directory.path().join("ordinary-stale-view");
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/ordinary-stale",
                stale.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    fs::remove_dir_all(&stale).expect("remove ordinary worktree outside Git");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/guarded-prune",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let pruned = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "prune"])
        .current_dir(&fixture.repository)
        .output()
        .expect("prune with managed state");

    assert!(
        pruned.status.success(),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    assert!(String::from_utf8_lossy(&pruned.stderr).contains("pruned stale Git worktree metadata"));
    assert!(destination.is_dir());
    assert!(fixture.repository.join(".git/riftri/prunes").is_dir());
    let inventory = git(&fixture.repository, &["worktree", "list", "--porcelain"]);
    assert!(!String::from_utf8_lossy(&inventory.stdout).contains(stale.to_string_lossy().as_ref()));

    let status = riftri(&fixture.repository, &["status"]);
    assert!(status.status.success());
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(status.contains("Active views: 1"));
    assert!(status.contains("Completed prunes: 1"));
    assert!(status.contains("Pending prunes: 0"));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_prune_preserves_a_missing_managed_view_for_repair() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("missing-prune-view");
    let preserved = fixture.directory.path().join("preserved-prune-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/missing-prune",
        ])
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&fixture.repository)
        .output()
        .expect("create managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    fs::rename(&destination, &preserved).expect("move managed view outside Riftri");

    let pruned = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "prune"])
        .current_dir(&fixture.repository)
        .output()
        .expect("attempt guarded prune");

    assert!(!pruned.status.success());
    assert!(String::from_utf8_lossy(&pruned.stderr).contains("missing or not registered"));
    assert!(preserved.is_dir());
    let inventory = git(&fixture.repository, &["worktree", "list", "--porcelain"]);
    assert!(
        String::from_utf8_lossy(&inventory.stdout).contains(destination.to_string_lossy().as_ref())
    );
}

#[test]
fn exec_leaves_non_riftri_worktree_removal_with_real_git() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("ordinary-removed-view");
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/ordinary-removed",
                destination.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let removed = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("remove ordinary worktree through scoped Git");

    assert!(
        removed.status.success(),
        "ordinary removal failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!destination.exists());
    assert!(!fixture.repository.join(".git/riftri/removals").exists());
}

#[test]
fn enabled_unmanaged_force_remove_and_move_still_use_real_git() {
    let fixture = RepositoryFixture::new();
    let removed = fixture.directory.path().join("ordinary-force-remove");
    let moved_from = fixture.directory.path().join("ordinary-move-source");
    let moved_to = fixture.directory.path().join("ordinary-move-destination");
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/ordinary-force-remove",
                removed.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    assert!(
        git(
            &fixture.repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature/ordinary-move",
                moved_from.to_str().expect("UTF-8 fixture path"),
                "HEAD",
            ],
        )
        .status
        .success()
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove", "--force"])
        .arg(&removed)
        .current_dir(&fixture.repository)
        .output()
        .expect("force-remove unmanaged worktree");
    assert!(
        removal.status.success(),
        "{}",
        String::from_utf8_lossy(&removal.stderr)
    );
    assert!(!removed.exists());

    let moved = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "move"])
        .arg(&moved_from)
        .arg(&moved_to)
        .current_dir(&fixture.repository)
        .output()
        .expect("move unmanaged worktree");
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(!moved_from.exists());
    assert!(moved_to.is_dir());
}

#[cfg(target_os = "macos")]
#[test]
fn shell_hook_routes_normal_git_adds_in_enabled_repositories_through_apfs() {
    let fixture = RepositoryFixture::new();
    let cache = tempdir().expect("shell hook cache");
    let destination = fixture.directory.path().join("optimized-shell-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new("sh")
        .args([
            "-c",
            "eval \"$(\"$RIFTRI_TEST_BIN\" shell hook sh)\"\n\
             git rev-parse --show-toplevel\n\
             git worktree add -b feature/enabled-shell \"$RIFTRI_TEST_DESTINATION\" HEAD",
        ])
        .current_dir(&fixture.repository)
        .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
        .env("RIFTRI_TEST_DESTINATION", &destination)
        .env("RIFTRI_CACHE_DIR", cache.path())
        .output()
        .expect("run enabled Git through shell hook");

    assert!(
        output.status.success(),
        "enabled shell add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()),
        fs::canonicalize(&fixture.repository).expect("canonical repository path")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("optimized APFS worktree"));
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert!(fixture.repository.join(".git/riftri/operations").is_dir());
}
