//! Generated directory trees shared by the APFS property tests.

use std::ffi::{CString, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;

/// One generated entry. Modes keep owner read (and search, for directories)
/// so the tree stays traversable; every other permission bit is arbitrary.
#[derive(Debug, Clone)]
pub enum Node {
    File { mode: u32, contents: Vec<u8> },
    Directory { mode: u32, children: Children },
    Symlink { target: OsString },
}

pub type Children = Vec<(OsString, Node)>;

/// What a tree looks like from the outside, keyed by path relative to its
/// root and sorted so two snapshots compare independently of directory order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Entry {
    File { mode: u32, contents: Vec<u8> },
    Directory { mode: u32 },
    Symlink { target: PathBuf },
    Other { mode: u32 },
}

pub type Snapshot = Vec<(PathBuf, Entry)>;

/// A temporary directory that can always be deleted, even after a test left
/// read-only or unreadable directories inside it.
pub struct Fixture(tempfile::TempDir);

impl Fixture {
    pub fn new() -> Self {
        Self(tempfile::tempdir().expect("fixture directory"))
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        restore_owner_access(self.path());
    }
}

fn restore_owner_access(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    for entry in fs::read_dir(path).into_iter().flatten().flatten() {
        restore_owner_access(&entry.path());
    }
}

/// A single path component: Unicode text or arbitrary bytes, with only the
/// two bytes no component can hold replaced.
pub fn draw_name(tc: &TestCase) -> OsString {
    let bytes = if tc.draw(gs::booleans()) {
        tc.draw(gs::text().min_size(1).max_size(8)).into_bytes()
    } else {
        tc.draw(gs::binary().min_size(1).max_size(8))
    };
    OsString::from_vec(
        bytes
            .into_iter()
            .map(|byte| if matches!(byte, 0 | b'/') { b'_' } else { byte })
            .collect(),
    )
}

pub fn draw_children(tc: &TestCase, depth: u8) -> Children {
    let count = tc.draw(gs::integers::<usize>().max_value(4));
    (0..count)
        .map(|_| (draw_name(tc), draw_node(tc, depth)))
        .collect()
}

fn draw_node(tc: &TestCase, depth: u8) -> Node {
    let kinds = if depth == 0 { 1 } else { 2 };
    match tc.draw(gs::integers::<u8>().max_value(kinds)) {
        0 => Node::File {
            mode: 0o400 | tc.draw(gs::integers::<u32>().max_value(0o377)),
            contents: tc.draw(gs::binary().max_size(256)),
        },
        1 => Node::Symlink {
            // Link targets are opaque bytes; APFS stores non-UTF-8 ones.
            target: OsString::from_vec(
                tc.draw(gs::binary().min_size(1).max_size(12))
                    .into_iter()
                    .map(|byte| if byte == 0 { b'_' } else { byte })
                    .collect(),
            ),
        },
        _ => Node::Directory {
            mode: 0o500 | tc.draw(gs::integers::<u32>().max_value(0o277)),
            children: draw_children(tc, depth - 1),
        },
    }
}

