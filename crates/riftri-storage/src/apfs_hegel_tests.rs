use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;

use super::{clone_path, clone_tree, clone_tree_bulk_for_evaluation};
use crate::StorageError;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Missing,
    File,
    Directory,
    /// A path no syscall can receive: it carries an interior NUL byte.
    EmbeddedNul,
}

fn place(tc: &TestCase, root: &Path, name: &str, shape: Shape) -> PathBuf {
    let path = root.join(name);
    match shape {
        Shape::Missing => {}
        Shape::File => fs::write(&path, tc.draw(gs::binary().max_size(64))).expect("write file"),
        Shape::Directory => fs::create_dir(&path).expect("create directory"),
        Shape::EmbeddedNul => {
            let mut bytes = path.into_os_string().into_vec();
            bytes.push(0);
            bytes.extend(tc.draw(gs::binary().max_size(8)));
            return PathBuf::from(OsString::from_vec(bytes));
        }
    }
    path
}

fn listing(root: &Path) -> Vec<(OsString, Option<Vec<u8>>)> {
    let mut entries = fs::read_dir(root)
        .expect("read fixture")
        .map(|entry| {
            let entry = entry.expect("fixture entry");
            (entry.file_name(), fs::read(entry.path()).ok())
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[hegel::test]
fn clone_path_clones_one_file_or_changes_nothing(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let shape = || {
        tc.draw(gs::sampled_from(vec![
            Shape::Missing,
            Shape::File,
            Shape::EmbeddedNul,
        ]))
    };
    let (source_shape, destination_shape) = (shape(), shape());
    let source = place(&tc, fixture.path(), "source", source_shape);
    let destination = place(&tc, fixture.path(), "destination", destination_shape);
    let before = listing(fixture.path());

    let result = clone_path(&source, &destination);

    if (source_shape, destination_shape) == (Shape::File, Shape::Missing) {
        result.expect("clone a file to a free path");
        let contents = fs::read(&source).expect("read source");
        assert_eq!(fs::read(&destination).expect("read clone"), contents);
        fs::write(&destination, b"private").expect("write clone");
        assert_eq!(fs::read(&source).expect("read source again"), contents);
        return;
    }
    let Err(StorageError::Clone {
        source_path,
        destination: reported,
        source: error,
    }) = result
    else {
        panic!("expected a clone error: {result:?}");
    };
    assert_eq!((source_path, reported), (source, destination));
    let malformed = [source_shape, destination_shape].contains(&Shape::EmbeddedNul);
    // A NUL is refused before the kernel could see a truncated path; anything
    // else is the kernel's own refusal, never a copy fallback.
    assert_eq!(error.kind() == ErrorKind::InvalidInput, malformed);
    assert_eq!(error.raw_os_error().is_none(), malformed);
    assert_eq!(listing(fixture.path()), before);
}

#[hegel::test]
fn the_bulk_candidate_refuses_what_the_iterative_clone_refuses(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let shape = || {
        tc.draw(gs::sampled_from(vec![
            Shape::Missing,
            Shape::File,
            Shape::Directory,
        ]))
    };
    let (source_shape, destination_shape) = (shape(), shape());
    let source = place(&tc, fixture.path(), "source", source_shape);
    let iterative = place(&tc, fixture.path(), "iterative", destination_shape);
    let bulk = place(&tc, fixture.path(), "bulk", destination_shape);

    let outcome = |result: Result<(), StorageError>| match result {
        Ok(()) => "cloned",
        Err(StorageError::InvalidSource(_)) => "invalid source",
        Err(StorageError::DestinationExists(_)) => "destination exists",
        Err(StorageError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => "missing",
        Err(other) => panic!("unexpected refusal: {other:?}"),
    };
    let expected = match (source_shape, destination_shape) {
        (Shape::Missing, _) => "missing",
        (Shape::File, _) => "invalid source",
        (_, Shape::Missing) => "cloned",
        _ => "destination exists",
    };

    assert_eq!(outcome(clone_tree(&source, &iterative)), expected);
    assert_eq!(
        outcome(clone_tree_bulk_for_evaluation(&source, &bulk)),
        expected
    );
}
