//! Property tests for the immutable-base integrity markers.
//!
//! A marker is what lets a base be reused without re-reading Git, so it has
//! to be a function of the tree alone (not of where the tree lives or the
//! order it was written in) and it has to move whenever the tree does.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::TestCase;
use hegel::generators as gs;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::{
    ChangedWhileReading, MARKER_V1_PREFIX, MARKER_V2_PREFIX, hash_file_bytes, hex_lower, marker,
    marker_v2, open_regular, sorted_xattr_names, worktree_marker,
};

/// Each case writes two or three small trees and hashes them three ways.
const TREE_CASES: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    File { content: Vec<u8>, executable: bool },
    Directory,
    Link { target: String },
}

/// A tree keyed by path components. A key's parent is always a `Directory`
/// key, and `BTreeMap` order lists every parent before its children.
type Tree = BTreeMap<Vec<String>, Node>;

fn name(tc: &TestCase) -> String {
    // Lowercase only: APFS is case-insensitive, so `a` and `A` are one entry.
    tc.draw(gs::text().alphabet("abc \u{e9}").min_size(1).max_size(2))
}

fn node(tc: &TestCase) -> Node {
    // Mostly files: they carry the bytes and the mode a marker must cover.
    match tc.draw(gs::integers::<u8>().max_value(4)) {
        0..=2 => Node::File {
            content: tc.draw(gs::binary().max_size(12)),
            executable: tc.draw(gs::booleans()),
        },
        3 => Node::Directory,
        _ => Node::Link {
            target: tc.draw(gs::text().alphabet("ab/.").min_size(1).max_size(4)),
        },
    }
}

/// Insert `node` at `path`, creating missing parents. Returns whether the
/// tree changed: an occupied path or a non-directory ancestor refuses it.
fn insert(tree: &mut Tree, path: Vec<String>, node: Node) -> bool {
    if tree.contains_key(&path) {
        return false;
    }
    let ancestors = (1..path.len()).map(|length| path[..length].to_vec());
    if ancestors.clone().any(|ancestor| {
        tree.get(&ancestor)
            .is_some_and(|node| *node != Node::Directory)
    }) {
        return false;
    }
    for ancestor in ancestors {
        tree.insert(ancestor, Node::Directory);
    }
    tree.insert(path, node);
    true
}

fn tree(tc: &TestCase) -> Tree {
    let mut tree = Tree::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(6)) {
        let path = (0..tc.draw(gs::integers::<usize>().min_value(1).max_value(3)))
            .map(|_| name(tc))
            .collect();
        insert(&mut tree, path, node(tc));
    }
    tree
}

fn remove_subtree(tree: &mut Tree, root: &[String]) {
    tree.retain(|path, _| !path.starts_with(root));
}

/// One kind of single change, each as likely as the others so the changes a
/// digest most easily misses (one byte at the same length, a mode bit, a
/// link target) are tried in every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    Remove,
    Rename,
    FlipByte,
    Append,
    FlipMode,
    Retarget,
    Retype,
}

