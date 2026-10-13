//! Property tests that run the setup plan against a real repository.

use std::ffi::OsString;
use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Output};

use hegel::TestCase;
use hegel::generators as gs;

use super::hegel_tests::ClosingTerminal;
use super::prepare;
use crate::hegel_tests::process_cases;

#[path = "../tests/support/mod.rs"]
mod support;

fn git(directory: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .current_dir(directory)
        .args([
            "-c",
            "user.name=Riftri Tests",
            "-c",
            "user.email=riftri@example.invalid",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git")
}

fn git_ok(directory: &Path, arguments: &[&str]) -> String {
    let output = git(directory, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output")
}

/// Generated answers to the setup prompts, up to where the input ends.
#[derive(Default)]
struct Dialogue {
    input: String,
    ended: bool,
    /// While set, the input does not end: every prompt gets an answer.
    attentive: bool,
}

impl Dialogue {
    fn line(&mut self, line: &str) {
        self.input.push_str(line);
        self.input.push('\n');
    }

    /// One of `choices`, or `None` when the input ends at this prompt.
    fn answer(&mut self, tc: &TestCase, choices: &[&'static str]) -> Option<&'static str> {
        if self.ended || (!self.attentive && tc.draw(gs::integers::<u8>().max_value(9)) == 0) {
            self.ended = true;
            return None;
        }
        let answer = tc.draw(gs::sampled_from(choices.to_vec()));
        self.line(answer);
        Some(answer)
    }
}

/// How far a generated setup dialogue gets.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stop {
    /// Input ends, or the answer is no, before anything is created.
    Cancelled,
    /// The plan is refused before the confirmation.
    Refused,
    /// The worktree is created and setup ends without launching an agent.
    Created,
    /// The worktree is created and an agent launch is returned.
    Launch,
}

// Dialogues that stop early are cheap, so they get most of the cases.
#[hegel::test(process_cases(20))]
fn setup_creates_nothing_the_dialogue_did_not_confirm(tc: TestCase) {
    check_dialogue(&tc, false);
}

#[hegel::test(process_cases(8))]
fn setup_creates_exactly_what_the_dialogue_confirmed(tc: TestCase) {
    check_dialogue(&tc, true);
}

/// Run one generated dialogue against a fresh repository and compare what
/// setup did with what was answered. `confirms` selects dialogues that reach
/// and accept the creation prompt.
fn check_dialogue(tc: &TestCase, confirms: bool) {
    let directory = support::writable_tempdir().expect("fixture directory");
    let root = directory.path().canonicalize().expect("fixture root");
    let repository = root.join("repository");
    std::fs::create_dir(&repository).expect("create repository");
    git_ok(&repository, &["init", "--quiet"]);
    std::fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
    git_ok(&repository, &["add", "tracked.txt"]);
    // Without a commit there is no tree to clone, and setup must say so.
    let unborn = !confirms && tc.draw(gs::integers::<u8>().max_value(5)) == 0;
    if !unborn {
        git_ok(&repository, &["commit", "--quiet", "-m", "fixture"]);
    }
    let occupied = root.join("occupied");
    std::fs::create_dir(&occupied).expect("create occupied directory");
    // Like Git, an add accepts an existing directory only while it is empty.
    std::fs::write(occupied.join("kept.txt"), "kept\n").expect("occupy directory");
    let linked = root.join("linked");
    std::os::unix::fs::symlink(&occupied, &linked).expect("link to a directory");
    let agent = std::env::current_exe().expect("an executable file");

    let mut typed = Dialogue {
        attentive: confirms,
        ..Dialogue::default()
    };
    let mut stop = None;

    let destination_flag = tc.draw(gs::booleans());
    let destination_choice = tc.draw(gs::sampled_from(if confirms {
        vec!["default", "named"]
    } else {
        vec!["default", "named", "occupied", "linked"]
    }));
    let named = match destination_choice {
        "default" => root.join("repository.task"),
        "named" => root.join("view with an'apostrophe"),
        "linked" => linked.clone(),
        _ => occupied.clone(),
    };
    let destination = if destination_flag {
        named
    } else if destination_choice == "default" {
        if typed.answer(tc, &[""]).is_none() {
            stop = Some(Stop::Cancelled);
        }
        named
    } else {
        typed.line(named.to_str().expect("fixture path"));
        named
    };

    let branch_flag = tc.draw(gs::booleans());
    let branch_choice = tc.draw(gs::sampled_from(if confirms {
        vec!["", "topic/setup"]
    } else {
        vec!["", "topic/setup", "not..a..branch"]
    }));
    let branch = if branch_choice.is_empty() {
        "task/first"
    } else {
        branch_choice
    };
    if stop.is_none() && !branch_flag && typed.answer(tc, &[branch_choice]).is_none() {
        stop = Some(Stop::Cancelled);
    }

    let blocked = riftri_core::doctor_for_destination(&repository, &destination)
        .destination_readiness
        .status
        == riftri_core::DestinationReadinessStatus::Blocked;
    let taken = destination == occupied || destination == linked;
    if stop.is_none() && (blocked || taken) {
        stop = Some(Stop::Refused);
    }
    if stop.is_none() {
        // Only a branch name Git rejects may be confirmed without creating.
        let answers: &[&str] = if confirms {
            &["y", " Yes "]
        } else if branch == "not..a..branch" {
            &["y", " Yes ", "n", ""]
        } else {
            &["n", "", "no", "yes please"]
        };
        match typed.answer(tc, answers) {
            Some("y" | " Yes ") if branch == "not..a..branch" => stop = Some(Stop::Refused),
            Some("y" | " Yes ") => {}
            _ => stop = Some(Stop::Cancelled),
        }
    }
    typed.attentive = false;
    let mut program = None;
    if stop.is_none() {
        let missing = root.join("missing agent; echo nope");
        match typed.answer(tc, &["0", "", "9", "3"]) {
            Some("3") => {
                let custom = if tc.draw(gs::booleans()) {
                    &agent
                } else {
                    &missing
                };
                typed.line(custom.to_str().expect("fixture path"));
                if custom == &agent {
                    program = Some(agent.clone());
                }
            }
            Some("9") => {}
            _ => stop = Some(Stop::Created),
        }
        // An unusable choice asks again; the dialogue then ends at "not now"
        // or at the end of input, which are the same decision.
        if stop.is_none() && program.is_none() {
            let _ = typed.answer(tc, &["0"]);
            stop = Some(Stop::Created);
        }
    }
    if stop.is_none() {
        stop = Some(match typed.answer(tc, &["y", "n"]) {
            Some("y") => Stop::Launch,
            _ => Stop::Created,
        });
    }
    let stop = stop.expect("every dialogue ends");

    let mut output = Vec::new();
    let result = prepare(
        &mut Cursor::new(typed.input.as_bytes()),
        &mut output,
        &repository,
        destination_flag.then(|| destination.clone()),
        branch_flag.then(|| OsString::from(branch)),
    );
    let output = String::from_utf8(output).expect("setup transcript");

    let created = matches!(stop, Stop::Created | Stop::Launch);
    match (&result, stop) {
        (Ok(None), Stop::Cancelled | Stop::Created) => {}
        (Err(_), Stop::Refused) => {}
        (Ok(Some(launch)), Stop::Launch) => {
            assert_eq!(launch.worktree, destination);
            assert_eq!(Some(&launch.program), program.as_ref());
        }
        _ => panic!("expected {stop:?}, setup printed:\n{output}"),
    }
    assert_eq!(
        output.contains("Setup cancelled. No worktree created or repository activation changed."),
        stop == Stop::Cancelled
    );
    assert_eq!(output.contains("\nCreated "), created, "{output}");

    // Git agrees: the worktree and its branch exist exactly when confirmed.
    let worktrees = git_ok(&repository, &["worktree", "list", "--porcelain"]);
    let registered = worktrees
        .lines()
        .any(|line| line.strip_prefix("worktree ") == destination.to_str());
    assert_eq!(registered, created, "{worktrees}");
    let branch_exists = git(
        &repository,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .status
    .success();
    assert_eq!(branch_exists, created);
    if created {
        assert_eq!(
            git_ok(&destination, &["status", "--porcelain=v1"]),
            "",
            "a new worktree is clean"
        );
        assert_eq!(
            git_ok(&destination, &["symbolic-ref", "--short", "HEAD"]).trim_end(),
            branch
        );
    } else if !taken {
        assert!(!destination.exists(), "{output}");
    } else {
        assert!(linked.is_symlink());
        assert_eq!(
            std::fs::read_dir(&occupied)
                .expect("occupied directory")
                .count(),
            1
        );
    }
    // Preparing never opts the repository in; only a confirmed launch does,
    // and that happens after this plan is returned.
    assert!(
        !git(
            &repository,
            &["config", "--local", "--get", "riftri.enabled"]
        )
        .status
        .success()
    );
}

#[test]
fn setup_creates_nothing_the_terminal_did_not_show_for_confirmation() {
    let directory = support::writable_tempdir().expect("fixture directory");
    let root = directory.path().canonicalize().expect("fixture root");
    let repository = root.join("repository");
    std::fs::create_dir(&repository).expect("create repository");
    git_ok(&repository, &["init", "--quiet"]);
    std::fs::write(repository.join("tracked.txt"), "tracked\n").expect("write file");
    git_ok(&repository, &["add", "tracked.txt"]);
    git_ok(&repository, &["commit", "--quiet", "-m", "fixture"]);
    let agent = std::env::current_exe().expect("an executable file");
    let attempt = std::cell::Cell::new(0);

    hegel::Hegel::new(|tc: TestCase| {
        let prompted = tc.draw(gs::booleans());
        let confirmed = tc.draw(gs::booleans());
        // The terminal closes at a prompt or message a person acts on, or
        // after an arbitrary number of bytes. Several per case: the cheap ones
        // stop before anything is created.
        let closings = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                None,
                None,
                Some("Worktree directory ["),
                Some("New branch ["),
                Some("Destination: "),
                Some("Create this worktree? [y/N] "),
                Some("Created "),
                Some("Choose [0]: "),
                Some("Executable name or path"),
                Some("Executable: "),
                Some("This sets repository-local"),
                Some("The agent starts here"),
                Some("Your shell profile"),
                Some("Enable this repository and launch the agent now? [y/N] "),
                Some("Ask the agent"),
                Some("Launching in "),
                Some("Setup cancelled."),
            ]))
            .min_size(2)
            .max_size(4),
        );
        for refuses in closings {
            // Each attempt gets its own destination and branch in the one
            // repository, so an earlier case never decides a later one.
            attempt.set(attempt.get() + 1);
            let destination = root.join(format!("view {}", attempt.get()));
            let branch = format!("task/{}", attempt.get());
            let mut input = String::new();
            if prompted {
                input += &format!("{}\n{branch}\n", destination.display());
            }
            input += &if confirmed {
                format!("y\n3\n{}\ny\n", agent.display())
            } else {
                "n\n".to_owned()
            };
            let budget = match refuses {
                Some(_) => usize::MAX,
                None => tc.draw(gs::integers::<usize>().max_value(2200)),
            };
            let mut terminal = ClosingTerminal {
                shown: Vec::new(),
                budget,
                refuses,
            };

            let result = prepare(
                &mut Cursor::new(input.as_bytes()),
                &mut terminal,
                &repository,
                (!prompted).then(|| destination.clone()),
                (!prompted).then(|| OsString::from(&branch)),
            );

            let shown = String::from_utf8(terminal.shown).expect("setup transcript");
            let asked = shown.contains("Create this worktree? [y/N] ");
            let launching = shown.ends_with("The worktree is kept when the agent exits.\n");
            // A launch is only ever returned after its whole plan was shown.
            assert_eq!(
                result.as_ref().is_ok_and(Option::is_some),
                launching,
                "{shown}"
            );
            let declined =
                shown.ends_with("No worktree created or repository activation changed.\n");
            assert!(result.is_err() || launching || declined, "{shown}");
            assert!(!declined || !confirmed, "{shown}");
            let created = git_ok(&repository, &["worktree", "list", "--porcelain"])
                .lines()
                .any(|line| line.strip_prefix("worktree ") == destination.to_str());
            assert!(!created || asked, "created without confirmation: {shown}");
            assert_eq!(destination.exists(), created);
            assert!(!shown.contains("\nCreated ") || created, "{shown}");
            assert!(
                !git(
                    &repository,
                    &["config", "--local", "--get", "riftri.enabled"]
                )
                .status
                .success()
            );
        }
    })
    .settings(process_cases(8))
    .run();
}
