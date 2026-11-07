use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const OVERRIDES: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

fn isolated(command: &str, repository: &Path) -> Command {
    let mut command = Command::new(command);
    command
        .current_dir(repository)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for name in OVERRIDES {
        command.env_remove(name);
    }
    command
}

fn git(repository: &Path, args: &[&str]) -> Output {
    let output = isolated("git", repository).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fixture(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    fs::create_dir(&repository).unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"],
        &["config", "user.email", "riftri@example.invalid"],
        &["config", "core.autocrlf", "false"],
    ] {
        git(&repository, args);
    }
    fs::write(repository.join("tracked.txt"), "original\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    fs::write(repository.join("tracked.txt"), "private staged data\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    fs::write(repository.join("tracked.txt"), "original\n").unwrap();
    repository
}

#[test]
fn explicit_and_intercepted_adds_reject_repository_overrides_without_mutation() {
    for name in OVERRIDES {
        for intercepted in [false, true] {
            let fixture_dir = tempfile::tempdir().unwrap();
            let repository = fixture(fixture_dir.path());
            git(&repository, &["config", "riftri.enabled", "true"]);
            let destination = fixture_dir.path().join("view");
            let value: OsString = match name {
                "GIT_INDEX_FILE" => repository.join(".git/index").into(),
                "GIT_WORK_TREE" => repository.clone().into(),
                "GIT_OBJECT_DIRECTORY" | "GIT_ALTERNATE_OBJECT_DIRECTORIES" => {
                    repository.join(".git/objects").into()
                }
                "GIT_CONFIG_COUNT" => OsString::from("1"),
                "GIT_CONFIG_PARAMETERS" => OsString::from("'riftri-test.injected=lifecycle'"),
                _ => repository.join(".git").into(),
            };
            let before_index = fs::read(repository.join(".git/index")).unwrap();
            let before_config = fs::read(repository.join(".git/config")).unwrap();
            let mut command = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
            if intercepted {
                command.args(["exec", "--", "git"]);
            }
            command
                .args(["worktree", "add"])
                .arg(&destination)
                .args(["-b", "feature/environment", "HEAD"])
                .env(name, &value);
            if name == "GIT_CONFIG_COUNT" {
                command
                    .env("GIT_CONFIG_KEY_0", "riftri-test.injected")
                    .env("GIT_CONFIG_VALUE_0", "lifecycle");
            }
            let output = command.output().unwrap();
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{name}, intercepted={intercepted}: {message}"
            );
            assert!(message.contains(name), "{message}");
            assert!(message.contains("unset"), "{message}");
            assert!(!destination.exists());
            assert!(!repository.join(".git/riftri").exists());
            assert!(!repository.join(".git/worktrees").exists());
            assert_eq!(
                fs::read(repository.join(".git/index")).unwrap(),
                before_index
            );
            assert_eq!(
                fs::read(repository.join(".git/config")).unwrap(),
                before_config
            );
            assert_eq!(
                git(&repository, &["show", ":tracked.txt"]).stdout,
                b"private staged data\n"
            );
            assert_eq!(
                git(&repository, &["branch", "--list", "feature/environment"]).stdout,
                b""
            );
        }
    }
}

#[test]
fn lifecycle_commands_reject_even_empty_repository_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    // The state directory exists so that inspection commands reach the Git
    // environment guard rather than stopping earlier on a missing
    // `--state-dir`; the guard must still refuse before writing anything into
    // it.
    let state = fixture_dir.path().join("state");
    fs::create_dir(&state).unwrap();
    for name in [
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        for args in [
            &["worktree", "add", "../view", "--detach"][..],
            &["worktree", "remove", "../view"],
            &["worktree", "remove", "../view", "--force", "--yes"],
            &["worktree", "move", "../view", "../moved"],
            &["worktree", "compact", "../view"],
            &["worktree", "prune"],
            &["repair"],
            &["gc", "--apply"],
        ] {
            let output = isolated(env!("CARGO_BIN_EXE_riftri"), &repository)
                .args(args)
                .arg("--state-dir")
                .arg(&state)
                .env(name, "")
                .output()
                .unwrap();
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{name}, {args:?}: {message}");
            assert!(
                message.contains(name) && message.contains("unset"),
                "{name}, {args:?}: {message}"
            );
            assert_eq!(
                fs::read_dir(&state).unwrap().count(),
                0,
                "{name}, {args:?}: refused commands must not write state"
            );
        }
    }
}

#[test]
fn normal_git_passthrough_preserves_object_store_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    git(&repository, &["config", "riftri.enabled", "true"]);
    // The blob exists only in the foreign store, so reading it from the
    // repository proves the override reached the delegated Git process.
    let foreign = fixture_dir.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    git(&foreign, &["init", "--quiet"]);
    fs::write(foreign.join("only-here.txt"), "foreign object\n").unwrap();
    let blob = String::from_utf8(git(&foreign, &["hash-object", "-w", "only-here.txt"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    let foreign_objects = foreign.join(".git/objects");
    for name in ["GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"] {
        let output = isolated(env!("CARGO_BIN_EXE_riftri"), &repository)
            .args(["exec", "--", "git", "cat-file", "blob", &blob])
            .env(name, &foreign_objects)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"foreign object\n", "{name}");
    }
}

#[test]
fn normal_git_passthrough_preserves_config_injection_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    git(&repository, &["config", "riftri.enabled", "true"]);
    // The injected key exists nowhere on disk, so reading it back proves the
    // environment configuration reached the delegated Git process.
    let count = isolated(env!("CARGO_BIN_EXE_riftri"), &repository)
        .args([
            "exec",
            "--",
            "git",
            "config",
            "--get",
            "riftri-test.injected",
        ])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "riftri-test.injected")
        .env("GIT_CONFIG_VALUE_0", "environment")
        .output()
        .unwrap();
    assert!(
        count.status.success(),
        "GIT_CONFIG_COUNT: {}",
        String::from_utf8_lossy(&count.stderr)
    );
    assert_eq!(count.stdout, b"environment\n");
    let parameters = isolated(env!("CARGO_BIN_EXE_riftri"), &repository)
        .args([
            "exec",
            "--",
            "git",
            "config",
            "--get",
            "riftri-test.injected",
        ])
        .env("GIT_CONFIG_PARAMETERS", "'riftri-test.injected=parameters'")
        .output()
        .unwrap();
    assert!(
        parameters.status.success(),
        "GIT_CONFIG_PARAMETERS: {}",
        String::from_utf8_lossy(&parameters.stderr)
    );
    assert_eq!(parameters.stdout, b"parameters\n");
}

