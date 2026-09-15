//! Shell completion generation is deterministic, offline, and covers every
//! supported shell.

use std::process::Command;

#[test]
fn completions_emit_a_script_for_each_supported_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(["completions", shell])
            .output()
            .expect("run riftri completions");
        assert!(
            output.status.success(),
            "completions failed for {shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let script = String::from_utf8_lossy(&output.stdout);
        assert!(
            script.contains("riftri"),
            "{shell} completion script never mentions riftri"
        );
        assert!(
            script.contains("worktree"),
            "{shell} completion script never mentions the worktree subcommand"
        );
    }
}

#[test]
fn completions_reject_an_unknown_shell() {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["completions", "tcsh"])
        .output()
        .expect("run riftri completions");
    assert!(!output.status.success());
}
