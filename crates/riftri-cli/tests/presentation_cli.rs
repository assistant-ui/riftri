use std::process::{Command, Output};

fn riftri(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(args)
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .output()
        .unwrap()
}

#[test]
fn presentation_options_are_global_and_pipes_remain_plain() {
    let plain = riftri(&["backends", ".", "--plain", "--no-animation"]);
    assert!(plain.status.success(), "{:?}", plain);
    let default = riftri(&["backends", "."]);
    assert_eq!(plain.stdout, default.stdout);
    assert!(!default.stdout.contains(&0x1b));
    assert!(!default.stderr.contains(&0x1b));
}

#[test]
fn presentation_never_changes_machine_receipts() {
    let report = riftri(&["doctor", "--json", "--no-animation"]);
    assert!(report.status.success(), "{:?}", report);
    serde_json::from_slice::<serde_json::Value>(&report.stdout).unwrap();
    assert!(!report.stdout.contains(&0x1b));

    let error = riftri(&["setup", "--json-errors", "--plain"]);
    assert!(!error.status.success());
    serde_json::from_slice::<serde_json::Value>(&error.stderr).unwrap();
    assert!(error.stdout.is_empty());
    assert!(!error.stderr.contains(&0x1b));
}

#[test]
fn shell_code_is_not_decorated() {
    let default = riftri(&["shell", "deactivate", "zsh"]);
    let plain = riftri(&["--plain", "shell", "deactivate", "zsh"]);
    assert!(default.status.success());
    assert!(plain.status.success());
    assert_eq!(default.stdout, plain.stdout);
    assert!(!default.stdout.contains(&0x1b));
}
