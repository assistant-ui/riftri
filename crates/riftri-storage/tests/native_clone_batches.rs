#![cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]

#[cfg(target_os = "macos")]
use riftri_storage::ApfsCloner as Native;
#[cfg(target_os = "linux")]
use riftri_storage::ReflinkCloner as Native;
#[cfg(target_os = "windows")]
use riftri_storage::RefsBlockCloner as Native;
use std::fs;

#[test]
fn clone_batches_preserve_every_file_and_private_writes() {
    let fixture = tempfile::tempdir().unwrap();
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if Native::probe(fixture.path()).status != riftri_storage::CapabilityStatus::Supported {
        let requirement = if cfg!(target_os = "linux") {
            "RIFTRI_REQUIRE_REFLINK"
        } else {
            "RIFTRI_REQUIRE_REFS"
        };
        assert_ne!(
            std::env::var_os(requirement).as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "required native COW test volume unavailable"
        );
        return;
    }
    let source = fixture.path().join("source");
    let view = fixture.path().join("view");
    // More than two 1024-item batches, plus a partial tail, spread across
    // directories so batches also cross directory traversal boundaries.
    for directory in 0..3 {
        fs::create_dir_all(source.join(directory.to_string())).unwrap();
        for file in 0..701 {
            fs::write(
                source.join(format!("{directory}/{file}")),
                format!("base {directory}/{file}"),
            )
            .unwrap();
        }
    }
    Native::make_tree_read_only(&source).unwrap();
    let result = Native::clone_tree_owner_writable(&source, &view);
    // Restore fixture permissions even when the clone fails.
    Native::make_tree_owner_writable(&source).unwrap();
    result.unwrap();
    for directory in 0..3 {
        for file in 0..701 {
            let relative = format!("{directory}/{file}");
            let expected = format!("base {relative}");
            assert_eq!(fs::read(view.join(&relative)).unwrap(), expected.as_bytes());
            if file == 0 || file == 700 {
                fs::write(view.join(&relative), b"private").unwrap();
                assert_eq!(
                    fs::read(source.join(&relative)).unwrap(),
                    expected.as_bytes()
                );
            }
        }
    }
}
