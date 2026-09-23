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
    use std::os::unix::io::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    /// Every blocking step in these fixtures is bounded by this. A hang here
    /// used to block a whole `cargo test` run and leave the riftri proxy, the
    /// Git shim, and the stand-in command alive after the interrupt.
    fn fixture_timeout() -> Duration {
        Duration::from_secs(
            std::env::var("RIFTRI_TEST_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(60),
        )
    }

    fn deadline() -> Instant {
        Instant::now() + fixture_timeout()
    }

    /// Kill and reap a process, or a whole group when `group` is set. Safe to
    /// call on an already-reaped child: ESRCH is the expected answer then.
    fn terminate(child: &mut Child, group: bool) {
        let pid = i32::try_from(child.id()).expect("child PID fits i32");
        let target = if group { -pid } else { pid };
        // SAFETY: the target is a process this fixture spawned.
        unsafe { libc::kill(target, libc::SIGKILL) };
        let _ = child.wait();
    }

    /// Block until `fd` has data or the deadline passes. Returns false on
    /// timeout so the caller can report what it was waiting for.
    fn wait_readable(fd: libc::c_int, deadline: Instant) -> bool {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let mut poller = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = libc::c_int::try_from(remaining.as_millis()).unwrap_or(libc::c_int::MAX);
            // SAFETY: one initialized pollfd describing a descriptor we own.
            let ready = unsafe { libc::poll(&mut poller, 1, millis) };
            if ready > 0 {
                return true;
            }
            if ready == 0 {
                return false;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                panic!("poll for fixture output: {error}");
            }
        }
    }

    /// A line reader with a deadline. `BufReader::read_line` cannot be
    /// interrupted, so a child that never writes the expected line blocks the
    /// test forever.
    struct BoundedLines<R> {
        source: R,
        fd: libc::c_int,
        pending: Vec<u8>,
    }

    impl<R: std::io::Read> BoundedLines<R> {
        fn new(source: R, fd: libc::c_int) -> Self {
            BoundedLines {
                source,
                fd,
                pending: Vec::new(),
            }
        }

        /// The next line, or None at end of input. Panics on timeout, naming
        /// `what` and dumping whatever arrived before the deadline.
        fn next_line(&mut self, deadline: Instant, what: &str) -> Option<String> {
            loop {
                if let Some(index) = self.pending.iter().position(|&byte| byte == b'\n') {
                    let line: Vec<u8> = self.pending.drain(..=index).collect();
                    return Some(String::from_utf8_lossy(&line).into_owned());
                }
                if !wait_readable(self.fd, deadline) {
                    panic!(
                        "timed out after {:?} waiting for {what}; output so far: {:?}",
                        fixture_timeout(),
                        String::from_utf8_lossy(&self.pending),
                    );
                }
                let mut chunk = [0u8; 4096];
                match self.source.read(&mut chunk) {
                    Ok(0) => return None,
                    Ok(count) => self.pending.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    // A pty master reports EIO once the last slave closes,
                    // which is this reader's end of input.
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => return None,
                    Err(error) => panic!("read fixture output while waiting for {what}: {error}"),
                }
            }
        }
    }

    /// Wait for `child` to exit, killing its process group if it overruns.
    fn bounded_exit_code(child: &mut Child, group: bool, what: &str) -> i32 {
        let deadline = deadline();
        loop {
            match child.try_wait().expect("poll for fixture exit") {
                Some(status) => {
                    return status
                        .code()
                        .unwrap_or_else(|| panic!("{what} exited without a code: {status:?}"));
                }
                None if Instant::now() >= deadline => {
                    let pid = child.id();
                    terminate(child, group);
                    panic!(
                        "timed out after {:?} waiting for {what} (pid {pid}); \
                         its process group was killed",
                        fixture_timeout(),
                    );
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
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
        reaped: bool,
    }

    impl Drop for SupervisedExec {
        fn drop(&mut self) {
            // Runs on the panic path too, so a failed assertion cannot leave
            // riftri and its stand-in Git alive. This child did not setsid, so
            // only the process itself may be signalled; killing its group
            // would kill the test runner.
            if !self.reaped {
                terminate(&mut self.riftri, false);
            }
        }
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
            SupervisedExec {
                riftri,
                shim_root,
                reaped: false,
            }
        }

        /// Read PIDs that the scoped script printed, one per line.
        fn read_pids(&mut self, count: usize) -> Vec<i32> {
            let stdout = self.riftri.stdout.take().expect("riftri stdout is piped");
            let fd = stdout.as_raw_fd();
            let mut lines = BoundedLines::new(stdout, fd);
            let deadline = deadline();
            (0..count)
                .map(|index| {
                    lines
                        .next_line(deadline, &format!("scoped command PID {index}"))
                        .unwrap_or_else(|| panic!("scoped command printed PID {index}"))
                        .trim()
                        .parse()
                        .expect("scoped command PID is numeric")
                })
                .collect()
        }

        fn wait_exit_code(mut self) -> i32 {
            let code = bounded_exit_code(&mut self.riftri, false, "riftri exec");
            self.reaped = true;
            assert_shim_removed(self.shim_root.path());
            code
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
        output: BoundedLines<File>,
        shim_root: TempDir,
        reaped: bool,
    }

    impl Drop for InteractiveExec {
        fn drop(&mut self) {
            // This child is a session leader from setsid(), so its process
            // group holds riftri, the Git shim, and the stand-in command.
            // Killing the group on every path, panics included, is what stops
            // a failed run from leaving them behind.
            if !self.reaped {
                terminate(&mut self.riftri, true);
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
            let master_fd = master.as_raw_fd();
            InteractiveExec {
                riftri,
                output: BoundedLines::new(master, master_fd),
                shim_root,
                reaped: false,
            }
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.riftri.id()).expect("riftri PID fits i32")
        }

        /// Read pty output lines until `marker` appears on its own line.
        fn await_marker(&mut self, marker: &str) {
            let deadline = deadline();
            let what = format!("marker {marker:?}");
            loop {
                let line = self
                    .output
                    .next_line(deadline, &what)
                    .unwrap_or_else(|| panic!("pty closed before {marker:?} appeared"));
                if line.trim() == marker {
                    return;
                }
            }
        }

        /// Read pty output lines until one is a PID.
        fn read_pid(&mut self) -> i32 {
            let deadline = deadline();
            loop {
                let line = self
                    .output
                    .next_line(deadline, "a scoped command PID")
                    .unwrap_or_else(|| panic!("pty closed before a PID appeared"));
                if let Ok(pid) = line.trim().parse() {
                    return pid;
                }
            }
        }

        fn wait_exit_code(mut self) -> i32 {
            let code = bounded_exit_code(&mut self.riftri, true, "interactive riftri exec");
            self.reaped = true;
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
                // riftri resolves Git aliases before planning, so the shim
                // runs `git config --get alias.<command>` first. A stand-in
                // that sleeps for every invocation never answers that probe
                // and the fixture deadlocks on itself. Real Git exits 1 for an
                // unset alias, so do that and sleep only for the real command.
                "#!/bin/sh\n\
                 case \"$1\" in config) exit 1 ;; esac\n\
                 trap '' HUP\n\
                 echo \"$$\"\n\
                 exec /bin/sleep 300\n",
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
