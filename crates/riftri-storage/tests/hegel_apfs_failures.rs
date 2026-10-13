#![cfg(target_os = "macos")]

#[allow(dead_code)]
mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::{ApfsCloner, StorageError};

use support::{Entry, Fixture, directories, draw_base, make_fifo, set_mode, snapshot, with_modes};

// Longer than any generated name, so it cannot alias one.
const BLOCKER: &str = "riftri-blocker";

#[derive(Debug, Clone, Copy)]
enum Blocker {
    Fifo,
    UnreadableFile,
    UnlistableDirectory,
}

/// Put one entry the clone cannot reproduce into a generated directory of
/// `base` and return its path.
fn add_blocker(tc: &TestCase, base: &Path, blocker: Blocker) -> std::path::PathBuf {
    let parent = base.join(tc.draw(gs::sampled_from(directories(&snapshot(base)))));
    let parent_mode = snapshot(&parent)[0].1.clone();
    let Entry::Directory { mode } = parent_mode else {
        unreachable!("a snapshot starts with its root directory");
    };
    set_mode(&parent, 0o700);
    let path = parent.join(BLOCKER);
    match blocker {
        Blocker::Fifo => make_fifo(&path),
        Blocker::UnreadableFile => {
            fs::write(&path, b"unreadable").expect("write blocker");
            set_mode(&path, 0o000);
        }
        Blocker::UnlistableDirectory => {
            fs::create_dir(&path).expect("create blocker");
            set_mode(&path, 0o000);
        }
    }
    set_mode(&parent, mode);
    path
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn a_failed_clone_leaves_no_destination_and_an_untouched_base(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    let blocker = tc.draw(gs::sampled_from(vec![
        Blocker::Fifo,
        Blocker::UnreadableFile,
        Blocker::UnlistableDirectory,
    ]));
    let blocked = add_blocker(&tc, &base, blocker);
    let before = snapshot(fixture.path());

    let view = fixture.path().join("view");
    let result = if tc.draw(gs::booleans()) {
        ApfsCloner::clone_tree_owner_writable(&base, &view)
    } else {
        ApfsCloner::clone_tree(&base, &view)
    };

    let error = result.expect_err("a tree with an uncloneable entry must be refused");
    match (blocker, &error) {
        (Blocker::Fifo, StorageError::UnsupportedEntry(path)) => assert_eq!(path, &blocked),
        (Blocker::UnreadableFile, StorageError::Clone { source_path, .. }) => {
            assert_eq!(source_path, &blocked);
        }
        (Blocker::UnlistableDirectory, StorageError::Io { path, .. }) => {
            assert_eq!(path, &blocked);
        }
        _ => panic!("unexpected error for {blocker:?}: {error:?}"),
    }
    assert_eq!(snapshot(fixture.path()), before);
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn clone_preconditions_are_checked_without_touching_either_path(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    let file = fixture.path().join("file");
    fs::write(&file, b"not a tree").expect("write file");
    let occupied = fixture.path().join("occupied");
    fs::create_dir(&occupied).expect("create occupied");
    fs::write(occupied.join("keep"), b"keep").expect("write occupied");
    let link = |name: &str, target: &Path| {
        let path = fixture.path().join(name);
        symlink(target, &path).expect("create link");
        path
    };
    let missing = fixture.path().join("missing");
    let fifo = fixture.path().join("fifo");
    make_fifo(&fifo);

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Refusal {
        InvalidSource,
        DestinationExists,
        Io,
    }
    let free = fixture.path().join("view");
    let cases = vec![
        (missing.clone(), free.clone(), Refusal::Io),
        (file.clone(), free.clone(), Refusal::InvalidSource),
        (fifo.clone(), free.clone(), Refusal::InvalidSource),
        // A linked base is refused instead of followed.
        (
            link("base-link", &base),
            free.clone(),
            Refusal::InvalidSource,
        ),
        (base.clone(), file.clone(), Refusal::DestinationExists),
        (base.clone(), fifo, Refusal::DestinationExists),
        (base.clone(), occupied.clone(), Refusal::DestinationExists),
        (base.clone(), base.clone(), Refusal::DestinationExists),
        (
            base.clone(),
            link("occupied-link", &occupied),
            Refusal::DestinationExists,
        ),
        // A dangling link does not "exist", but creating the clone
        // directory over it must still fail and leave the link in place.
        (base.clone(), link("dangling", &missing), Refusal::Io),
        (base.clone(), missing.join("view"), Refusal::Io),
        (base.clone(), file.join("view"), Refusal::Io),
        // A clone into its own base would otherwise copy itself forever.
        (base.clone(), base.join("inside"), Refusal::Io),
    ];
    let (source, destination, expected) = tc.draw(gs::sampled_from(cases));
    let before = snapshot(fixture.path());

    let result = if tc.draw(gs::booleans()) {
        ApfsCloner::clone_tree_owner_writable(&source, &destination)
    } else {
        ApfsCloner::clone_tree(&source, &destination)
    };

    let refusal = match result.expect_err("precondition must fail") {
        StorageError::InvalidSource(path) => {
            assert_eq!(path, source);
            Refusal::InvalidSource
        }
        StorageError::DestinationExists(path) => {
            assert_eq!(path, destination);
            Refusal::DestinationExists
        }
        StorageError::Io { .. } => Refusal::Io,
        other => panic!("unexpected refusal: {other:?}"),
    };
    assert_eq!(refusal, expected);
    assert_eq!(snapshot(fixture.path()), before);
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn restoring_write_access_makes_any_tree_removable(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    if tc.draw(gs::booleans()) {
        // Worktrees hold sockets and FIFOs; they must not block removal.
        add_blocker(&tc, &base, Blocker::Fifo);
    }
    let readable = snapshot(&base);
    let mut locked = readable.clone();
    // Children first, so a directory is still reachable when it is locked.
    for (path, entry) in locked.iter_mut().rev() {
        if let Entry::Directory { mode } = entry {
            *mode = tc.draw(gs::integers::<u32>().max_value(0o777));
            set_mode(&base.join(path), *mode);
        }
    }
    let write_bits = support::umask_write_bits(fixture.path());

    ApfsCloner::make_tree_owner_writable(&base).expect("restore write access");

    assert_eq!(
        snapshot(&base),
        with_modes(
            &locked,
            |mode| mode | write_bits,
            |mode| mode | 0o700 | write_bits
        )
    );
    fs::remove_dir_all(&base).expect("remove restored tree");
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn permission_passes_refuse_what_they_cannot_protect(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    let read_only = tc.draw(gs::booleans());
    let pass = |path: &Path| {
        if read_only {
            ApfsCloner::make_tree_read_only(path)
        } else {
            ApfsCloner::make_tree_owner_writable(path)
        }
    };

    // A link is never followed out of the tree, and a missing path is an
    // error instead of a silent success.
    let link = fixture.path().join("link");
    symlink(&base, &link).expect("link to base");
    let before = snapshot(fixture.path());
    pass(&link).expect("a link needs no mode change");
    assert_eq!(snapshot(fixture.path()), before);
    let error = pass(&fixture.path().join("missing")).expect_err("missing tree");
    assert!(matches!(error, StorageError::Io { .. }), "{error:?}");

    // An immutable base cannot contain an entry Git cannot store.
    let fifo = add_blocker(&tc, &base, Blocker::Fifo);
    let contents = |root: &Path| with_modes(&snapshot(root), |_| 0, |_| 0);
    let before = contents(&base);
    match ApfsCloner::make_tree_read_only(&base) {
        Err(StorageError::UnsupportedEntry(path)) => assert_eq!(path, fifo),
        other => panic!("expected the FIFO to be refused: {other:?}"),
    }
    assert_eq!(contents(&base), before);
}
