use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(unix)]
use tempfile::tempdir;

mod support;
use support::{WritableTempDir, writable_tempdir};

struct RepositoryFixture {
    directory: WritableTempDir,
    repository: PathBuf,
}

impl RepositoryFixture {
    fn new() -> Self {
        let directory = writable_tempdir().expect("fixture directory");
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
fn state_unregister_accepts_missing_relative_parent_paths() {
    let fixture = RepositoryFixture::new();
    let directory = fs::canonicalize(fixture.directory.path()).expect("resolve fixture directory");
    let missing = directory.join("removed-custom-state");
    let existing = directory.join("existing-custom-state");
    fs::create_dir(&existing).expect("create existing state directory");
    for state in [&missing, &existing] {
        assert!(
            git(
                &fixture.repository,
                &[
                    "config",
                    "--local",
                    "--add",
                    "riftri.stateDirectory",
                    state.to_str().expect("UTF-8 fixture state path"),
                ],
            )
            .status
            .success()
        );
    }

    let refused = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["state", "unregister"])
        .arg(&existing)
        .arg("--repository")
        .arg(&fixture.repository)
        .output()
        .expect("refuse existing state registration");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("still exists"));

    let unregistered = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["state", "unregister", "../removed-custom-state"])
        .arg("--repository")
        .arg(&fixture.repository)
        .current_dir(&fixture.repository)
        .output()
        .expect("unregister missing state directory");
    assert!(
        unregistered.status.success(),
        "{}",
        String::from_utf8_lossy(&unregistered.stderr)
    );
    assert!(
        String::from_utf8_lossy(&unregistered.stdout).contains("Unregistered missing Riftri state")
    );
    assert!(!missing.exists());

    let unknown = riftri(
        &fixture.repository,
        &["state", "unregister", "../never-registered"],
    );
    assert_eq!(unknown.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("not registered"));

    let registered = git(
        &fixture.repository,
        &[
            "config",
            "--local",
            "--path",
            "--get-all",
            "riftri.stateDirectory",
        ],
    );
    assert!(registered.status.success());
    assert_eq!(
        String::from_utf8_lossy(&registered.stdout).trim(),
        existing.to_string_lossy()
    );
}

#[test]
fn state_unregister_help_hides_the_legacy_alias() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["state", "--help"])
        .output()
        .expect("state help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("unregister"), "{help}");
    assert!(!help.contains("forget-missing"), "{help}");

    for command in ["unregister", "forget-missing"] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["state", command, "--help"])
            .output()
            .expect("unregister help");
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains("missing state directory"), "{help}");
        assert!(help.contains("No files are deleted"), "{help}");
    }
}

#[cfg(unix)]
#[test]
fn state_unregister_keeps_existing_files_and_symlinks_registered() {
    use std::os::unix::fs::symlink;

    let fixture = RepositoryFixture::new();
    let root = fs::canonicalize(fixture.directory.path()).expect("resolve fixture");
    let file = root.join("state-file");
    let dangling = root.join("dangling-state");
    let link = root.join("linked-state");
    fs::write(&file, "keep me\n").expect("existing file");
    symlink(root.join("absent"), &dangling).expect("dangling symlink");
    symlink(&file, &link).expect("existing symlink target");
    for path in [&file, &dangling, &link] {
        assert!(
            Command::new("git")
                .args(["config", "--local", "--add", "riftri.stateDirectory"])
                .arg(path)
                .current_dir(&fixture.repository)
                .status()
                .unwrap()
                .success()
        );
    }
    let before = git(
        &fixture.repository,
        &[
            "config",
            "--local",
            "--null",
            "--get-all",
            "riftri.stateDirectory",
        ],
    )
    .stdout;
    for command in ["unregister", "forget-missing"] {
        for path in [&file, &dangling, &link] {
            let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
                .args(["state", command])
                .arg(path)
                .current_dir(&fixture.repository)
                .output()
                .expect("reject existing entry");
            assert_eq!(output.status.code(), Some(3));
            assert!(String::from_utf8_lossy(&output.stderr).contains("still exists"));
        }
    }
    let after = git(
        &fixture.repository,
        &[
            "config",
            "--local",
            "--null",
            "--get-all",
            "riftri.stateDirectory",
        ],
    )
    .stdout;
    assert_eq!(after, before);
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep me\n");
    assert_eq!(fs::read_link(&dangling).unwrap(), root.join("absent"));
    assert_eq!(fs::read_link(&link).unwrap(), file);
}

