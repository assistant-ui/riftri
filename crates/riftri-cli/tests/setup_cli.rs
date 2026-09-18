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
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Output, Stdio};
    use std::time::{Duration, Instant};

    use super::support;

    struct Fixture {
        directory: support::WritableTempDir,
        repository: PathBuf,
        view: PathBuf,
    }

    enum SignalAt {
        Prompt(i32),
        Progress(i32),
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

        fn run(&self, input: &[u8]) -> (ExitStatus, String) {
            self.run_terminal(&[input], false)
        }

        fn run_terminal(&self, answers: &[&[u8]], rich: bool) -> (ExitStatus, String) {
            self.run_terminal_options(answers, rich, None, &[])
        }

        fn run_terminal_options(
            &self,
            answers: &[&[u8]],
            rich: bool,
            signal: Option<SignalAt>,
            options: &[&str],
        ) -> (ExitStatus, String) {
            let mut master = -1;
            let mut slave = -1;
            let mut size = libc::winsize {
                ws_row: 24,
                ws_col: 90,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: openpty writes two fresh descriptors; optional pointers are null.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::addr_of_mut!(size),
                    )
                },
                0
            );
            // SAFETY: these descriptors were returned with exclusive ownership by openpty.
            let (mut master, slave) =
                unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            let inspector = slave.try_clone().unwrap();
            let mut process = command(env!("CARGO_BIN_EXE_riftri"));
            if !rich {
                process.arg("--plain");
            }
            process.args(options);
            process
                .env("TERM", "xterm-256color")
                .env_remove("CI")
                .env_remove("NO_COLOR")
                .env_remove("RIFTRI_NO_ANIMATION")
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
            let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
            let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut output = Vec::new();
                let mut buffer = [0; 4096];
                let mut prompts = 0;
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            output.extend_from_slice(&buffer[..count]);
                            let count = output
                                .windows(b"\x1b[?2004h".len())
                                .filter(|window| *window == b"\x1b[?2004h")
                                .count();
                            for _ in prompts..count {
                                let _ = ready_sender.send(());
                            }
                            prompts = count;
                            if output
                                .windows(b"waiting:".len())
                                .any(|window| window == b"waiting:")
                            {
                                let _ = progress_sender.send(());
                            }
                        }
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => panic!("read setup terminal: {error}"),
                    }
                }
                let _ = sender.send(String::from_utf8_lossy(&output).into_owned());
            });
            for answer in answers {
                if rich
                    && ready_receiver
                        .recv_timeout(Duration::from_secs(10))
                        .is_err()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(inspector);
                    panic!(
                        "rich prompt did not open: {}",
                        receiver
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap_or_default()
                    );
                }
                master.write_all(answer).unwrap();
            }
            if let Some(signal) = signal {
                let (signal, receiver) = match signal {
                    SignalAt::Prompt(signal) => (signal, &ready_receiver),
                    SignalAt::Progress(signal) => (signal, &progress_receiver),
                };
                if receiver.recv_timeout(Duration::from_secs(10)).is_err() {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("signal test did not reach its synchronization point");
                }
                // SAFETY: the child is still owned by this test.
                assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
                let deadline = Instant::now() + Duration::from_secs(3);
                while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if child.try_wait().unwrap().is_none() {
                    child.kill().unwrap();
                }
            }
            let status = wait(&mut child);
            drop(child);
            // SAFETY: master is an open terminal descriptor and mode is writable.
            let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
            assert_eq!(
                unsafe { libc::tcgetattr(master.as_raw_fd(), mode.as_mut_ptr()) },
                0
            );
            // SAFETY: tcgetattr initialized mode above.
            let mode = unsafe { mode.assume_init() };
            assert_ne!(
                mode.c_lflag & libc::ICANON,
                0,
                "terminal must leave raw mode"
            );
            assert_ne!(mode.c_lflag & libc::ECHO, 0, "terminal must restore echo");
            drop(inspector);
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
    fn rich_setup_defaults_to_no_and_restores_the_terminal_on_cancel_or_interrupt() {
        for (answer, code) in [(&b"\r"[..], 0), (&b"\x1b"[..], 0), (&b"\x03"[..], 130)] {
            let fixture = Fixture::new();
            if !fixture.supported() {
                return;
            }
            let (status, output) = fixture.run_terminal(&[answer], true);
            assert_eq!(status.code(), Some(code), "{output}");
            assert!(
                output.contains("\x1b[?1049h"),
                "Ratatui must open its screen: {output}"
            );
            assert!(
                output.contains("\x1b[?1049l"),
                "screen must be restored: {output}"
            );
            assert!(
                output.contains("\x1b[?25h"),
                "cursor must be visible: {output}"
            );
            fixture.no_mutation();
        }
    }

    #[test]
    fn rich_setup_creates_a_clean_isolated_worktree_and_not_now_does_not_enable() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let (status, output) = fixture.run_terminal(&[b"\x1b[B\r", b"\r"], true);
        assert!(status.success(), "{output}");
        assert!(
            git(&fixture.view, &["status", "--porcelain=v1"])
                .stdout
                .is_empty()
        );
        assert!(
            !git(
                &fixture.repository,
                &["config", "--local", "--get", "riftri.enabled"]
            )
            .status
            .success()
        );
        fs::write(fixture.view.join("tracked.txt"), "private change\n").unwrap();
        assert_eq!(
            fs::read(fixture.repository.join("tracked.txt")).unwrap(),
            b"tracked\n"
        );
        assert!(output.contains("Worktree kept"));
    }

    #[test]
    fn rich_setup_restores_terminal_before_agent_and_preserves_child_exit_code() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let agent = fixture.directory.path().join("terminal-check-agent");
        fs::write(&agent, "#!/bin/sh\nset -eu\nstty -a | grep -q -- '-icanon' && exit 99\ntest \"$RIFTRI_SHIM_ACTIVE\" = 1\nprintf 'agent-has-terminal\\n'\nexit 7\n").unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let path = format!("{}\r", agent.display());
        let (status, output) =
            fixture.run_terminal(&[b"y\r", b"3\r", path.as_bytes(), b"y\r"], true);
        assert_eq!(status.code(), Some(7), "{output}");
        let restored = output.rfind("\x1b[?1049l").unwrap();
        assert!(output.find("agent-has-terminal").unwrap() > restored);
        assert!(fixture.view.exists());
    }

    #[test]
    fn rich_setup_restores_terminal_on_external_termination() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let (status, output) =
            fixture.run_terminal_options(&[], true, Some(SignalAt::Prompt(libc::SIGTERM)), &[]);
        assert_eq!(status.code(), Some(128 + libc::SIGTERM), "{output}");
        assert!(output.contains("\x1b[?1049l"));
        fixture.no_mutation();
    }

    #[test]
    fn reduced_motion_keeps_keyboard_setup_but_uses_plain_phase_lines() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        let (status, output) =
            fixture.run_terminal_options(&[b"y\r", b"\r"], true, None, &["--no-animation"]);
        assert!(status.success(), "{output}");
        assert!(output.contains("\x1b[?1049h"));
        assert!(output.contains("riftri: worktree-add: intent-recorded"));
        assert!(!output.contains('⠋'));
        assert!(!output.contains('⠙'));
    }

    #[test]
    fn rich_setup_restores_normal_termination_after_confirmation() {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let fixture = Fixture::new();
            if !fixture.supported() {
                return;
            }
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(
                    fixture
                        .repository
                        .join(".git/riftri-worktree-metadata.lock"),
                )
                .unwrap();
            fs2::FileExt::lock_exclusive(&lock).unwrap();
            let (status, output) = fixture.run_terminal_options(
                &[b"y\r"],
                true,
                Some(SignalAt::Progress(signal)),
                &["--no-animation"],
            );
            drop(lock);
            assert_eq!(
                status.signal(),
                Some(signal),
                "signal must not be swallowed after the prompt: {output}"
            );
            assert!(!fixture.view.exists());
        }
    }

    #[test]
    fn json_error_receipt_stays_undecorated_even_in_a_terminal() {
        let fixture = Fixture::new();
        let (status, output) = fixture.run_terminal_options(&[], true, None, &["--json-errors"]);
        assert!(!status.success());
        assert!(!output.contains('\x1b'));
        let receipt: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(receipt["operation"], "setup");
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
    fn occupied_destination_is_preserved_without_launching_an_agent() {
        let fixture = Fixture::new();
        if !fixture.supported() {
            return;
        }
        fs::create_dir(&fixture.view).unwrap();
        fs::write(fixture.view.join("keep.txt"), "user data\n").unwrap();
        let (status, output) = fixture.run(b"y\n");
        assert!(!status.success(), "{output}");
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
