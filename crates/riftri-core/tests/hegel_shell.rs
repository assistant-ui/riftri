//! Property tests for the commands Riftri suggests to a user or harness.
//!
//! A suggested command is run by pasting it into a POSIX shell, so the only
//! acceptable renderings of a path are "this exact path, as one argument" and
//! "no command at all".

#![cfg(unix)]

#[path = "support/shell_words.rs"]
mod shell_words;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use riftri_core::{
    command_path, repair_command, shell_quoted_path, status_command, unregister_state_command,
};
use shell_words::shell_words;

/// Every case starts a shell, which is cheap. Hegel revisits and varies
/// earlier examples, so it takes a few hundred cases for each shell
/// metacharacter to turn up in a path on its own.
const SHELL_CASES: u64 = 300;

/// A path as raw bytes: mostly text dense with shell syntax, sometimes
/// arbitrary bytes, occasionally with a control character a terminal would
/// act on. Never a NUL, which no path can contain.
fn path(tc: &TestCase) -> PathBuf {
    let mut bytes = if tc.draw(gs::weighted_booleans(0.8)) {
        tc.draw(
            gs::text()
                .alphabet("a/ '\"$`\\;&|*?[]{}()<>~!#=%:-\u{e9}")
                .min_size(2)
                .max_size(16),
        )
        .into_bytes()
    } else {
        tc.draw(gs::binary().max_size(12))
    };
    if tc.draw(gs::weighted_booleans(0.1)) {
        bytes.push(tc.draw(gs::sampled_from(vec![b'\n', 0x1b, 0x7f])));
    }
    bytes.retain(|byte| *byte != 0);
    PathBuf::from(OsString::from_vec(bytes))
}

/// A path can be shown as a command exactly when it is Unicode text with no
/// control characters a terminal would act on.
fn representable(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| !text.chars().any(char::is_control))
}

fn words(fixed: &[&str]) -> Vec<OsString> {
    fixed.iter().map(OsString::from).collect()
}

#[hegel::test(test_cases = SHELL_CASES)]
fn a_quoted_path_is_that_exact_path_or_nothing(tc: TestCase) {
    let path = path(&tc);
    let Some(quoted) = shell_quoted_path(&path) else {
        assert!(!representable(&path), "{path:?} was refused");
        return;
    };
    assert!(representable(&path), "{path:?} was rendered as {quoted:?}");
    assert_eq!(shell_words(&quoted), [path.into_os_string()]);
}

#[hegel::test(test_cases = SHELL_CASES)]
fn suggested_commands_run_against_the_exact_paths_they_name(tc: TestCase) {
    let (state, repository) = (path(&tc), path(&tc));
    let state_word = state.clone().into_os_string();

    let commands = [
        (repair_command(&state), "repair"),
        (status_command(&state), "status"),
    ];
    for (command, verb) in commands {
        assert_eq!(command.is_some(), representable(&state), "{state:?}");
        if let Some(command) = command {
            let mut expected = words(&["riftri", verb, "--state-dir"]);
            expected.push(state_word.clone());
            assert_eq!(shell_words(&command), expected);
        }
    }

    let unregister = unregister_state_command(&repository, &state);
    assert_eq!(
        unregister.is_some(),
        representable(&state) && representable(&repository)
    );
    if let Some(command) = unregister {
        let mut expected = words(&["riftri", "state", "unregister"]);
        expected.extend([state_word, OsString::from("--repository")]);
        expected.push(repository.into_os_string());
        assert_eq!(shell_words(&command), expected);
    }
}

/// A suggested command must target the same directory wherever it is run, so
/// its path is made absolute without otherwise changing what it names.
#[hegel::test]
fn command_paths_are_absolute_and_stable(tc: TestCase) {
    let path = path(&tc);
    tc.assume(!path.as_os_str().is_empty());
    let absolute = command_path(&path);
    assert!(absolute.is_absolute(), "{absolute:?}");
    assert_eq!(command_path(&absolute), absolute);
    let expected = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .expect("current directory")
            .join(path)
    };
    assert_eq!(absolute, expected);
}
