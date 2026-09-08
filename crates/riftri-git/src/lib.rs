//! Read-only interaction with the user's installed Git executable.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;
use thiserror::Error;

/// Information about the Git executable used by Riftri.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitInfo {
    pub command: PathBuf,
    pub version: String,
}

/// A validated Git object ID. SHA-1 and SHA-256 repositories are supported.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitError> {
        let value = value.into();
        if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(GitError::InvalidOutput {
                context: "object ID",
                detail: format!("expected 40 or 64 hexadecimal characters, got {value:?}"),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_null(&self) -> bool {
        self.0.bytes().all(|byte| byte == b'0')
    }
}

/// Stable repository identity used by cache keys.
///
/// Main and linked worktrees belonging to one repository have the same common
/// Git directory even though their per-worktree Git directories differ.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RepositoryIdentity {
    pub common_git_dir: PathBuf,
}

/// Read-only facts about an existing Git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryInfo {
    /// The working-tree root, or `None` for a bare repository.
    pub root: Option<PathBuf>,
    pub identity: RepositoryIdentity,
    pub is_bare: bool,
    pub head_commit: Option<ObjectId>,
    pub head_tree: Option<ObjectId>,
    /// Cleanliness is meaningful only for a non-bare repository.
    pub clean: Option<bool>,
}

/// Commit and tree IDs resolved by the real Git executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRevision {
    pub commit: ObjectId,
    pub tree: ObjectId,
}

/// One record from `git worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<ObjectId>,
    /// Full refname as raw Git bytes so non-UTF-8 refs remain representable.
    pub branch: Option<Vec<u8>>,
    pub detached: bool,
    pub bare: bool,
    pub locked_reason: Option<Vec<u8>>,
    pub prunable_reason: Option<Vec<u8>>,
}

/// Errors returned while communicating with the installed Git executable.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("could not start Git command {command:?}: {source}")]
    Start {
        command: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Git command failed ({arguments}): {stderr}")]
    CommandFailed { arguments: String, stderr: String },

    #[error("invalid Git output for {context}: {detail}")]
    InvalidOutput {
        context: &'static str,
        detail: String,
    },
}

/// A handle to the real Git executable.
#[derive(Debug, Clone)]
pub struct Git {
    command: PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        Self::new("git")
    }
}

