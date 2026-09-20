//! A reader that closes stdout early (`riftri … | head`, or `| less` then `q`)
//! must not crash the command. Rust ignores SIGPIPE, so an unhandled write to a
//! closed pipe would panic with `BrokenPipe` and abort with code 101; the CLI
//! instead exits cleanly. Both the human renderer and the machine (`--json`)
//! path share that handling, so both are exercised here.

use std::io::Read;
use std::process::{Command, Stdio};

mod support;

// Spawn the command with the read end of its stdout already closed, so the very
// first byte it writes hits a broken pipe regardless of buffering. stderr is
// drained concurrently to keep the child from blocking.
fn run_with_closed_stdout(args: &[&str]) -> (Option<i32>, bool, String) {
    let directory = support::writable_tempdir().expect("fixture directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(args)
        .current_dir(directory.path())
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn riftri");

    // Dropping our only handle to the read end closes the pipe for the child.
    drop(child.stdout.take());

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_string(&mut stderr)
        .expect("read riftri stderr");
    let status = child.wait().expect("await riftri");
    let panicked = stderr.contains("panicked") || stderr.contains("BrokenPipe");
    (status.code(), panicked, stderr)
}

#[test]
fn human_output_exits_cleanly_when_the_reader_closes_the_pipe() {
    let (code, panicked, stderr) = run_with_closed_stdout(&["backends", ".", "--plain"]);
    assert!(
        !panicked,
        "human output panicked on a broken pipe: {stderr}"
    );
    // 101 is Rust's panic/abort exit code; a clean broken-pipe exit is 0.
    assert_eq!(code, Some(0), "expected a clean exit, stderr: {stderr}");
}

#[test]
fn machine_output_exits_cleanly_when_the_reader_closes_the_pipe() {
    let (code, panicked, stderr) = run_with_closed_stdout(&["doctor", "--json"]);
    assert!(
        !panicked,
        "machine output panicked on a broken pipe: {stderr}"
    );
    assert_eq!(code, Some(0), "expected a clean exit, stderr: {stderr}");
}
