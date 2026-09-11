#![cfg(target_os = "linux")]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use riftri_storage::{BackendKind, CapabilityStatus, OverlayFsMounter};
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
    if std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref() == Some(OsStr::new("1")) {
        assert_eq!(
            capability.status,
            CapabilityStatus::Supported,
            "required OverlayFS probe failed: {}",
            capability.explanation
        );
    }
}
