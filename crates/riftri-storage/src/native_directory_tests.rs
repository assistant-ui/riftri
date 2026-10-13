use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use tempfile::tempdir;

#[test]
fn only_changed_or_special_directory_modes_need_restoration() {
    for mode in [0o500, 0o550, 0o555, 0o700, 0o755, 0o1755, 0o2755, 0o4755] {
        for writable in [None, Some(0o200), Some(0o220), Some(0o222)] {
            let fixture = tempdir().unwrap();
            let source = fixture.path().join("source");
            let destination = fixture.path().join("destination");
            fs::create_dir_all(source.join("nested")).unwrap();
            symlink("nested", source.join("link")).unwrap();
            let mut files = Vec::new();
            let mut directories = Vec::new();
            super::prepare_clone_directory(
                &source,
                &destination,
                mode,
                writable,
                &mut files,
                &mut directories,
            )
            .unwrap();
            let final_mode = writable.map_or(mode, |bits| mode | 0o700 | bits);
            let root_mode = directories.iter().find(|(path, _)| path == &destination);
            let needs_restore = final_mode != mode | 0o700 || mode & 0o7000 != 0;
            assert_eq!(
                root_mode.is_some(),
                needs_restore,
                "mode={mode:o}, writable={writable:?}"
            );
            if let Some((_, pending)) = root_mode {
                assert_eq!(*pending, final_mode);
            }
            for (path, mode) in directories.into_iter().rev() {
                fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            }
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o7777,
                final_mode
            );
            assert_eq!(
                fs::read_link(destination.join("link")).unwrap(),
                std::path::Path::new("nested")
            );
            // Restore traversal for disposable-fixture cleanup.
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn omitting_identical_final_chmod_preserves_inherited_acls() {
    use std::process::Command;
    let fixture = tempdir().unwrap();
    let source = fixture.path().join("source");
    let parent = fixture.path().join("parent");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir(&parent).unwrap();
    assert!(Command::new("/bin/chmod").args([
        "+a", "everyone allow list,search,readattr,readextattr,readsecurity,file_inherit,directory_inherit",
    ]).arg(&parent).status().unwrap().success());
    let destination = parent.join("clone");
    let mut files = Vec::new();
    let mut directories = Vec::new();
    super::prepare_clone_directory(
        &source,
        &destination,
        0o555,
        Some(0o200),
        &mut files,
        &mut directories,
    )
    .unwrap();
    assert!(directories.is_empty());
    for path in [&destination, &destination.join("nested")] {
        let acl = |path: &std::path::Path| {
            let result = Command::new("/bin/ls")
                .arg("-lde")
                .arg(path)
                .output()
                .unwrap();
            assert!(result.status.success());
            String::from_utf8(result.stdout)
                .unwrap()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let before = acl(path);
        assert!(
            before.contains("inherited"),
            "missing inherited ACL: {before}"
        );
        let mode = fs::metadata(path).unwrap().permissions();
        fs::set_permissions(path, mode.clone()).unwrap();
        assert_eq!(
            before,
            acl(path),
            "the redundant final chmod changes ACL semantics"
        );
        assert_eq!(
            mode.mode(),
            fs::metadata(path).unwrap().permissions().mode()
        );
    }
}
