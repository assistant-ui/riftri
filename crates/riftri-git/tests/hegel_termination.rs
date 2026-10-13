//! Property tests for waiting on a child under the termination contract:
//! the child's own end is reported unchanged, a termination signal sent to
//! the waiter reaches the child, and a second wait is refused.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Mutex;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_git::termination::{TerminationError, run_forwarding_terminations};
use riftri_git::{Git, GitError};

/// The forwarding state is process-global and serves one wait at a time,
/// so the properties that wait on a child take turns.
static WAITER: Mutex<()> = Mutex::new(());

fn waiter() -> std::sync::MutexGuard<'static, ()> {
    WAITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

/// A FIFO the child blocks on. Opening the write end returns only once the
/// child has opened the read end, which proves the child is running and so
/// that the waiter installed its forwarding before starting it.
fn fifo(directory: &tempfile::TempDir) -> std::path::PathBuf {
    let path = directory.path().join("gate");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path in a directory this
    // test owns.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    path
}

#[hegel::test(test_cases = 30)]
fn a_child_that_ends_on_its_own_is_reported_unchanged(tc: TestCase) {
    let _turn = waiter();
    let code = tc.draw(gs::integers::<u8>());
    let script = format!("exit {code}");

    let status = run_forwarding_terminations(&mut shell(&script)).expect("wait for the child");
    assert_eq!(status.code(), Some(i32::from(code)));
    let status = Git::new("/bin/sh")
        .passthrough(&[OsString::from("-c"), OsString::from(script)])
        .expect("pass through to the stand-in");
    assert_eq!(status.code(), Some(i32::from(code)));

    let signal = tc.draw(gs::sampled_from(vec![
        libc::SIGKILL,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGUSR1,
    ]));
    let status = run_forwarding_terminations(&mut shell(&format!("kill -{signal} $$")))
        .expect("wait for the child");
    assert_eq!(status.signal(), Some(signal));
}

#[hegel::test(test_cases = 30)]
fn a_command_that_cannot_start_leaves_the_next_wait_usable(tc: TestCase) {
    let _turn = waiter();
    let name: String = tc.draw(gs::text().alphabet("abc-_ ").min_size(1).max_size(8));
    let missing = std::path::Path::new("/nonexistent-riftri-directory").join(name);

    let error = run_forwarding_terminations(&mut Command::new(&missing))
        .expect_err("a missing program cannot start");
    assert!(matches!(error, TerminationError::Spawn(_)), "{error:?}");
    let passthrough = Git::new(&missing).passthrough(&[]);
    match passthrough {
        Err(GitError::Start { command, .. }) => assert_eq!(command, missing),
        other => panic!("expected a start failure, got {other:?}"),
    }

    let status = run_forwarding_terminations(&mut shell("exit 0")).expect("wait after a failure");
    assert!(status.success());
}

#[hegel::test]
fn termination_errors_describe_their_cause(tc: TestCase) {
    use std::error::Error;

    let message: String = tc.draw(gs::text().max_size(12));
    let signal = tc.draw(gs::integers::<i32>());
    let cause = || std::io::Error::other(message.clone());

    for error in [
        TerminationError::Spawn(cause()),
        TerminationError::Wait(cause()),
    ] {
        assert_eq!(error.to_string(), message);
        assert_eq!(error.source().expect("an I/O cause").to_string(), message);
    }
    let disposition = TerminationError::Disposition {
        signal,
        source: cause(),
    };
    assert_eq!(
        disposition.to_string(),
        format!("install termination handling for signal {signal}: {message}")
    );
    assert_eq!(
        disposition.source().expect("an I/O cause").to_string(),
        message
    );
    assert!(TerminationError::AlreadyWaiting.source().is_none());
    assert!(
        TerminationError::AlreadyWaiting
            .to_string()
            .contains("one child")
    );
}

#[hegel::test(test_cases = 20)]
fn a_second_wait_is_refused_without_disturbing_the_first(tc: TestCase) {
    let _turn = waiter();
    let code = tc.draw(gs::integers::<u8>());
    let attempts = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    let directory = tempfile::tempdir().unwrap();
    let gate = fifo(&directory);
    let script = format!("read line < '{}'; exit {code}", gate.display());

    let status = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            Git::new("/bin/sh").passthrough(&[OsString::from("-c"), OsString::from(&script)])
        });
        let release = OpenOptions::new().write(true).open(&gate).unwrap();
        for _ in 0..attempts {
            let refused = Git::new("/bin/sh")
                .passthrough(&[OsString::from("-c"), OsString::from("exit 0")])
                .expect_err("an overlapping wait must be refused");
            assert!(refused.to_string().contains("only one child"), "{refused}");
        }
        drop(release);
        first.join().unwrap()
    });

    assert_eq!(
        status.expect("the first wait").code(),
        Some(i32::from(code))
    );
}

#[hegel::test(test_cases = 20)]
fn a_termination_signal_sent_to_the_waiter_ends_the_child(tc: TestCase) {
    let _turn = waiter();
    // SIGTERM and SIGHUP are forwarded whether or not a terminal is shared.
    let signal = tc.draw(gs::sampled_from(vec![libc::SIGTERM, libc::SIGHUP]));
    let directory = tempfile::tempdir().unwrap();
    let gate = fifo(&directory);
    let script = format!("exec cat '{}'", gate.display());

    let status = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| run_forwarding_terminations(&mut shell(&script)));
        let held_open = OpenOptions::new().write(true).open(&gate).unwrap();
        // SAFETY: kill(2) signals this process, whose disposition for
        // `signal` is the waiter's forwarding handler while the child runs.
        assert_eq!(unsafe { libc::kill(libc::getpid(), signal) }, 0);
        let status = waiting.join().unwrap();
        drop(held_open);
        status
    });

    assert_eq!(status.expect("reap the child").signal(), Some(signal));
}
