//! Riftri's own read-only Git queries may skip Apple's `/usr/bin/git`
//! trampoline; everything that can start a hook, filter, or helper must not
//! (D047). The trampoline adds `SDKROOT` to the environment of the Git it
//! starts, so Git's Trace2 records which processes went through it.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[allow(dead_code)]
mod support;

const TRAMPOLINE: &str = "/usr/bin/git";
/// Variables the trampoline adds for the tool it starts.
const TRAMPOLINE_VARIABLES: [&str; 4] = ["SDKROOT", "CPATH", "LIBRARY_PATH", "MANPATH"];

/// Whether this Mac has a trampoline that resolves to a working Git.
fn trampoline_available() -> bool {
    let resolved = Command::new("/usr/bin/xcrun")
        .args(["--find", "git"])
        .output()
        .is_ok_and(|output| output.status.success());
    resolved
        && Command::new(TRAMPOLINE)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
}

struct Fixture {
    root: support::WritableTempDir,
    repository: PathBuf,
    trace: PathBuf,
    hooks: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = support::writable_tempdir().unwrap();
        let repository = root.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let fixture = Self {
            trace: root.path().join("trace.json"),
            hooks: root.path().join("hooks.log"),
            repository,
            root,
        };
        fixture.git(&["init", "--quiet"]);
        for (key, value) in [
            ("user.name", "Trampoline fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgSign", "false"),
            ("core.autocrlf", "false"),
        ] {
            fixture.git(&["config", "--local", key, value]);
        }
        fs::write(fixture.repository.join("tracked.txt"), "original\n").unwrap();
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "--quiet", "-m", "test: trampoline fixture"]);

        // Each hook records whether it saw the trampoline's environment. The
        // FSMonitor hook runs whenever Git reads the index, the others on
        // ref and index updates.
        let log = fixture.hooks.display().to_string();
        let record = |name: &str| {
            format!("#!/bin/sh\nprintf '%s %s\\n' {name} \"${{SDKROOT:-unset}}\" >> '{log}'\n")
        };
        let hooks = fixture.repository.join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        for name in ["reference-transaction", "post-index-change"] {
            executable(
                &hooks.join(name),
                &format!("{}cat >/dev/null\n", record(name)),
            );
        }
        let monitor = fixture.root.path().join("fsmonitor-hook");
        executable(&monitor, &format!("{}printf '/\\0'\n", record("fsmonitor")));
        fixture.git(&[
            "config",
            "--local",
            "core.fsmonitor",
            monitor.to_str().unwrap(),
        ]);
        fixture
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.repository);
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy();
            if key.starts_with("GIT_") || key.starts_with("RIFTRI_") {
                command.env_remove(name);
            }
        }
        for name in TRAMPOLINE_VARIABLES {
            command.env_remove(name);
        }
        command
            // Only Apple's Git: no installed shim, Homebrew, or Nix Git first.
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", self.root.path())
            .env("RIFTRI_CACHE_DIR", self.root.path().join("cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn git(&self, arguments: &[&str]) {
        success(self.command(TRAMPOLINE).args(arguments).output().unwrap());
    }

    /// Run Riftri with Trace2 recording every Git process it starts.
    fn traced(&self, arguments: &[&str]) -> Vec<Process> {
        let _ = fs::remove_file(&self.trace);
        let _ = fs::remove_file(&self.hooks);
        success(
            self.command(env!("CARGO_BIN_EXE_riftri"))
                .args(arguments)
                .env("GIT_TRACE2_EVENT", &self.trace)
                .env("GIT_TRACE2_ENV_VARS", "SDKROOT")
                .output()
                .unwrap(),
        );
        processes(&self.trace)
    }

    /// Every hook run since the last traced command, each of which must have
    /// gone through the trampoline.
    fn assert_hooks_saw_the_trampoline(&self) {
        let log = fs::read_to_string(&self.hooks).unwrap_or_default();
        assert!(!log.is_empty(), "no hook ran");
        for line in log.lines() {
            assert!(
                !line.ends_with(" unset"),
                "a hook skipped the trampoline: {line}"
            );
        }
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[derive(Debug)]
struct Process {
    arguments: Vec<String>,
    through_trampoline: bool,
}

fn processes(trace: &Path) -> Vec<Process> {
    let mut by_session = BTreeMap::<String, Process>::new();
    for line in fs::read_to_string(trace).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        let process = by_session
            .entry(event["sid"].as_str().unwrap().to_owned())
            .or_insert(Process {
                arguments: Vec::new(),
                through_trampoline: false,
            });
        match event["event"].as_str() {
            Some("start") => {
                process.arguments = event["argv"].as_array().unwrap()[1..]
                    .iter()
                    .map(|argument| argument.as_str().unwrap().to_owned())
                    .collect();
            }
            Some("def_param") if event["param"] == "SDKROOT" => {
                process.through_trampoline = true;
            }
            _ => {}
        }
    }
    by_session.into_values().collect()
}

/// The queries D047 lets skip the trampoline: none reads the index or work
/// tree, writes anything, or reads more than refs, configuration, repository
/// layout, and commits.
fn is_direct_query(arguments: &[String]) -> bool {
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    let command = match arguments.as_slice() {
        ["--git-dir", _, rest @ ..] | ["-c", _, rest @ ..] => rest,
        all => all,
    };
    match command {
        ["--version"] => true,
        ["rev-parse", rest @ ..] => !rest.iter().any(|argument| argument.contains(':')),
        ["show", rest @ ..] => {
            rest.contains(&"--no-patch") && rest.contains(&"--no-show-signature")
        }
        ["worktree", "list", ..] | ["show-ref", ..] | ["symbolic-ref", ..] => true,
        ["for-each-ref", ..] => true,
        ["config", rest @ ..] => {
            rest.iter().any(|argument| {
                matches!(*argument, "--get" | "--get-all" | "--get-regexp" | "--list")
            }) && !rest.iter().any(|argument| {
                matches!(
                    *argument,
                    "--add" | "--replace-all" | "--unset" | "--unset-all" | "--edit"
                )
            })
        }
        _ => false,
    }
}

/// Commands that can start a hook, filter, or helper, or read the index.
fn can_run_user_code(arguments: &[String]) -> bool {
    matches!(
        arguments.first().map(String::as_str),
        Some(
            "status"
                | "reset"
                | "ls-files"
                | "check-attr"
                | "ls-tree"
                | "cat-file"
                | "diff"
                | "update-index"
                | "branch"
                | "update-ref"
                | "checkout-index"
                | "read-tree"
                | "lfs"
        )
    ) || matches!(
        arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["worktree", "add" | "remove" | "move" | "prune", ..] | ["config", _, "--replace-all", ..]
    )
}

fn assert_routes(processes: &[Process], expect_direct: bool) {
    assert!(!processes.is_empty());
    for process in processes {
        if !process.through_trampoline {
            assert!(
                is_direct_query(&process.arguments),
                "skipped the trampoline: {:?}",
                process.arguments
            );
        }
        if can_run_user_code(&process.arguments) {
            assert!(
                process.through_trampoline,
                "must use the trampoline: {:?}",
                process.arguments
            );
        }
    }
    let direct = processes
        .iter()
        .filter(|process| !process.through_trampoline)
        .count();
    if expect_direct {
        assert!(
            direct > 0,
            "no query skipped the trampoline: {processes:#?}"
        );
    } else {
        assert_eq!(direct, 0, "{processes:#?}");
    }
}

#[test]
fn lifecycle_queries_skip_the_trampoline_and_hooks_never_do() {
    if !trampoline_available() {
        eprintln!("skipping: no working /usr/bin/git trampoline");
        return;
    }
    let fixture = Fixture::new();
    let view = fixture.root.path().join("view");
    let view_argument = view.to_str().unwrap();

    assert_routes(&fixture.traced(&["enable"]), true);

    let add = fixture.traced(&[
        "worktree",
        "add",
        "--no-progress",
        "-b",
        "topic",
        view_argument,
        "HEAD",
    ]);
    assert_routes(&add, true);
    fixture.assert_hooks_saw_the_trampoline();
    // The version Riftri keys bases with comes from the same binary either way.
    assert!(
        add.iter()
            .any(|process| process.arguments == ["--version"] && !process.through_trampoline)
    );

    assert_routes(&fixture.traced(&["doctor"]), true);
    assert_routes(
        &fixture.traced(&["worktree", "remove", "--no-progress", view_argument]),
        true,
    );
    fixture.assert_hooks_saw_the_trampoline();
    assert!(!view.exists());
}

#[test]
fn the_shim_keeps_the_trampoline_for_everything_but_lifecycle_planning() {
    if !trampoline_available() {
        eprintln!("skipping: no working /usr/bin/git trampoline");
        return;
    }
    let fixture = Fixture::new();
    fixture.traced(&["enable"]);
    let view = fixture.root.path().join("view");
    let view_argument = view.to_str().unwrap();

    // An ordinary command, and its passthrough, never skip the trampoline.
    let status = fixture.traced(&["exec", "--", "git", "status", "--short"]);
    assert_routes(&status, false);
    assert!(
        status
            .iter()
            .any(|process| process.arguments == ["status", "--short"])
    );
    fixture.assert_hooks_saw_the_trampoline();

    let add = fixture.traced(&[
        "exec",
        "--",
        "git",
        "worktree",
        "add",
        "-b",
        "topic",
        view_argument,
    ]);
    assert_routes(&add, true);
    fixture.assert_hooks_saw_the_trampoline();
    assert_routes(
        &fixture.traced(&["exec", "--", "git", "worktree", "remove", view_argument]),
        true,
    );
    assert!(!view.exists());
}
