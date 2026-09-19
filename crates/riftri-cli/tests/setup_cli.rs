use std::process::{Command, Stdio};

#[cfg(unix)]
mod support;

#[test]
fn setup_documents_the_interactive_workflow() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["setup", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in ["--repository", "--destination", "--branch", "agent"] {
        assert!(help.contains(text), "missing {text}: {help}");
    }
}

#[test]
fn setup_rejects_noninteractive_input_without_creating_state() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("setup")
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("interactive terminal"), "{error}");
    assert!(error.contains("riftri worktree add"), "{error}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn setup_json_errors_do_not_include_prompts() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["setup", "--json-errors"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(receipt["operation"], "setup");
}

#[cfg(unix)]
mod terminal {
    use std::fs::{self, File};
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Output, Stdio};
    use std::time::{Duration, Instant};

    use super::support;

    struct Fixture {
        directory: support::WritableTempDir,
        repository: PathBuf,
        view: PathBuf,
    }

    fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_COUNT", "0");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
            "RIFTRI_BYPASS",
            "RIFTRI_REAL_GIT",
            "RIFTRI_SHIM_ACTIVE",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn git(repository: &Path, args: &[&str]) -> Output {
        command("git")
            .current_dir(repository)
            .args(args)
            .output()
            .unwrap()
    }

    fn git_with_input(repository: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = command("git")
            .current_dir(repository)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn destination_is_case_sensitive(parent: &Path) -> bool {
        let probe = tempfile::Builder::new()
            .prefix(".riftri-test-case-")
            .tempdir_in(parent)
            .expect("create case-sensitivity probe");
        fs::write(probe.path().join("Case"), b"upper").expect("create first case probe");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe.path().join("case"))
            .is_ok()
    }

    impl Fixture {
        fn new() -> Self {
            let directory = support::writable_tempdir().unwrap();
            let repository = directory.path().join("repository with spaces");
            let view = directory.path().join("view with an'apostrophe");
            fs::create_dir(&repository).unwrap();
            for args in [
                &["init", "--quiet"][..],
                &["config", "user.name", "Riftri Tests"][..],
                &["config", "user.email", "riftri@example.invalid"][..],
                &["config", "commit.gpgSign", "false"][..],
                &["config", "core.autocrlf", "false"][..],
            ] {
                assert!(git(&repository, args).status.success());
            }
            fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
            assert!(git(&repository, &["add", "tracked.txt"]).status.success());
            assert!(
                git(&repository, &["commit", "--quiet", "-m", "fixture"])
                    .status
                    .success()
            );
            Self {
                directory,
                repository,
                view,
            }
        }

        fn supported(&self) -> bool {
            let output = command(env!("CARGO_BIN_EXE_riftri"))
                .args(["doctor", "--json", "--repository"])
                .arg(&self.repository)
                .arg("--destination")
                .arg(&self.view)
                .output()
                .unwrap();
            assert!(output.status.success());
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let supported = report["destination_readiness"]["copy_on_write"] == true;
            if std::env::vars_os().any(|(key, value)| {
                key.to_string_lossy().starts_with("RIFTRI_REQUIRE_") && value == "1"
            }) {
                assert!(
                    supported,
                    "native filesystem integration must support creation: {report}"
                );
            }
            supported
        }

        fn no_mutation(&self) {
            assert!(!self.view.exists());
            assert!(!self.repository.join(".git/riftri").exists());
            assert!(
                !git(
                    &self.repository,
                    &["config", "--local", "--get", "riftri.enabled"]
                )
                .status
                .success()
            );
            assert!(
                !git(
                    &self.repository,
                    &["show-ref", "--verify", "refs/heads/task/setup"]
                )
                .status
                .success()
            );
        }

        /// The diagnostic the explicit `riftri worktree add` prints for the
        /// same destination. Setup's plan step must refuse with identical
        /// wording, so tests derive the expected text from this output.
        fn explicit_add_diagnostic(&self) -> String {
            let output = command(env!("CARGO_BIN_EXE_riftri"))
                .args(["worktree", "add"])
                .arg(&self.view)
                .args(["-b", "task/explicit", "--repository"])
                .arg(&self.repository)
                .output()
                .unwrap();
            assert!(!output.status.success());
            String::from_utf8_lossy(&output.stderr).into_owned()
        }

        fn run(&self, input: &[u8]) -> (ExitStatus, String) {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: openpty writes two fresh descriptors; optional pointers are null.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            // SAFETY: these descriptors were returned with exclusive ownership by openpty.
            let (mut master, slave) =
                unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            let mut process = command(env!("CARGO_BIN_EXE_riftri"));
            process
                .args(["setup", "--repository"])
                .arg(&self.repository)
                .arg("--destination")
                .arg(&self.view)
                .args(["--branch", "task/setup"])
                .env(
                    "RIFTRI_TEST_RESULT",
                    self.directory.path().join("agent-result"),
                )
                .env(
                    "RIFTRI_TEST_SIBLING",
                    self.directory.path().join("agent-created"),
                )
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave));
            // SAFETY: only async-signal-safe system calls run between fork and exec.
            unsafe {
                process.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[allow(clippy::unnecessary_cast)]
                    let request = libc::TIOCSCTTY as libc::c_ulong;
                    if libc::ioctl(0, request, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = process.spawn().unwrap();
            drop(process);
            let mut reader = master.try_clone().unwrap();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut output = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => output.extend_from_slice(&buffer[..count]),
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => panic!("read setup terminal: {error}"),
                    }
                }
                let _ = sender.send(String::from_utf8_lossy(&output).into_owned());
            });
            master.write_all(input).unwrap();
            let status = wait(&mut child);
            drop(child);
            let output = receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("terminal must close");
            (status, output)
        }
    }

    fn wait(child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("setup terminal did not finish within 45 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn declining_creation_leaves_no_branch_state_or_activation() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let (status, output) = fixture.run(b"\n");
        assert!(status.success(), "{output}");
        fixture.no_mutation();
    }

    #[test]
    fn blocked_checkout_stops_before_creation_and_agent_selection() {
        let fixture = Fixture::new();
        assert!(
            git(&fixture.repository, &["config", "core.autocrlf", "true"])
                .status
                .success()
        );
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
        assert!(output.contains("Blocked:"), "{output}");
        assert!(!output.contains("Which coding agent"), "{output}");
        fixture.no_mutation();
    }

    #[test]
    fn occupied_destination_is_refused_before_the_confirmation_prompt() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        fs::create_dir(&fixture.view).unwrap();
        fs::write(fixture.view.join("keep.txt"), "user data\n").unwrap();
        let diagnostic = format!("destination already exists: {}", fixture.view.display());
        let explicit = fixture.explicit_add_diagnostic();
        assert!(explicit.contains(&diagnostic), "{explicit}");
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
        assert!(output.contains(&diagnostic), "{output}");
        assert!(!output.contains("Create this worktree?"), "{output}");
        assert!(!output.contains("Which coding agent"), "{output}");
        assert_eq!(
            fs::read(fixture.view.join("keep.txt")).unwrap(),
            b"user data\n"
        );
        assert!(!fixture.view.join(".git").exists());
        assert!(
            !git(
                &fixture.repository,
                &["show-ref", "--verify", "refs/heads/task/setup"]
            )
            .status
            .success()
        );
    }

    #[test]
    fn symlinked_destination_is_refused_before_the_confirmation_prompt() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        std::os::unix::fs::symlink(&fixture.repository, &fixture.view).unwrap();
        let diagnostic = format!("destination already exists: {}", fixture.view.display());
        let explicit = fixture.explicit_add_diagnostic();
        assert!(explicit.contains(&diagnostic), "{explicit}");
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
        assert!(output.contains(&diagnostic), "{output}");
        assert!(!output.contains("Create this worktree?"), "{output}");
        assert!(!output.contains("Which coding agent"), "{output}");
        // The symlink and its target are untouched.
        assert_eq!(fs::read_link(&fixture.view).unwrap(), fixture.repository);
        assert!(fixture.repository.join("tracked.txt").is_file());
        assert!(
            !git(
                &fixture.repository,
                &["show-ref", "--verify", "refs/heads/task/setup"]
            )
            .status
            .success()
        );
    }

    #[test]
    fn case_colliding_checkout_paths_are_refused_before_the_confirmation_prompt() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        if destination_is_case_sensitive(fixture.directory.path()) {
            return;
        }
        // Point HEAD at a tree whose paths collide on a case-insensitive
        // destination filesystem; the repository working tree is never asked
        // to materialize both, so plumbing builds the commit directly.
        let upper = git_with_input(
            &fixture.repository,
            &["hash-object", "-w", "--stdin"],
            b"upper\n",
        );
        let lower = git_with_input(
            &fixture.repository,
            &["hash-object", "-w", "--stdin"],
            b"lower\n",
        );
        let tree_input = format!(
            "100644 blob {}\tCase.txt\0100644 blob {}\tcase.txt\0",
            upper.trim(),
            lower.trim()
        );
        let tree = git_with_input(
            &fixture.repository,
            &["mktree", "-z"],
            tree_input.as_bytes(),
        );
        let commit = git(
            &fixture.repository,
            &["commit-tree", tree.trim(), "-m", "case-collision fixture"],
        );
        assert!(commit.status.success());
        let commit = String::from_utf8(commit.stdout).unwrap();
        assert!(
            git(&fixture.repository, &["update-ref", "HEAD", commit.trim()])
                .status
                .success()
        );
        let explicit = fixture.explicit_add_diagnostic();
        assert!(
            explicit.contains("cannot coexist on the destination filesystem"),
            "{explicit}"
        );
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
        assert!(output.contains("Git tree path"), "{output}");
        assert!(
            output.contains("cannot coexist on the destination filesystem"),
            "{output}"
        );
        assert!(!output.contains("Create this worktree?"), "{output}");
        assert!(!output.contains("Which coding agent"), "{output}");
        assert!(!fixture.view.exists());
        fixture.no_mutation();
    }

    #[test]
    fn an_existing_branch_is_never_reset_by_setup() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        assert!(
            git(&fixture.repository, &["branch", "task/setup"])
                .status
                .success()
        );
        let before = git(&fixture.repository, &["rev-parse", "task/setup"]).stdout;
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
        assert!(!output.contains("Which coding agent"), "{output}");
        assert!(!fixture.view.exists());
        assert_eq!(
            git(&fixture.repository, &["rev-parse", "task/setup"]).stdout,
            before
        );
        assert!(
            !git(
                &fixture.repository,
                &["config", "--local", "--get", "riftri.enabled"]
            )
            .status
            .success()
        );
    }

    #[test]
    fn not_now_and_declining_launch_keep_a_clean_worktree_without_activation() {
        for answer in ["y\n0\n", "y\n3\n/bin/sh\nn\n"] {
            let fixture = Fixture::new();
            if !fixture.supported() {
                return;
            }
            let (status, output) = fixture.run(answer.as_bytes());
            assert!(status.success(), "{output}");
            assert!(fixture.view.join("tracked.txt").is_file(), "{output}");
            let clean = git(&fixture.view, &["status", "--porcelain=v1"]);
            assert!(clean.status.success() && clean.stdout.is_empty());
            assert!(
                !git(
                    &fixture.repository,
                    &["config", "--local", "--get", "riftri.enabled"]
                )
                .status
                .success()
            );
            assert!(output.contains("Worktree kept"), "{output}");
        }
    }

    #[test]
    fn chosen_agent_starts_in_the_view_and_uses_scoped_git_with_its_exit_code() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let agent = fixture.directory.path().join("agent with spaces");
        fs::write(&agent, "#!/bin/sh\nset -eu\ntest \"$RIFTRI_SHIM_ACTIVE\" = 1\npwd -P > \"$RIFTRI_TEST_RESULT\"\ngit worktree add --detach \"$RIFTRI_TEST_SIBLING\" HEAD\ngit worktree remove \"$RIFTRI_TEST_SIBLING\"\nexit 7\n").unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let answers = format!("y\n3\n{}\ny\n", agent.display());
        let (status, output) = fixture.run(answers.as_bytes());
        assert_eq!(status.code(), Some(7), "{output}");
        assert_eq!(
            fs::read_to_string(fixture.directory.path().join("agent-result"))
                .unwrap()
                .trim(),
            fixture.view.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            git(
                &fixture.repository,
                &["config", "--local", "--get", "riftri.enabled"]
            )
            .stdout,
            b"true\n"
        );
        assert!(
            output.contains("reused base"),
            "the agent's ordinary Git add must use Riftri: {output}"
        );
        assert!(
            fixture.view.exists(),
            "agent exit must not delete the worktree"
        );
        assert!(!fixture.directory.path().join("agent-created").exists());
        assert!(
            git(&fixture.view, &["status", "--porcelain=v1"])
                .stdout
                .is_empty()
        );
        assert_eq!(
            fs::read(fixture.repository.join("tracked.txt")).unwrap(),
            b"tracked\n"
        );
    }
}
