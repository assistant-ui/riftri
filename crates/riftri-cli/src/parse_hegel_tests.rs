//! Property tests for command-line parsing and what it hands to receipts.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Parser;
use hegel::TestCase;
use hegel::generators as gs;

use super::hegel_tests::{assert_receipt_keys, is_kebab_case, native_path};
use super::{Cli, InvocationContext, usage_receipt};

/// One command line shape: the words before any option, the operation it
/// names, and which selections it accepts.
struct Shape {
    words: &'static [&'static str],
    operation: &'static str,
    repository: Selection,
    state_directory: bool,
    json: bool,
    always_machine: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Selection {
    /// The command addresses no repository a receipt could repeat.
    None,
    /// `--repository PATH` only.
    Option,
    /// A positional path, or `--repository PATH` in its place.
    PositionalOrOption,
}

const fn shape(
    words: &'static [&'static str],
    operation: &'static str,
    repository: Selection,
    state_directory: bool,
    json: bool,
    always_machine: bool,
) -> Shape {
    Shape {
        words,
        operation,
        repository,
        state_directory,
        json,
        always_machine,
    }
}

const SHAPES: [Shape; 24] = [
    shape(&["setup"], "setup", Selection::Option, false, false, false),
    shape(
        &["enable"],
        "enable",
        Selection::PositionalOrOption,
        false,
        false,
        false,
    ),
    shape(
        &["disable"],
        "disable",
        Selection::PositionalOrOption,
        false,
        false,
        false,
    ),
    shape(
        &["doctor"],
        "doctor",
        Selection::PositionalOrOption,
        false,
        true,
        false,
    ),
    shape(
        &["status"],
        "status",
        Selection::PositionalOrOption,
        true,
        true,
        false,
    ),
    shape(
        &["repair"],
        "repair",
        Selection::PositionalOrOption,
        true,
        true,
        false,
    ),
    shape(
        &["gc"],
        "garbage-collection",
        Selection::PositionalOrOption,
        true,
        true,
        false,
    ),
    shape(
        &["state", "unregister", "missing"],
        "state",
        Selection::Option,
        false,
        false,
        false,
    ),
    shape(
        &["state", "forget-missing", "missing"],
        "state",
        Selection::Option,
        false,
        false,
        false,
    ),
    shape(
        &["worktree", "list"],
        "worktree-list",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["worktree", "owner", "view"],
        "worktree-owner",
        Selection::Option,
        false,
        true,
        false,
    ),
    shape(
        &["worktree", "inspect", "view", "other"],
        "worktree-inspect",
        Selection::Option,
        false,
        true,
        false,
    ),
    shape(
        &["worktree", "add", "view", "--detach"],
        "worktree-add",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["worktree", "remove", "view"],
        "worktree-remove",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["worktree", "move", "view", "other"],
        "worktree-move",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["worktree", "compact", "view"],
        "worktree-compact",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["worktree", "prune"],
        "worktree-prune",
        Selection::Option,
        true,
        true,
        false,
    ),
    shape(
        &["backends"],
        "backends",
        Selection::None,
        false,
        true,
        false,
    ),
    shape(
        &["man", "pages"],
        "man",
        Selection::None,
        false,
        false,
        false,
    ),
    shape(
        &["completions", "zsh"],
        "completions",
        Selection::None,
        false,
        false,
        true,
    ),
    shape(
        &["shell", "hook", "powershell"],
        "shell",
        Selection::None,
        false,
        false,
        true,
    ),
    shape(
        &["shell", "deactivate", "sh"],
        "shell",
        Selection::None,
        false,
        false,
        true,
    ),
    shape(
        &["shell", "status"],
        "shell",
        Selection::None,
        false,
        false,
        false,
    ),
    shape(
        &["overlayfs", "install-helper"],
        "overlayfs-helper-install",
        Selection::None,
        false,
        false,
        false,
    ),
];

/// `path` as one command-line value. Clap reads a leading hyphen as an
/// option, so such a path is spelled relative to the current directory.
fn path_value(path: &Path) -> PathBuf {
    if path.as_os_str().as_encoded_bytes().first() == Some(&b'-') {
        Path::new(".").join(path)
    } else {
        path.to_path_buf()
    }
}

