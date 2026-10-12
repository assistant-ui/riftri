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

use super::tests::{git, git_stdout};
use super::{AddWorktreeRequest, WorktreeMode};
use crate::test_support::{WritableTempDir, writable_tempdir};

#[path = "worktree_state_hegel_tests.rs"]
mod state;

pub(super) const REGULAR: u32 = 0o100644;

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
