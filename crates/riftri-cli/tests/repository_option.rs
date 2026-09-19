use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn riftri(cwd: &Path, arguments: &[&str], target: &OsStr, flag: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_riftri"));
    command.current_dir(cwd).args(arguments);
    if flag {
        command.arg("--repository");
    }
    command.arg(target).output().expect("run CLI")
}

fn fixture_repository(root: &Path) {
    fs::create_dir(root).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn repository_option_matches_positional_selection_outside_the_target() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target with spaces");
    let other = fixture.path().join("other");
    fixture_repository(&target);
    fixture_repository(&other);
    for args in [
        vec!["enable"],
        vec!["doctor", "--json"],
        vec!["shell", "status"],
        vec!["status", "--json"],
        vec!["repair", "--json"],
        vec!["gc", "--json"],
        vec!["disable"],
    ] {
        let positional = riftri(&other, &args, target.as_os_str(), false);
        let flagged = riftri(&other, &args, target.as_os_str(), true);
        assert!(
            positional.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&positional.stderr)
        );
        assert!(
            flagged.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&flagged.stderr)
        );
        assert_eq!(positional.stdout, flagged.stdout, "{args:?}");
    }
    let config = Command::new("git")
        .current_dir(&other)
        .args(["config", "--local", "--get", "riftri.enabled"])
        .output()
        .unwrap();
    assert_eq!(
        config.status.code(),
        Some(1),
        "invoking repository must not be enabled"
    );
    assert!(!other.join(".git/riftri").exists());
}

#[test]
fn conflicting_repository_selectors_are_usage_errors() {
    for args in [
        vec!["enable"],
        vec!["disable"],
        vec!["doctor"],
        vec!["status"],
        vec!["repair"],
        vec!["gc"],
        vec!["shell", "status"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
            .args(&args)
            .args(["first", "--repository", "second"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    }
}

// APFS rejects invalid UTF-8 names; exercise actual native-byte paths on Linux.
#[cfg(target_os = "linux")]
#[test]
fn repository_option_preserves_non_utf8_path_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join(OsStr::from_bytes(b"repository-\xff"));
    fixture_repository(&target);
    let output = riftri(fixture.path(), &["enable"], target.as_os_str(), true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = Command::new("git")
        .current_dir(&target)
        .args(["config", "--local", "--get", "riftri.enabled"])
        .output()
        .unwrap();
    assert_eq!(config.stdout, b"true\n");
}
