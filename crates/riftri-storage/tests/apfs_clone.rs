#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use riftri_storage::ApfsCloner;
use tempfile::tempdir;

#[test]
fn clones_a_tree_with_private_writes_and_git_modes() {
    let fixture = tempdir().expect("fixture directory");
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

    ApfsCloner::clone_tree(&source, &destination).expect("clone APFS tree");

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

    let error = ApfsCloner::clone_tree(&source, &destination)
        .expect_err("existing destination must be rejected");

    assert!(error.to_string().contains("already exists"));
}
