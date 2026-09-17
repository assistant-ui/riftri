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
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    /// A spawned `riftri exec` process with no terminal on any standard
    /// descriptor, matching supervised (non-interactive) usage, plus a private
    /// temporary directory that receives the process-scoped Git shim.
    struct SupervisedExec {
        riftri: Child,
        shim_root: TempDir,
    }

    impl SupervisedExec {
        fn spawn(script: &str) -> Self {
            let shim_root = tempfile::tempdir().expect("create private TMPDIR");
            let riftri = Command::new(env!("CARGO_BIN_EXE_riftri"))
                .args(["exec", "--", "/bin/sh", "-c", script])
                .env("TMPDIR", shim_root.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start riftri exec");
            SupervisedExec { riftri, shim_root }
        }

        /// Read PIDs that the scoped script printed, one per line.
        fn read_pids(&mut self, count: usize) -> Vec<i32> {
            let stdout = self.riftri.stdout.take().expect("riftri stdout is piped");
            let mut lines = BufReader::new(stdout).lines();
            (0..count)
                .map(|index| {
                    lines
                        .next()
                        .unwrap_or_else(|| panic!("scoped command printed PID {index}"))
                        .expect("read scoped command PID")
                        .trim()
                        .parse()
                        .expect("scoped command PID is numeric")
                })
                .collect()
        }

        fn wait_exit_code(mut self) -> i32 {
            let status = self.riftri.wait().expect("wait for riftri exec");
            let code = status.code().expect("riftri exec exits with a code");
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
        output: BufReader<File>,
        shim_root: TempDir,
    }

    impl InteractiveExec {
        fn spawn(script: &str) -> Self {
            let shim_root = tempfile::tempdir().expect("create private TMPDIR");
            let (master, slave) = open_pty();
            let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
            command
                .args(["exec", "--", "/bin/sh", "-c", script])
                .env("TMPDIR", shim_root.path())
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
            let riftri = command.spawn().expect("start riftri exec on a pty");
            InteractiveExec {
                riftri,
                output: BufReader::new(master),
                shim_root,
            }
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.riftri.id()).expect("riftri PID fits i32")
        }

        /// Read pty output lines until `marker` appears on its own line.
        fn await_marker(&mut self, marker: &str) {
            loop {
                let mut line = String::new();
                let read = self.output.read_line(&mut line).expect("read pty output");
                assert!(read > 0, "pty closed before {marker:?} appeared");
                if line.trim() == marker {
                    return;
                }
            }
        }

        fn wait_exit_code(mut self) -> i32 {
            let status = self.riftri.wait().expect("wait for riftri exec");
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
}

#[cfg(unix)]
fn shell_command(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

#[cfg(windows)]
fn shell_command(script: &str) -> Vec<String> {
    vec!["cmd".into(), "/C".into(), script.into()]
}
