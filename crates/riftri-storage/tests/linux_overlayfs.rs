#![cfg(target_os = "linux")]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::process::Command;

use riftri_storage::{
    BackendKind, CapabilityStatus, OverlayFsMountIdentity, OverlayFsMountState, OverlayFsMounter,
    StorageError,
};
use tempfile::tempdir;

#[test]
fn active_probe_verifies_copy_up_without_leaving_artifacts() {
    let fixture = tempdir().expect("fixture directory");
    assert_clean_probe(fixture.path());
}

#[test]
fn active_probe_accepts_mount_option_delimiters_in_destination_path() {
    let fixture = tempdir().expect("fixture directory");
    let destination = fixture.path().join("comma,colon:backslash\\ and space");
    fs::create_dir(&destination).expect("unusual destination directory");
    assert_clean_probe(&destination);
}

#[test]
fn active_probe_accepts_non_utf8_destination_path() {
    let fixture = tempdir().expect("fixture directory");
    let destination = fixture
        .path()
        .join(OsString::from_vec(b"non-utf8-\xff".to_vec()));
    fs::create_dir(&destination).expect("non-UTF-8 destination directory");
    assert_clean_probe(&destination);
}

#[test]
fn durable_layout_mounts_directly_at_the_requested_view() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower,with:delimiters");
    let layout_root = fixture.path().join("state/overlays/v1/operation-1");
    let merged = fixture.path().join(OsString::from_vec(
        b"worktree,colon:space and non-utf8-\xff".to_vec(),
    ));
    fs::create_dir_all(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");

    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    assert_eq!(layout.root(), layout_root);
    assert_eq!(layout.lower(), lower);
    assert_eq!(layout.upper(), layout_root.join("upper"));
    assert_eq!(layout.work(), layout_root.join("work"));
    assert_eq!(layout.merged(), merged);

    let identity = OverlayFsMounter::mount(&layout).expect("mount durable view");
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("inspect durable view"),
        OverlayFsMountState::Active
    );
    assert_eq!(
        fs::read(merged.join("payload")).expect("read lower"),
        b"base"
    );

    fs::write(merged.join("payload"), b"view").expect("copy up private write");
    assert_eq!(
        fs::read(lower.join("payload")).expect("read lower"),
        b"base"
    );
    assert_eq!(
        fs::read(layout.upper().join("payload")).expect("read upper"),
        b"view"
    );

    assert!(OverlayFsMounter::unmount(&layout, &identity).expect("unmount durable view"));
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("inspect unmounted view"),
        OverlayFsMountState::Absent
    );
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove durable private layers");
    assert!(!layout_root.exists());
}

#[test]
fn unmount_refuses_a_mount_identity_mismatch() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");

    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let identity = OverlayFsMounter::mount(&layout).expect("mount durable view");
    let mut wrong_identity = identity.clone();
    wrong_identity.mount_id = identity.mount_id.saturating_add(1);

    let error = OverlayFsMounter::unmount(&layout, &wrong_identity)
        .expect_err("mismatched identity must not unmount");
    assert!(matches!(error, StorageError::OverlayFsMountConflict { .. }));
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("original mount remains"),
        OverlayFsMountState::Active
    );

    OverlayFsMounter::unmount(&layout, &identity).expect("unmount original view");
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove durable private layers");
}

#[test]
fn mount_recovery_survives_creator_process_exit() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    let identity_path = fixture.path().join("mount-identity.json");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");
    OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");

    let status = Command::new(std::env::current_exe().expect("integration test executable"))
        .arg("--exact")
        .arg("persistent_mount_helper")
        .arg("--nocapture")
        .env("RIFTRI_OVERLAYFS_MOUNT_HELPER", "1")
        .env("RIFTRI_OVERLAYFS_LAYOUT_ROOT", &layout_root)
        .env("RIFTRI_OVERLAYFS_LOWER", &lower)
        .env("RIFTRI_OVERLAYFS_MERGED", &merged)
        .env("RIFTRI_OVERLAYFS_IDENTITY", &identity_path)
        .status()
        .expect("run mount helper process");
    assert!(status.success(), "mount helper process failed: {status}");

    let identity: OverlayFsMountIdentity =
        serde_json::from_slice(&fs::read(&identity_path).expect("read persisted mount identity"))
            .expect("decode persisted mount identity");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("reload durable layout after creator exit");
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("recover mount state"),
        OverlayFsMountState::Active
    );
    assert_eq!(
        fs::read(merged.join("payload")).expect("read recovered mount"),
        b"base"
    );

    OverlayFsMounter::unmount(&layout, &identity).expect("unmount recovered view");
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove recovered private layers");
}

#[test]
fn persistent_mount_helper() {
    if std::env::var_os("RIFTRI_OVERLAYFS_MOUNT_HELPER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let layout_root = required_helper_path("RIFTRI_OVERLAYFS_LAYOUT_ROOT");
    let lower = required_helper_path("RIFTRI_OVERLAYFS_LOWER");
    let merged = required_helper_path("RIFTRI_OVERLAYFS_MERGED");
    let identity_path = required_helper_path("RIFTRI_OVERLAYFS_IDENTITY");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("mount helper loads durable layout");
    let identity = OverlayFsMounter::mount(&layout).expect("mount helper creates durable view");
    fs::write(
        identity_path,
        serde_json::to_vec(&identity).expect("encode mount identity"),
    )
    .expect("persist mount identity");
}

#[test]
fn prepare_rejects_a_nonempty_mountpoint_before_mutating_layout_state() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(merged.join("unexpected"), b"preserve").expect("unexpected destination file");

    let error = OverlayFsMounter::prepare(&layout_root, &lower, &merged)
        .expect_err("nonempty mountpoint must fail closed");
    assert!(matches!(error, StorageError::InvalidOverlayFsLayout { .. }));
    assert!(!layout_root.exists());
    assert_eq!(
        fs::read(merged.join("unexpected")).expect("preserved destination file"),
        b"preserve"
    );
}

fn assert_clean_probe(destination: &Path) {
    let before = fs::read_dir(destination)
        .expect("read destination before probe")
        .count();

    let capability = OverlayFsMounter::probe(destination);

    assert_eq!(capability.kind, BackendKind::OverlayFs);
    assert!(capability.volume.is_some());
    let after = fs::read_dir(destination)
        .expect("read destination after probe")
        .count();
    assert_eq!(before, after, "active probe left a visible artifact");
    if overlayfs_mounts_required() {
        assert_eq!(
            capability.status,
            CapabilityStatus::Supported,
            "required OverlayFS probe failed: {}",
            capability.explanation
        );
    }
}

fn overlayfs_mounts_required() -> bool {
    std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref() == Some(OsStr::new("1"))
}

fn required_helper_path(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("missing {name}"))
}