#[test]
fn normal_git_passthrough_preserves_repository_overrides() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());
    git(&repository, &["config", "riftri.enabled", "true"]);
    let output = isolated(env!("CARGO_BIN_EXE_riftri"), fixture_dir.path())
        .args(["exec", "--", "git", "show", ":tracked.txt"])
        .env("GIT_DIR", repository.join(".git"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"private staged data\n");
}

/// What cloud agent sandboxes inject so Git can reach a remote: no
/// credential prompts, and a URL rewrite. None of it can change what a
/// lifecycle operation reads or writes locally (D048).
const NETWORK_ONLY: [(&str, &str); 4] = [
    ("credential.interactive", "false"),
    ("url.https://github.com/.insteadOf", "git@github.com:"),
    (
        "HTTP.https://example.invalid/.extraHeader",
        "X-Riftri-Test: 1",
    ),
    ("Credential.https://example.invalid.useHttpPath", "true"),
];

fn inject(command: &mut Command, entries: &[(&str, &str)]) {
    command.env("GIT_CONFIG_COUNT", entries.len().to_string());
    for (index, (key, value)) in entries.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
}

#[test]
fn lifecycle_commands_accept_network_only_injected_configuration() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let repository = fixture(fixture_dir.path());

    let mut doctor = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
    doctor.args(["doctor", "--json"]);
    inject(&mut doctor, &NETWORK_ONLY);
    let output = doctor.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["repository_compatibility"]["available"], true,
        "{report:#}"
    );

    let state = fixture_dir.path().join("state");
    fs::create_dir(&state).unwrap();
    let mut gc = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
    gc.args(["gc", "--json", "--state-dir"]).arg(&state);
    inject(&mut gc, &NETWORK_ONLY);
    let output = gc.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Whether this volume can make the view depends on the machine; the
    // configuration guard must not be what stops it.
    let mut add = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
    add.args(["worktree", "add"])
        .arg(fixture_dir.path().join("view"))
        .args(["-b", "feature/network-only", "HEAD"]);
    inject(&mut add, &NETWORK_ONLY);
    let output = add.output().unwrap();
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(!message.contains("GIT_CONFIG_COUNT"), "{message}");
}

#[test]
fn injected_configuration_that_can_reach_local_operations_is_still_refused() {
    let refused = [
        ("core.hooksPath", "/nonexistent"),
        ("filter.lfs.smudge", "cat"),
        ("riftri.enabled", "true"),
        // `insteadOf` needs the URL it rewrites, and only names its own variables
        ("url.insteadOf", "https://example.invalid/"),
        ("url.https://example.invalid/.other", "x"),
        // a section name that only starts like an allowed one
        ("credentials.helper", "store"),
        ("httpx.proxy", "http://127.0.0.1:1"),
    ];
    for extra in refused {
        let fixture_dir = tempfile::tempdir().unwrap();
        let repository = fixture(fixture_dir.path());
        let destination = fixture_dir.path().join("view");
        let mut entries = NETWORK_ONLY.to_vec();
        entries.push(extra);
        let mut command = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
        command.args(["worktree", "add"]).arg(&destination).args([
            "-b",
            "feature/environment",
            "HEAD",
        ]);
        inject(&mut command, &entries);
        let output = command.output().unwrap();
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{extra:?}: {message}");
        assert!(
            message.contains("GIT_CONFIG_COUNT") && message.contains("unset"),
            "{extra:?}: {message}"
        );
        assert!(message.contains(extra.0), "{extra:?}: {message}");
        assert!(!destination.exists(), "{extra:?}");
        assert!(!repository.join(".git/riftri").exists(), "{extra:?}");
        assert!(!repository.join(".git/worktrees").exists(), "{extra:?}");
    }

    // A count Git would reject, or one naming a key that isn't there, is
    // refused rather than guessed at.
    for (count, keys) in [("2", 1), ("one", 1), (" 1", 1), ("-1", 0)] {
        let fixture_dir = tempfile::tempdir().unwrap();
        let repository = fixture(fixture_dir.path());
        let mut command = isolated(env!("CARGO_BIN_EXE_riftri"), &repository);
        command
            .args(["worktree", "add"])
            .arg(fixture_dir.path().join("view"))
            .args(["-b", "feature/environment", "HEAD"])
            .env("GIT_CONFIG_COUNT", count);
        for index in 0..keys {
            command
                .env(format!("GIT_CONFIG_KEY_{index}"), "credential.interactive")
                .env(format!("GIT_CONFIG_VALUE_{index}"), "false");
        }
        let output = command.output().unwrap();
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{count:?}: {message}");
        assert!(
            message.contains("GIT_CONFIG_COUNT") && message.contains("unset"),
            "{count:?}: {message}"
        );
        assert!(!repository.join(".git/worktrees").exists(), "{count:?}");
    }
}
