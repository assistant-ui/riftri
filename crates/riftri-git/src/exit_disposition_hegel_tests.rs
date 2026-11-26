//! Property tests for how a finished Git process is described in an error.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus};
use std::sync::LazyLock;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator};

use super::{Git, GitError, ObjectId, command_failed, describe_exit_disposition};

/// The shell's own signal table, so the names are checked against this
/// platform's numbering instead of a second copy of the list under test.
static SHELL_SIGNAL_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let output = Command::new("/bin/sh")
        .args(["-c", "for signal in $(seq 1 31); do kill -l $signal; done"])
        .output()
        .expect("list the shell's signal names");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .expect("signal names are ASCII")
        .lines()
        .map(|name| format!("SIG{}", name.to_ascii_uppercase()))
        .collect()
});

#[hegel::test]
fn an_exit_is_described_by_its_code(tc: TestCase) {
    let code = tc.draw(gs::integers::<i32>().max_value(255).min_value(0));

    let description = describe_exit_disposition(ExitStatus::from_raw(code << 8));

    assert_eq!(description, format!("exit code {code}"));
}

#[hegel::test(test_cases = 500)]
fn a_signal_death_is_described_by_its_number_and_platform_name(tc: TestCase) {
    let signal = tc.draw(gs::integers::<i32>().min_value(1).max_value(31));
    let dumped_core = tc.draw(gs::booleans());
    let status = ExitStatus::from_raw(signal | if dumped_core { 0x80 } else { 0 });

    let description = describe_exit_disposition(status);

    let numbered = format!("killed by signal {signal}");
    let named = format!(
        "{numbered} ({})",
        SHELL_SIGNAL_NAMES[usize::try_from(signal - 1).expect("positive signal")]
    );
    assert!(
        description == numbered || description == named,
        "{description}"
    );
}

#[hegel::test]
fn a_status_that_is_neither_an_exit_nor_a_signal_is_still_described(tc: TestCase) {
    // A stopped child: the low byte is 0x7f and the stop signal sits above it.
    let signal = tc.draw(gs::integers::<i32>().min_value(1).max_value(31));
    let status = ExitStatus::from_raw((signal << 8) | 0x7f);

    assert_eq!(
        describe_exit_disposition(status),
        "terminated without an exit code"
    );
}

#[hegel::test]
fn a_failed_command_always_reports_how_it_ended(tc: TestCase) {
    let raw = tc.draw(hegel::one_of!(
        gs::integers::<i32>()
            .min_value(1)
            .max_value(255)
            .map(|code| code << 8),
        gs::integers::<i32>().min_value(1).max_value(31)
    ));
    let stderr = tc.draw(gs::binary().max_size(24));
    let output = std::process::Output {
        status: ExitStatus::from_raw(raw),
        stdout: Vec::new(),
        stderr: stderr.clone(),
    };

    let error = command_failed(&["status".into(), "--porcelain".into()], &output);

    let disposition = describe_exit_disposition(output.status);
    let diagnostic = String::from_utf8_lossy(&stderr).trim().to_owned();
    let expected = if diagnostic.is_empty() {
        format!("Git command failed (status --porcelain): {disposition}")
    } else {
        format!("Git command failed (status --porcelain): {diagnostic} ({disposition})")
    };
    assert!(matches!(error, GitError::CommandFailed { .. }));
    assert_eq!(error.to_string(), expected);
}

/// A Git that ends before reading a blob request leaves the request writing
/// into a broken pipe. The caller must still learn how Git ended. Usually the
/// request reaches the pipe buffer before Git ends, so each case waits for
/// the end first to take the broken-pipe path. A process another test is
/// starting can briefly inherit the pipe, so the write may still succeed and
/// the reader then sees the malformed reply first, which every streaming
/// reader may report instead (see `hegel_fail_closed::assert_refused`).
#[hegel::test(test_cases = 40)]
fn a_blob_request_git_never_read_reports_how_git_ended(tc: TestCase) {
    let (ending, expected) = tc.draw(hegel::one_of!(
        gs::integers::<u8>()
            .min_value(1)
            .map(|code| (format!("exit {code}"), format!("exit code {code}"))),
        gs::sampled_from(vec![libc::SIGHUP, libc::SIGKILL, libc::SIGTERM]).map(|signal| (
            format!("kill -{signal} $$"),
            format!("killed by signal {signal}")
        ))
    ));
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("git");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf refused >&2\n{ending}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut reader = Git::new(&script)
        .small_blob_reader(directory.path())
        .unwrap();
    let pid = reader.child.as_ref().unwrap().id();
    // SAFETY: `waitid` writes only into the zeroed `siginfo_t` it is given,
    // and `WNOWAIT` leaves the ended child for the reader to reap.
    let waited = unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0);

    match reader
        .read(&[ObjectId::parse("a".repeat(40)).unwrap()], 1)
        .unwrap_err()
    {
        GitError::InvalidOutput { .. } => {}
        error => {
            let error = error.to_string();
            assert!(
                error.contains("refused") && error.contains(&expected),
                "{error}"
            );
        }
    }
    assert!(reader.child.is_none(), "the ended Git was not reaped");
}
