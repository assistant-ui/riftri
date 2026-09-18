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
                    receipt["message"]
                        .as_str()
                        .expect("error message")
                        .to_owned()
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
                assert!(
                    message.contains("riftri status --state-dir <STATE_DIR>"),
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