/// Create `children` under the existing directory `root`.
///
/// APFS refuses non-UTF-8 names and treats case or normalization variants as
/// one name, so an entry the volume will not create is left out. Properties
/// compare against a snapshot of what actually landed on disk.
pub fn materialize(root: &Path, children: &Children) {
    for (name, node) in children {
        let path = root.join(name);
        match node {
            Node::File { mode, contents } => {
                if fs::File::create_new(&path).is_ok() {
                    fs::write(&path, contents).expect("write generated file");
                    set_mode(&path, *mode);
                }
            }
            Node::Symlink { target } => {
                let _ = symlink(target, &path);
            }
            Node::Directory { mode, children } => {
                if fs::create_dir(&path).is_ok() {
                    materialize(&path, children);
                    set_mode(&path, *mode);
                }
            }
        }
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set generated mode");
}

pub fn make_fifo(path: &Path) {
    let native = CString::new(path.as_os_str().as_bytes()).expect("fifo path");
    // SAFETY: `native` is a valid NUL-terminated path for this call.
    let result = unsafe { libc::mkfifo(native.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

/// Record the directory `root` (under the empty path) and every entry below
/// it. A directory that cannot be listed is recorded by its mode alone, and a
/// file that cannot be read as `Entry::Other`.
pub fn snapshot(root: &Path) -> Snapshot {
    let mode = fs::symlink_metadata(root)
        .expect("snapshot root")
        .permissions()
        .mode()
        & 0o7777;
    let mut entries = vec![(PathBuf::new(), Entry::Directory { mode })];
    collect(root, Path::new(""), &mut entries);
    entries.sort();
    entries
}

/// Relative paths of the directories in `snapshot`, parents before children.
pub fn directories(snapshot: &Snapshot) -> Vec<PathBuf> {
    paths_matching(snapshot, |entry| matches!(entry, Entry::Directory { .. }))
}

pub fn files(snapshot: &Snapshot) -> Vec<PathBuf> {
    paths_matching(snapshot, |entry| matches!(entry, Entry::File { .. }))
}

fn paths_matching(snapshot: &Snapshot, wanted: impl Fn(&Entry) -> bool) -> Vec<PathBuf> {
    snapshot
        .iter()
        .filter(|(_, entry)| wanted(entry))
        .map(|(path, _)| path.clone())
        .collect()
}

/// `snapshot` with every file and directory mode rewritten.
pub fn with_modes(
    snapshot: &Snapshot,
    file: impl Fn(u32) -> u32,
    directory: impl Fn(u32) -> u32,
) -> Snapshot {
    snapshot
        .iter()
        .cloned()
        .map(|(path, entry)| {
            let entry = match entry {
                Entry::File { mode, contents } => Entry::File {
                    mode: file(mode),
                    contents,
                },
                Entry::Directory { mode } => Entry::Directory {
                    mode: directory(mode),
                },
                other => other,
            };
            (path, entry)
        })
        .collect()
}

/// A generated tree at `<fixture>/base` whose root mode is generated too.
pub fn draw_base(tc: &TestCase, fixture: &Fixture) -> PathBuf {
    let base = fixture.path().join("base");
    fs::create_dir(&base).expect("create base");
    materialize(&base, &draw_children(tc, 3));
    set_mode(
        &base,
        0o500 | tc.draw(gs::integers::<u32>().max_value(0o277)),
    );
    base
}

fn collect(directory: &Path, relative: &Path, entries: &mut Snapshot) {
    let Ok(listing) = fs::read_dir(directory) else {
        return;
    };
    for entry in listing {
        let name = entry.expect("snapshot entry").file_name();
        let path = directory.join(&name);
        let relative = relative.join(&name);
        let metadata = fs::symlink_metadata(&path).expect("snapshot metadata");
        let mode = metadata.permissions().mode() & 0o7777;
        let entry = if metadata.is_dir() {
            collect(&path, &relative, entries);
            Entry::Directory { mode }
        } else if metadata.is_file() {
            match fs::read(&path) {
                Ok(contents) => Entry::File { mode, contents },
                Err(_) => Entry::Other { mode },
            }
        } else if metadata.file_type().is_symlink() {
            Entry::Symlink {
                target: fs::read_link(&path).expect("snapshot link"),
            }
        } else {
            Entry::Other { mode }
        };
        entries.push((relative, entry));
    }
}

/// The write bits a newly created file gets under the process umask, read
/// from a real file so the tests do not share the crate's own umask logic.
pub fn umask_write_bits(scratch: &Path) -> u32 {
    let probe = scratch.join("umask-probe");
    fs::write(&probe, b"").expect("write umask probe");
    let mode = fs::metadata(&probe)
        .expect("umask probe metadata")
        .permissions()
        .mode();
    fs::remove_file(&probe).expect("remove umask probe");
    mode & 0o222
}
