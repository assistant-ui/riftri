#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::{BackendKind, CapabilityStatus, probe_backends};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Anchor {
    /// A relative destination, resolved against the working directory.
    WorkingDirectory,
    Root,
    Directory,
    DirectoryLink,
    DanglingLink,
    File,
}

// Cheap per case, and the file-as-destination corner is one case in dozens.
#[hegel::test(test_cases = 200)]
fn probing_uses_the_nearest_existing_directory_or_reports_nothing(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let root = fixture.path();
    fs::create_dir(root.join("directory")).expect("create directory");
    symlink("directory", root.join("directory-link")).expect("link to directory");
    symlink("absent-target", root.join("dangling-link")).expect("dangling link");
    fs::write(root.join("file"), b"not a directory").expect("write file");

    let anchor = tc.draw(gs::sampled_from(vec![
        Anchor::WorkingDirectory,
        Anchor::Root,
        Anchor::Directory,
        Anchor::DirectoryLink,
        Anchor::DanglingLink,
        Anchor::File,
    ]));
    let mut destination = match anchor {
        Anchor::WorkingDirectory => PathBuf::new(),
        Anchor::Root => root.to_path_buf(),
        Anchor::Directory => root.join("directory"),
        Anchor::DirectoryLink => root.join("directory-link"),
        Anchor::DanglingLink => root.join("dangling-link"),
        Anchor::File => root.join("file"),
    };
    // Components that do not exist yet: arbitrary bytes behind a prefix no
    // fixture entry shares. A separator inside one only adds more levels.
    let mut has_nul = false;
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(3)) {
        let mut component = b"absent-".to_vec();
        component.extend(tc.draw(gs::binary().max_size(8)));
        has_nul |= component.contains(&0);
        destination.push(OsString::from_vec(component));
    }
    // Only a NUL stops the lookup. A non-UTF-8 name is merely absent: the
    // probe describes the volume, not whether APFS would accept the name.
    let nearest: Option<PathBuf> = match anchor {
        _ if has_nul => None,
        Anchor::File => None,
        Anchor::WorkingDirectory => Some(std::env::current_dir().expect("working directory")),
        Anchor::Root | Anchor::DanglingLink => Some(root.to_path_buf()),
        Anchor::Directory | Anchor::DirectoryLink => Some(root.join("directory")),
    };

    let capabilities = probe_backends(&destination);

    let kinds = capabilities
        .iter()
        .map(|capability| capability.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [BackendKind::ApfsClone, BackendKind::FullCopyFallback]
    );
    for capability in &capabilities {
        // A full copy is never selectable without the caller's opt-in.
        assert_eq!(
            capability.requires_explicit_fallback,
            capability.kind == BackendKind::FullCopyFallback
        );
        let Some(nearest) = &nearest else {
            assert_eq!(capability.status, CapabilityStatus::Unavailable);
            assert_eq!(capability.volume, None);
            assert!(!capability.explanation.is_empty());
            continue;
        };
        let volume = capability.volume.as_ref().expect("inspected volume");
        assert_eq!(volume.requested_path, destination);
        assert_eq!(
            volume.probe_path,
            fs::canonicalize(nearest).expect("canonical ancestor")
        );
        assert_eq!(
            volume.identity.device_id,
            fs::metadata(nearest).expect("ancestor metadata").dev()
        );
        assert!(!volume.read_only);
        let native_clone = volume.identity.filesystem.eq_ignore_ascii_case("apfs");
        let supported = capability.kind == BackendKind::FullCopyFallback || native_clone;
        assert_eq!(
            capability.status == CapabilityStatus::Supported,
            supported,
            "{capability:?}"
        );
    }
}
