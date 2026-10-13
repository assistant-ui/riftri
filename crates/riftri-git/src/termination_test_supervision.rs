//! Test-only subprocess supervision; no production signal behavior lives here.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output};
use std::time::{Duration, Instant};

const CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) struct TestChild {
    child: Child,
    group: Option<i32>,
    cleaned: bool,
}

impl TestChild {
    pub(super) fn spawn(command: &mut Command, own_group: bool) -> io::Result<Self> {
        if own_group {
            command.process_group(0);
        }
        let child = command.spawn()?;
        let group = own_group.then(|| i32::try_from(child.id()).expect("child PID fits i32"));
        Ok(Self {
            child,
            group,
            cleaned: false,
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn wait(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("test child {} did not exit within {timeout:?}", self.id()),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        if let Some(group) = self.group {
            // The helper has its own group. Sweep it even after the leader
            // exited: a descendant may still hold inherited output handles.
            // SAFETY: this negative PID names only the group we created.
            let result = unsafe { libc::kill(-group, libc::SIGKILL) };
            if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                let _ = writeln!(
                    io::stderr(),
                    "could not stop test group {group}: {}",
                    io::Error::last_os_error()
                );
            }
        } else if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        // A kernel-stuck child can remain unreapable even after SIGKILL.
        // Diagnose it without turning cleanup into another unbounded wait.
        if let Err(error) = self.wait(CLEANUP_TIMEOUT) {
            let _ = writeln!(
                io::stderr(),
                "test cleanup left child {} unreaped: {error}",
                self.id()
            );
        }
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn read_log(file: &mut File) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(64 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn output(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    // Regular files cannot keep a pipe reader waiting for a descendant to
    // close its inherited descriptor. Keep captured diagnostics bounded too.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = TestChild::spawn(command, true)?;
    let status = child.wait(timeout);
    child.cleanup();
    let stdout = read_log(&mut stdout)?;
    let stderr = read_log(&mut stderr)?;
    match status {
        Ok(status) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "{error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
        )),
    }
}

pub(super) fn disable_core_dumps() {
    // SIGQUIT is intentional in this isolated helper. Keep it in coverage,
    // but do not generate crash artifacts while testing signal forwarding.
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads this initialized value and changes this helper
    // process (and its future children), never the parent test runner.
    let result = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) };
    assert_eq!(
        result,
        0,
        "disable helper core dumps: {}",
        io::Error::last_os_error()
    );
}

pub(super) fn fail_wait(mut child: TestChild, error: io::Error) -> ! {
    child.cleanup();
    // Do not let the property engine shrink an environmental timeout by
    // spawning more possibly unreapable children. This runs only in the
    // isolated helper, after the forwarding guard has been disarmed/dropped.
    // Direct stderr bypasses libtest capture, which exit(1) would not flush.
    let _ = writeln!(
        io::stderr(),
        "{error}; stopping helper without shrinking timeout"
    );
    std::process::exit(1);
}

const FIXTURE_ENV: &str = "RIFTRI_TERMINATION_DEADLINE_FIXTURE";

fn fixture_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "termination::hegel_tests::supervision::deadline_fixture",
            "--nocapture",
        ])
        .env(FIXTURE_ENV, mode);
    command
}

#[test]
#[allow(clippy::zombie_processes)] // Outer TestChild owns the descendant's group.
fn deadline_fixture() {
    match std::env::var(FIXTURE_ENV).as_deref() {
        Ok("slow") => std::thread::sleep(Duration::from_secs(1)),
        Ok("hold-output") => {
            fixture_command("descendant").spawn().unwrap();
        }
        Ok("descendant") => std::thread::sleep(Duration::from_secs(10)),
        Ok("fatal-wait") => {
            let mut child = TestChild::spawn(&mut fixture_command("slow"), false).unwrap();
            let error = child.wait(Duration::from_millis(50)).unwrap_err();
            fail_wait(child, error);
        }
        _ => {}
    }
}

#[test]
fn helper_deadline_is_enforced() {
    let error = output(&mut fixture_command("slow"), Duration::from_millis(50)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn a_target_timeout_exits_the_helper_with_diagnostics() {
    let result = output(&mut fixture_command("fatal-wait"), Duration::from_secs(2)).unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("stopping helper without shrinking timeout")
    );
}

#[test]
fn target_deadline_and_cleanup_are_bounded() {
    let mut child = TestChild::spawn(&mut fixture_command("slow"), false).unwrap();
    let error = child.wait(Duration::from_millis(50)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    child.cleanup();
    assert!(
        child.child.try_wait().unwrap().is_some(),
        "fixture must be reaped"
    );
}

#[test]
fn descendant_output_handles_do_not_delay_helper_completion() {
    let started = Instant::now();
    let result = output(&mut fixture_command("hold-output"), Duration::from_secs(2)).unwrap();
    assert!(result.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "did not wait for descendant EOF"
    );
}
