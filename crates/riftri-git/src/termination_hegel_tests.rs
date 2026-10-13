//! Property tests for the process-global forwarding guard.
//!
//! The guard owns this process's signal dispositions and a single waiter
//! slot, so the properties run in a re-invoked copy of the test binary: there
//! they cannot race the other tests that install a guard, and a signal that
//! escapes forwarding kills the helper instead of the whole suite.

use std::process::{Command, Stdio};

use hegel::TestCase;
use hegel::generators as gs;

use super::TerminationError;
use super::forwarding::ForwardingGuard;

#[path = "termination_test_supervision.rs"]
mod supervision;

/// Names the property a re-invoked test binary should run; unset in a plain
/// run of the suite, where the properties below return immediately.
const HELPER_ENV: &str = "RIFTRI_TERMINATION_HEGEL_HELPER";

const CATCHABLE: [libc::c_int; 6] = [
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGUSR1,
    libc::SIGUSR2,
];
/// Numbers sigaction(2) refuses: out of range, or impossible to catch.
const UNCATCHABLE: [libc::c_int; 5] = [0, -1, libc::SIGKILL, libc::SIGSTOP, 10_000];

fn runs_in_helper(property: &str) -> bool {
    std::env::var(HELPER_ENV).is_ok_and(|selected| selected == property)
}

fn run_in_helper(property: &str) {
    let executable = std::env::current_exe().expect("locate the test executable");
    let mut command = Command::new(executable);
    command
        .args(["--exact", &format!("termination::hegel_tests::{property}")])
        .env(HELPER_ENV, property);
    let output = supervision::output(&mut command, std::time::Duration::from_secs(30))
        .expect("run the forwarding helper subprocess");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{:?}\nstdout: {stdout}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

fn disposition(signal: libc::c_int) -> libc::sighandler_t {
    // SAFETY: a null new action only reads the current disposition into the
    // zero-initialized structure.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut current), 0);
        current.sa_sigaction
    }
}

#[test]
fn guard_restores_every_disposition_it_replaced() {
    run_in_helper("guard_restores_every_disposition_it_replaced_property");
}

#[hegel::test]
fn guard_restores_every_disposition_it_replaced_property(tc: TestCase) {
    if !runs_in_helper("guard_restores_every_disposition_it_replaced_property") {
        return;
    }
    let inherited = CATCHABLE
        .iter()
        .map(|&signal| {
            let handler = if tc.draw(gs::booleans()) {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            // SAFETY: signal(2) only replaces this helper's disposition.
            unsafe {
                libc::signal(signal, handler);
            }
            (signal, handler)
        })
        .collect::<Vec<_>>();
    let candidates = [&CATCHABLE[..], &UNCATCHABLE].concat();
    // The guard is only ever given distinct signals.
    let requested = tc.draw(gs::samples(candidates).max_size(6).without_replacement());
    let split = tc.draw(gs::integers::<usize>().max_value(requested.len()));
    let (forwarded, ignored) = requested.split_at(split);

    let installed = ForwardingGuard::install(forwarded, ignored);

    match (
        requested.iter().find(|signal| UNCATCHABLE.contains(signal)),
        &installed,
    ) {
        (Some(&refused), Err(TerminationError::Disposition { signal, .. })) => {
            assert_eq!(*signal, refused);
        }
        (None, Ok(guard)) => {
            let expected = requested
                .iter()
                .map(|signal| {
                    *inherited
                        .iter()
                        .find(|(inherited, _)| inherited == signal)
                        .expect("a catchable signal")
                })
                .collect::<Vec<_>>();
            assert_eq!(guard.inherited_dispositions(), expected);
            for &signal in forwarded {
                let handler = disposition(signal);
                assert!(handler != libc::SIG_DFL && handler != libc::SIG_IGN);
            }
            for &signal in ignored {
                assert_eq!(disposition(signal), libc::SIG_IGN);
            }
            assert!(matches!(
                ForwardingGuard::install(&[], &[]),
                Err(TerminationError::AlreadyWaiting)
            ));
        }
        (refused, _) => panic!(
            "refused signal {refused:?} but install returned {:?}",
            installed.as_ref().map(|_| ())
        ),
    }

    drop(installed);
    for (signal, handler) in inherited {
        assert_eq!(disposition(signal), handler, "signal {signal}");
    }
    // A refused or finished wait must leave the waiter slot free.
    drop(ForwardingGuard::install(&[], &[]).expect("install after the previous guard"));
}

#[test]
fn a_signal_reaches_the_child_whether_it_lands_before_or_after_arming() {
    run_in_helper("a_signal_reaches_the_child_whether_it_lands_before_or_after_arming_property");
}

#[hegel::test(test_cases = 20)]
fn a_signal_reaches_the_child_whether_it_lands_before_or_after_arming_property(tc: TestCase) {
    use std::os::unix::process::ExitStatusExt;

    if !runs_in_helper(
        "a_signal_reaches_the_child_whether_it_lands_before_or_after_arming_property",
    ) {
        return;
    }
    let signal = tc.draw(gs::sampled_from(CATCHABLE[..5].to_vec()));
    supervision::disable_core_dumps();
    let lands_before_arming = tc.draw(gs::booleans());
    // SAFETY: signal(2) only replaces this helper's disposition; the child
    // must not inherit an ignore left behind by an earlier case.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
    }
    let guard = ForwardingGuard::install(&[signal], &[]).expect("install forwarding");
    // `cat` blocks on its open standard input until a signal ends it.
    let mut command = Command::new("cat");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child =
        supervision::TestChild::spawn(&mut command, false).expect("start the forwarding target");
    let target = i32::try_from(child.id()).expect("child PID fits i32");

    let raise = || {
        // SAFETY: raise(3) delivers to this helper, whose forwarding handler
        // either records the signal or passes it to the armed target.
        unsafe {
            libc::raise(signal);
        }
    };
    if lands_before_arming {
        raise();
        guard.arm(target);
    } else {
        guard.arm(target);
        raise();
    }

    let status = child.wait(std::time::Duration::from_secs(5));
    guard.disarm();
    drop(guard);
    let status = match status {
        Ok(status) => status,
        Err(error) => supervision::fail_wait(child, error),
    };
    assert_eq!(status.signal(), Some(signal), "{status:?}");
}
