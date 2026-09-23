//! Termination contract of the native `riftri exec` scoped command, exercised
//! against the compiled executable and independent of the npm launcher.

use std::process::{Command, Stdio};

/// `riftri exec` propagates the scoped command's normal exit status.
#[test]
fn exec_propagates_normal_exit_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--"])
        .args(shell_command("exit 7"))
        .stdin(Stdio::null())
        .output()
        .expect("run riftri exec");
    assert_eq!(output.status.code(), Some(7), "exit status must propagate");
}

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    /// Longest any single fixture step may block. The interactive fixtures
    /// read a pty and wait on children; without a bound, one wedged child
    /// hangs the whole test run and an interrupted run leaves the process
    /// tree orphaned (#366). Generous against loaded CI: normal steps finish
    /// in milliseconds.
    const FIXTURE_DEADLINE: Duration = Duration::from_secs(30);

    /// Line reader with a deadline on every read, for pty masters and pipes.
    ///
    /// A pty master read reports `EIO` on Linux once the child side is gone —
    /// the pty's spelling of end-of-file — so both that and a zero-length
    /// read end the stream instead of failing it.
    struct BoundedLines {
        source: File,
        buffered: Vec<u8>,
    }

    impl BoundedLines {
        fn new(source: File) -> Self {
            use std::os::unix::io::AsRawFd;
            // SAFETY: fcntl on an owned, open descriptor.
            unsafe {
                let flags = libc::fcntl(source.as_raw_fd(), libc::F_GETFL);
                assert!(flags != -1, "F_GETFL: {}", std::io::Error::last_os_error());
                assert!(
                    libc::fcntl(source.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) != -1,
                    "F_SETFL: {}",
                    std::io::Error::last_os_error()
                );
            }
            BoundedLines {
                source,
                buffered: Vec::new(),
            }
        }

        /// Next full line within the deadline; `None` once the stream ends.
        /// Panics — instead of hanging — when nothing arrives in time.
        fn next_line(&mut self, waiting_for: &str) -> Option<String> {
            use std::io::Read;
            use std::os::unix::io::AsRawFd;
            let deadline = Instant::now() + FIXTURE_DEADLINE;
            loop {
                if let Some(newline) = self.buffered.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = self.buffered.drain(..=newline).collect();
                    return Some(String::from_utf8_lossy(&line).trim_end().to_owned());
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    remaining > Duration::ZERO,
                    "no output within {FIXTURE_DEADLINE:?} while waiting for {waiting_for}; \
                     partial output: {:?}",
                    String::from_utf8_lossy(&self.buffered)
                );
                let mut poll = libc::pollfd {
                    fd: self.source.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let timeout =
                    libc::c_int::try_from(remaining.as_millis().min(1_000)).expect("poll timeout");
                // SAFETY: polls one valid descriptor owned by this struct.
                let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
                if ready == 0 {
                    continue;
                }
                assert!(ready > 0, "poll: {}", std::io::Error::last_os_error());
                let mut chunk = [0_u8; 512];
                match self.source.read(&mut chunk) {
                    Ok(0) => return self.trailing_line(),
                    Ok(count) => self.buffered.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                        return self.trailing_line();
                    }
                    Err(error) => panic!("read fixture output: {error}"),
                }
            }
        }

        fn trailing_line(&mut self) -> Option<String> {
            if self.buffered.is_empty() {
                return None;
            }
            let line = String::from_utf8_lossy(&self.buffered)
                .trim_end()
                .to_owned();
            self.buffered.clear();
            Some(line)
        }
    }

    /// Wait for `child` within the deadline; panic — instead of hanging — when
    /// it never exits. The caller's `Drop` then kills the process tree.
    fn wait_bounded(child: &mut Child, what: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + FIXTURE_DEADLINE;
        loop {
            if let Some(status) = child.try_wait().expect("poll child status") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "{what} still running after {FIXTURE_DEADLINE:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Install `SIG_IGN` for `signals` in a forked child before it execs,
    /// reproducing what `nohup` does for SIGHUP and what a shell without job
    /// control does to SIGINT and SIGQUIT before starting an asynchronous
    /// command. An ignored disposition survives exec, so the spawned `riftri`
    /// really does inherit it.
    ///
    /// # Safety
    ///
    /// The returned closure runs between fork and exec and calls only the
    /// async-signal-safe `signal(2)`.
    unsafe fn ignore_before_exec<'command>(
        command: &'command mut Command,
        signals: &'static [libc::c_int],
    ) -> &'command mut Command {
        if signals.is_empty() {
            return command;
        }
        // SAFETY: delegated to this function's own safety contract.
        unsafe {
            command.pre_exec(move || {
                for &signal in signals {
                    if libc::signal(signal, libc::SIG_IGN) == libc::SIG_ERR {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            })
        }
    }

    /// A file the scoped command polls for, so a test can hold the command
    /// alive across the signals under test and release it only afterwards. The
    /// command's exit status then reports whether it survived: 7 if it did,
    /// `128 + signal` if it did not.
    struct ReleaseFlag {
        directory: TempDir,
    }

    impl ReleaseFlag {
        const SURVIVED: i32 = 7;

        fn new() -> Self {
            ReleaseFlag {
                directory: tempfile::tempdir().expect("create release-flag directory"),
            }
        }

        fn path(&self) -> PathBuf {
            self.directory.path().join("release")
        }

        /// Shell script that prints its PID, waits for the flag, then exits
        /// with [`ReleaseFlag::SURVIVED`].
        fn script(&self) -> String {
            let path = self.path();
            let path = path.display();
            format!(
                "echo \"$$\"\n\
                 while [ ! -f '{path}' ]; do /bin/sleep 0.05; done\n\
                 exit {}\n",
                Self::SURVIVED
            )
        }

        fn release(&self) {
            std::fs::write(self.path(), b"").expect("release the scoped command");
        }
    }

    /// A spawned `riftri exec` process with no terminal on any standard
    /// descriptor, matching supervised (non-interactive) usage, plus a private
    /// temporary directory that receives the process-scoped Git shim.
    struct SupervisedExec {
        riftri: Child,
        shim_root: TempDir,
    }

    impl SupervisedExec {
        fn spawn(script: &str) -> Self {
            Self::spawn_ignoring(script, &[])
        }

        /// Spawn `riftri exec` from a parent that ignores `ignored`, so riftri
        /// inherits those dispositions exactly as it would under `nohup` or as
        /// an asynchronous command of a shell without job control.
        fn spawn_ignoring(script: &str, ignored: &'static [libc::c_int]) -> Self {
            let shim_root = tempfile::tempdir().expect("create private TMPDIR");
            let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
            command
                .args(["exec", "--", "/bin/sh", "-c", script])
                .env("TMPDIR", shim_root.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // SAFETY: the installed closure calls only async-signal-safe code.
            let riftri = unsafe { ignore_before_exec(&mut command, ignored) }
                .spawn()
                .expect("start riftri exec");
            SupervisedExec { riftri, shim_root }
        }

        /// Read PIDs that the scoped script printed, one per line.
        fn read_pids(&mut self, count: usize) -> Vec<i32> {
            let stdout = self.riftri.stdout.take().expect("riftri stdout is piped");
            let mut lines = BoundedLines::new(File::from(std::os::fd::OwnedFd::from(stdout)));
            (0..count)
                .map(|index| {
                    lines
                        .next_line("a scoped command PID")
                        .unwrap_or_else(|| panic!("scoped command printed PID {index}"))
                        .trim()
                        .parse()
                        .expect("scoped command PID is numeric")
                })
                .collect()
        }

        fn wait_exit_code(mut self) -> i32 {
            let status = wait_bounded(&mut self.riftri, "supervised riftri exec");
            let code = status.code().expect("riftri exec exits with a code");
            assert_shim_removed(self.shim_root.path());
            code
        }
    }

    impl Drop for SupervisedExec {
        /// A panicking or interrupted test must not leave the supervised
        /// riftri (and through it the scoped command) running.
        fn drop(&mut self) {
            if let Ok(None) = self.riftri.try_wait() {
                let _ = self.riftri.kill();
                let _ = self.riftri.wait();
            }
        }
    }

    fn assert_shim_removed(shim_root: &Path) {
        let leftovers: Vec<_> = std::fs::read_dir(shim_root)
            .expect("read private TMPDIR")
            .map(|entry| entry.expect("read TMPDIR entry").file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary Git shim must be cleaned up, found {leftovers:?}"
        );
    }

    fn signal(pid: i32, signal: libc::c_int) {
        assert_eq!(
            // SAFETY: pid names a process this test spawned and still owns.
            unsafe { libc::kill(pid, signal) },
            0,
            "send signal {signal} to {pid}"
        );
    }

    fn assert_terminates(pid: i32, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        // SAFETY: signal 0 only probes for existence.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "{description} (PID {pid}) survived termination"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// PID-directed SIGTERM at riftri alone stops the scoped command and its
    /// descendants, and the temporary Git shim is removed.
    #[test]
    fn sigterm_to_riftri_stops_scoped_child_and_descendants() {
        let mut exec = SupervisedExec::spawn("echo \"$$\"; /bin/sleep 30 & echo \"$!\"; wait");
        let pids = exec.read_pids(2);
        let riftri_pid = i32::try_from(exec.riftri.id()).expect("riftri PID fits i32");
        signal(riftri_pid, libc::SIGTERM);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGTERM);
        assert_terminates(pids[0], "scoped shell");
        assert_terminates(pids[1], "scoped shell descendant");
    }

    /// PID-directed SIGINT at riftri alone stops the scoped command.
    #[test]
    fn sigint_to_riftri_stops_scoped_child() {
        let mut exec = SupervisedExec::spawn("echo \"$$\"; exec /bin/sleep 30");
        let pids = exec.read_pids(1);
        let riftri_pid = i32::try_from(exec.riftri.id()).expect("riftri PID fits i32");
        signal(riftri_pid, libc::SIGINT);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGINT);
        assert_terminates(pids[0], "scoped command");
    }

    /// PID-directed SIGHUP at riftri alone stops the scoped command.
    #[test]
    fn sighup_to_riftri_stops_scoped_child() {
        let mut exec = SupervisedExec::spawn("echo \"$$\"; exec /bin/sleep 30");
        let pids = exec.read_pids(1);
        let riftri_pid = i32::try_from(exec.riftri.id()).expect("riftri PID fits i32");
        signal(riftri_pid, libc::SIGHUP);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGHUP);
        assert_terminates(pids[0], "scoped command");
    }

    /// A scoped command that dies from a signal of its own still maps to the
    /// conventional 128 + signal exit status, with the shim cleaned up.
    #[test]
    fn scoped_child_signal_death_maps_to_exit_status() {
        let mut exec = SupervisedExec::spawn("echo \"$$\"; exec /bin/sleep 30");
        let pids = exec.read_pids(1);
        signal(pids[0], libc::SIGKILL);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGKILL);
    }

    /// Normal completion still works under the forwarding setup, cleans up the
    /// shim, and leaves the exit status untouched.
    #[test]
    fn scoped_child_normal_exit_is_untouched() {
        let mut exec = SupervisedExec::spawn("echo \"$$\"; exit 0");
        let pids = exec.read_pids(1);
        assert_eq!(exec.wait_exit_code(), 0);
        assert_terminates(pids[0], "scoped command");
    }

    /// ioctl request numbers are `c_ulong` on the platforms these tests run
    /// on, while the type of the libc constant varies by target.
    #[allow(clippy::unnecessary_cast)]
    const TIOCSCTTY_REQUEST: libc::c_ulong = libc::TIOCSCTTY as libc::c_ulong;

    /// Open a fresh pseudo-terminal pair, returning `(master, slave)`.
    fn open_pty() -> (File, File) {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        // SAFETY: openpty only writes the two descriptor out-parameters; the
        // name, termios, and winsize pointers are optional and null.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty returned exclusive ownership of both descriptors.
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    /// A spawned `riftri exec` that is the session leader of a fresh
    /// pseudo-terminal, matching interactive usage: every standard descriptor
    /// is the pty slave, the pty is the controlling terminal, and the scoped
    /// command shares riftri's foreground process group.
    struct InteractiveExec {
        riftri: Child,
        output: BoundedLines,
        shim_root: TempDir,
    }

    impl Drop for InteractiveExec {
        /// The fixture runs riftri as its own session leader, so a panicking
        /// or interrupted test can reap the entire interactive process tree
        /// by signalling that group — this is what previously stayed behind
        /// as orphans when the unbounded reads were interrupted.
        fn drop(&mut self) {
            if let Ok(None) = self.riftri.try_wait() {
                signal(-self.pid(), libc::SIGKILL);
                let _ = self.riftri.wait();
            }
        }
    }

    impl InteractiveExec {
        fn spawn(script: &str) -> Self {
            Self::spawn_with(script, &[], &[])
        }

        /// Spawn `riftri exec` from a parent that ignores `ignored`, matching
        /// `nohup riftri exec …` from a terminal.
        fn spawn_ignoring(script: &str, ignored: &'static [libc::c_int]) -> Self {
            Self::spawn_with(script, ignored, &[])
        }

        fn spawn_with(
            script: &str,
            ignored: &'static [libc::c_int],
            environment: &[(&str, &OsStr)],
        ) -> Self {
            let shim_root = tempfile::tempdir().expect("create private TMPDIR");
            let (master, slave) = open_pty();
            let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
            command
                .args(["exec", "--", "/bin/sh", "-c", script])
                .env("TMPDIR", shim_root.path())
                .envs(environment.iter().copied())
                .stdin(Stdio::from(slave.try_clone().expect("duplicate pty slave")))
                .stdout(Stdio::from(slave.try_clone().expect("duplicate pty slave")))
                .stderr(Stdio::from(slave));
            // SAFETY: the closure runs in the forked child before exec and
            // calls only async-signal-safe functions.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // Adopt the pty as the controlling terminal; the fresh
                    // session leader's process group becomes the terminal's
                    // foreground process group.
                    if libc::ioctl(0, TIOCSCTTY_REQUEST, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            // SAFETY: the installed closure calls only async-signal-safe code.
            let riftri = unsafe { ignore_before_exec(&mut command, ignored) }
                .spawn()
                .expect("start riftri exec on a pty");
            InteractiveExec {
                riftri,
                output: BoundedLines::new(master),
                shim_root,
            }
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.riftri.id()).expect("riftri PID fits i32")
        }

        /// Read pty output lines until `marker` appears on its own line.
        fn await_marker(&mut self, marker: &str) {
            loop {
                let line = self
                    .output
                    .next_line(marker)
                    .unwrap_or_else(|| panic!("pty closed before {marker:?} appeared"));
                if line.trim() == marker {
                    return;
                }
            }
        }

        /// Read pty output lines until one is a PID.
        fn read_pid(&mut self) -> i32 {
            loop {
                let line = self
                    .output
                    .next_line("a PID")
                    .expect("pty closed before a PID appeared");
                if let Ok(pid) = line.trim().parse() {
                    return pid;
                }
            }
        }

        fn wait_exit_code(mut self) -> i32 {
            let status = wait_bounded(&mut self.riftri, "interactive riftri exec");
            let code = status.code().expect("riftri exec exits with a code");
            assert_shim_removed(self.shim_root.path());
            code
        }
    }

    /// Keyboard-style SIGINT to the foreground process group must not kill
    /// riftri while the scoped command catches the interrupt and keeps
    /// running: riftri stays alive, keeps waiting for the command's real
    /// exit, propagates its status, and still removes the temporary Git shim.
    #[test]
    fn interactive_sigint_survivor_keeps_riftri_waiting() {
        let mut exec = InteractiveExec::spawn(
            "trap 'trapped=1' INT\n\
             echo READY\n\
             while [ -z \"${trapped-}\" ]; do /bin/sleep 0.1 || :; done\n\
             echo TRAPPED\n\
             /bin/sleep 0.5\n\
             exit 7\n",
        );
        exec.await_marker("READY");
        let riftri_pid = exec.pid();
        // The session leader's PID doubles as the foreground process group
        // ID, so this is exactly the delivery a terminal performs for Ctrl-C.
        signal(-riftri_pid, libc::SIGINT);
        exec.await_marker("TRAPPED");
        assert_eq!(
            // SAFETY: signal 0 only probes for existence.
            unsafe { libc::kill(riftri_pid, 0) },
            0,
            "riftri must survive the foreground SIGINT"
        );
        assert_eq!(exec.wait_exit_code(), 7);
    }

    /// When the scoped command does die from the foreground SIGINT, riftri
    /// reflects the conventional 130 exit status instead of dying alongside
    /// the command, and the shim is still cleaned up.
    #[test]
    fn interactive_sigint_death_propagates_130() {
        let mut exec = InteractiveExec::spawn("echo READY; exec /bin/sleep 30");
        exec.await_marker("READY");
        signal(-exec.pid(), libc::SIGINT);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGINT);
    }

    /// `nohup riftri exec -- …`: SIGHUP and SIGTERM are forwarded signals, but
    /// riftri inherited SIG_IGN for both, and an ignore survives exec. The
    /// scoped command must therefore start immune to them, exactly as it would
    /// under a bare `nohup`, instead of being handed SIG_DFL by the forwarding
    /// setup.
    #[test]
    fn inherited_ignore_of_forwarded_signals_survives_into_scoped_command() {
        let flag = ReleaseFlag::new();
        let mut exec =
            SupervisedExec::spawn_ignoring(&flag.script(), &[libc::SIGHUP, libc::SIGTERM]);
        let pids = exec.read_pids(1);
        let riftri_pid = i32::try_from(exec.riftri.id()).expect("riftri PID fits i32");
        signal(riftri_pid, libc::SIGHUP);
        signal(riftri_pid, libc::SIGTERM);
        flag.release();
        assert_eq!(
            exec.wait_exit_code(),
            ReleaseFlag::SURVIVED,
            "the scoped command must keep the SIG_IGN riftri inherited"
        );
        assert_terminates(pids[0], "scoped command");
    }

    /// The same contract on a terminal: the kernel hangs a terminal up by
    /// signalling the whole foreground process group, and a `nohup`-style
    /// inherited SIG_IGN has to carry through riftri into the scoped command.
    #[test]
    fn interactive_inherited_sighup_ignore_survives_into_scoped_command() {
        let flag = ReleaseFlag::new();
        let mut exec = InteractiveExec::spawn_ignoring(&flag.script(), &[libc::SIGHUP]);
        let scoped_pid = exec.read_pid();
        // The session leader's PID doubles as the foreground process group ID,
        // so this is exactly the delivery the kernel performs on hangup.
        signal(-exec.pid(), libc::SIGHUP);
        flag.release();
        assert_eq!(
            exec.wait_exit_code(),
            ReleaseFlag::SURVIVED,
            "a hangup must not kill a scoped command started under nohup"
        );
        assert_terminates(scoped_pid, "scoped command");
    }

    /// `sh -c 'riftri exec -- … & wait'`: a shell without job control must
    /// start an asynchronous command with SIGINT and SIGQUIT ignored, so
    /// Ctrl-C cannot reach a background job. Supervised mode forwards SIGINT,
    /// which must not turn that inherited immunity into SIG_DFL.
    #[test]
    fn inherited_ignore_keeps_background_scoped_command_immune_to_sigint() {
        let flag = ReleaseFlag::new();
        let mut exec =
            SupervisedExec::spawn_ignoring(&flag.script(), &[libc::SIGINT, libc::SIGQUIT]);
        let pids = exec.read_pids(1);
        let riftri_pid = i32::try_from(exec.riftri.id()).expect("riftri PID fits i32");
        signal(riftri_pid, libc::SIGINT);
        flag.release();
        assert_eq!(
            exec.wait_exit_code(),
            ReleaseFlag::SURVIVED,
            "a background job must keep the POSIX SIGINT immunity it was started with"
        );
        assert_terminates(pids[0], "scoped command");
    }

    /// A stand-in for the real Git executable: it reports its PID and then
    /// stays alive, modelling a long `git clone`. `riftri exec` resolves the
    /// real Git as the first `git` on `PATH`, so putting this directory first
    /// makes the process-scoped shim delegate to it.
    struct RealGitStandIn {
        directory: TempDir,
    }

    impl RealGitStandIn {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("create real-Git stand-in directory");
            let executable = directory.path().join("git");
            // SIGHUP is ignored because the pty fixture makes riftri itself the
            // session leader, so riftri's exit hangs up the foreground process
            // group. Without the ignore this stand-in could die from that
            // artifact rather than from the SIGTERM forwarding under test.
            std::fs::write(
                &executable,
                "#!/bin/sh\ntrap '' HUP\necho \"$$\"\nexec /bin/sleep 300\n",
            )
            .expect("write real-Git stand-in");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                .expect("make the real-Git stand-in executable");
            RealGitStandIn { directory }
        }

        /// `PATH` with the stand-in's directory first.
        fn path(&self) -> OsString {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            std::env::join_paths(
                std::iter::once(self.directory.path().to_path_buf())
                    .chain(std::env::split_paths(&inherited)),
            )
            .expect("build a PATH with the real-Git stand-in first")
        }
    }

    /// Terminating riftri while the scoped command is `git` must reach the
    /// real Git. The scoped `git` is riftri's own shim, so a PID-directed
    /// SIGTERM stops at the shim unless the shim forwards it on; before that
    /// forwarding existed the real Git survived, orphaned, still writing.
    #[test]
    fn interactive_sigterm_reaches_the_real_git_behind_the_shim() {
        let real_git = RealGitStandIn::new();
        let mut exec = InteractiveExec::spawn_with(
            "echo \"$$\"\nexec git hold\n",
            &[],
            &[("PATH", real_git.path().as_os_str())],
        );
        let shim_pid = exec.read_pid();
        let real_git_pid = exec.read_pid();
        assert_ne!(shim_pid, real_git_pid, "the shim must run the real Git");
        signal(exec.pid(), libc::SIGTERM);
        assert_eq!(exec.wait_exit_code(), 128 + libc::SIGTERM);
        assert_terminates(real_git_pid, "real Git behind the process-scoped shim");
        assert_terminates(shim_pid, "process-scoped Git shim");
    }
}

#[cfg(unix)]
fn shell_command(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

#[cfg(windows)]
fn shell_command(script: &str) -> Vec<String> {
    vec!["cmd".into(), "/C".into(), script.into()]
}