#[cfg(unix)]
#[test]
fn state_unregister_and_legacy_alias_resolve_native_paths_through_parent_symlinks() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    for command in ["forget-missing", "unregister"] {
        let fixture = RepositoryFixture::new();
        let root = fs::canonicalize(fixture.directory.path()).expect("resolve fixture");
        let parent = root.join("real parent");
        let alias = root.join("parent alias");
        fs::create_dir(&parent).expect("state parent");
        symlink(&parent, &alias).expect("parent alias");
        let name = OsString::from_vec(b"missing-state-\xff".to_vec());
        let registered = parent.join(&name);
        assert!(
            Command::new("git")
                .args(["config", "--local", "--add", "riftri.stateDirectory"])
                .arg(&registered)
                .current_dir(&fixture.repository)
                .status()
                .unwrap()
                .success()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["state", command])
            .arg(Path::new("../parent alias").join(&name))
            .current_dir(&fixture.repository)
            .output()
            .expect("unregister native path");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!registered.exists());
        assert_eq!(fs::read_link(&alias).unwrap(), parent);
        assert_eq!(
            git(
                &fixture.repository,
                &["config", "--local", "--get-all", "riftri.stateDirectory"]
            )
            .status
            .code(),
            Some(1)
        );
    }
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
fn doctor_explains_destination_readiness_and_repository_activation() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("future-worktree");

    let doctor = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["doctor", "--destination"])
        .arg(&destination)
        .arg("--json")
        .current_dir(&fixture.repository)
        .output()
        .expect("run destination-aware doctor");
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
    let readiness = &report["destination_readiness"];
    assert_eq!(
        readiness["destination"],
        destination.to_string_lossy().as_ref()
    );
    assert_eq!(readiness["copy_on_write"], readiness["backend"].is_string());
    if readiness["backend"].is_string() {
        assert_eq!(readiness["status"], "needs-activation");
        let root = git(
            &fixture.repository,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        );
        assert!(root.status.success());
        let root = String::from_utf8(root.stdout).expect("UTF-8 fixture root");
        assert_eq!(
            readiness["next_command"],
            format!(
                "riftri enable '{}'",
                root.strip_suffix('\n').expect("Git path terminator")
            )
        );
    } else {
        assert_eq!(readiness["status"], "blocked");
        assert!(
            readiness["blockers"]
                .as_array()
                .expect("readiness blockers")
                .iter()
                .any(|blocker| blocker["kind"] == "storage-backend")
        );
    }
    assert!(
        readiness["blockers"]
            .as_array()
            .expect("readiness blockers")
            .iter()
            .any(|blocker| {
                blocker["kind"] == "repository-activation"
                    && blocker["remedy"].as_str().is_some_and(|remedy| {
                        remedy.contains("riftri enable")
                            && remedy.contains("one repository at a time")
                    })
            })
    );
    assert!(!fixture.repository.join(".git/riftri").exists());
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn doctor_json_preserves_non_utf8_destination_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fixture = RepositoryFixture::new();
    let destination = OsStr::from_bytes(b"destination-\xff");
    let doctor = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["doctor", "--destination"])
        .arg(destination)
        .arg("--json")
        .current_dir(&fixture.repository)
        .output()
        .expect("run doctor with a native path");
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
    assert_eq!(report["native_path_encoding"], "unix-bytes-hex");
    assert_eq!(
        report["destination_readiness"]["destination"],
        "destination-\u{fffd}"
    );
    assert_eq!(
        report["destination_readiness"]["destination_native_hex"],
        "64657374696e6174696f6e2dff"
    );
    for capability in report["storage_capabilities"].as_array().unwrap() {
        if let Some(volume) = capability.get("volume") {
            assert_eq!(volume["requested_path"], "destination-\u{fffd}");
            assert_eq!(
                volume["requested_path_native_hex"],
                "64657374696e6174696f6e2dff"
            );
            let probe_hex = volume["probe_path_native_hex"].as_str().unwrap();
            let probe_bytes: Vec<u8> = (0..probe_hex.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&probe_hex[index..index + 2], 16).unwrap())
                .collect();
            assert_eq!(
                probe_bytes,
                fs::canonicalize(&fixture.repository)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
            );
        }
    }
    assert!(!fixture.repository.join(destination).exists());
    assert!(!fixture.repository.join(".git/riftri").exists());
}

#[test]
fn doctor_human_output_leads_with_a_decisive_destination_summary() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("human-summary");

    let doctor = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["doctor", "--destination"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("run destination-aware doctor");
    assert!(doctor.status.success());

    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.starts_with("Riftri doctor\nDestination readiness: "));
    assert!(stdout.contains(&format!("Destination: {}", destination.display())));
    assert!(stdout.contains("Copy-on-write: "));
    assert!(stdout.contains("OverlayFS helper: "));
    assert!(stdout.contains("repository-activation:"));
}

