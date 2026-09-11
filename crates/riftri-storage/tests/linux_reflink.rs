#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use riftri_storage::{CapabilityStatus, ReflinkCloner};
use tempfile::tempdir;

fn reflink_available(path: &std::path::Path) -> bool {
    let capability = ReflinkCloner::probe(path);
    if capability.status == CapabilityStatus::Supported {
        return true;
    }
    assert_ne!(
        std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "Linux reflink test volume is required but unavailable: {}",
        capability.explanation,
    );
    eprintln!(
        "skipping Linux reflink test on this volume: {}",
        capability.explanation
    );
    false
}

#[test]
fn active_probe_leaves_no_directory_entries() {
    let fixture = tempdir().expect("fixture directory");
    let before = fs::read_dir(fixture.path())
        .expect("read fixture before probe")
        .count();

    let capability = ReflinkCloner::probe(fixture.path());

    let after = fs::read_dir(fixture.path())
        .expect("read fixture after probe")
        .count();
    assert_eq!(before, after, "active probe left a visible artifact");
    if std::env::var_os("RIFTRI_REQUIRE_REFLINK").as_deref() == Some(std::ffi::OsStr::new("1")) {
        assert_eq!(
            capability.status,
            CapabilityStatus::Supported,
            "required reflink probe failed: {}",
            capability.explanation
        );
    }
}

#[test]
fn clones_a_tree_with_private_writes_and_git_modes() {
    let fixture = tempdir().expect("fixture directory");
    if !reflink_available(fixture.path()) {
        return;
    }
    let source = fixture.path().join("base");
    let destination = fixture.path().join("view");
    fs::create_dir(&source).expect("create base");
    fs::write(source.join("regular.txt"), "base contents\n").expect("write regular file");
    fs::write(source.join("executable.sh"), "#!/bin/sh\nexit 0\n").expect("write executable");
    fs::set_permissions(
        source.join("executable.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("set executable mode");
    symlink("regular.txt", source.join("link")).expect("create symlink");

    ReflinkCloner::clone_tree(&source, &destination).expect("clone reflink tree");

    assert_eq!(
        fs::read_to_string(destination.join("regular.txt")).expect("read clone"),
        "base contents\n"
    );
    assert_ne!(
        fs::metadata(source.join("regular.txt"))
            .expect("source metadata")
            .ino(),
        fs::metadata(destination.join("regular.txt"))
            .expect("destination metadata")
            .ino()
    );
    assert_ne!(
        fs::metadata(destination.join("executable.sh"))
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read_link(destination.join("link")).expect("read cloned symlink"),
        std::path::Path::new("regular.txt")
    );

    fs::write(destination.join("regular.txt"), "private change\n").expect("write private clone");
    assert_eq!(
        fs::read_to_string(source.join("regular.txt")).expect("read source"),
        "base contents\n"
    );
}

#[test]
fn refuses_to_replace_an_existing_destination() {
    let fixture = tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    let destination = fixture.path().join("destination");
    fs::create_dir(&source).expect("create source");
    fs::create_dir(&destination).expect("create destination");

    let error = ReflinkCloner::clone_tree(&source, &destination)
        .expect_err("existing destination must be rejected");

    assert!(error.to_string().contains("already exists"));
}

#[test]
fn cached_clone_consumes_materially_less_new_space_than_its_logical_size() {
    const LOGICAL_BYTES: usize = 32 * 1024 * 1024;

    let fixture = tempdir().expect("fixture directory");
    if !reflink_available(fixture.path()) {
        return;
    }
    let source = fixture.path().join("base");
    let destination = fixture.path().join("view");
    fs::create_dir(&source).expect("create base");
    let mut file = fs::File::create(source.join("payload.bin")).expect("create payload");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut block = [0_u8; 64 * 1024];
    for _ in 0..(LOGICAL_BYTES / block.len()) {
        for chunk in block.chunks_exact_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }
        file.write_all(&block).expect("write payload block");
    }
    file.sync_all().expect("sync payload");

    let available_before = fs2::available_space(fixture.path()).expect("space before reflink");
    ReflinkCloner::clone_tree(&source, &destination).expect("clone reflink tree");
    fs::File::open(&destination)
        .expect("open cloned directory")
        .sync_all()
        .expect("sync cloned directory");
    let available_after = fs2::available_space(fixture.path()).expect("space after reflink");
    let physical_growth = available_before.saturating_sub(available_after);

    eprintln!("reflink logical bytes: {LOGICAL_BYTES}; measured volume growth: {physical_growth}");
    assert!(
        physical_growth < (LOGICAL_BYTES as u64 / 4),
        "reflink of {LOGICAL_BYTES} bytes consumed {physical_growth} new physical bytes"
    );
}