impl Git {
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
        }
    }

    pub fn detect(&self) -> Result<GitInfo, GitError> {
        let output = self.run(None, &["--version"])?;
        let version = utf8_line(&output.stdout, "git --version")?;

        Ok(GitInfo {
            command: self.command.clone(),
            version,
        })
    }

    /// Inspect a normal, linked, unborn, detached, or bare repository.
    pub fn inspect_repository(&self, path: &Path) -> Result<RepositoryInfo, GitError> {
        let is_bare = match self
            .run_text(
                Some(path),
                &["rev-parse", "--is-bare-repository"],
                "bare repository flag",
            )?
            .as_str()
        {
            "true" => true,
            "false" => false,
            value => {
                return Err(GitError::InvalidOutput {
                    context: "bare repository flag",
                    detail: format!("expected true or false, got {value:?}"),
                });
            }
        };

        let common_git_dir = self.run_path(
            Some(path),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "common Git directory",
        )?;
        let root = if is_bare {
            None
        } else {
            Some(self.run_path(
                Some(path),
                &["rev-parse", "--path-format=absolute", "--show-toplevel"],
                "working-tree root",
            )?)
        };

        let head_commit = self.resolve_optional_object(path, "HEAD^{commit}")?;
        let head_tree = self.resolve_optional_object(path, "HEAD^{tree}")?;
        let clean = if is_bare {
            None
        } else {
            let status = self.run(Some(path), &["status", "--porcelain=v1", "-z"])?;
            Some(status.stdout.is_empty())
        };

        Ok(RepositoryInfo {
            root,
            identity: RepositoryIdentity { common_git_dir },
            is_bare,
            head_commit,
            head_tree,
            clean,
        })
    }

    /// Resolve `revision` to exact commit and tree IDs using Git's semantics.
    pub fn resolve_revision(
        &self,
        path: &Path,
        revision: &OsStr,
    ) -> Result<ResolvedRevision, GitError> {
        let commit = self.resolve_required_object(path, revision, "^{commit}")?;
        let tree = self.resolve_required_object(path, revision, "^{tree}")?;
        Ok(ResolvedRevision { commit, tree })
    }

    /// Return Git's stable, NUL-delimited worktree inventory.
    pub fn list_worktrees(&self, path: &Path) -> Result<Vec<WorktreeInfo>, GitError> {
        let output = self.run(Some(path), &["worktree", "list", "--porcelain", "-z"])?;
        parse_worktree_porcelain(&output.stdout)
    }

    fn resolve_optional_object(
        &self,
        path: &Path,
        revision: &str,
    ) -> Result<Option<ObjectId>, GitError> {
        let output = self.output(
            Some(path),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                revision,
            ],
        )?;
        if !output.status.success() {
            return Ok(None);
        }

        parse_object_output(&output.stdout).map(Some)
    }

    fn resolve_required_object(
        &self,
        path: &Path,
        revision: &OsStr,
        suffix: &str,
    ) -> Result<ObjectId, GitError> {
        let mut expression = revision.to_os_string();
        expression.push(suffix);
        let arguments = [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--end-of-options"),
            expression,
        ];
        let output = self.run_os(Some(path), &arguments)?;
        parse_object_output(&output.stdout)
    }

    fn run_path(
        &self,
        path: Option<&Path>,
        arguments: &[&str],
        context: &'static str,
    ) -> Result<PathBuf, GitError> {
        let output = self.run(path, arguments)?;
        let bytes = trim_line_endings(&output.stdout);
        if bytes.is_empty() {
            return Err(GitError::InvalidOutput {
                context,
                detail: "path was empty".to_owned(),
            });
        }
        Ok(PathBuf::from(os_string_from_git(bytes, context)?))
    }

    fn run_text(
        &self,
        path: Option<&Path>,
        arguments: &[&str],
        context: &'static str,
    ) -> Result<String, GitError> {
        let output = self.run(path, arguments)?;
        utf8_line(&output.stdout, context)
    }

    fn run(&self, path: Option<&Path>, arguments: &[&str]) -> Result<Output, GitError> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        self.run_os(path, &arguments)
    }

    fn run_os(&self, path: Option<&Path>, arguments: &[OsString]) -> Result<Output, GitError> {
        let output = self.output_os(path, arguments)?;

        if output.status.success() {
            Ok(output)
        } else {
            Err(GitError::CommandFailed {
                arguments: display_arguments(arguments),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    fn output(&self, path: Option<&Path>, arguments: &[&str]) -> Result<Output, GitError> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        self.output_os(path, &arguments)
    }

    fn output_os(&self, path: Option<&Path>, arguments: &[OsString]) -> Result<Output, GitError> {
        let mut command = Command::new(&self.command);
        command.args(arguments);

        if let Some(path) = path {
            command.current_dir(path);
        }

        command.output().map_err(|source| GitError::Start {
            command: self.command.clone(),
            source,
        })
    }
}

/// Parse `git worktree list --porcelain -z` without decoding paths as UTF-8.
pub fn parse_worktree_porcelain(input: &[u8]) -> Result<Vec<WorktreeInfo>, GitError> {
    #[derive(Default)]
    struct PartialWorktree {
        path: Option<PathBuf>,
        head: Option<ObjectId>,
        branch: Option<Vec<u8>>,
        detached: bool,
        bare: bool,
        locked_reason: Option<Vec<u8>>,
        prunable_reason: Option<Vec<u8>>,
    }

    fn finish(
        worktrees: &mut Vec<WorktreeInfo>,
        partial: &mut Option<PartialWorktree>,
    ) -> Result<(), GitError> {
        let Some(partial) = partial.take() else {
            return Ok(());
        };
        let path = partial.path.ok_or_else(|| GitError::InvalidOutput {
            context: "worktree porcelain",
            detail: "record did not contain a worktree path".to_owned(),
        })?;
        if usize::from(partial.detached)
            + usize::from(partial.bare)
            + usize::from(partial.branch.is_some())
            != 1
        {
            return Err(GitError::InvalidOutput {
                context: "worktree porcelain",
                detail: format!(
                    "{} did not have exactly one of branch, detached, or bare",
                    path.display()
                ),
            });
        }
        worktrees.push(WorktreeInfo {
            path,
            head: partial.head,
            branch: partial.branch,
            detached: partial.detached,
            bare: partial.bare,
            locked_reason: partial.locked_reason,
            prunable_reason: partial.prunable_reason,
        });
        Ok(())
    }

    let mut worktrees = Vec::new();
    let mut partial: Option<PartialWorktree> = None;

    for field in input.split(|byte| *byte == 0) {
        if field.is_empty() {
            finish(&mut worktrees, &mut partial)?;
            continue;
        }

        if let Some(value) = field.strip_prefix(b"worktree ") {
            finish(&mut worktrees, &mut partial)?;
            let path = PathBuf::from(os_string_from_git(value, "worktree path")?);
            partial = Some(PartialWorktree {
                path: Some(path),
                ..PartialWorktree::default()
            });
            continue;
        }

        let record = partial.as_mut().ok_or_else(|| GitError::InvalidOutput {
            context: "worktree porcelain",
            detail: format!(
                "field appeared before a worktree path: {:?}",
                String::from_utf8_lossy(field)
            ),
        })?;

        if let Some(value) = field.strip_prefix(b"HEAD ") {
            let head = parse_object_bytes(value)?;
            record.head = (!head.is_null()).then_some(head);
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            record.branch = Some(value.to_vec());
        } else if field == b"detached" {
            record.detached = true;
        } else if field == b"bare" {
            record.bare = true;
        } else if field == b"locked" {
            record.locked_reason = Some(Vec::new());
        } else if let Some(value) = field.strip_prefix(b"locked ") {
            record.locked_reason = Some(value.to_vec());
        } else if field == b"prunable" {
            record.prunable_reason = Some(Vec::new());
        } else if let Some(value) = field.strip_prefix(b"prunable ") {
            record.prunable_reason = Some(value.to_vec());
        }
        // Unknown fields are ignored for forward compatibility as required by
        // Git's porcelain format contract.
    }

    finish(&mut worktrees, &mut partial)?;
    Ok(worktrees)
}

fn parse_object_output(bytes: &[u8]) -> Result<ObjectId, GitError> {
    parse_object_bytes(trim_line_endings(bytes))
}

fn parse_object_bytes(bytes: &[u8]) -> Result<ObjectId, GitError> {
    let value = std::str::from_utf8(bytes).map_err(|error| GitError::InvalidOutput {
        context: "object ID",
        detail: error.to_string(),
    })?;
    ObjectId::parse(value)
}

fn utf8_line(bytes: &[u8], context: &'static str) -> Result<String, GitError> {
    let value =
        std::str::from_utf8(trim_line_endings(bytes)).map_err(|error| GitError::InvalidOutput {
            context,
            detail: error.to_string(),
        })?;
    Ok(value.to_owned())
}

fn trim_line_endings(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn os_string_from_git(bytes: &[u8], _context: &'static str) -> Result<OsString, GitError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn os_string_from_git(bytes: &[u8], context: &'static str) -> Result<OsString, GitError> {
    String::from_utf8(bytes.to_vec())
        .map(OsString::from)
        .map_err(|error| GitError::InvalidOutput {
            context,
            detail: error.to_string(),
        })
}

fn display_arguments(arguments: &[OsString]) -> String {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::{TempDir, tempdir};

    use super::{Git, parse_worktree_porcelain};

    struct RepositoryFixture {
        directory: TempDir,
    }

    impl RepositoryFixture {
        fn unborn() -> Self {
            let directory = tempdir().expect("temporary directory");
            git(directory.path(), &["init", "--quiet"]);
            git(directory.path(), &["config", "user.name", "Riftri Tests"]);
            git(
                directory.path(),
                &["config", "user.email", "riftri@example.invalid"],
            );
            Self { directory }
        }

        fn committed() -> Self {
            let fixture = Self::unborn();
            fs::write(fixture.path().join("tracked.txt"), "tracked\n").expect("write fixture");
            git(fixture.path(), &["add", "--", "tracked.txt"]);
            git(fixture.path(), &["commit", "--quiet", "-m", "initial"]);
            fixture
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }
    }

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .expect("start Git fixture command");
        assert!(status.success(), "git {arguments:?} failed");
    }

    #[test]
    fn detects_installed_git() {
        let info = Git::default().detect().expect("Git should be installed");

        assert!(info.version.starts_with("git version"));
    }

    #[test]
    fn inspects_an_unborn_repository() {
        let fixture = RepositoryFixture::unborn();

        let repository = Git::default()
            .inspect_repository(fixture.path())
            .expect("inspect repository");

        assert_eq!(
            repository
                .root
                .as_deref()
                .expect("working-tree root")
                .canonicalize()
                .expect("canonical root"),
            fixture.path().canonicalize().expect("canonical fixture")
        );
        assert!(!repository.is_bare);
        assert!(repository.head_commit.is_none());
        assert!(repository.head_tree.is_none());
        assert_eq!(repository.clean, Some(true));
    }

    #[test]
    fn resolves_commit_and_tree_ids_in_a_normal_repository() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();

        let repository = git
            .inspect_repository(fixture.path())
            .expect("inspect repository");
        let resolved = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve HEAD");

        assert_eq!(repository.head_commit, Some(resolved.commit));
        assert_eq!(repository.head_tree, Some(resolved.tree));
        assert_eq!(repository.clean, Some(true));
    }

    #[test]
    fn inspects_a_bare_repository() {
        let directory = tempdir().expect("temporary directory");
        git(directory.path(), &["init", "--bare", "--quiet"]);

        let repository = Git::default()
            .inspect_repository(directory.path())
            .expect("inspect bare repository");

        assert!(repository.root.is_none());
        assert!(repository.is_bare);
        assert_eq!(repository.clean, None);
        assert_eq!(
            repository
                .identity
                .common_git_dir
                .canonicalize()
                .expect("canonical common directory"),
            directory.path().canonicalize().expect("canonical fixture")
        );
    }

    #[test]
    fn detects_a_detached_worktree() {
        let fixture = RepositoryFixture::committed();
        git(fixture.path(), &["checkout", "--quiet", "--detach", "HEAD"]);

        let worktrees = Git::default()
            .list_worktrees(fixture.path())
            .expect("list worktrees");

        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].detached);
        assert!(worktrees[0].branch.is_none());
    }

    #[test]
    fn main_and_linked_worktrees_share_repository_identity() {
        let fixture = RepositoryFixture::committed();
        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent.path().join("linked worktree");
        let linked_string = linked.to_str().expect("UTF-8 fixture path");
        git(
            fixture.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked_string,
                "HEAD",
            ],
        );
        let git = Git::default();

        let main = git
            .inspect_repository(fixture.path())
            .expect("inspect main worktree");
        let linked_info = git
            .inspect_repository(&linked)
            .expect("inspect linked worktree");
        let worktrees = git.list_worktrees(fixture.path()).expect("list worktrees");

        assert_eq!(main.identity, linked_info.identity);
        assert_eq!(
            linked_info
                .root
                .as_deref()
                .expect("linked root")
                .canonicalize()
                .expect("canonical linked root"),
            linked.canonicalize().expect("canonical linked fixture")
        );
        assert_eq!(worktrees.len(), 2);
        assert!(worktrees.iter().any(|worktree| {
            worktree.path.canonicalize().ok().as_deref() == linked.canonicalize().ok().as_deref()
        }));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn discovers_a_linked_worktree_with_a_non_utf8_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let fixture = RepositoryFixture::committed();
        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent
            .path()
            .join(OsString::from_vec(b"linked-\xff worktree".to_vec()));
        let status = Command::new("git")
            .args([
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
            ])
            .arg("--detach")
            .arg(&linked)
            .arg("HEAD")
            .current_dir(fixture.path())
            .status()
            .expect("create non-UTF-8 linked worktree");
        assert!(status.success());

        let git = Git::default();
        let main = git
            .inspect_repository(fixture.path())
            .expect("inspect main worktree");
        let linked_repository = git
            .inspect_repository(&linked)
            .expect("inspect linked worktree");
        let worktrees = git.list_worktrees(fixture.path()).expect("list worktrees");

        assert_eq!(main.identity, linked_repository.identity);
        assert!(worktrees.iter().any(|worktree| {
            worktree.path.as_os_str().as_bytes() == linked.as_os_str().as_bytes()
        }));
    }

    #[cfg(unix)]
    #[test]
    fn porcelain_parser_preserves_non_utf8_paths_and_reasons() {
        use std::os::unix::ffi::OsStrExt;

        let input = b"worktree /tmp/riftri-\xff path\0HEAD 0123456789abcdef0123456789abcdef01234567\0detached\0locked needs repair\xff\0\0";

        let worktrees = parse_worktree_porcelain(input).expect("parse porcelain");

        assert_eq!(worktrees.len(), 1);
        assert_eq!(
            worktrees[0].path.as_os_str().as_bytes(),
            b"/tmp/riftri-\xff path"
        );
        assert_eq!(
            worktrees[0].locked_reason.as_deref(),
            Some(b"needs repair\xff".as_slice())
        );
    }

    #[test]
    fn porcelain_parser_rejects_ambiguous_head_state() {
        let input = b"worktree /tmp/example\0HEAD 0123456789abcdef0123456789abcdef01234567\0detached\0branch refs/heads/main\0\0";

        let error = parse_worktree_porcelain(input).expect_err("ambiguous state must fail");

        assert!(error.to_string().contains("exactly one"));
    }
}
