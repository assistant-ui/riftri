//! Fixtures shared by the Git integration tests.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use riftri_git::{Git, ObjectId};
use tempfile::TempDir;

pub(crate) fn isolated_git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(repository)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
    command
}

pub(crate) fn git(repository: &Path, args: &[&str]) -> Output {
    let output = isolated_git(repository).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Run Git with raw arguments and standard input, for object names and paths
/// that are not UTF-8.
pub(crate) fn git_with_input(repository: &Path, args: &[&OsStr], input: &[u8]) -> Output {
    let mut child = isolated_git(repository)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub(crate) fn object_id(output: &Output) -> ObjectId {
    ObjectId::parse(String::from_utf8_lossy(&output.stdout).trim()).unwrap()
}

/// A scratch directory holding a repository at `repository()`. The directory
/// path is canonical because Git reports resolved paths.
pub(crate) struct Fixture {
    _directory: TempDir,
    root: PathBuf,
}

impl Fixture {
    pub(crate) fn new(object_format: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let fixture = Self {
            _directory: directory,
            root,
        };
        std::fs::create_dir(fixture.repository()).unwrap();
        git(
            &fixture.repository(),
            &[
                "-c",
                "init.defaultBranch=main",
                "init",
                "--quiet",
                &format!("--object-format={object_format}"),
            ],
        );
        fixture
    }

    /// A repository whose `main` holds one commit of an empty tree, so a
    /// linked worktree without a checkout is clean.
    pub(crate) fn committed() -> Self {
        let fixture = Self::new("sha1");
        fixture.commit("initial");
        fixture
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn repository(&self) -> PathBuf {
        self.root.join("repository")
    }

    pub(crate) fn commit(&self, message: &str) -> ObjectId {
        let output = isolated_git(&self.repository())
            .args(["commit", "--quiet", "--allow-empty", "-m", message])
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        object_id(&git(&self.repository(), &["rev-parse", "HEAD"]))
    }

    pub(crate) fn blob(&self, content: &[u8]) -> ObjectId {
        let arguments = ["hash-object", "-w", "--stdin"].map(OsStr::new);
        object_id(&git_with_input(&self.repository(), &arguments, content))
    }

    /// Write a tree from `(mode, kind, object, name)` entries without
    /// touching the filesystem, so names may hold bytes no volume accepts.
    pub(crate) fn tree(&self, entries: &[(u32, &str, &ObjectId, &[u8])]) -> ObjectId {
        let mut input = Vec::new();
        for (mode, kind, object, name) in entries {
            input.extend(format!("{mode:06o} {kind} {}\t", object.as_str()).into_bytes());
            input.extend_from_slice(name);
            input.push(0);
        }
        let arguments = ["mktree", "-z", "--missing"].map(OsStr::new);
        object_id(&git_with_input(&self.repository(), &arguments, &input))
    }
}

pub(crate) const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Riftri Tests"),
    ("GIT_AUTHOR_EMAIL", "riftri@example.invalid"),
    ("GIT_COMMITTER_NAME", "Riftri Tests"),
    ("GIT_COMMITTER_EMAIL", "riftri@example.invalid"),
];

/// How a scripted Git invocation ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    Exit(u8),
    Signal(i32),
}

/// A Git stand-in that replays scripted output for chosen invocations and
/// runs the real Git for the rest. The script is checked in and reached
/// through a symlink, so no test ever executes a file it has just written.
pub(crate) struct ScriptedGit {
    directory: TempDir,
}

impl ScriptedGit {
    pub(crate) fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/scripted-git.sh"),
            directory.path().join("git"),
        )
        .unwrap();
        Self { directory }
    }

    pub(crate) fn git(&self) -> Git {
        Git::new(self.directory.path().join("git"))
    }

    /// Script the invocation with this zero-based index, counted from the
    /// last `reset`.
    pub(crate) fn script(&self, invocation: usize, stdout: &[u8], end: End) {
        let end = match end {
            End::Exit(code) => code.to_string(),
            End::Signal(signal) => format!("signal-{signal}"),
        };
        let file = |extension: &str| {
            self.directory
                .path()
                .join(format!("{invocation}.{extension}"))
        };
        std::fs::write(file("out"), stdout).unwrap();
        std::fs::write(file("end"), end).unwrap();
    }

    /// Make the scripted invocation with this index read all of its standard
    /// input before replying, as a Git that consumed the whole request would.
    pub(crate) fn drain_input(&self, invocation: usize) {
        std::fs::write(
            self.directory.path().join(format!("{invocation}.drain")),
            "",
        )
        .unwrap();
    }

    /// Forget every script and restart the invocation count.
    pub(crate) fn reset(&self) {
        for entry in std::fs::read_dir(self.directory.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name() != Some(OsStr::new("git")) {
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    pub(crate) fn invocations(&self) -> usize {
        std::fs::read_to_string(self.directory.path().join("count"))
            .map_or(0, |count| count.trim().parse().unwrap())
    }
}
