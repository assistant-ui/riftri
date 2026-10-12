//! What `riftri` does when a person is at the keyboard: confirmations before
//! destructive commands, and the setup dialogue through to the agent launch.

use hegel::TestCase;
use hegel::generators as gs;

use crate::common::{Scratch, riftri, run_on_terminal};
use crate::contract::process_cases;

/// What a person might type at a yes/no prompt, or `None` for end of input.
fn typed_answer(tc: &TestCase) -> Option<&'static str> {
    tc.draw(gs::sampled_from(vec![
        Some("y"),
        Some("Y"),
        Some("yes"),
        Some("YES"),
        Some(" y "),
        Some("n"),
        Some("no"),
        Some(""),
        Some("yes please"),
        Some("yy"),
        None,
    ]))
}

#[hegel::test(process_cases(16))]
fn destructive_commands_proceed_only_after_an_explicit_yes(tc: TestCase) {
    let scratch = Scratch::new();
    let repository = scratch.repository("repository");
    let unmanaged = scratch.root().join("unmanaged");
    std::fs::create_dir(&unmanaged).expect("create directory");
    let collect = tc.draw(gs::booleans());
    let skip_prompt = tc.draw(gs::booleans());
    let answer = typed_answer(&tc);

    let mut command = riftri();
    command.arg("--plain").current_dir(&repository);
    if collect {
        command.args(["gc", "--apply"]);
    } else {
        command
            .args(["worktree", "remove", "--force"])
            .arg(&unmanaged);
    }
    if skip_prompt {
        command.arg("--yes");
    }
    let input = answer
        .map(|answer| format!("{answer}\n"))
        .unwrap_or_default();
    let (code, shown) = run_on_terminal(command, input.as_bytes());

    assert_eq!(shown.contains("Continue? [y/N]"), !skip_prompt, "{shown}");
    let confirmed = skip_prompt
        || answer.is_some_and(|answer| ["y", "Y", "yes", "Yes", "YES"].contains(&answer.trim()));
    if !confirmed {
        assert_eq!(code, Some(1), "{shown}");
        assert!(shown.contains("aborted without confirmation"), "{shown}");
        assert!(!shown.contains("Riftri garbage collection"), "{shown}");
    } else if collect {
        assert_eq!(code, Some(0), "{shown}");
        assert!(shown.contains("Riftri garbage collection"), "{shown}");
        assert!(shown.contains("Mode: applied"), "{shown}");
    } else {
        // Confirmed, then refused by policy: the directory is not a managed
        // worktree, so there is nothing Riftri may remove.
        assert_eq!(code, Some(3), "{shown}");
        assert!(!shown.contains("aborted without confirmation"), "{shown}");
    }
    assert!(unmanaged.is_dir());
}

#[cfg(target_os = "macos")]
mod macos {
    use std::os::unix::fs::PermissionsExt;

    use hegel::TestCase;
    use hegel::generators as gs;

    use crate::common::{Scratch, copy_on_write_supported, git, git_ok, riftri, run_on_terminal};
    use crate::contract::process_cases;

    #[hegel::test(process_cases(4))]
    fn setup_launches_the_confirmed_agent_and_exits_with_its_status(tc: TestCase) {
        let scratch = Scratch::new();
        let repository = scratch.repository("repository");
        if !copy_on_write_supported(&repository) {
            return;
        }
        let view = scratch.root().join("view with spaces");
        let status = tc.draw(gs::integers::<u8>());
        // Mostly launches: declining is one answer, a launch has a status to carry.
        let launch = tc.draw(gs::integers::<u8>().max_value(3)) != 0;
        let bypass = tc.draw(gs::booleans());
        let agent = scratch.root().join("agent; echo nope");
        let record = scratch.root().join("agent-ran-in");
        std::fs::write(
            &agent,
            format!("#!/bin/sh\npwd -P > \"$RIFTRI_TEST_RECORD\"\nexit {status}\n"),
        )
        .expect("write agent");
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))
            .expect("chmod agent");

        let mut command = riftri();
        command
            .args(["--plain", "setup", "--repository"])
            .arg(&repository)
            .arg("--destination")
            .arg(&view)
            .args(["--branch", "task/setup"])
            .env("RIFTRI_TEST_RECORD", &record);
        if bypass {
            command.env("RIFTRI_BYPASS", "1");
        }
        let input = format!(
            "y\n3\n{}\n{}\n",
            agent.display(),
            if launch { "y" } else { "n" }
        );
        let (code, shown) = run_on_terminal(command, input.as_bytes());

        // The worktree is created either way; only a confirmed launch opts the
        // repository in and runs the agent, whose status becomes Riftri's.
        assert_eq!(git_ok(&view, &["status", "--porcelain=v1"]), "", "{shown}");
        let enabled = git(&repository)
            .args(["config", "--local", "--get", "riftri.enabled"])
            .output()
            .expect("read config")
            .status
            .success();
        assert_eq!(enabled, launch, "{shown}");
        assert_eq!(record.exists(), launch, "{shown}");
        // Setup tells the caller when an inherited bypass will mute interception.
        assert_eq!(shown.contains("RIFTRI_BYPASS is set"), bypass, "{shown}");
        if launch {
            assert_eq!(code, Some(i32::from(status)), "{shown}");
            let ran_in = std::fs::read_to_string(&record).expect("agent record");
            assert_eq!(std::path::Path::new(ran_in.trim_end()), view);
        } else {
            assert_eq!(code, Some(0), "{shown}");
            assert!(shown.contains("No agent launched"), "{shown}");
        }
    }
}