/// Apply one generated change. A change with nothing to apply to (no file to
/// edit, say) adds an entry instead, and a no-op rejects the case.
fn mutate(tc: &TestCase, original: &Tree) -> Tree {
    let mut tree = original.clone();
    let change = tc.draw(gs::sampled_from(vec![
        Change::Add,
        Change::Remove,
        Change::Rename,
        Change::FlipByte,
        Change::Append,
        Change::FlipMode,
        Change::Retarget,
        Change::Retype,
    ]));
    let candidates = tree
        .iter()
        .filter(|(_, node)| match (change, node) {
            (Change::Add, _) => false,
            (Change::FlipByte, Node::File { content, .. }) => !content.is_empty(),
            (Change::Append | Change::FlipMode, node) => matches!(node, Node::File { .. }),
            (Change::Retarget, node) => matches!(node, Node::Link { .. }),
            (Change::FlipByte, _) => false,
            (Change::Remove | Change::Rename | Change::Retype, _) => true,
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        let mut path = match original.keys().next() {
            Some(sibling) if tc.draw(gs::booleans()) => sibling[..sibling.len() - 1].to_vec(),
            _ => Vec::new(),
        };
        path.push(name(tc));
        tc.assume(insert(&mut tree, path, node(tc)));
        return tree;
    }
    let path = tc.draw(gs::sampled_from(candidates));
    let replacement = match (change, tree[&path].clone()) {
        (Change::Remove, _) => None,
        (Change::Rename, _) => {
            let mut renamed = path.clone();
            *renamed.last_mut().expect("non-empty path") = name(tc);
            tc.assume(!tree.contains_key(&renamed));
            for (old, node) in original.range(path.clone()..) {
                if old.starts_with(&path) {
                    let mut new = renamed.clone();
                    new.extend_from_slice(&old[path.len()..]);
                    tree.insert(new, node.clone());
                }
            }
            None
        }
        (
            Change::FlipByte,
            Node::File {
                mut content,
                executable,
            },
        ) => {
            let index = tc.draw(gs::integers::<usize>().max_value(content.len() - 1));
            content[index] ^= tc.draw(gs::integers::<u8>().min_value(1));
            Some(Node::File {
                content,
                executable,
            })
        }
        (
            Change::Append,
            Node::File {
                mut content,
                executable,
            },
        ) => {
            content.push(tc.draw(gs::integers::<u8>()));
            Some(Node::File {
                content,
                executable,
            })
        }
        (
            Change::FlipMode,
            Node::File {
                content,
                executable,
            },
        ) => Some(Node::File {
            content,
            executable: !executable,
        }),
        (Change::Retarget, Node::Link { target }) => Some(Node::Link {
            target: target + "a",
        }),
        // The same bytes under another entry kind are a different tree.
        (_, Node::File { content, .. }) => Some(Node::Link {
            target: hex_lower(content) + "x",
        }),
        (_, Node::Link { target }) => Some(Node::File {
            content: target.into_bytes(),
            executable: false,
        }),
        (_, Node::Directory) => Some(Node::Link {
            target: ".".to_owned(),
        }),
    };
    remove_subtree(&mut tree, &path);
    tree.extend(replacement.map(|node| (path, node)));
    tree
}

/// Write `tree` under a fresh directory. `reversed` writes the entries of
/// each kind in the opposite order, which no marker may observe.
fn materialize(tree: &Tree, reversed: bool) -> tempfile::TempDir {
    let fixture = tempdir().expect("tree fixture");
    let on_disk = |path: &[String]| {
        path.iter()
            .fold(fixture.path().to_path_buf(), |p, c| p.join(c))
    };
    for (path, node) in tree {
        if *node == Node::Directory {
            fs::create_dir(on_disk(path)).expect("create directory");
        }
    }
    let mut leaves = tree
        .iter()
        .filter(|(_, node)| **node != Node::Directory)
        .collect::<Vec<_>>();
    if reversed {
        leaves.reverse();
    }
    for (path, node) in leaves {
        let path = on_disk(path);
        match node {
            Node::File {
                content,
                executable,
            } => {
                fs::write(&path, content).expect("write file");
                let mode = if *executable { 0o755 } else { 0o644 };
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set mode");
            }
            Node::Link { target } => symlink(target, &path).expect("create symlink"),
            Node::Directory => unreachable!("directories were filtered out"),
        }
    }
    fixture
}

fn make_fifo(path: &Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed");
}

fn markers(root: &Path) -> [Vec<u8>; 3] {
    [
        marker(root).expect("v1 marker"),
        worktree_marker(root).expect("worktree marker"),
        marker_v2(root).expect("v2 marker"),
    ]
}

#[hegel::test(test_cases = TREE_CASES)]
fn markers_depend_only_on_the_tree(tc: TestCase) {
    let tree = tree(&tc);
    let first = materialize(&tree, false);
    let second = materialize(&tree, true);

    let [v1, worktree, v2] = markers(first.path());
    assert_eq!(
        markers(second.path()),
        [v1.clone(), worktree.clone(), v2.clone()]
    );
    // A tree with no FIFOs, sockets, or devices digests the same either way.
    assert_eq!(worktree, v1);
    for (marker, prefix) in [(&v1, MARKER_V1_PREFIX), (&v2, MARKER_V2_PREFIX)] {
        let digest = marker.strip_prefix(prefix).expect("scheme prefix");
        assert_eq!(digest.len(), 65);
        assert_eq!(digest[64], b'\n');
        assert!(
            digest[..64]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        );
    }
}

#[hegel::test(test_cases = TREE_CASES)]
fn markers_detect_any_single_change_to_a_tree(tc: TestCase) {
    let original = tree(&tc);
    let changed = mutate(&tc, &original);
    tc.note(&format!("changed tree: {changed:?}"));
    let before = materialize(&original, false);
    let after = materialize(&changed, false);

    let before = markers(before.path());
    let after = markers(after.path());
    for (scheme, (before, after)) in ["v1", "worktree", "v2"]
        .into_iter()
        .zip(before.iter().zip(&after))
    {
        assert_ne!(before, after, "the {scheme} marker missed the change");
    }
}

/// A base can never hold a FIFO, so the base markers refuse one outright.
/// A live worktree can, and its marker must notice the entry appear and its
/// mode change.
#[hegel::test(test_cases = TREE_CASES)]
fn special_entries_are_refused_in_bases_and_hashed_in_worktrees(tc: TestCase) {
    let mut tree = tree(&tc);
    let mut path = if tree.is_empty() || tc.draw(gs::booleans()) {
        Vec::new()
    } else {
        tc.draw(gs::sampled_from(tree.keys().cloned().collect::<Vec<_>>()))
    };
    path.push(name(&tc));
    // Reserve the FIFO's place with a directory so its parents exist on disk.
    tc.assume(insert(&mut tree, path.clone(), Node::Directory));
    tree.remove(&path);
    let fixture = materialize(&tree, false);
    let without = markers(fixture.path());

    let fifo = path
        .iter()
        .fold(fixture.path().to_path_buf(), |parent, component| {
            parent.join(component)
        });
    let modes = [0o600_u32, 0o640];
    let mode = tc.draw(gs::sampled_from(modes.to_vec()));
    make_fifo(&fifo);
    fs::set_permissions(&fifo, fs::Permissions::from_mode(mode)).expect("set FIFO mode");

    for refused in [marker(fixture.path()), marker_v2(fixture.path())] {
        let error = refused.expect_err("a FIFO was accepted in an immutable base");
        assert_eq!(error.to_string(), "unsupported entry in immutable base");
    }
    let with = worktree_marker(fixture.path()).expect("worktree marker");
    assert_ne!(with, without[1], "the FIFO was not hashed");
    let other = modes[usize::from(mode == modes[0])];
    fs::set_permissions(&fifo, fs::Permissions::from_mode(other)).expect("change FIFO mode");
    assert_ne!(
        worktree_marker(fixture.path()).expect("worktree marker"),
        with
    );
    fs::remove_file(&fifo).expect("remove FIFO");
    assert_eq!(markers(fixture.path()), without);
}

/// Only a regular file is ever opened for hashing, and a file whose length
/// no longer matches its metadata is reported as changed, not hashed short.
#[hegel::test(test_cases = TREE_CASES)]
fn integrity_inputs_must_be_unchanged_regular_files(tc: TestCase) {
    let fixture = tempdir().expect("fixture");
    let content = tc.draw(gs::binary().max_size(64));
    let file = fixture.path().join("file");
    fs::write(&file, &content).expect("write file");

    let mut read = Vec::new();
    open_regular(&file)
        .expect("open a regular file")
        .read_to_end(&mut read)
        .expect("read file");
    assert_eq!(read, content);

    let claimed = if tc.draw(gs::booleans()) {
        content.len() as u64
    } else {
        tc.draw(gs::integers::<u64>().max_value(128))
    };
    let mut digest = Sha256::new();
    let mut buffer = vec![0; tc.draw(gs::integers::<usize>().min_value(1).max_value(16))];
    match hash_file_bytes(&file, claimed, &mut digest, &mut buffer) {
        Ok(()) => {
            assert_eq!(claimed, content.len() as u64);
            assert_eq!(digest.finalize(), Sha256::digest(&content));
        }
        Err(error) => {
            assert_ne!(claimed, content.len() as u64);
            assert!(
                error
                    .get_ref()
                    .is_some_and(|inner| inner.is::<ChangedWhileReading>())
            );
            assert_eq!(error.to_string(), "integrity input changed while reading");
        }
    }

    let other: PathBuf = fixture.path().join("other");
    match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => symlink(&file, &other).expect("symlink to a file"),
        1 => symlink("absent", &other).expect("dangling symlink"),
        2 => fs::create_dir(&other).expect("directory"),
        _ => make_fifo(&other),
    }
    assert!(open_regular(&other).is_err());
    let mut digest = Sha256::new();
    assert!(hash_file_bytes(&other, content.len() as u64, &mut digest, &mut buffer).is_err());
}

#[hegel::test]
fn hex_rendering_matches_the_standard_formatter(tc: TestCase) {
    let bytes = tc.draw(gs::binary().max_size(64));
    let expected = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(hex_lower(&bytes), expected);
}

/// The kernel returns attribute names as one NUL-separated buffer. Every name
/// comes back exactly once, in byte order, whatever padding surrounds it.
#[hegel::test]
fn xattr_names_are_split_and_sorted_by_bytes(tc: TestCase) {
    let mut names = tc.draw(gs::vecs(gs::binary().min_size(1).max_size(6)).max_size(6));
    for name in &mut names {
        name.retain(|byte| *byte != 0);
    }
    names.retain(|name| !name.is_empty());
    let mut buffer = Vec::new();
    for name in &names {
        buffer.extend(std::iter::repeat_n(
            0,
            tc.draw(gs::integers::<usize>().max_value(2)),
        ));
        buffer.extend_from_slice(name);
        buffer.push(0);
    }
    names.sort_unstable();
    assert_eq!(sorted_xattr_names(&buffer), names);
}
