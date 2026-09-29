//! The installable agent skill tells an agent what to run, so a command or
//! flag it names that the CLI no longer accepts is not stale prose: it is a
//! failed agent run. Nothing else checks it.
//!
//! These tests parse every `riftri ...` invocation the skill shows, in fenced
//! blocks and inline backticks alike, and prove the CLI still accepts that
//! shape. They deliberately do not execute the commands, which would create
//! worktrees; they resolve the subcommand and check each flag against its
//! `--help`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn skill_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("skills/riftri-worktrees/SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Every `riftri ...` invocation the skill shows, fenced or inline.
fn documented_invocations(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();

    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced && line.trim_start().starts_with("riftri ") {
            found.insert(line.trim().to_owned());
        }
    }

    let mut rest = text;
    while let Some(open) = rest.find("`riftri ") {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        found.insert(after[..close].trim().to_owned());
        rest = &after[close..];
    }

    assert!(
        !found.is_empty(),
        "the skill names no riftri commands; the scanner matched nothing"
    );
    found
}

/// Split an invocation into its subcommand path and its flags, stopping at
/// `--`, after which the words belong to the spawned program.
fn split_invocation(invocation: &str) -> (Vec<String>, Vec<String>) {
    let mut subcommand: Vec<String> = Vec::new();
    let mut flags = Vec::new();
    let mut operands_started = false;

    let mut words = invocation.split_whitespace().skip(1).peekable();
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if word.starts_with("--") {
            flags.push(word.to_owned());
            // A flag before any subcommand word is a global one and leaves the
            // subcommand path still to come, which is the shape the skill uses
            // for `riftri --json-errors worktree add`. A flag after one ends
            // the path, so a following bare word is that flag's value.
            if !subcommand.is_empty() {
                operands_started = true;
            }
            continue;
        }
        if word.starts_with('-') {
            // A short flag takes the next word as its value.
            words.next();
            operands_started = true;
            continue;
        }
        let looks_like_a_subcommand = word
            .chars()
            .all(|character| character.is_ascii_lowercase() || character == '-');
        if !operands_started && looks_like_a_subcommand {
            subcommand.push(word.to_owned());
        } else {
            operands_started = true;
        }
    }
    (subcommand, flags)
}

fn help_for(subcommand: &[String]) -> Option<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(subcommand)
        .arg("--help")
        .output()
        .expect("run riftri --help");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn the_skill_declares_a_name_and_a_description() {
    let text = skill_text();
    let front = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---"))
        .map(|(front, _)| front)
        .expect("the skill must open with YAML frontmatter");

    assert!(
        front.lines().any(|line| line.starts_with("name: ")),
        "the skill must declare a name"
    );
    assert!(
        front.lines().any(|line| line.starts_with("description: ")),
        "the skill must declare a description, which is how an agent decides to load it"
    );
}

#[test]
fn every_command_the_skill_names_still_exists() {
    let text = skill_text();
    let mut missing = Vec::new();

    for invocation in documented_invocations(&text) {
        let (subcommand, _) = split_invocation(&invocation);
        if subcommand.is_empty() {
            continue;
        }
        if help_for(&subcommand).is_none() {
            missing.push(format!(
                "{invocation}  (subcommand `{}`)",
                subcommand.join(" ")
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "the skill tells an agent to run commands the CLI rejects:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_flag_the_skill_names_is_accepted_by_its_command() {
    let text = skill_text();
    let mut missing = Vec::new();

    for invocation in documented_invocations(&text) {
        let (subcommand, flags) = split_invocation(&invocation);
        if flags.is_empty() {
            continue;
        }
        let Some(help) = help_for(&subcommand) else {
            continue;
        };
        for flag in flags {
            if !help.contains(&flag) {
                missing.push(format!(
                    "`{flag}` in `{invocation}` is not offered by `riftri {} --help`",
                    subcommand.join(" ")
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "the skill names flags the CLI does not accept:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn the_scanner_reads_both_fenced_and_inline_commands() {
    // A fixture, not the real skill: a scanner that silently matched nothing
    // would make every assertion above vacuous.
    let fixture = "Prose mentioning `riftri status --json` inline.\n\n```sh\nriftri worktree add ../x -b b HEAD\n```\n";
    let found = documented_invocations(fixture);
    assert!(found.contains("riftri status --json"), "{found:?}");
    assert!(
        found.contains("riftri worktree add ../x -b b HEAD"),
        "{found:?}"
    );

    let (subcommand, flags) = split_invocation("riftri --json-errors worktree add ../x -b b HEAD");
    assert_eq!(subcommand, vec!["worktree".to_owned(), "add".to_owned()]);
    assert_eq!(flags, vec!["--json-errors".to_owned()]);

    // Words after `--` belong to the spawned program, not to riftri.
    let (subcommand, flags) = split_invocation("riftri exec --worktree ../x -- claude --verbose");
    assert_eq!(subcommand, vec!["exec".to_owned()]);
    assert_eq!(flags, vec!["--worktree".to_owned()]);
}
