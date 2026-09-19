//! Termination-signal discipline for a child process Riftri must never
//! abandon.
//!
//! Two Riftri processes wait on a child whose death or survival the user
//! attributes to Riftri: `riftri exec` waiting on the scoped command, and the
//! process-scoped Git shim waiting on the real Git executable it delegates to.
//! Both need the same contract, so both go through [`run_forwarding_terminations`].
//!
//! The contract has three parts.
//!
//! * **Forwarding.** A termination signal delivered to the waiting process is
//!   forwarded to the child instead of killing the waiter outright, so the
//!   waiter survives long enough to reap the child, clean up, and propagate the
//!   child's exit status.
//! * **Inherited dispositions.** The child starts from the dispositions the
//!   waiting process itself inherited. An ignored disposition — unlike a caught
//!   handler — survives `exec`, so `nohup`'s ignored SIGHUP and the ignored
//!   SIGINT/SIGQUIT a job-control-less shell gives an asynchronous command must
//!   be reinstalled in the child before it execs. Everything else becomes
//!   SIG_DFL, because a caught handler cannot cross `exec` anyway.
//! * **Delivery scope.** A supervised waiter — one that does not own the
//!   controlling terminal in the foreground — runs the child in its own process
//!   group and forwards to that whole group, stopping the child's descendants
//!   without touching unrelated processes. An interactive waiter keeps the
//!   child in its own process group so terminal job control and keyboard signal
//!   delivery are unchanged; SIGTERM and SIGHUP are then forwarded to the child
//!   itself, while SIGINT and SIGQUIT are ignored by the waiter for the
//!   duration of the wait, exactly like a shell waiting on a foreground job.
//!   The terminal already delivers both to the whole foreground process group,
//!   so the child alone decides whether the interrupt is fatal, and a child
//!   that catches Ctrl-C keeps running under an intact waiter.
//!
//! All dispositions the waiter replaced are restored once the child is reaped.

use std::io;
use std::process::{Command, ExitStatus};

/// Why a child could not be run under the termination contract.
#[derive(Debug)]
pub enum TerminationError {
    /// A signal disposition could not be installed.
    Disposition {
        /// The signal number whose disposition could not be replaced.
        signal: i32,
        source: io::Error,
    },
    /// The child could not be started.
    Spawn(io::Error),
    /// The child could not be reaped.
    Wait(io::Error),
}

impl std::fmt::Display for TerminationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TerminationError::Disposition { signal, source } => {
                write!(
                    formatter,
                    "install termination handling for signal {signal}: {source}"
                )
            }
            TerminationError::Spawn(source) | TerminationError::Wait(source) => {
                write!(formatter, "{source}")
            }
        }
    }
}

impl std::error::Error for TerminationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TerminationError::Disposition { source, .. }
            | TerminationError::Spawn(source)
            | TerminationError::Wait(source) => Some(source),
        }
    }
}

