//! Expanding Git LFS pointers from the local object store, against a store
//! and a staging tree in any shape: objects missing, truncated, corrupted, or
//! replaced by links and special files. Materialization writes exactly the
//! verified bytes into regular files it was given and nothing anywhere else.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

use hegel::TestCase;
use hegel::generators as gs;
use sha2::{Digest, Sha256};

use super::super::{GitLfsObject, GitLfsPointer, WorktreeError, materialize_git_lfs_objects};
use super::snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Intact,
    Missing,
    /// The pointer records another length than the object has.
    WrongSize,
    /// Same length, different bytes.
    Corrupt,
    Symlink,
    Directory,
    Fifo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    Pointer,
    Missing,
    Symlink,
    Directory,
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn lfs_objects_are_expanded_only_from_verified_regular_files(tc: TestCase) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(fixture.path()).unwrap();
    let staging = root.join("staging");
    let store = root.join("store");
    let outside = root.join("outside");
    for directory in [&staging, &store, &outside] {
        fs::create_dir(directory).unwrap();
    }
    fs::write(outside.join("kept"), b"user data\n").unwrap();

    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
    let mut objects = Vec::new();
    let mut plan = Vec::new();
    for index in 0..count {
        let data = tc.draw(gs::binary().min_size(1).max_size(48));
        let source = tc.draw(gs::sampled_from(vec![
            Source::Intact,
            Source::Intact,
            Source::Intact,
            Source::Missing,
            Source::WrongSize,
            Source::Corrupt,
            Source::Symlink,
            Source::Directory,
            Source::Fifo,
        ]));
        let destination = tc.draw(gs::sampled_from(vec![
            Destination::Pointer,
            Destination::Pointer,
            Destination::Pointer,
            Destination::Missing,
            Destination::Symlink,
            Destination::Directory,
        ]));
        let source_path = store.join(format!("object-{index}"));
        let mut stored = data.clone();
        match source {
            Source::Intact | Source::WrongSize => fs::write(&source_path, &stored).unwrap(),
            Source::Corrupt => {
                stored[0] ^= 0xff;
                fs::write(&source_path, &stored).unwrap();
            }
            Source::Missing => {}
            Source::Symlink => {
                let real: PathBuf = store.join(format!("real-{index}"));
                fs::write(&real, &stored).unwrap();
                symlink(&real, &source_path).unwrap();
            }
            Source::Directory => fs::create_dir(&source_path).unwrap(),
            Source::Fifo => {
                assert!(
                    Command::new("mkfifo")
                        .arg(&source_path)
                        .status()
                        .unwrap()
                        .success()
                );
            }
        }
        let checkout_path = PathBuf::from(format!("asset-{index}.bin"));
        let target = staging.join(&checkout_path);
        match destination {
            Destination::Pointer => fs::write(&target, b"pointer\n").unwrap(),
            Destination::Missing => {}
            Destination::Symlink => symlink(outside.join("kept"), &target).unwrap(),
            Destination::Directory => fs::create_dir(&target).unwrap(),
        }
        objects.push(GitLfsObject {
            checkout_path,
            source_path,
            pointer: GitLfsPointer {
                oid: crate::base_integrity::hex_lower(Sha256::digest(&data)),
                size: data.len() as u64 + u64::from(source == Source::WrongSize),
            },
        });
        plan.push((source, destination, data));
    }
    let store_before = snapshot_without_fifos(&store);
    let outside_before = snapshot(&outside);

    let outcome = materialize_git_lfs_objects(&staging, &objects);
    tc.note(&format!(
        "{:?}: {outcome:?}",
        plan.iter()
            .map(|(source, destination, _)| (source, destination))
            .collect::<Vec<_>>()
    ));
    assert_eq!(
        snapshot_without_fifos(&store),
        store_before,
        "the object store changed"
    );
    assert_eq!(snapshot(&outside), outside_before, "a write left staging");

    let first_bad = plan.iter().position(|(source, destination, _)| {
        *source != Source::Intact || *destination != Destination::Pointer
    });
    match first_bad {
        None => outcome.expect("every object is intact"),
        Some(bad) => {
            let error = outcome.expect_err("an unverifiable object was expanded");
            // The failure is the first bad object's, so nothing after an
            // object that failed verification was looked at.
            let text = format!("{error:?}");
            assert!(
                [
                    objects[bad].pointer.oid.clone(),
                    format!("asset-{bad}.bin"),
                    format!("object-{bad}"),
                ]
                .iter()
                .any(|name| text.contains(name)),
                "object {bad} is the first bad one, but the failure is {text}"
            );
            assert!(
                matches!(
                    error,
                    WorktreeError::Unsupported(_) | WorktreeError::Io { .. }
                ),
                "{error:?}"
            );
        }
    }
    for (index, (_, destination, data)) in plan.iter().enumerate() {
        let target = staging.join(&objects[index].checkout_path);
        if first_bad.is_none_or(|bad| index < bad) {
            assert_eq!(&fs::read(&target).unwrap(), data, "object {index}");
        } else if first_bad != Some(index) && *destination == Destination::Pointer {
            assert_eq!(
                fs::read(&target).unwrap(),
                b"pointer\n",
                "object {index} was expanded after an earlier one failed"
            );
        }
    }
}

/// `snapshot` reads every file, which would block on a FIFO.
fn snapshot_without_fifos(root: &std::path::Path) -> Vec<(std::ffi::OsString, Option<Vec<u8>>)> {
    use std::os::unix::fs::FileTypeExt;
    let mut entries = fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let kind = fs::symlink_metadata(entry.path()).unwrap().file_type();
            let data = (kind.is_file()).then(|| fs::read(entry.path()).unwrap());
            assert!(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_fifo());
            (entry.file_name(), data)
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