#[hegel::test(test_cases = 400, derandomize = true)]
fn parsed_commands_carry_their_selection_into_the_receipt_context(tc: TestCase) {
    let shape = &SHAPES[tc.draw(gs::integers::<usize>().max_value(SHAPES.len() - 1))];
    let mut arguments = vec![OsString::from("riftri")];
    let leading_globals = tc.draw(gs::booleans());
    let globals = [
        "--json-errors",
        "--no-progress",
        "--plain",
        "--no-animation",
    ]
    .map(|flag| (flag, tc.draw(gs::booleans())));
    let push_globals = |arguments: &mut Vec<OsString>| {
        for (flag, present) in globals {
            if present {
                arguments.push(flag.into());
            }
        }
    };
    if leading_globals {
        push_globals(&mut arguments);
    }
    arguments.extend(shape.words.iter().map(OsString::from));

    let repository = (shape.repository != Selection::None && tc.draw(gs::booleans()))
        .then(|| path_value(&native_path(&tc)));
    if let Some(repository) = &repository {
        if shape.repository == Selection::Option || tc.draw(gs::booleans()) {
            arguments.push("--repository".into());
        }
        arguments.push(repository.clone().into_os_string());
    }
    let state_directory =
        (shape.state_directory && tc.draw(gs::booleans())).then(|| path_value(&native_path(&tc)));
    if let Some(state_directory) = &state_directory {
        arguments.push("--state-dir".into());
        arguments.push(state_directory.clone().into_os_string());
    }
    let json = shape.json && tc.draw(gs::booleans());
    if json {
        arguments.push("--json".into());
    }
    if !leading_globals {
        push_globals(&mut arguments);
    }

    let cli = Cli::try_parse_from(&arguments).expect("a documented command line parses");

    assert_eq!(
        [
            cli.json_errors,
            cli.no_progress,
            cli.plain,
            cli.no_animation
        ],
        globals.map(|(_, present)| present)
    );
    assert_eq!(cli.command.operation_name(), shape.operation);
    assert_eq!(cli.command.machine_output(), json || shape.always_machine);
    let expected_repository = match shape.repository {
        Selection::None => None,
        _ => Some(repository.unwrap_or_else(|| PathBuf::from("."))),
    };
    assert_eq!(
        cli.command.invocation_context(),
        InvocationContext {
            repository: expected_repository,
            state_directory,
        }
    );
}

#[hegel::test(test_cases = 300)]
fn rejected_command_lines_become_usage_receipts(tc: TestCase) {
    const WORDS: [&str; 30] = [
        "worktree",
        "add",
        "remove",
        "move",
        "list",
        "inspect",
        "owner",
        "status",
        "repair",
        "gc",
        "state",
        "unregister",
        "shell",
        "hook",
        "exec",
        "setup",
        "completions",
        "zsh",
        "--json",
        "--json-errors",
        "--apply",
        "--yes",
        "--force",
        "--detach",
        "-b",
        "--state-dir",
        "--repository",
        "--all-states",
        "--",
        "path",
    ];
    let mut arguments = vec![OsString::from("riftri")];
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(7)) {
        arguments.push(if tc.draw(gs::booleans()) {
            tc.draw(gs::sampled_from(WORDS.to_vec())).into()
        } else {
            native_path(&tc).into_os_string()
        });
    }

    match Cli::try_parse_from(&arguments) {
        Ok(cli) => {
            assert!(is_kebab_case(cli.command.operation_name()));
            // A parsed command always has a context to hand a receipt.
            let _ = cli.command.invocation_context();
        }
        Err(error) => {
            let receipt = usage_receipt(&error);
            assert_receipt_keys(&receipt);
            assert_eq!(receipt["category"], "usage");
            assert_eq!(receipt["code"], "usage-error");
            let message = receipt["message"].as_str().expect("message");
            assert!(!message.is_empty());
            assert_eq!(message, message.trim_end());
            for field in [
                "operation",
                "phase",
                "nextCommand",
                "repository",
                "repositoryNativeHex",
                "stateDirectory",
                "stateDirectoryNativeHex",
            ] {
                assert!(receipt[field].is_null(), "{field}");
            }
        }
    }
}
