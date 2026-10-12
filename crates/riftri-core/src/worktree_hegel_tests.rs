//! Hegel property tests for `worktree.rs`, one file per subsystem. The
//! fixtures every file shares live here.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hegel::TestCase;
use hegel::generators as gs;

use super::tests::{git, git_stdout};
use super::{AddWorktreeRequest, WorktreeMode};
use crate::test_support::{WritableTempDir, writable_tempdir};

#[path = "worktree_add_state_hegel_tests.rs"]
mod add_state;
#[path = "worktree_checkout_hegel_tests.rs"]
mod checkout;
#[path = "worktree_destination_hegel_tests.rs"]
mod destination;
#[path = "worktree_lfs_hegel_tests.rs"]
mod lfs;
#[path = "worktree_lifecycle_hegel_tests.rs"]
mod lifecycle;
#[path = "worktree_policy_hegel_tests.rs"]
mod policy;
#[path = "worktree_recovery_hegel_tests.rs"]
mod recovery;
#[path = "worktree_refusal_hegel_tests.rs"]
mod refusal;
#[path = "worktree_removal_state_hegel_tests.rs"]
mod removal_state;
#[path = "worktree_state_hegel_tests.rs"]
mod state;

pub(super) const REGULAR: u32 = 0o100644;
pub(super) const EXECUTABLE: u32 = 0o100755;
pub(super) const SYMLINK: u32 = 0o120000;

/// One entry of a generated Git tree. `path` is raw bytes so a tree can hold
/// names the filesystem under test cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeFile {
    pub path: Vec<u8>,
    pub mode: u32,
    pub data: Vec<u8>,
}

impl TreeFile {
    pub fn regular(path: &str, data: &[u8]) -> Self {
        Self {
            path: path.as_bytes().to_vec(),
            mode: REGULAR,
            data: data.to_vec(),
        }
    }
}

/// A repository whose commits are written with `git fast-import`, so a tree
/// never has to exist in a working directory first.
pub(super) struct Fixture {
    root: WritableTempDir,
    pub repository: PathBuf,
    pub state: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let root = writable_tempdir().expect("fixture");
        // Git reports resolved paths, and the tests compare against them.
        let resolved = fs::canonicalize(root.path()).expect("resolve fixture");
        let repository = resolved.join("repository");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet", "-b", "main"]);
        git(&repository, &["config", "core.autocrlf", "false"]);
        Self {
            root,
            state: resolved.join("state"),
            repository,
        }
    }

    pub fn path(&self, name: impl AsRef<OsStr>) -> PathBuf {
        self.repository
            .parent()
            .expect("fixture root")
            .join(name.as_ref())
    }

    /// Commit exactly `files` as a root commit on `branch` and return its ID.
    pub fn commit(&self, branch: &str, files: &[TreeFile]) -> String {
        let mut stream = format!(
            "commit refs/heads/{branch}\ncommitter Riftri Tests <riftri@example.invalid> 1700000000 +0000\ndata 7\ninitial\n"
        )
        .into_bytes();
        for file in files {
            stream.extend_from_slice(format!("M {:o} inline ", file.mode).as_bytes());
            stream.extend_from_slice(&quote_fast_import_path(&file.path));
            stream.extend_from_slice(format!("\ndata {}\n", file.data.len()).as_bytes());
            stream.extend_from_slice(&file.data);
            stream.push(b'\n');
        }
        let mut child = Command::new("git")
            // Keep case aliases apart even where the repository folds them.
            .args(["-c", "core.ignorecase=false"])
            .args(["fast-import", "--quiet", "--force"])
            .current_dir(&self.repository)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start git fast-import");
        child
            .stdin
            .take()
            .expect("fast-import stdin")
            .write_all(&stream)
            .expect("write fast-import stream");
        let output = child.wait_with_output().expect("finish git fast-import");
        assert!(
            output.status.success(),
            "git fast-import: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        git_stdout(
            &self.repository,
            &["rev-parse", &format!("refs/heads/{branch}")],
        )
    }

    pub fn add_request(&self, destination: &Path, revision: &str) -> AddWorktreeRequest {
        AddWorktreeRequest {
            repository: self.repository.clone(),
            destination: destination.to_path_buf(),
            revision: OsString::from(revision),
            mode: WorktreeMode::Detached,
            state_dir: Some(self.state.clone()),
            sparse_directories: Vec::new(),
        }
    }

    /// Keeps the fixture alive for callers that only hold paths into it.
    pub fn root(&self) -> &Path {
        self.root.path()
    }
}