#[test]
fn doctor_marks_an_enabled_supported_destination_ready() {
    let fixture = RepositoryFixture::new();
    let destination = fixture
        .directory
        .path()
        .join("ready worktree ' $HOME ; [x]");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let doctor = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["doctor", "--destination"])
        .arg(&destination)
        .arg("--json")
        .current_dir(&fixture.repository)
        .output()
        .expect("run destination-aware doctor");
    assert!(doctor.status.success());

    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
    let readiness = &report["destination_readiness"];
    if readiness["backend"].is_string() {
        assert_eq!(readiness["status"], "ready");
        assert_eq!(readiness["copy_on_write"], true);
        assert_eq!(readiness["blockers"], serde_json::json!([]));
        let quoted_destination = riftri_core::shell_quoted_path(&destination)
            .expect("representable fixture destination");
        let root = git(
            &fixture.repository,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        );
        assert!(root.status.success());
        let root = String::from_utf8(root.stdout).expect("UTF-8 fixture root");
        let quoted_root = riftri_core::shell_quoted_path(Path::new(
            root.strip_suffix('\n').expect("Git path terminator"),
        ))
        .expect("representable fixture root");
        assert_eq!(
            readiness["next_command"],
            format!(
                "riftri worktree add {quoted_destination} --detach HEAD --repository {quoted_root}"
            )
        );
    } else {
        assert_eq!(readiness["status"], "blocked");
        assert_eq!(readiness["copy_on_write"], false);
    }
    assert!(!fixture.repository.join(".git/riftri").exists());
    assert!(!destination.exists());

    #[cfg(unix)]
    if readiness["backend"].is_string() {
        let command = readiness["next_command"].as_str().expect("next command");
        let added = Command::new("sh")
            .args([
                "-c",
                &format!("riftri() {{ \"$RIFTRI_TEST_BINARY\" \"$@\"; }}\n{command}"),
            ])
            .env("RIFTRI_TEST_BINARY", env!("CARGO_BIN_EXE_riftri"))
            .current_dir(&fixture.repository)
            .output()
            .expect("run suggested command");
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        assert_eq!(
            fs::read(destination.join("tracked.txt")).expect("read exact destination"),
            b"tracked\n"
        );
        let status = git(&destination, &["status", "--porcelain"]);
        assert!(status.status.success());
        assert!(status.stdout.is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn doctor_commands_keep_the_selected_repository_outside_git() {
    let mut fixture = RepositoryFixture::new();
    let repository = fixture.directory.path().join("repo ' $(false); & space");
    fs::rename(&fixture.repository, &repository).expect("rename repository");
    fixture.repository = repository;
    let destination = fixture.directory.path().join("view ' $(false); & space");
    let binary = Path::new(env!("CARGO_BIN_EXE_riftri"));
    let path = std::env::join_paths(
        std::iter::once(binary.parent().expect("binary directory").to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ),
    )
    .expect("test PATH");

    for status in ["needs-activation", "ready"] {
        let doctor = Command::new(binary)
            .arg("doctor")
            .arg(&fixture.repository)
            .arg("--destination")
            .arg(&destination)
            .arg("--json")
            .current_dir(fixture.directory.path())
            .output()
            .expect("run doctor outside Git");
        assert!(
            doctor.status.success(),
            "{}",
            String::from_utf8_lossy(&doctor.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&doctor.stdout).expect("parse doctor JSON");
        let readiness = &report["destination_readiness"];
        assert_eq!(readiness["status"], status);
        let command = readiness["next_command"].as_str().expect("next command");
        let output = Command::new("sh")
            .args(["-c", command])
            .env("PATH", &path)
            .current_dir(fixture.directory.path())
            .output()
            .expect("run suggested command outside Git");
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert_eq!(
        fs::read(destination.join("tracked.txt")).expect("read worktree file"),
        b"tracked\n"
    );
    assert_eq!(
        git(&destination, &["rev-parse", "HEAD"]).stdout,
        git(&fixture.repository, &["rev-parse", "HEAD"]).stdout
    );
    let status = git(&destination, &["status", "--porcelain=v1"]);
    assert!(status.status.success());
    assert!(status.stdout.is_empty());
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
        assert!(gc_output.contains("Removed filesystem-accounted allocated: 0 bytes"));
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
fn status_reports_a_malformed_journal_but_repair_fails_closed() {
    let fixture = RepositoryFixture::new();
    let state = fixture.directory.path().join("malformed-state");
    let journal = state.join("operations/corrupt.json");
    fs::create_dir_all(journal.parent().expect("journal parent"))
        .expect("create journal directory");
    fs::write(&journal, b"{not-json\n").expect("write malformed journal");
    let reported_journal = fs::canonicalize(&journal).expect("resolve malformed journal");

    let status = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["status", "--state-dir"])
        .arg(&state)
        .current_dir(&fixture.repository)
        .output()
        .expect("run status");
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let output = String::from_utf8_lossy(&status.stdout);
    assert!(output.contains(reported_journal.to_string_lossy().as_ref()));
    assert!(output.contains("malformed durable operation journal"));

    let repair = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["repair", "--state-dir"])
        .arg(&state)
        .current_dir(&fixture.repository)
        .output()
        .expect("run repair");
    assert!(!repair.status.success());
    assert_eq!(fs::read(journal).expect("journal remains"), b"{not-json\n");
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

/// Regression test: evaluating shell deactivation inside a `riftri exec`
/// session used to leave the process-scoped shim first on `PATH` with its
/// delegation environment stripped, so `git --version` answered as Riftri and
/// ordinary commands like `git log` failed. Deactivation must remove the
/// process-scoped shim entry too, restoring the real Git for the rest of the
/// session.
#[cfg(unix)]
#[test]
fn deactivation_inside_exec_restores_real_git_for_the_rest_of_the_session() {
    let fixture = RepositoryFixture::new();
    let cache = tempdir().expect("shell shim cache");
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "eval \"$(\"$RIFTRI_TEST_BIN\" shell deactivate sh)\"\n\
             git --version\n\
             git log -1 --format=%s\n\
             printf 'scope=%s\\n' \"${RIFTRI_PROCESS_SHIM_DIR-unset}\"\n\
             case :$PATH: in *riftri-git-shim-*) printf 'shim=retained\\n' ;; *) printf 'shim=removed\\n' ;; esac",
        ])
        .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
        .env("RIFTRI_CACHE_DIR", cache.path())
        .current_dir(&fixture.repository)
        .output()
        .expect("deactivate inside riftri exec");

    assert!(
        output.status.success(),
        "deactivated exec session failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 session output");
    assert!(
        stdout.contains("git version"),
        "git --version did not reach the real Git: {stdout}"
    );
    assert!(
        !stdout.contains("riftri "),
        "the Riftri CLI answered a Git command: {stdout}"
    );
    assert!(
        stdout.contains("initial"),
        "git log did not reach the real Git: {stdout}"
    );
    assert!(stdout.contains("scope=unset"), "{stdout}");
    assert!(stdout.contains("shim=removed"), "{stdout}");
}

/// Fail-safe guard: even when the shim's environment is stripped without a
/// proper deactivation — so the shim stays first on `PATH` — a `git`-named
/// invocation must delegate to the real Git instead of answering as Riftri.
#[cfg(unix)]
#[test]
fn exec_shim_delegates_to_real_git_when_its_environment_is_stripped() {
    let fixture = RepositoryFixture::new();
    for stripped in [
        "RIFTRI_SHIM_ACTIVE RIFTRI_REAL_GIT",
        "RIFTRI_SHIM_ACTIVE RIFTRI_REAL_GIT RIFTRI_PROCESS_SHIM_DIR",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args([
                "exec",
                "--",
                "sh",
                "-c",
                &format!(
                    "unset {stripped}\n\
                     git --version\n\
                     git log -1 --format=%s"
                ),
            ])
            .current_dir(&fixture.repository)
            .output()
            .expect("run stripped-environment shim");

        assert!(
            output.status.success(),
            "stripped shim failed ({stripped}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 session output");
        assert!(
            stdout.contains("git version"),
            "stripped shim answered as Riftri ({stripped}): {stdout}"
        );
        assert!(
            stdout.contains("initial"),
            "stripped shim broke git log ({stripped}): {stdout}"
        );
    }
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
fn shell_hook_rejects_a_symlinked_shim_directory() {
    let cache = tempdir().expect("shell hook cache");
    let cache_root = cache.path().join("cache");
    let outside = cache.path().join("outside");
    fs::create_dir_all(cache_root.join("shims")).expect("create cache parent");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(outside.join("git"), "protected\n").expect("write protected Git file");
    std::os::unix::fs::symlink(&outside, cache_root.join("shims/v1"))
        .expect("symlink shim directory");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["shell", "hook", "sh"])
        .env("RIFTRI_CACHE_DIR", &cache_root)
        .output()
        .expect("render shell hook");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("real directory"));
    assert_eq!(
        fs::read_to_string(outside.join("git")).expect("read protected Git file"),
        "protected\n"
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
fn shell_status_reports_bypass_without_deactivating_the_hook() {
    let fixture = RepositoryFixture::new();
    let cache = tempdir().expect("shell hook cache");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    for (bypass, effective) in [
        ("1", "inactive"),
        ("TrUe", "inactive"),
        ("YES", "inactive"),
        ("0", "active"),
        ("false", "active"),
        ("", "active"),
    ] {
        let output = Command::new("sh")
            .args([
                "-ec",
                "eval \"$(\"$RIFTRI_TEST_BIN\" shell hook sh)\"\n\
                 \"$RIFTRI_TEST_BIN\" shell status",
            ])
            .current_dir(&fixture.repository)
            .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
            .env("RIFTRI_CACHE_DIR", cache.path())
            .env("RIFTRI_BYPASS", bypass)
            .output()
            .expect("inspect hooked shell with bypass");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 shell output");
        assert!(stdout.contains("Shell interception: active"), "{stdout}");
        assert!(
            stdout.contains(&format!("Effective optimized interception: {effective}")),
            "RIFTRI_BYPASS={bypass}: {stdout}"
        );
        if effective == "inactive" {
            assert!(stdout.contains("RIFTRI_BYPASS"), "{stdout}");
        }
    }
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

#[cfg(target_os = "windows")]
#[test]
fn powershell_hook_is_session_scoped_idempotent_and_reversible() {
    let fixture = RepositoryFixture::new();
    let cache = Path::new("cache with ' quote");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let script = r#"
$hook = (& $env:RIFTRI_TEST_BIN shell hook powershell) -join [Environment]::NewLine
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Invoke-Expression $hook
Invoke-Expression $hook
$shimDirectory = Join-Path $env:RIFTRI_CACHE_DIR 'shims\v1'
$shimDirectory = (Resolve-Path $shimDirectory).Path
$shim = Join-Path $shimDirectory 'git.exe'
$matches = @($env:PATH -split ';' | Where-Object { $_ -eq $shimDirectory }).Count
if ($matches -ne 1) { exit 41 }
Write-Output "shim=$((Get-Command git -CommandType Application).Source)"
& powershell.exe -NoLogo -NoProfile -NonInteractive -Command 'git --version; exit $LASTEXITCODE'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Set-Location $env:RIFTRI_TEST_REPOSITORY
& $env:RIFTRI_TEST_BIN shell status $env:RIFTRI_TEST_REPOSITORY
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$deactivate = (& $env:RIFTRI_TEST_BIN shell deactivate powershell) -join [Environment]::NewLine
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Invoke-Expression $deactivate
if (Test-Path Env:RIFTRI_SHIM_ACTIVE) { exit 42 }
if (Test-Path Env:RIFTRI_REAL_GIT) { exit 43 }
if (Test-Path Env:RIFTRI_SHELL_SHIM_DIR) { exit 45 }
if ((Get-Command git -CommandType Application).Source -eq $shim) { exit 44 }
Write-Output 'deactivated=true'
"#;
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .current_dir(fixture.directory.path())
        .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
        .env("RIFTRI_TEST_REPOSITORY", &fixture.repository)
        .env("RIFTRI_CACHE_DIR", cache)
        .output()
        .expect("activate, inspect, and deactivate PowerShell hook");

    assert!(
        output.status.success(),
        "PowerShell lifecycle failed with {:?}: {}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 PowerShell output");
    assert!(stdout.contains("shim="));
    assert!(stdout.contains("shims\\v1\\git.exe"));
    assert!(stdout.contains("git version "));
    assert!(stdout.contains("Shell interception: active"));
    assert!(stdout.contains("Repository optimization: enabled"));
    assert!(stdout.contains("Effective optimized interception: active"));
    assert!(stdout.contains("deactivated=true"));
}

#[test]
fn enabled_unsupported_add_fails_without_falling_back() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("unsupported-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "add"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("run unsupported enabled add");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires an existing local branch"));
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
fn exec_routes_an_existing_branch_through_apfs() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("existing-branch-view");
    assert!(
        git(&fixture.repository, &["branch", "feature/existing"])
            .status
            .success()
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "add"])
        .arg(&destination)
        .arg("feature/existing")
        .current_dir(&fixture.repository)
        .output()
        .expect("run existing-branch Git worktree add");

    assert!(
        output.status.success(),
        "existing-branch add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("optimized APFS worktree"));
    assert_eq!(
        String::from_utf8_lossy(&git(&destination, &["branch", "--show-current"]).stdout).trim(),
        "feature/existing"
    );
    assert!(
        git(&destination, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
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
        .arg(destination.file_name().expect("worktree basename"))
        .current_dir(&fixture.repository)
        .output()
        .expect("remove enabled worktree");

    assert!(
        removed.status.success(),
        "enabled removal failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!destination.exists());
    assert!(fixture.repository.join(".git/riftri/removals").is_dir());

    let status = riftri(&fixture.repository, &["status", "--json"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).expect("parse status");
    assert_eq!(status["operations"]["active_views"], 0);
    assert_eq!(status["operations"]["completed_removals"], 1);
    assert_eq!(status["bases"][0]["reference_count"], 0);
    assert_eq!(status["diagnostic_issues"], serde_json::json!([]));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_forced_removal_of_a_managed_view_is_journaled() {
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
    fs::write(
        destination.join("tracked.txt"),
        "discarded tracked change\n",
    )
    .expect("write tracked change");
    fs::write(
        destination.join("untracked.txt"),
        "discarded untracked change\n",
    )
    .expect("write untracked change");

    let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove", "--force"])
        .arg(destination.file_name().expect("worktree basename"))
        .current_dir(&fixture.repository)
        .output()
        .expect("force-remove managed worktree");

    assert!(
        removal.status.success(),
        "{}",
        String::from_utf8_lossy(&removal.stderr)
    );
    assert!(!destination.exists());
    let journals = fs::read_dir(fixture.repository.join(".git/riftri/removals"))
        .expect("read removal journals")
        .map(|entry| entry.expect("journal entry").path())
        .collect::<Vec<_>>();
    assert_eq!(journals.len(), 1);
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(&journals[0]).expect("read removal journal"))
            .expect("parse removal journal");
    assert_eq!(journal["force"], true);
    assert_eq!(
        journal["force_snapshot"]
            .as_str()
            .expect("force snapshot")
            .len(),
        64
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
    assert!(
        String::from_utf8_lossy(&removal.stderr).contains("resolve worktree destination"),
        "{}",
        String::from_utf8_lossy(&removal.stderr)
    );
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
        .arg(destination.file_name().expect("worktree basename"))
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

    let other = RepositoryFixture::new();
    assert!(riftri(&other.repository, &["enable"]).status.success());
    for options in [
        &["--git-dir=.git"][..],
        &["--git-dir", ".git"][..],
        &["--work-tree=."][..],
        &["--work-tree", "."][..],
    ] {
        let removal = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git"])
            .args(options)
            .arg("-C")
            .arg(fixture.directory.path())
            .args(["-C", "repository", "worktree", "remove"])
            .arg(&destination)
            .current_dir(&other.repository)
            .output()
            .expect("guard removal after directory changes");

        assert!(
            !removal.status.success(),
            "{options:?} bypassed the managed removal guard"
        );
        assert!(String::from_utf8_lossy(&removal.stderr).contains("managed Riftri worktree"));
        assert!(destination.is_dir());
        let status = git(&destination, &["status", "--porcelain=v1"]);
        assert!(status.status.success());
        assert!(status.stdout.is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn exec_path_cannot_bypass_managed_lifecycle_guards() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("exec-path-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add", "--detach"])
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("add managed worktree");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let exec_path = git(&fixture.repository, &["--exec-path"]);
    assert!(exec_path.status.success());
    let option = format!(
        "--exec-path={}",
        String::from_utf8(exec_path.stdout)
            .expect("Git exec path")
            .trim()
    );
    for arguments in [
        vec!["remove", destination.to_str().unwrap()],
        vec!["move", destination.to_str().unwrap(), "../moved-view"],
        vec!["prune"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git", &option, "worktree"])
            .args(&arguments)
            .current_dir(&fixture.repository)
            .output()
            .expect("guard lifecycle with exec path");
        assert!(
            !output.status.success(),
            "{arguments:?} bypassed the managed lifecycle guard"
        );
        assert_eq!(
            fs::read(destination.join("tracked.txt")).expect("preserved worktree"),
            b"tracked\n"
        );
    }
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
        .arg(source.file_name().expect("worktree basename"))
        .arg(&destination)
        .current_dir(&fixture.repository)
        .output()
        .expect("move managed worktree");

    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("private.txt")).expect("read private worktree data"),
        "private move data\n"
    );
    assert!(fixture.repository.join(".git/riftri/moves").is_dir());

    let status = riftri(&fixture.repository, &["status", "--json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).expect("parse status");
    assert_eq!(status["operations"]["active_views"], 1);
    assert_eq!(status["operations"]["completed_moves"], 1);
    assert_eq!(
        status["worktrees"][0]["path"],
        fs::canonicalize(&destination)
            .expect("resolve destination")
            .to_str()
            .expect("UTF-8 fixture path")
    );
    assert_eq!(status["diagnostic_issues"], serde_json::json!([]));
}

#[cfg(target_os = "macos")]
#[test]
fn enabled_worktree_selectors_preserve_ambiguity_and_path_boundaries() {
    let fixture = RepositoryFixture::new();
    assert!(
        git(&fixture.repository, &["config", "core.ignorecase", "true"])
            .status
            .success()
    );
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let left = fixture.directory.path().join("left/shared");
    let right = fixture.directory.path().join("right/shared");
    for path in [&left, &right] {
        fs::create_dir_all(path.parent().expect("worktree parent")).expect("create parent");
        let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["worktree", "add", "--detach"])
            .arg(path)
            .current_dir(&fixture.repository)
            .output()
            .expect("add managed worktree");
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
    }
    for selector in ["shared", "hared", "./shared", "left//shared"] {
        for operation in ["remove", "move"] {
            let mut args = vec!["exec", "--", "git", "worktree", operation, selector];
            if operation == "move" {
                args.push("moved");
            }
            let output = riftri(&fixture.repository, &args);
            assert!(!output.status.success(), "{operation} accepted {selector}");
            assert!(left.is_dir() && right.is_dir());
        }
    }
    let guarded = riftri(
        &fixture.repository,
        &[
            "exec",
            "--",
            "git",
            "worktree",
            "move",
            "--force",
            "left/shared",
            "moved",
        ],
    );
    assert!(!guarded.status.success());
    assert!(left.is_dir() && right.is_dir());

    // A unique suffix wins even when an unrelated local directory has that name.
    fs::create_dir_all(fixture.repository.join("left/shared")).expect("create local directory");
    let moved = riftri(
        &fixture.repository,
        &[
            "exec",
            "--",
            "git",
            "worktree",
            "move",
            "LEFT/SHARED",
            "moved",
        ],
    );
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(!left.exists());
    assert!(right.is_dir());
    assert!(fixture.repository.join("left/shared").is_dir());
    assert!(fixture.repository.join("moved/.git").is_file());
    let status = riftri(&fixture.repository, &["status", "--json"]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).expect("parse status");
    assert_eq!(status["operations"]["active_views"], 2);
    assert_eq!(status["operations"]["completed_moves"], 1);
    assert_eq!(status["operations"]["completed_removals"], 0);
    assert_eq!(status["diagnostic_issues"], serde_json::json!([]));
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
        .arg(removed.file_name().expect("worktree basename"))
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
        .arg(moved_from.file_name().expect("worktree basename"))
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

/// Editors and IDE Git integrations run `git worktree add -h` to discover the
/// options they may pass, so the one subcommand Riftri intercepts eagerly must
/// still be able to answer for itself.
#[test]
fn enabled_add_help_reaches_real_git() {
    let fixture = RepositoryFixture::new();
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    for help in ["--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git", "worktree", "add", help])
            .current_dir(&fixture.repository)
            // Keep any Git installation that would page its help from blocking
            // on a terminal this test does not have.
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .output()
            .expect("ask Git for worktree add usage");

        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            rendered.contains("git worktree add"),
            "`worktree add {help}` printed no usage: {rendered}"
        );
        assert!(
            !rendered.contains("is not supported by the optimized add path"),
            "`worktree add {help}` was refused: {rendered}"
        );
    }
}

/// `git worktree add <path> <commit-ish>` only checks out a branch when
/// `<commit-ish>` is an existing local branch. A tag, a raw commit, a
/// remote-tracking ref, or `HEAD` must be refused up front, with the real
/// limitation and the bypass escape hatch, instead of failing part-way through
/// as a branch that does not exist.
#[test]
fn enabled_add_refuses_a_non_branch_revision_before_mutating_anything() {
    let fixture = RepositoryFixture::new();
    assert!(
        git(&fixture.repository, &["tag", "v1.0.0"])
            .status
            .success()
    );
    let head = git(&fixture.repository, &["rev-parse", "HEAD"]);
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).expect("UTF-8 commit id");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());

    for revision in ["v1.0.0", head.trim(), "origin/main", "HEAD"] {
        let destination = fixture.directory.path().join("non-branch-view");
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git", "worktree", "add"])
            .arg(&destination)
            .arg(revision)
            .current_dir(&fixture.repository)
            .output()
            .expect("add a worktree at a non-branch revision");

        assert!(!output.status.success(), "{revision} was not refused");
        let message = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            message.contains(revision) && message.contains("is not one"),
            "{revision}: {message}"
        );
        assert!(message.contains("--detach"), "{revision}: {message}");
        assert!(message.contains("RIFTRI_BYPASS=1"), "{revision}: {message}");
        assert!(
            !message.contains("existing local branch does not exist"),
            "{revision}: {message}"
        );
        assert!(!destination.exists(), "{revision} left a partial worktree");
    }
}

/// `git worktree prune` must keep working for ordinary callers once managed
/// Riftri state exists: IDEs pass `--no-optional-locks` unconditionally, and
/// `--dry-run` only reports. Options that could remove metadata outside the
/// journal stay refused, now naming what was actually passed.
#[cfg(target_os = "macos")]
#[test]
fn enabled_prune_delegates_neutral_and_read_only_invocations() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("prune-neutral-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/prune-neutral",
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

    for arguments in [
        &["worktree", "prune", "--dry-run"][..],
        &["worktree", "prune", "-n"][..],
        &["worktree", "prune", "-v"][..],
        &["--no-optional-locks", "worktree", "prune"][..],
        &["--no-advice", "worktree", "prune"][..],
        &["worktree", "prune"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git"])
            .args(arguments)
            .current_dir(&fixture.repository)
            .output()
            .expect("prune through the intercepted shim");

        assert!(
            output.status.success(),
            "git {arguments:?} was refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Every form above leaves the live managed view in place.
        assert!(
            destination.is_dir(),
            "git {arguments:?} removed a live view"
        );
    }

    let expired = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "prune", "--expire"])
        .arg("1.day.ago")
        .current_dir(&fixture.repository)
        .output()
        .expect("prune with an expiry window");
    assert!(!expired.status.success());
    let message = String::from_utf8_lossy(&expired.stderr);
    assert!(
        message.contains("`git worktree prune --expire 1.day.ago`"),
        "refusal did not describe the actual input: {message}"
    );
    assert!(message.contains("RIFTRI_BYPASS=1"), "{message}");
}

/// `git worktree prune -v` is a verbose *prune*: with a hand-deleted managed
/// worktree it must hit the same journaled-path refusal as a bare prune, and
/// real Git must never remove the managed lifecycle metadata behind the
/// journal. A dry run stays delegated and removes nothing even with an
/// expiry window, while a dry run with an unknown option stays fail-closed.
#[cfg(target_os = "macos")]
#[test]
fn enabled_prune_verbose_never_bypasses_the_journal() {
    let fixture = RepositoryFixture::new();
    let destination = fixture.directory.path().join("prune-verbose-view");
    assert!(riftri(&fixture.repository, &["enable"]).status.success());
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/prune-verbose",
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

    // Hand-delete the managed view so real Git would consider it prunable.
    fs::remove_dir_all(&destination).expect("delete managed view by hand");
    let metadata = fixture.repository.join(".git/worktrees/prune-verbose-view");
    assert!(metadata.is_dir(), "expected linked-worktree metadata");

    for arguments in [
        &["worktree", "prune", "-v"][..],
        &["worktree", "prune", "--verbose"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git"])
            .args(arguments)
            .current_dir(&fixture.repository)
            .output()
            .expect("verbose prune through the intercepted shim");

        // The journaled path refuses while the managed view is missing, and
        // real Git must not have pruned the metadata behind the journal.
        assert!(
            !output.status.success(),
            "git {arguments:?} succeeded against a missing managed view"
        );
        assert!(
            metadata.is_dir(),
            "git {arguments:?} removed managed lifecycle metadata"
        );
    }

    // A dry run only reports: delegated, successful, metadata intact.
    for arguments in [
        &["worktree", "prune", "--dry-run"][..],
        &["worktree", "prune", "--dry-run", "-v"][..],
        &["worktree", "prune", "--dry-run", "--expire", "1.day.ago"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "git"])
            .args(arguments)
            .current_dir(&fixture.repository)
            .output()
            .expect("dry-run prune through the intercepted shim");
        assert!(
            output.status.success(),
            "git {arguments:?} was refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            metadata.is_dir(),
            "git {arguments:?} removed metadata despite --dry-run"
        );
    }

    // Unknown options stay fail-closed even beside a dry run.
    let refused = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "prune",
            "--dry-run",
            "--unknown-option",
        ])
        .current_dir(&fixture.repository)
        .output()
        .expect("unknown prune option through the intercepted shim");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("RIFTRI_BYPASS=1"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
