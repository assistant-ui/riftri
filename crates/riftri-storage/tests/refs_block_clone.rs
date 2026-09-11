#![cfg(target_os = "windows")]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

use riftri_storage::{CapabilityStatus, RefsBlockCloner};
use tempfile::tempdir;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE;

fn refs_available(path: &Path) -> bool {
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
        "skipping Windows ReFS test on this volume: {}",
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

    let capability = RefsBlockCloner::probe(fixture.path());

    let after = fs::read_dir(fixture.path())
        .expect("read fixture after probe")
        .count();
    assert_eq!(before, after, "active probe left a visible artifact");
    if std::env::var_os("RIFTRI_REQUIRE_REFS").as_deref() == Some(OsStr::new("1")) {
        assert_eq!(
            capability.status,
            CapabilityStatus::Supported,
            "required ReFS probe failed: {}",
            capability.explanation
        );
    }
}

#[test]
fn clones_aligned_data_and_keeps_writes_private() {
    let fixture = tempdir().expect("fixture directory");
    if !refs_available(fixture.path()) {
        return;
    }
    let source = fixture.path().join("base");
    let destination = fixture.path().join("view");
    fs::create_dir(&source).expect("create base");
    let mut contents = vec![0x35_u8; 128 * 1024];
    contents.extend_from_slice(b"unaligned tail");
    fs::write(source.join("payload.bin"), &contents).expect("write source payload");
    RefsBlockCloner::make_tree_read_only(&source).expect("protect immutable base");

    RefsBlockCloner::clone_tree(&source, &destination).expect("clone ReFS tree");
    RefsBlockCloner::make_tree_owner_writable(&destination).expect("make view writable");

    assert_eq!(
        fs::read(destination.join("payload.bin")).expect("read cloned payload"),
        contents
    );
    assert_eq!(
        fs::metadata(destination.join("payload.bin"))
            .expect("inspect cloned payload")
            .file_attributes()
            & FILE_ATTRIBUTE_SPARSE_FILE,
        0,
        "a non-sparse source must produce a non-sparse view"
    );
    let mut cloned = fs::OpenOptions::new()
        .write(true)
        .open(destination.join("payload.bin"))
        .expect("open cloned payload");
    cloned.write_all(b"private").expect("write private change");
    cloned.sync_all().expect("sync private change");
    assert_eq!(
        &fs::read(source.join("payload.bin")).expect("read immutable source")[..7],
        &[0x35; 7]
    );
}

#[test]
fn refuses_to_replace_an_existing_destination() {
    let fixture = tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    let destination = fixture.path().join("destination");
    fs::create_dir(&source).expect("create source");
    fs::create_dir(&destination).expect("create destination");

    let error = RefsBlockCloner::clone_tree(&source, &destination)
        .expect_err("existing destination must be rejected");

    assert!(error.to_string().contains("already exists"));
}

#[test]
fn block_clone_consumes_materially_less_new_space_than_its_logical_size() {
    const LOGICAL_BYTES: usize = 32 * 1024 * 1024;

    let fixture = tempdir().expect("fixture directory");
    if !refs_available(fixture.path()) {
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

    let available_before = fs2::available_space(fixture.path()).expect("space before block clone");
    RefsBlockCloner::clone_tree(&source, &destination).expect("clone ReFS tree");
    let available_after = fs2::available_space(fixture.path()).expect("space after block clone");
    let physical_growth = available_before.saturating_sub(available_after);

    eprintln!(
        "ReFS block-clone logical bytes: {LOGICAL_BYTES}; measured volume growth: {physical_growth}"
    );
    assert!(
        physical_growth < (LOGICAL_BYTES as u64 / 4),
        "block clone of {LOGICAL_BYTES} bytes consumed {physical_growth} new physical bytes"
    );
}
