#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

#[test]
fn symlinked_base_parents_have_actionable_human_and_json_errors() {
    for parent in ["bases", "bases/v1"] {
        let fixture = tempfile::tempdir().expect("fixture");
        let root = fixture.path().canonicalize().expect("resolve fixture");
        let state = root.join("state with spaces");
        let outside = root.join("outside");
        let unsafe_parent = state.join(parent);
        fs::create_dir_all(unsafe_parent.parent().unwrap()).expect("state layout");
        fs::create_dir(&outside).expect("outside directory");
        let private_file = outside.join("private.txt");
        fs::write(&private_file, "preserve me\n").expect("outside data");
        symlink(&outside, &unsafe_parent).expect("redirected base parent");

        for apply in [false, true] {
            for json_errors in [false, true] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
                command.args(["gc", "--state-dir"]).arg(&state);
                if apply {
                    command.args(["--apply", "--yes"]);
                }
                if json_errors {
                    command.arg("--json-errors");
                }
                let output = command.output().expect("run collection");
                assert_eq!(output.status.code(), Some(3));
                assert!(output.stdout.is_empty());
                let message = if json_errors {
                    let receipt: serde_json::Value =
                        serde_json::from_slice(&output.stderr).expect("one JSON receipt");
                    // The receipt must agree with its own message: the safety
                    // stop directs the caller to `riftri status`, so it is an
                    // inspection, not a refusal with nothing to do next.
                    assert_eq!(receipt["code"], "invalid-request");
                    assert_eq!(receipt["category"], "policy");
                    assert_eq!(receipt["cleanup"], "not-needed");
                    assert_eq!(receipt["recovery"], "inspect");
                    let next_command = receipt["nextCommand"].as_str().expect("next command");
                    assert_eq!(
                        next_command,
                        riftri_core::status_command(&state)
                            .expect("the fixture path is representable")
                    );
                    // And the receipt names the affected state directory the
                    // command targets, not merely the caller's selection.
                    assert_eq!(receipt["stateDirectory"], state.display().to_string());
                    let message = receipt["message"]
                        .as_str()
                        .expect("error message")
                        .to_owned();
                    assert!(
                        message.contains(next_command),
                        "human guidance must name the same command as nextCommand: {message}"
                    );
                    message
                } else {
                    String::from_utf8(output.stderr).expect("human error")
                };
                assert!(message.contains("is a symbolic link"), "{message}");
                assert!(
                    message.contains(&unsafe_parent.display().to_string()),
                    "{message}"
                );
                assert!(
                    message.contains("outside Riftri's expected storage layout"),
                    "{message}"
                );
                // The affected state directory contains a space, so the
                // suggested command must quote it as one shell argument.
                assert!(
                    message.contains(&format!(
                        "riftri status --state-dir {}",
                        riftri_core::shell_quoted_path(&state)
                            .expect("the fixture path is representable")
                    )),
                    "{message}"
                );
                assert!(message.contains(&state.display().to_string()), "{message}");
                assert!(
                    message.contains("does not repair an existing redirected layout"),
                    "{message}"
                );
                assert_eq!(fs::read_to_string(&private_file).unwrap(), "preserve me\n");
                assert!(unsafe_parent.is_symlink());
            }
        }
    }
}