/// Run `command` to completion under the termination contract described in the
/// module documentation.
#[cfg(unix)]
pub fn run_forwarding_terminations(command: &mut Command) -> Result<ExitStatus, TerminationError> {
    use std::os::unix::process::CommandExt;

    let interactive = shares_foreground_terminal();
    if !interactive {
        command.process_group(0);
    }
    // SIGQUIT gets the same treatment as SIGINT in interactive mode because it
    // is the other keyboard-generated termination signal (Ctrl-\) that the
    // terminal delivers to the whole foreground process group: a child that
    // catches or ignores it must not lose its waiter either. In supervised mode
    // SIGQUIT keeps its default disposition, unchanged from the original
    // forwarding contract.
    let (forwarded, ignored): (&[libc::c_int], &[libc::c_int]) = if interactive {
        // The terminal already delivers keyboard-generated SIGINT and SIGQUIT
        // to the whole foreground process group, which includes the child;
        // forwarding either would deliver the same interrupt twice, and dying
        // from either would orphan a child that chose to survive it.
        (
            &[libc::SIGTERM, libc::SIGHUP],
            &[libc::SIGINT, libc::SIGQUIT],
        )
    } else {
        (&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP], &[])
    };
    let guard = ForwardingGuard::install(forwarded, ignored)?;
    // Every signal whose disposition was replaced must be restored in the
    // child, not just the ignored ones: an inherited SIG_IGN for a *forwarded*
    // signal is exactly what `nohup` and a job-control-less shell rely on, and
    // it survives exec, so failing to reinstall it hands the child a SIG_DFL it
    // was never supposed to have.
    let inherited = guard.inherited_dispositions();
    if !inherited.is_empty() {
        // SAFETY: the closure runs in the forked child before exec and calls
        // only the async-signal-safe signal(2).
        unsafe {
            command.pre_exec(move || {
                for &(signal, handler) in &inherited {
                    if libc::signal(signal, handler) == libc::SIG_ERR {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().map_err(TerminationError::Spawn)?;
    // A PID wider than `i32` cannot name a kill(2) target; forwarding is then
    // impossible, but abandoning the child would be worse than losing it, so
    // the wait still happens.
    if let Ok(child_pid) = i32::try_from(child.id()) {
        guard.arm(if interactive { child_pid } else { -child_pid });
    }
    let status = child.wait();
    guard.disarm();
    status.map_err(TerminationError::Wait)
}

/// Windows has no POSIX signal forwarding to preserve here: the console
/// already delivers Ctrl-C and Ctrl-Break events to every process attached to
/// it, including the child, and a hard `TerminateProcess` of the waiter cannot
/// be intercepted, so no additional termination handling is possible.
#[cfg(not(unix))]
pub fn run_forwarding_terminations(command: &mut Command) -> Result<ExitStatus, TerminationError> {
    command.status().map_err(TerminationError::Spawn)
}

/// Report whether the current process owns a controlling terminal in the
/// foreground, which is when creating a new process group for the child would
/// steal terminal job-control semantics from it.
#[cfg(unix)]
fn shares_foreground_terminal() -> bool {
    // SAFETY: getpgrp, isatty, and tcgetpgrp only read process and descriptor
    // state for the current process.
    let process_group = unsafe { libc::getpgrp() };
    [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .any(|descriptor| unsafe {
            libc::isatty(descriptor) == 1 && libc::tcgetpgrp(descriptor) == process_group
        })
}

#[cfg(unix)]
use forwarding::ForwardingGuard;

#[cfg(unix)]
mod forwarding {
    use std::sync::atomic::{AtomicI32, Ordering};

    use super::TerminationError;

    /// Signal-forwarding target: `0` before the child exists, its PID in
    /// shared-process-group (interactive) mode, or its negated process-group ID
    /// in own-group (supervised) mode. Process-global, like the signal
    /// dispositions it backs; a Riftri process waits on one such child.
    static FORWARD_TARGET: AtomicI32 = AtomicI32::new(0);
    /// Most recent signal received before the child's PID was known.
    static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

    extern "C" fn forward_signal(signal: libc::c_int) {
        let target = FORWARD_TARGET.load(Ordering::SeqCst);
        if target == 0 {
            PENDING_SIGNAL.store(signal, Ordering::SeqCst);
        } else {
            // SAFETY: kill(2) is async-signal-safe, and the target names the
            // child or its dedicated process group.
            unsafe {
                libc::kill(target, signal);
            }
        }
    }

    /// Installs forwarding handlers for the `forwarded` signals, ignores the
    /// `ignored` signals, and restores the previous dispositions when dropped.
    pub(super) struct ForwardingGuard {
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl ForwardingGuard {
        pub(super) fn install(
            forwarded: &[libc::c_int],
            ignored: &[libc::c_int],
        ) -> Result<Self, TerminationError> {
            let mut guard = ForwardingGuard {
                previous: Vec::with_capacity(forwarded.len() + ignored.len()),
            };
            for &signal in forwarded {
                guard.replace_disposition(signal, forward_signal as *const () as usize)?;
            }
            for &signal in ignored {
                guard.replace_disposition(signal, libc::SIG_IGN)?;
            }
            Ok(guard)
        }

        fn replace_disposition(
            &mut self,
            signal: libc::c_int,
            handler: libc::sighandler_t,
        ) -> Result<(), TerminationError> {
            // SAFETY: the action structures are zero-initialized before every
            // field sigaction reads is assigned, and the handler is either
            // SIG_IGN or an async-signal-safe function.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, &action, &mut previous) != 0 {
                    return Err(TerminationError::Disposition {
                        signal,
                        source: std::io::Error::last_os_error(),
                    });
                }
                self.previous.push((signal, previous));
            }
            Ok(())
        }

        /// For every signal whose disposition was replaced, the disposition a
        /// child spawned now should start from: the disposition this process
        /// itself inherited if that was SIG_IGN, and SIG_DFL otherwise, because
        /// a caught handler never survives exec while an ignore does.
        pub(super) fn inherited_dispositions(&self) -> Vec<(libc::c_int, libc::sighandler_t)> {
            self.previous
                .iter()
                .map(|&(signal, previous)| {
                    let handler = if previous.sa_sigaction == libc::SIG_IGN {
                        libc::SIG_IGN
                    } else {
                        libc::SIG_DFL
                    };
                    (signal, handler)
                })
                .collect()
        }

        /// Publish the forwarding target and deliver any signal that arrived
        /// before the child's PID was known.
        pub(super) fn arm(&self, target: i32) {
            FORWARD_TARGET.store(target, Ordering::SeqCst);
            let pending = PENDING_SIGNAL.swap(0, Ordering::SeqCst);
            if pending != 0 {
                // SAFETY: the target names the just-spawned child or its
                // dedicated process group.
                unsafe {
                    libc::kill(target, pending);
                }
            }
        }

        /// Stop forwarding immediately once the child is reaped so a late
        /// signal cannot reach a recycled PID.
        pub(super) fn disarm(&self) {
            FORWARD_TARGET.store(0, Ordering::SeqCst);
        }
    }

    impl Drop for ForwardingGuard {
        fn drop(&mut self) {
            FORWARD_TARGET.store(0, Ordering::SeqCst);
            PENDING_SIGNAL.store(0, Ordering::SeqCst);
            for (signal, previous) in self.previous.drain(..) {
                // SAFETY: `previous` was returned by sigaction for `signal`.
                unsafe {
                    libc::sigaction(signal, &previous, std::ptr::null_mut());
                }
            }
        }
    }
}