fn quote_fast_import_path(path: &[u8]) -> Vec<u8> {
    let mut quoted = vec![b'"'];
    for byte in path {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_') {
            quoted.push(*byte);
        } else {
            quoted.extend_from_slice(format!("\\{byte:03o}").as_bytes());
        }
    }
    quoted.push(b'"');
    quoted
}

/// What a checkout holds at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    Directory,
    File { executable: bool, data: Vec<u8> },
    Symlink(Vec<u8>),
}

/// Every path under `root` except its top-level `.git`, keyed by raw bytes.
pub(super) fn snapshot(root: &Path) -> BTreeMap<Vec<u8>, Entry> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read directory") {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            if directory == root && entry.file_name() == ".git" {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("entry under root")
                .as_os_str()
                .as_bytes()
                .to_vec();
            let metadata = fs::symlink_metadata(&path).expect("entry metadata");
            let value = if metadata.file_type().is_symlink() {
                Entry::Symlink(
                    fs::read_link(&path)
                        .expect("read symlink")
                        .into_os_string()
                        .into_vec(),
                )
            } else if metadata.is_dir() {
                pending.push(path);
                Entry::Directory
            } else {
                Entry::File {
                    executable: metadata.permissions().mode() & 0o100 != 0,
                    data: fs::read(&path).expect("read file"),
                }
            };
            entries.insert(relative, value);
        }
    }
    entries
}

/// Whether Git lists `path` as a worktree of `repository`.
pub(super) fn registered(repository: &Path, path: &Path) -> bool {
    riftri_git::Git::default()
        .list_worktrees(repository)
        .expect("list worktrees")
        .iter()
        .any(|worktree| worktree.path == path)
}

/// `git status` for a worktree, with untracked and ignored files included.
pub(super) fn status(worktree: &Path) -> String {
    git_stdout(
        worktree,
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ],
    )
}

/// A path component that is valid on every filesystem under test but still
/// exercises quoting: spaces, shell and Git metacharacters, and non-ASCII.
pub(super) fn component(tc: &TestCase) -> String {
    tc.draw(
        gs::text()
            .alphabet("abcXYZ019 _-+=,;'&$#@!()[]{}~^%\u{e9}\u{4e2d}\u{1f600}")
            .min_size(1)
            .max_size(6),
    )
}

/// Paths of a tree that every supported filesystem can hold: distinct after
/// ASCII lowercasing, with no entry that is both a file and a directory.
pub(super) fn portable_paths(tc: &TestCase, maximum: usize) -> Vec<String> {
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(maximum));
    let mut paths: Vec<String> = Vec::new();
    for index in 0..count {
        let depth = tc.draw(gs::integers::<usize>().max_value(2));
        // A unique lowercase leading component keeps generated names from
        // aliasing each other or a `.git` entry on any filesystem.
        let mut path = format!("p{index}");
        for _ in 0..depth {
            path.push('/');
            path.push_str(component(tc).trim_matches(' '));
            if path.ends_with('/') {
                path.push('x');
            }
        }
        paths.push(path);
    }
    paths
}

/// Rewrite one durable journal in place, the way a kill between two steps or
/// an older Riftri would have left it.
pub(super) fn edit_journal(journal: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value =
        serde_json::from_slice(&fs::read(journal).expect("read journal")).expect("journal is JSON");
    edit(&mut value);
    fs::write(
        journal,
        serde_json::to_vec_pretty(&value).expect("encode journal"),
    )
    .expect("write journal");
}

/// A path as journals store it.
pub(super) fn journal_path_value(path: &Path) -> serde_json::Value {
    serde_json::json!({ "encoding": "unix-bytes", "units": path.as_os_str().as_bytes() })
}
