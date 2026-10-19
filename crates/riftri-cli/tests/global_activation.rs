#[cfg(unix)]
#[path = "support/real_git.rs"]
mod real_git;
#[cfg(unix)]
mod support;

#[cfg(unix)]
mod unix {
    use std::env;
    use std::ffi::OsStr;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::time::Instant;

    use tempfile::tempdir;

    use crate::support::{WritableTempDir, writable_tempdir};

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

    fn executable(name: &str) -> Option<PathBuf> {
        env::var_os("PATH").and_then(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join(name))
                .find(|candidate| {
                    candidate.metadata().is_ok_and(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                })
        })
    }

    fn render_hook(cache: &Path, shell: &str) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["shell", "hook", shell])
            .env("RIFTRI_CACHE_DIR", cache)
            .output()
            .expect("render shell hook");
        assert!(
            output.status.success(),
            "hook failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 hook")
    }

    #[test]
    fn relative_cache_hook_survives_directory_changes_and_deactivates() {
        let fixture = RepositoryFixture::new();
        for shell_name in ["sh", "bash", "zsh"] {
            let Some(shell) = executable(shell_name) else {
                assert_ne!(shell_name, "sh", "sh is required");
                continue;
            };
            let output = Command::new(shell)
                .args([
                    "-ec",
                    // Establish a deactivated baseline in this child only;
                    // deactivation intentionally removes an outer shim too.
                    r#"eval "$("$RIFTRI_TEST_BIN" shell deactivate sh)"
original_path=$PATH
original_git=$(command -v git)
original_version=$(git --version)
eval "$("$RIFTRI_TEST_BIN" shell hook sh)"
shim=$(command -v git)
cd "$RIFTRI_TEST_REPOSITORY"
"$RIFTRI_TEST_BIN" shell status
eval "$("$RIFTRI_TEST_BIN" shell hook sh)"
[ "$(command -v git)" = "$shim" ]
PATH=$PATH:${shim%/*}; export PATH
eval "$("$RIFTRI_TEST_BIN" shell deactivate sh)"
[ "$PATH" = "$original_path" ]
[ "$(command -v git)" = "$original_git" ]
[ "$(git --version)" = "$original_version" ]
[ "${RIFTRI_SHIM_ACTIVE-unset}" = unset ]
[ "${RIFTRI_REAL_GIT-unset}" = unset ]
[ "${RIFTRI_SHELL_SHIM_DIR-unset}" = unset ]
"#,
                ])
                .current_dir(fixture.directory.path())
                .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
                .env("RIFTRI_TEST_REPOSITORY", &fixture.repository)
                .env(
                    "RIFTRI_CACHE_DIR",
                    format!("cache-{shell_name} with ' quote"),
                )
                .output()
                .expect("run shell lifecycle after a directory change");
            assert!(
                output.status.success(),
                "{shell_name}: {:?}\n{}\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("Shell interception: active"),
                "{shell_name}"
            );
        }
    }

    #[test]
    fn global_hook_compatibility_matrix_keeps_disabled_repositories_on_real_git() {
        let fixture = RepositoryFixture::new();
        let mut tested = Vec::new();
        for shell_name in ["sh", "bash", "zsh"] {
            let Some(shell) = executable(shell_name) else {
                continue;
            };
            tested.push(shell_name);
            let cache = fixture.directory.path().join(format!("cache-{shell_name}"));
            let destination = fixture
                .directory
                .path()
                .join(format!("ordinary-{shell_name}"));
            let branch = format!("feature/ordinary-{shell_name}");
            let hook = render_hook(&cache, shell_name);
            let direct_root = git(&fixture.repository, &["rev-parse", "--show-toplevel"]);
            let output = Command::new(shell)
                .args([
                    "-c",
                    "eval \"$RIFTRI_TEST_HOOK\"\n\
                     eval \"$RIFTRI_TEST_HOOK\"\n\
                     case :${PATH#*:}: in *:\"$RIFTRI_TEST_SHIM\":*) exit 42 ;; esac\n\
                     git rev-parse --show-toplevel\n\
                     git worktree add -b \"$RIFTRI_TEST_BRANCH\" \"$RIFTRI_TEST_DESTINATION\" HEAD",
                ])
                .current_dir(&fixture.repository)
                .env("RIFTRI_TEST_HOOK", hook)
                .env("RIFTRI_TEST_SHIM", cache.join("shims/v1"))
                .env("RIFTRI_TEST_BRANCH", &branch)
                .env("RIFTRI_TEST_DESTINATION", &destination)
                .output()
                .expect("run global shell compatibility case");
            assert!(
                output.status.success(),
                "{shell_name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stdout.starts_with(&direct_root.stdout),
                "{shell_name}"
            );
            assert!(destination.is_dir(), "{shell_name}");
            assert!(
                git(&destination, &["status", "--porcelain=v1"])
                    .stdout
                    .is_empty(),
                "{shell_name}"
            );
            assert!(
                !fixture.repository.join(".git/riftri").exists(),
                "disabled repository was unexpectedly optimized in {shell_name}"
            );
        }
        assert!(
            tested.contains(&"sh"),
            "the required POSIX shell was not tested"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn global_hook_is_inherited_by_plain_claude_and_codex_style_children() {
        let fixture = RepositoryFixture::new();
        let cache = fixture.directory.path().join("global-cache");
        let agents = fixture.directory.path().join("agents");
        fs::create_dir(&agents).expect("create agent fixture directory");
        let script = "#!/bin/sh\nexec git worktree add -b \"$2\" \"$1\" HEAD\n";
        let mut runners = Vec::new();
        for name in ["plain-shell", "claude", "codex"] {
            let runner = agents.join(name);
            fs::write(&runner, script).expect("write agent fixture");
            fs::set_permissions(&runner, fs::Permissions::from_mode(0o755))
                .expect("make agent fixture executable");
            runners.push((name, runner));
        }
        let enabled = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("enable")
            .current_dir(&fixture.repository)
            .output()
            .expect("enable fixture repository");
        assert!(enabled.status.success());
        let hook = render_hook(&cache, "sh");

        for (name, runner) in runners {
            let destination = fixture.directory.path().join(format!("view-{name}"));
            let branch = format!("feature/{name}");
            let output = Command::new("sh")
                .args([
                    "-c",
                    "eval \"$RIFTRI_TEST_HOOK\"\n\
                     exec \"$RIFTRI_TEST_AGENT\" \"$RIFTRI_TEST_DESTINATION\" \"$RIFTRI_TEST_BRANCH\"",
                ])
                .current_dir(&fixture.repository)
                .env("RIFTRI_TEST_HOOK", &hook)
                .env("RIFTRI_TEST_AGENT", &runner)
                .env("RIFTRI_TEST_DESTINATION", &destination)
                .env("RIFTRI_TEST_BRANCH", &branch)
                .output()
                .expect("run agent compatibility case");
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("created an optimized APFS worktree"),
                "{name} did not inherit optimized interception"
            );
            assert!(
                git(&destination, &["status", "--porcelain=v1"])
                    .stdout
                    .is_empty(),
                "{name}"
            );
        }

        let status = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("status")
            .current_dir(&fixture.repository)
            .output()
            .expect("inspect shared storage");
        let status = String::from_utf8(status.stdout).expect("UTF-8 status");
        assert!(status.contains("Active views: 3"));
        assert!(status.contains("Retained bases: 1"));
        assert!(status.contains("refs=3"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn optimized_add_honors_quiet_without_hiding_errors() {
        let fixture = RepositoryFixture::new();
        let cache = fixture.directory.path().join("quiet-cache");
        assert!(
            Command::new(env!("CARGO_BIN_EXE_riftri"))
                .arg("enable")
                .current_dir(&fixture.repository)
                .output()
                .expect("enable fixture repository")
                .status
                .success()
        );

        for (arguments, destination, quiet) in [
            (
                vec!["--quiet", "--detach", "../quiet-view"],
                "../quiet-view",
                true,
            ),
            (vec!["--detach", "../normal-view"], "../normal-view", false),
            (vec!["--detach", "--", "--quiet"], "--quiet", false),
        ] {
            let run = || {
                Command::new(env!("CARGO_BIN_EXE_riftri"))
                    .args(["exec", "--", "git", "worktree", "add"])
                    .args(&arguments)
                    .current_dir(&fixture.repository)
                    .env("RIFTRI_CACHE_DIR", &cache)
                    .output()
                    .expect("run intercepted add")
            };
            let output = run();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr.is_empty(), quiet);
            let destination = fixture.repository.join(destination);
            assert_eq!(
                fs::read(destination.join("tracked.txt")).expect("read worktree file"),
                b"tracked\n"
            );
            let status = git(&destination, &["status", "--porcelain=v1"]);
            assert!(status.status.success());
            assert!(status.stdout.is_empty());

            let error = run();
            assert!(!error.status.success());
            assert!(!error.stderr.is_empty());
        }
    }

    #[test]
    fn process_scoped_passthrough_preserves_stdin_stdout_stderr_and_exit_status() {
        let fixture = RepositoryFixture::new();
        let run = |program: &OsStr, arguments: &[&str], input: &[u8]| {
            let mut child = Command::new(program)
                .args(arguments)
                .current_dir(&fixture.repository)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start passthrough case");
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(input)
                .expect("write passthrough stdin");
            child.wait_with_output().expect("wait for passthrough case")
        };
        let direct = run(OsStr::new("git"), &["hash-object", "--stdin"], b"riftri\n");
        let scoped = run(
            OsStr::new(env!("CARGO_BIN_EXE_riftri")),
            &["exec", "--", "git", "hash-object", "--stdin"],
            b"riftri\n",
        );
        assert_eq!(scoped.status.code(), direct.status.code());
        assert_eq!(scoped.stdout, direct.stdout);
        assert_eq!(scoped.stderr, direct.stderr);
    }

    /// The Git commands one `git <arguments>` starts through the
    /// process-scoped shim, recorded by a wrapper that `riftri exec` captures
    /// as the real Git. The log is cleared inside the scope, so it holds only
    /// what the shim and Git start for this command.
    fn git_processes_through_the_shim(repository: &Path, arguments: &[&str]) -> Vec<String> {
        let directory = tempdir().expect("counting Git directory");
        let log = directory.path().join("git-invocations");
        let counting = directory.path().join("git");
        fs::write(
            &counting,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RIFTRI_TEST_GIT_LOG\"\nexec \"$RIFTRI_TEST_REAL_GIT\" \"$@\"\n",
        )
        .expect("write counting Git");
        fs::set_permissions(&counting, fs::Permissions::from_mode(0o755))
            .expect("make counting Git executable");
        let path = env::join_paths(
            std::iter::once(directory.path().to_path_buf())
                .chain(env::split_paths(&env::var_os("PATH").expect("PATH is set"))),
        )
        .expect("counting PATH");
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args([
                "exec",
                "--",
                "sh",
                "-c",
                ": > \"$RIFTRI_TEST_GIT_LOG\"; exec git \"$@\"",
                "sh",
            ])
            .args(arguments)
            .current_dir(repository)
            .env("PATH", path)
            // An inherited scope would delegate past the counting wrapper.
            .env_remove("RIFTRI_SHIM_ACTIVE")
            .env_remove("RIFTRI_REAL_GIT")
            .env_remove("RIFTRI_BYPASS")
            .env("RIFTRI_TEST_REAL_GIT", crate::real_git::real_git())
            .env("RIFTRI_TEST_GIT_LOG", &log)
            .output()
            .expect("run Git through the shim");
        assert!(
            output.status.success(),
            "git {arguments:?} failed through the shim: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(&log)
            .expect("read Git invocation log")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Plumbing that agents and editors run constantly is a builtin Git runs
    /// before any alias, so the shim delegates it without first asking Git for
    /// an alias of the same name: one Git process, the command itself.
    #[test]
    fn common_builtins_start_one_git_process_through_the_shim() {
        let fixture = RepositoryFixture::new();
        let enable = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("enable")
            .current_dir(&fixture.repository)
            .output()
            .expect("enable repository");
        assert!(enable.status.success());
        for arguments in [
            &["cat-file", "-t", "HEAD"][..],
            &["rev-list", "-n", "1", "HEAD"][..],
            &["ls-tree", "HEAD"][..],
            &["for-each-ref", "refs/heads"][..],
            &["show-ref", "--head"][..],
            &["symbolic-ref", "HEAD"][..],
            &["merge-base", "HEAD", "HEAD"][..],
            &["describe", "--always"][..],
            &["diff-index", "--quiet", "HEAD"][..],
            &["check-attr", "-a", "tracked.txt"][..],
        ] {
            let calls = git_processes_through_the_shim(&fixture.repository, arguments);
            assert_eq!(
                calls,
                [arguments.join(" ")],
                "git {arguments:?} started extra Git processes"
            );
        }
    }

    #[test]
    fn process_scoped_signal_exit_uses_the_shell_conventional_status() {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["exec", "--", "sh", "-c", "kill -TERM $$"])
            .output()
            .expect("run signalled child");
        assert_eq!(output.status.code(), Some(128 + 15));
    }

    #[test]
    #[ignore = "manual release-mode latency benchmark; reports results without a flaky timing threshold"]
    fn global_shim_passthrough_latency_benchmark() {
        const ITERATIONS: u32 = 50;
        let cache = tempdir().expect("benchmark cache");
        render_hook(cache.path(), "sh");
        let shim = cache.path().join("shims/v1/git");
        let real_git = executable("git")
            .expect("real Git")
            .canonicalize()
            .expect("resolve real Git");
        let measure = |program: &Path, shimmed: bool| {
            let started = Instant::now();
            for _ in 0..ITERATIONS {
                let mut command = Command::new(program);
                command.arg("--version").stdout(Stdio::null());
                if shimmed {
                    command
                        .env("RIFTRI_SHIM_ACTIVE", "1")
                        .env("RIFTRI_REAL_GIT", &real_git);
                }
                assert!(command.status().expect("run benchmark command").success());
            }
            started.elapsed()
        };
        let direct = measure(&real_git, false);
        let intercepted = measure(&shim, true);
        eprintln!(
            "global Git passthrough: direct={:?}/op intercepted={:?}/op overhead={:?}/op",
            direct / ITERATIONS,
            intercepted / ITERATIONS,
            intercepted.saturating_sub(direct) / ITERATIONS
        );
    }
}
