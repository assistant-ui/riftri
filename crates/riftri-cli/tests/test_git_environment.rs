//! Test wrappers must delegate to Git, never back into an activated Riftri shim.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::process::Command;

#[path = "support/real_git.rs"]
mod real_git;

#[test]
fn git_discovery_probe() {
    let Some(expected) = std::env::var_os("RIFTRI_TEST_EXPECTED_GIT") else {
        return;
    };
    // Only discover the target; executing an incorrectly selected shim here
    // could start the very unbounded wrapper recursion this regression guards.
    assert_eq!(real_git::real_git().as_os_str(), expected);
}

#[test]
fn test_wrappers_resolve_git_in_active_and_stripped_shells() {
    let fixture = tempfile::tempdir().unwrap();
    let shim = fixture.path().join("shim");
    fs::create_dir(&shim).unwrap();
    symlink(env!("CARGO_BIN_EXE_riftri"), shim.join("git")).unwrap();
    // Spaces and a trailing newline must survive discovery intact. `riftri
    // exec` runs the discovered Git once to check its version, and
    // `/usr/bin/git` is an `xcrun` trampoline that picks its tool by the name
    // it was started as, so link the developer tool it dispatches to.
    let native_git = fixture.path().join("real Git\n");
    let developer_git = Command::new("/usr/bin/xcrun")
        .args(["--find", "git"])
        .output()
        .unwrap();
    assert!(developer_git.status.success());
    let developer_git = std::ffi::OsStr::from_bytes(
        developer_git
            .stdout
            .strip_suffix(b"\n")
            .expect("xcrun prints one path"),
    );
    symlink(developer_git, &native_git).unwrap();
    fs::write(
        shim.join("riftri-real-git"),
        native_git.as_os_str().as_bytes(),
    )
    .unwrap();
    let path = std::env::join_paths([
        shim.as_path(),
        std::path::Path::new("/usr/bin"),
        std::path::Path::new("/bin"),
    ])
    .unwrap();
    for active in [false, true] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("RIFTRI_") {
                command.env_remove(name);
            }
        }
        command
            .args(["--exact", "git_discovery_probe", "--nocapture"])
            .env("PATH", &path)
            .env("RIFTRI_TEST_EXPECTED_GIT", &native_git);
        if active {
            command
                .env("RIFTRI_SHIM_ACTIVE", "1")
                .env("RIFTRI_REAL_GIT", &native_git);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "active={active}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
