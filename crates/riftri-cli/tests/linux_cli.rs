#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

use riftri_storage::{CapabilityStatus, OverlayFsMounter, ReflinkCloner};

mod support;
use support::writable_tempdir as tempdir;

const PRIVATE_XATTR: &str = "user.riftri.private";

fn read_private_xattr(path: &Path) -> rustix::io::Result<Vec<u8>> {
    let mut value = Vec::with_capacity(64);
    rustix::fs::getxattr(
        path,
        PRIVATE_XATTR,
        rustix::buffer::spare_capacity(&mut value),
    )?;
    Ok(value)
}

fn allocated_bytes(path: &Path) -> u64 {
    let metadata = fs::symlink_metadata(path).expect("inspect allocated-byte fixture");
    let mut bytes = metadata.blocks().saturating_mul(512);
    if metadata.is_dir() {
        for entry in fs::read_dir(path).expect("read allocated-byte directory") {
            bytes = bytes.saturating_add(allocated_bytes(
                &entry.expect("read allocated-byte entry").path(),
            ));
        }
    }
    bytes
}

fn git(path: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command")
}

fn require_reflink(path: &Path) -> bool {
    let capability = ReflinkCloner::probe(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref(),
        Some(OsStr::new("1")),
        "Linux reflink test volume is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Linux reflink CLI test on this volume: {}",
        capability.explanation
    );
    false
}

fn require_overlayfs(path: &Path) -> bool {
    if std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref() != Some(OsStr::new("1")) {
        return false;
    }
    let capability = OverlayFsMounter::probe_current_namespace(path);
    assert_eq!(
        capability.status,
        CapabilityStatus::Supported,
        "Linux OverlayFS test namespace is required but unavailable: {}",
        capability.explanation
    );
    true
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
    assert!(
        git(repository, &["add", "--", "tracked.txt"])
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
fn explicit_and_transparent_commands_create_linux_reflink_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_reflink(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let explicit = fixture.path().join("explicit");
    let transparent = fixture.path().join("transparent");
    initialize_repository(&repository);

    let explicit_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&explicit)
        .args(["-b", "feature/linux-explicit", "HEAD"])
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
            .contains("Created Linux reflink-backed Git worktree")
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

    let transparent_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/linux-transparent",
        ])
        .arg(&transparent)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("run transparent Riftri add");
    assert!(
        transparent_output.status.success(),
        "transparent add failed: {}",
        String::from_utf8_lossy(&transparent_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&transparent_output.stderr)
            .contains("optimized Linux reflink worktree")
    );
    assert!(
        git(&transparent, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
}

#[test]
fn explicit_and_transparent_commands_manage_linux_overlayfs_worktrees() {
    let fixture = tempdir().expect("fixture directory");
    if !require_overlayfs(fixture.path()) {
        return;
    }
    let repository = fixture.path().join("repository");
    let explicit = fixture.path().join("explicit");
    let transparent = fixture.path().join("transparent");
    initialize_repository(&repository);

    let explicit_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add"])
        .arg(&explicit)
        .args(["-b", "feature/overlay-explicit", "HEAD"])
        .current_dir(&repository)
        .output()
        .expect("run explicit OverlayFS add");
    assert!(
        explicit_output.status.success(),
        "explicit OverlayFS add failed: {}",
        String::from_utf8_lossy(&explicit_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit_output.stdout)
            .contains("Created OverlayFS-backed Git worktree")
    );
    assert!(
        git(&explicit, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    assert!(
        Command::new(env!("CARGO_BIN_EXE_riftri"))
            .arg("enable")
            .current_dir(&repository)
            .status()
            .expect("enable repository")
            .success()
    );
    let transparent_output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/overlay-transparent",
        ])
        .arg(&transparent)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("run transparent OverlayFS add");
    assert!(
        transparent_output.status.success(),
        "transparent OverlayFS add failed: {}",
        String::from_utf8_lossy(&transparent_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&transparent_output.stderr)
            .contains("optimized OverlayFS worktree")
    );
    assert!(
        git(&transparent, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    let transparent_remove = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&transparent)
        .current_dir(&repository)
        .output()
        .expect("run transparent OverlayFS removal");
    assert!(
        transparent_remove.status.success(),
        "transparent OverlayFS removal failed: {}",
        String::from_utf8_lossy(&transparent_remove.stderr)
    );
    let explicit_remove = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "remove"])
        .arg(&explicit)
        .current_dir(&repository)
        .output()
        .expect("run explicit OverlayFS removal");
    assert!(
        explicit_remove.status.success(),
        "explicit OverlayFS removal failed: {}",
        String::from_utf8_lossy(&explicit_remove.stderr)
    );
    assert!(!explicit.exists());
    assert!(!transparent.exists());
}

