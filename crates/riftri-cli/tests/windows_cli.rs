#![cfg(target_os = "windows")]

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use riftri_storage::{CapabilityStatus, RefsBlockCloner};

mod support;
use support::writable_tempdir as tempdir;

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn require_refs(path: &Path) -> bool {
    let capability = RefsBlockCloner::probe(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_REFS").as_deref(),
        Some(OsStr::new("1")),
        "Windows ReFS test volume is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Windows ReFS CLI test on this volume: {}",
        capability.explanation
    );
    false
}

fn initialize_repository(repository: &Path) {
    fs::create_dir(repository).expect("create repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"][..],
        &["config", "user.email", "riftri@example.invalid"][..],
        &["config", "core.autocrlf", "false"][..],
    ] {
        assert!(git(repository, arguments).status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
    fs::write(repository.join("payload.bin"), vec![0x42; 256 * 1024])
        .expect("write block-clone payload");
    assert!(
        git(repository, &["add", "--", "tracked.txt", "payload.bin"])
            .status
            .success()
    );
    assert!(
        git(repository, &["commit", "--quiet", "-m", "initial"])
            .status
            .success()
    );
}

#[test]
fn explicit_and_process_scoped_commands_create_refs_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_refs(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let explicit = fixture.path().join("explicit");
    let transparent = fixture.path().join("transparent");
    let powershell = fixture.path().join("powershell");
    initialize_repository(&repository);

    let explicit_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&explicit)
        .args(["-b", "feature/windows-explicit", "HEAD"])
        .current_dir(&repository)
        .output()
        .expect("run explicit Riftri add");
    assert!(
        explicit_output.status.success(),
        "explicit add failed: {}",
        String::from_utf8_lossy(&explicit_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit_output.stdout)
            .contains("Created ReFS block clone-backed Git worktree")
    );
    assert!(
        git(&explicit, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    let enable_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("enable")
        .current_dir(&repository)
        .output()
        .expect("enable repository");
    assert!(enable_output.status.success());
    assert!(
        String::from_utf8_lossy(&enable_output.stdout)
            .contains("Invoke-Expression (& riftri shell hook powershell | Out-String)")
    );

    let transparent_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/windows-transparent",
        ])
        .arg(&transparent)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("run process-scoped Riftri add");
    assert!(
        transparent_output.status.success(),
        "process-scoped add failed: {}",
        String::from_utf8_lossy(&transparent_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&transparent_output.stderr)
            .contains("optimized ReFS block clone worktree")
    );
    assert!(
        git(&transparent, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    let cache = fixture.path().join("powershell-cache");
    let powershell_output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$hook = (& $env:RIFTRI_TEST_BIN shell hook powershell) -join [Environment]::NewLine; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; Invoke-Expression $hook; & powershell.exe -NoLogo -NoProfile -NonInteractive -Command 'git worktree add -b feature/windows-powershell $env:RIFTRI_TEST_DESTINATION HEAD; exit $LASTEXITCODE'; exit $LASTEXITCODE",
        ])
        .current_dir(&repository)
        .env("RIFTRI_TEST_BIN", env!("CARGO_BIN_EXE_riftri"))
        .env("RIFTRI_TEST_DESTINATION", &powershell)
        .env("RIFTRI_CACHE_DIR", &cache)
        .output()
        .expect("run PowerShell-activated Riftri add");
    assert!(
        powershell_output.status.success(),
        "PowerShell add failed: {}\n{}",
        String::from_utf8_lossy(&powershell_output.stdout),
        String::from_utf8_lossy(&powershell_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&powershell_output.stderr)
            .contains("optimized ReFS block clone worktree")
    );
    assert!(
        git(&powershell, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}
