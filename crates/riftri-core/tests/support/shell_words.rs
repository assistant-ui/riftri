//! Shared by the property tests that check a suggested command against a
//! real shell.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

/// The words a POSIX shell splits `command` into.
pub(crate) fn shell_words(command: &str) -> Vec<OsString> {
    let script = format!("for word in {command}; do printf '%s\\0' \"$word\"; done");
    let output = Command::new("sh")
        .args(["-c", &script])
        .output()
        .expect("run sh");
    assert!(output.status.success(), "sh rejected {command:?}");
    output
        .stdout
        .split(|byte| *byte == 0)
        .take(output.stdout.iter().filter(|byte| **byte == 0).count())
        .map(|word| OsString::from_vec(word.to_vec()))
        .collect()
}