#[test]
fn installed_helper_manages_overlayfs_from_an_ordinary_shell() {
    if std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS_HELPER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let repository = fixture.path().join("repository");
    let worktree = fixture.path().join("helper-view");
    initialize_repository(&repository);
    fs::write(repository.join("executable.sh"), "#!/bin/sh\nexit 0\n")
        .expect("write executable fixture");
    fs::set_permissions(
        repository.join("executable.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("set executable fixture mode");
    symlink("tracked.txt", repository.join("tracked-link")).expect("create symlink fixture");
    fs::write(repository.join("large.bin"), vec![0x5a; 8 * 1024 * 1024])
        .expect("write allocation fixture");
    assert!(
        git(
            &repository,
            &["add", "--", "executable.sh", "tracked-link", "large.bin",],
        )
        .status
        .success()
    );
    assert!(
        git(
            &repository,
            &["commit", "--quiet", "-m", "metadata fixtures"],
        )
        .status
        .success()
    );

    let helper = Path::new(OverlayFsMounter::DEFAULT_HELPER_PATH);
    let metadata = fs::symlink_metadata(helper).expect("installed helper metadata");
    assert_eq!(std::os::unix::fs::MetadataExt::uid(&metadata), 0);
    assert_ne!(
        std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o4000,
        0
    );
    let rejected = Command::new(helper)
        .arg("doctor")
        .output()
        .expect("invoke installed helper with a normal CLI command");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("invalid internal OverlayFS helper request")
    );

    let enable = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("enable")
        .current_dir(&repository)
        .output()
        .expect("enable repository");
    assert!(enable.status.success());

    let add = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args([
            "exec",
            "--",
            "git",
            "worktree",
            "add",
            "-b",
            "feature/helper-view",
        ])
        .arg(&worktree)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .expect("create helper-backed worktree");
    assert!(
        add.status.success(),
        "helper-backed add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        String::from_utf8_lossy(&add.stderr).contains("optimized OverlayFS worktree"),
        "unexpected add output: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        git(&worktree, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );
    assert_ne!(
        fs::metadata(worktree.join("tracked.txt"))
            .expect("tracked view metadata")
            .permissions()
            .mode()
            & 0o200,
        0,
        "helper-backed checkout did not restore the owner's write bit"
    );
    let journal_path = fs::read_dir(repository.join(".git/riftri/operations"))
        .expect("read add journals")
        .next()
        .expect("one add journal")
        .expect("read add journal entry")
        .path();
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal_path).expect("read helper-backed add journal"))
            .expect("decode helper-backed add journal");
    let operation_id = journal["operation_id"]
        .as_str()
        .expect("helper-backed operation ID");
    let layout_root = repository
        .join(".git/riftri/overlays/v1")
        .join(operation_id);
    assert!(
        allocated_bytes(&layout_root.join("upper")) < 1024 * 1024,
        "metadata-only helper activation allocated too much private data"
    );
    assert_eq!(
        journal["overlayfs"]["mount_context"]["profile"],
        "privileged-trusted-xattr"
    );
    assert_eq!(
        journal["overlayfs"]["mount_identity"]["profile"],
        "privileged-trusted-xattr"
    );

    fs::set_permissions(
        worktree.join("executable.sh"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("change helper-view executable mode");
    assert_eq!(
        fs::metadata(worktree.join("executable.sh"))
            .expect("helper-view executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    fs::set_permissions(
        worktree.join("executable.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("restore helper-view executable mode");

    fs::remove_file(worktree.join("tracked-link")).expect("replace helper-view symlink");
    symlink("executable.sh", worktree.join("tracked-link"))
        .expect("create private helper-view symlink");
    assert_eq!(
        String::from_utf8_lossy(&git(&worktree, &["show", "HEAD:tracked-link"]).stdout),
        "tracked.txt"
    );
    fs::remove_file(worktree.join("tracked-link")).expect("remove private helper-view symlink");
    symlink("tracked.txt", worktree.join("tracked-link")).expect("restore helper-view symlink");

    rustix::fs::setxattr(
        worktree.join("tracked.txt"),
        PRIVATE_XATTR,
        b"helper-view",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("set helper-view xattr");
    assert_eq!(
        read_private_xattr(&worktree.join("tracked.txt")).expect("read helper-view xattr"),
        b"helper-view"
    );
    assert!(
        git(&worktree, &["status", "--porcelain=v1"])
            .stdout
            .is_empty()
    );

    fs::write(worktree.join("tracked.txt"), "private\n").expect("write through merged view");
    assert_eq!(
        String::from_utf8_lossy(&git(&worktree, &["show", "HEAD:tracked.txt"]).stdout),
        "tracked\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&git(&worktree, &["status", "--porcelain=v1"]).stdout),
        " M tracked.txt\n"
    );
    assert!(
        git(&worktree, &["checkout", "--", "tracked.txt"])
            .status
            .success()
    );

    let remove = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "git", "worktree", "remove"])
        .arg(&worktree)
        .current_dir(&repository)
        .output()
        .expect("remove helper-backed worktree");
    assert!(
        remove.status.success(),
        "helper-backed removal failed: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    assert!(!worktree.exists());
}
