use std::process::Command;

#[test]
fn status_recognizes_process_scopes_without_implying_repository_consent() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repository");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&repository)
            .status()
            .unwrap()
            .success()
    );
    let binary = env!("CARGO_BIN_EXE_riftri");
    for enabled in [false, true] {
        if enabled {
            assert!(
                Command::new(binary)
                    .arg("enable")
                    .arg(&repository)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        for nested in [false, true] {
            let mut command = Command::new(binary);
            command.args(["exec", "--", binary]);
            if nested {
                command.args(["exec", "--", binary]);
            }
            let output = command
                .args(["shell", "status"])
                .arg(&repository)
                .env("RIFTRI_CACHE_DIR", fixture.path().join("unused-cache"))
                .env_remove("RIFTRI_SHIM_ACTIVE")
                .env_remove("RIFTRI_REAL_GIT")
                .env_remove("RIFTRI_BYPASS")
                .env_remove("RIFTRI_PROCESS_SHIM_DIR")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("Shell interception: active"), "{text}");
            let effective = if enabled { "active" } else { "inactive" };
            assert!(
                text.contains(&format!("Effective optimized interception: {effective}")),
                "{text}"
            );
            assert!(
                !text.contains("unused-cache"),
                "must report the actual scoped shim: {text}"
            );
        }
    }
}
