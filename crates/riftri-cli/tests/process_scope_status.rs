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

/// Every inspection failure used to be reported as "no repository here", so
/// a repository Riftri could not read looked like a mistyped path.
#[test]
fn status_separates_an_uninspectable_repository_from_an_absent_one() {
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
    let status = |path: &std::path::Path, real_git: Option<&std::path::Path>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
        command
            .args(["shell", "status"])
            .arg(path)
            .env("RIFTRI_CACHE_DIR", fixture.path().join("unused-cache"))
            .env_remove("RIFTRI_SHIM_ACTIVE")
            .env_remove("RIFTRI_REAL_GIT")
            .env_remove("RIFTRI_BYPASS");
        if let Some(real_git) = real_git {
            command
                .env("RIFTRI_SHIM_ACTIVE", "1")
                .env("RIFTRI_REAL_GIT", real_git);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };

    let absent = status(fixture.path(), None);
    assert!(absent.contains("Repository: none at "), "{absent}");
    assert!(
        absent.contains("Repository optimization: not applicable"),
        "{absent}"
    );

    let bare = fixture.path().join("bare");
    assert!(
        Command::new("git")
            .args(["init", "--quiet", "--bare"])
            .arg(&bare)
            .status()
            .unwrap()
            .success()
    );
    let bare = status(&bare, None);
    assert!(bare.contains("Repository: bare\n"), "{bare}");
    assert!(
        bare.contains("Repository optimization: not applicable"),
        "{bare}"
    );

    let uninspectable = status(&repository, Some(&fixture.path().join("missing-git")));
    assert!(
        uninspectable.contains("Repository: unavailable ("),
        "{uninspectable}"
    );
    assert!(uninspectable.contains("missing-git"), "{uninspectable}");
    assert!(
        uninspectable.contains("Repository optimization: unknown"),
        "{uninspectable}"
    );
    assert!(
        uninspectable.contains("Effective optimized interception: unknown"),
        "{uninspectable}"
    );
}
