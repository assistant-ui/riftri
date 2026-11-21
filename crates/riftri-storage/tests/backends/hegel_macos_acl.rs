#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::has_macos_acl;

fn chmod(arguments: &[&str], path: &Path) {
    let status = Command::new("/bin/chmod")
        .args(arguments)
        .arg(path)
        .status()
        .expect("run chmod");
    assert!(status.success(), "chmod {arguments:?} {}", path.display());
}

// Every case spawns chmod a few times, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn acl_presence_tracks_grants_removals_and_inheritance(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let entry = fixture.path().join("entry");
    let is_directory = tc.draw(gs::booleans());
    if is_directory {
        fs::create_dir(&entry).expect("create directory");
    } else {
        fs::write(&entry, b"contents").expect("write file");
    }
    let link = fixture.path().join("link");
    symlink(&entry, &link).expect("link to entry");

    let mut inheritable = false;
    for step in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(4)) {
        let has_acl = tc.draw(gs::booleans());
        if has_acl {
            let who = tc.draw(gs::sampled_from(vec!["everyone", "staff"]));
            // A deny rule on the directory would stop the test adding children.
            let rule = if is_directory {
                "allow"
            } else {
                tc.draw(gs::sampled_from(vec!["allow", "deny"]))
            };
            let permission = tc.draw(gs::sampled_from(vec![
                "read",
                "write",
                "execute",
                "append",
                "readattr",
                "writeattr",
                "readextattr",
                "writeextattr",
            ]));
            let inherit = is_directory && tc.draw(gs::booleans());
            let flags = if inherit { ",file_inherit" } else { "" };
            chmod(
                &["+a", &format!("{who} {rule} {permission}{flags}")],
                &entry,
            );
            inheritable |= inherit;
        } else {
            chmod(&["-N"], &entry);
            inheritable = false;
        }

        assert_eq!(has_macos_acl(&entry).expect("inspect entry"), has_acl);
        // The link is inspected itself, never the entry it names.
        assert!(!has_macos_acl(&link).expect("inspect link"));
        if is_directory {
            let child = entry.join(format!("child-{step}"));
            fs::write(&child, b"child").expect("write child");
            assert_eq!(has_macos_acl(&child).expect("inspect child"), inheritable);
        }
    }
}

#[hegel::test]
fn inspecting_a_path_that_cannot_be_opened_is_an_error_not_an_absent_acl(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let file = fixture.path().join("file");
    fs::write(&file, b"contents").expect("write file");
    let dangling = fixture.path().join("dangling");
    symlink("absent-target", &dangling).expect("dangling link");

    let parent = tc.draw(gs::sampled_from(vec![
        fixture.path().to_path_buf(),
        file,
        dangling,
    ]));
    // Arbitrary bytes, including NUL, separators, and non-UTF-8 sequences.
    let mut path = parent.into_os_string().into_vec();
    path.extend(tc.draw(gs::binary().max_size(8)));
    let path = OsString::from_vec(path);
    let path = Path::new(&path);

    match has_macos_acl(path) {
        Ok(has_acl) => {
            assert!(fs::symlink_metadata(path).is_ok(), "{path:?} is missing");
            assert!(!has_acl);
        }
        Err(_) => assert!(fs::symlink_metadata(path).is_err(), "{path:?} exists"),
    }
}
