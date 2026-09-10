//! Interaction with the user's installed Git executable.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

#[cfg(unix)]
use std::process::Stdio;

use serde::Serialize;
use thiserror::Error;

/// Absolute real-Git path supplied to a process-scoped Riftri shim.
pub const REAL_GIT_ENV: &str = "RIFTRI_REAL_GIT";
/// Marker proving that `REAL_GIT_ENV` belongs to a Riftri process scope.
pub const SHIM_ACTIVE_ENV: &str = "RIFTRI_SHIM_ACTIVE";

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

/// One recursive entry from an exact Git tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: u32,
    pub object_kind: Vec<u8>,
    pub object_id: ObjectId,
    pub path: PathBuf,
}

/// One effective path attribute reported by `git check-attr -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitAttribute {
    pub path: PathBuf,
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

/// Branch behavior for a new linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeHead<'a> {
    NewBranch(&'a OsStr),
    Detached,
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

    #[error("could not write input to Git command {command:?}: {source}")]
    WriteInput {
        command: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not wait for Git command {command:?}: {source}")]
    Wait {
        command: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A handle to the real Git executable.
#[derive(Debug, Clone)]
pub struct Git {
    command: PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        let scoped_command = std::env::var_os(SHIM_ACTIVE_ENV)
            .and_then(|_| std::env::var_os(REAL_GIT_ENV))
            .filter(|command| !command.is_empty());
        Self::new(scoped_command.unwrap_or_else(|| OsString::from("git")))
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

    /// Run the real Git executable with inherited process I/O and return its
    /// status without interpreting a non-zero Git exit as a Riftri error.
    pub fn passthrough(&self, arguments: &[OsString]) -> Result<ExitStatus, GitError> {
        Command::new(&self.command)
            .args(arguments)
            .status()
            .map_err(|source| GitError::Start {
                command: self.command.clone(),
                source,
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

    /// List every entry in an exact tree without interpreting path bytes.
    pub fn list_tree(&self, path: &Path, tree: &ObjectId) -> Result<Vec<TreeEntry>, GitError> {
        let arguments = [
            OsString::from("ls-tree"),
            OsString::from("-r"),
            OsString::from("-z"),
            OsString::from("--full-tree"),
            OsString::from(tree.as_str()),
        ];
        let output = self.run_os(Some(path), &arguments)?;
        parse_tree_entries(&output.stdout)
    }

    /// Check whether any repository configuration key matches a Git regexp.
    pub fn has_config_matching(&self, path: &Path, pattern: &str) -> Result<bool, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--get-regexp"),
            OsString::from(pattern),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Read one repository configuration value as raw Git bytes.
    pub fn config_value(&self, path: &Path, key: &str) -> Result<Option<Vec<u8>>, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--get"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            Ok(Some(
                output
                    .stdout
                    .strip_suffix(&[0])
                    .unwrap_or(&output.stdout)
                    .to_vec(),
            ))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Read one value from this repository's local configuration only.
    pub fn local_config_value(&self, path: &Path, key: &str) -> Result<Option<Vec<u8>>, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--null"),
            OsString::from("--get"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            Ok(Some(
                output
                    .stdout
                    .strip_suffix(&[0])
                    .unwrap_or(&output.stdout)
                    .to_vec(),
            ))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Read a repository-local boolean using Git's own boolean parser.
    pub fn local_config_bool(&self, path: &Path, key: &str) -> Result<Option<bool>, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--bool"),
            OsString::from("--get"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            match trim_line_endings(&output.stdout) {
                b"true" => Ok(Some(true)),
                b"false" => Ok(Some(false)),
                value => Err(GitError::InvalidOutput {
                    context: "repository-local boolean configuration",
                    detail: format!(
                        "Git normalized {key} to unexpected value {:?}",
                        String::from_utf8_lossy(value)
                    ),
                }),
            }
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Atomically replace one repository-local configuration value through Git.
    pub fn set_local_config(&self, path: &Path, key: &str, value: &OsStr) -> Result<(), GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--replace-all"),
            OsString::from(key),
            value.to_os_string(),
        ];
        self.run_os(Some(path), &arguments)?;
        Ok(())
    }

    /// Remove a repository-local configuration key. Missing keys are accepted.
    pub fn unset_local_config(&self, path: &Path, key: &str) -> Result<(), GitError> {
        if self.local_config_value(path, key)?.is_none() {
            return Ok(());
        }
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--unset-all"),
            OsString::from(key),
        ];
        self.run_os(Some(path), &arguments)?;
        Ok(())
    }

    /// Return every attribute Git resolves for the supplied tree paths.
    /// `--cached` prevents a mutable working-tree attributes file from being
    /// treated as the requested tree; repository, global, and system attribute
    /// sources still participate and therefore make the prototype refuse.
    pub fn effective_attributes_for_paths(
        &self,
        path: &Path,
        paths: &[PathBuf],
    ) -> Result<Vec<GitAttribute>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let arguments = [
                OsString::from("check-attr"),
                OsString::from("--cached"),
                OsString::from("--all"),
                OsString::from("-z"),
                OsString::from("--stdin"),
            ];
            let mut input = Vec::new();
            for entry in paths {
                input.extend_from_slice(entry.as_os_str().as_bytes());
                input.push(0);
            }
            let output = self.run_os_with_input(Some(path), &arguments, input)?;
            parse_attribute_records(&output.stdout)
        }

        #[cfg(not(unix))]
        {
            let mut attributes = Vec::new();
            for chunk in paths.chunks(128) {
                let mut arguments = vec![
                    OsString::from("check-attr"),
                    OsString::from("--cached"),
                    OsString::from("--all"),
                    OsString::from("-z"),
                    OsString::from("--"),
                ];
                arguments.extend(chunk.iter().map(|entry| entry.as_os_str().to_os_string()));
                let output = self.run_os(Some(path), &arguments)?;
                attributes.extend(parse_attribute_records(&output.stdout)?);
            }
            Ok(attributes)
        }
    }

    /// Return whether Git resolves any attribute for the supplied tree paths.
    pub fn paths_have_effective_attributes(
        &self,
        path: &Path,
        paths: &[PathBuf],
    ) -> Result<bool, GitError> {
        Ok(!self.effective_attributes_for_paths(path, paths)?.is_empty())
    }

    /// Materialize an exact tree with Git's checkout machinery and an isolated
    /// temporary index. `destination` must already exist and `temporary_index`
    /// must not exist.
    pub fn materialize_tree(
        &self,
        repository: &Path,
        tree: &ObjectId,
        destination: &Path,
        temporary_index: &Path,
    ) -> Result<(), GitError> {
        let environment = [(OsStr::new("GIT_INDEX_FILE"), temporary_index.as_os_str())];
        let read_tree = [OsString::from("read-tree"), OsString::from(tree.as_str())];
        self.run_os_with_env(Some(repository), &read_tree, &environment)?;

        let mut prefix = destination.as_os_str().to_os_string();
        prefix.push(std::path::MAIN_SEPARATOR.to_string());
        let checkout = [
            OsString::from("checkout-index"),
            OsString::from("--all"),
            OsString::from("--force"),
            OsString::from("--prefix"),
            prefix,
        ];
        self.run_os_with_env(Some(repository), &checkout, &environment)?;
        Ok(())
    }

    /// Create real linked-worktree metadata while suppressing Git's checkout.
    pub fn add_worktree_no_checkout(
        &self,
        repository: &Path,
        destination: &Path,
        revision: &OsStr,
        head: WorktreeHead<'_>,
    ) -> Result<(), GitError> {
        let mut arguments = vec![
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--quiet"),
            OsString::from("--no-checkout"),
        ];
        match head {
            WorktreeHead::NewBranch(branch) => {
                arguments.push(OsString::from("-b"));
                arguments.push(branch.to_os_string());
            }
            WorktreeHead::Detached => arguments.push(OsString::from("--detach")),
        }
        arguments.push(destination.as_os_str().to_os_string());
        arguments.push(revision.to_os_string());
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Populate the linked worktree index from HEAD without writing files.
    pub fn synchronize_worktree_index(&self, worktree: &Path) -> Result<(), GitError> {
        self.run(Some(worktree), &["reset", "--mixed", "--quiet", "HEAD"])?;
        self.run(Some(worktree), &["update-index", "--refresh"])?;
        Ok(())
    }

    pub fn worktree_is_clean(&self, worktree: &Path) -> Result<bool, GitError> {
        let output = self.run(Some(worktree), &["status", "--porcelain=v1", "-z"])?;
        Ok(output.stdout.is_empty())
    }

    /// Remove a linked worktree through Git's normal dirty-worktree checks.
    pub fn remove_worktree(&self, repository: &Path, worktree: &Path) -> Result<(), GitError> {
        let arguments = [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--"),
            worktree.as_os_str().to_os_string(),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Remove Git's linked-worktree registration and the worktree directory.
    /// Callers must enforce Riftri's clean-worktree policy before using force.
    pub fn remove_worktree_force(
        &self,
        repository: &Path,
        worktree: &Path,
    ) -> Result<(), GitError> {
        let arguments = [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            worktree.as_os_str().to_os_string(),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Move a linked worktree through Git so its administrative metadata and
    /// worktree-local `.git` pointer are updated together.
    pub fn move_worktree(
        &self,
        repository: &Path,
        source: &Path,
        destination: &Path,
    ) -> Result<(), GitError> {
        let arguments = [
            OsString::from("worktree"),
            OsString::from("move"),
            OsString::from("--"),
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Prune stale linked-worktree administrative metadata. Callers must first
    /// prove that every Riftri-managed view is still present and registered.
    pub fn prune_worktrees(&self, repository: &Path) -> Result<(), GitError> {
        self.run(Some(repository), &["worktree", "prune"])?;
        Ok(())
    }

    /// Resolve a local branch only when that exact ref exists.
    pub fn local_branch_target(
        &self,
        repository: &Path,
        branch: &OsStr,
    ) -> Result<Option<ObjectId>, GitError> {
        let mut reference = OsString::from("refs/heads/");
        reference.push(branch);
        let exists_arguments = [
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            reference.clone(),
        ];
        let exists = self.output_os(Some(repository), &exists_arguments)?;
        if exists.status.code() == Some(1) {
            return Ok(None);
        }
        if !exists.status.success() {
            return Err(command_failed(&exists_arguments, &exists));
        }

        let arguments = [
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--hash"),
            reference,
        ];
        let output = self.run_os(Some(repository), &arguments)?;
        parse_object_output(&output.stdout).map(Some)
    }

    /// Delete a local branch during rollback after the caller verifies its
    /// target still matches the commit created for the failed transaction.
    pub fn delete_branch_force(&self, repository: &Path, branch: &OsStr) -> Result<(), GitError> {
        let arguments = [
            OsString::from("branch"),
            OsString::from("-D"),
            OsString::from("--"),
            branch.to_os_string(),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
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
            Err(command_failed(arguments, &output))
        }
    }

    fn run_os_with_env(
        &self,
        path: Option<&Path>,
        arguments: &[OsString],
        environment: &[(&OsStr, &OsStr)],
    ) -> Result<Output, GitError> {
        let output = self.output_os_with_env(path, arguments, environment)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(command_failed(arguments, &output))
        }
    }

    fn output(&self, path: Option<&Path>, arguments: &[&str]) -> Result<Output, GitError> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        self.output_os(path, &arguments)
    }

    fn output_os(&self, path: Option<&Path>, arguments: &[OsString]) -> Result<Output, GitError> {
        self.output_os_with_env(path, arguments, &[])
    }

    fn output_os_with_env(
        &self,
        path: Option<&Path>,
        arguments: &[OsString],
        environment: &[(&OsStr, &OsStr)],
    ) -> Result<Output, GitError> {
        let mut command = Command::new(&self.command);
        command.args(arguments).envs(environment.iter().copied());

        if let Some(path) = path {
            command.current_dir(path);
        }

        command.output().map_err(|source| GitError::Start {
            command: self.command.clone(),
            source,
        })
    }

    #[cfg(unix)]
    fn run_os_with_input(
        &self,
        path: Option<&Path>,
        arguments: &[OsString],
        input: Vec<u8>,
    ) -> Result<Output, GitError> {
        use std::io::Write;

        let mut command = Command::new(&self.command);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            command.current_dir(path);
        }

        let mut child = command.spawn().map_err(|source| GitError::Start {
            command: self.command.clone(),
            source,
        })?;
        let mut stdin = child.stdin.take().ok_or_else(|| GitError::InvalidOutput {
            context: "Git command input",
            detail: "piped standard input was unavailable".to_owned(),
        })?;
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().map_err(|source| GitError::Wait {
            command: self.command.clone(),
            source,
        })?;
        let write_result = writer.join().map_err(|_| GitError::InvalidOutput {
            context: "Git command input",
            detail: "input writer thread panicked".to_owned(),
        })?;

        if !output.status.success() {
            return Err(command_failed(arguments, &output));
        }
        write_result.map_err(|source| GitError::WriteInput {
            command: self.command.clone(),
            source,
        })?;
        Ok(output)
    }
}

/// Parse NUL-delimited `git check-attr -z` path/name/value triples.
pub fn parse_attribute_records(input: &[u8]) -> Result<Vec<GitAttribute>, GitError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if input.last() != Some(&0) {
        return Err(GitError::InvalidOutput {
            context: "Git attribute records",
            detail: "output did not end with a NUL delimiter".to_owned(),
        });
    }

    let fields = input[..input.len() - 1]
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return Err(GitError::InvalidOutput {
            context: "Git attribute records",
            detail: "output did not contain path/name/value triples".to_owned(),
        });
    }

    fields
        .chunks_exact(3)
        .map(|fields| {
            if fields[0].is_empty() || fields[1].is_empty() {
                return Err(GitError::InvalidOutput {
                    context: "Git attribute record",
                    detail: "path and attribute name must not be empty".to_owned(),
                });
            }
            Ok(GitAttribute {
                path: PathBuf::from(os_string_from_git(fields[0], "attribute path")?),
                name: fields[1].to_vec(),
                value: fields[2].to_vec(),
            })
        })
        .collect()
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

/// Parse `git ls-tree -r -z --full-tree` records without decoding paths.
pub fn parse_tree_entries(input: &[u8]) -> Result<Vec<TreeEntry>, GitError> {
    let mut entries = Vec::new();
    for record in input
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let delimiter = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| GitError::InvalidOutput {
                context: "tree entry",
                detail: "record did not contain a tab-delimited path".to_owned(),
            })?;
        let metadata = &record[..delimiter];
        let path = &record[delimiter + 1..];
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode_bytes = fields.next().unwrap_or_default();
        let object_kind = fields.next().unwrap_or_default();
        let object_id = fields.next().unwrap_or_default();
        if fields.next().is_some()
            || mode_bytes.is_empty()
            || object_kind.is_empty()
            || object_id.is_empty()
        {
            return Err(GitError::InvalidOutput {
                context: "tree entry",
                detail: "record metadata did not contain mode, type, and object ID".to_owned(),
            });
        }
        let mode_text =
            std::str::from_utf8(mode_bytes).map_err(|error| GitError::InvalidOutput {
                context: "tree mode",
                detail: error.to_string(),
            })?;
        let mode = u32::from_str_radix(mode_text, 8).map_err(|error| GitError::InvalidOutput {
            context: "tree mode",
            detail: error.to_string(),
        })?;
        if path.is_empty() {
            return Err(GitError::InvalidOutput {
                context: "tree entry",
                detail: "path was empty".to_owned(),
            });
        }
        entries.push(TreeEntry {
            mode,
            object_kind: object_kind.to_vec(),
            object_id: parse_object_bytes(object_id)?,
            path: PathBuf::from(os_string_from_git(path, "tree path")?),
        });
    }
    Ok(entries)
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

fn command_failed(arguments: &[OsString], output: &Output) -> GitError {
    GitError::CommandFailed {
        arguments: display_arguments(arguments),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::{TempDir, tempdir};

    use super::{Git, WorktreeHead, parse_attribute_records, parse_worktree_porcelain};

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
            git(directory.path(), &["config", "core.autocrlf", "false"]);
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
    fn lists_and_materializes_an_exact_tree_with_an_isolated_index() {
        let fixture = RepositoryFixture::committed();
        fs::create_dir(fixture.path().join("nested")).expect("create nested directory");
        fs::write(fixture.path().join("nested/file.txt"), "nested\n").expect("write nested file");
        git(fixture.path(), &["add", "--", "nested/file.txt"]);
        git(fixture.path(), &["commit", "--quiet", "-m", "nested"]);
        let git = Git::default();
        let resolved = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve tree");

        let entries = git
            .list_tree(fixture.path(), &resolved.tree)
            .expect("list exact tree");

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.object_kind == b"blob"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == Path::new("tracked.txt"))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == Path::new("nested/file.txt"))
        );

        let output = tempdir().expect("materialization parent");
        let destination = output.path().join("tree");
        fs::create_dir(&destination).expect("create materialization destination");
        let temporary_index = output.path().join("temporary.index");
        git.materialize_tree(
            fixture.path(),
            &resolved.tree,
            &destination,
            &temporary_index,
        )
        .expect("materialize exact tree");

        assert_eq!(
            fs::read_to_string(destination.join("tracked.txt")).expect("read materialized file"),
            "tracked\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt"))
                .expect("read nested materialized file"),
            "nested\n"
        );
    }

    #[test]
    fn creates_suppressed_checkout_and_synchronizes_its_index() {
        let fixture = RepositoryFixture::committed();
        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent.path().join("suppressed");
        let git = Git::default();
        let revision = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve HEAD");

        git.add_worktree_no_checkout(
            fixture.path(),
            &linked,
            OsStr::new("HEAD"),
            WorktreeHead::NewBranch(OsStr::new("feature/suppressed")),
        )
        .expect("add no-checkout worktree");

        assert!(linked.join(".git").is_file());
        assert!(!linked.join("tracked.txt").exists());
        assert_eq!(
            git.local_branch_target(fixture.path(), OsStr::new("feature/suppressed"))
                .expect("read branch"),
            Some(revision.commit.clone())
        );

        fs::write(linked.join("tracked.txt"), "tracked\n").expect("materialize linked file");
        git.synchronize_worktree_index(&linked)
            .expect("synchronize index");
        assert!(git.worktree_is_clean(&linked).expect("check clean"));

        fs::write(linked.join("tracked.txt"), "changed\n").expect("modify linked file");
        assert!(!git.worktree_is_clean(&linked).expect("check dirty"));
        fs::write(linked.join("tracked.txt"), "tracked\n").expect("restore linked file");
        assert!(git.worktree_is_clean(&linked).expect("check restored"));

        git.remove_worktree_force(fixture.path(), &linked)
            .expect("remove linked worktree");
        git.delete_branch_force(fixture.path(), OsStr::new("feature/suppressed"))
            .expect("delete rollback branch");
        assert!(!linked.exists());
        assert_eq!(
            git.local_branch_target(fixture.path(), OsStr::new("feature/suppressed"))
                .expect("check removed branch"),
            None
        );
    }

    #[test]
    fn reads_repository_configuration_without_human_output() {
        let fixture = RepositoryFixture::committed();
        git(
            fixture.path(),
            &["config", "filter.riftri-test.clean", "cat"],
        );
        let git = Git::default();

        assert!(
            git.has_config_matching(fixture.path(), r"^filter\.")
                .expect("match filter config")
        );
        assert_eq!(
            git.config_value(fixture.path(), "filter.riftri-test.clean")
                .expect("read filter config")
                .as_deref(),
            Some(b"cat".as_slice())
        );
        assert_eq!(
            git.config_value(fixture.path(), "filter.missing.clean")
                .expect("read missing config"),
            None
        );
    }

    #[test]
    fn writes_and_removes_repository_local_configuration() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();

        git.set_local_config(fixture.path(), "riftri.enabled", OsStr::new("true"))
            .expect("enable repository");
        assert_eq!(
            git.local_config_value(fixture.path(), "riftri.enabled")
                .expect("read local configuration"),
            Some(b"true".to_vec())
        );
        assert_eq!(
            git.local_config_bool(fixture.path(), "riftri.enabled")
                .expect("read local boolean"),
            Some(true)
        );

        git.unset_local_config(fixture.path(), "riftri.enabled")
            .expect("disable repository");
        assert_eq!(
            git.local_config_value(fixture.path(), "riftri.enabled")
                .expect("read removed local configuration"),
            None
        );
    }

    #[test]
    fn detects_external_attributes_for_tree_paths() {
        let fixture = RepositoryFixture::committed();
        let attributes = fixture.path().join(".git/info/attributes");
        fs::write(attributes, "*.txt riftri-test\n").expect("write info attributes");
        let git = Git::default();

        let attributes = git
            .effective_attributes_for_paths(
                fixture.path(),
                &[Path::new("tracked.txt").to_path_buf()],
            )
            .expect("read attributes");
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].path, Path::new("tracked.txt"));
        assert_eq!(attributes[0].name, b"riftri-test");
        assert_eq!(attributes[0].value, b"set");
        assert!(
            git.paths_have_effective_attributes(
                fixture.path(),
                &[Path::new("tracked.txt").to_path_buf()]
            )
            .expect("check attributes")
        );
        assert!(
            !git.paths_have_effective_attributes(
                fixture.path(),
                &[Path::new("unmatched.bin").to_path_buf()]
            )
            .expect("check unmatched attributes")
        );
    }

    #[cfg(unix)]
    #[test]
    fn attribute_parser_preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;

        let attributes =
            parse_attribute_records(b"invalid-\xff\0text\0auto\0").expect("parse attribute record");

        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].path.as_os_str().as_bytes(), b"invalid-\xff");
        assert_eq!(attributes[0].name, b"text");
        assert_eq!(attributes[0].value, b"auto");
    }

    #[test]
    fn attribute_parser_rejects_incomplete_records() {
        assert!(parse_attribute_records(b"tracked.txt\0text\0").is_err());
        assert!(parse_attribute_records(b"tracked.txt\0text").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn checks_large_attribute_path_sets_with_one_git_process() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = RepositoryFixture::committed();
        let wrapper_directory = tempdir().expect("wrapper directory");
        let wrapper = wrapper_directory.path().join("git-wrapper");
        let calls = wrapper_directory.path().join("git-wrapper.calls");
        fs::write(
            &wrapper,
            "#!/bin/sh\nprintf 'call\\n' >> \"$0.calls\"\nexec git \"$@\"\n",
        )
        .expect("write Git wrapper");
        let mut permissions = fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");
        let paths = (0..300)
            .map(|index| PathBuf::from(format!("path-{index}.txt")))
            .collect::<Vec<_>>();

        let attributes = Git::new(&wrapper)
            .paths_have_effective_attributes(fixture.path(), &paths)
            .expect("check attributes");

        assert!(!attributes);
        assert_eq!(
            fs::read_to_string(calls).expect("read wrapper calls"),
            "call\n",
        );
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

    #[cfg(unix)]
    #[test]
    fn tree_parser_preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;

        let input = b"100644 blob 0123456789abcdef0123456789abcdef01234567\tname-\xff\0";

        let entries = super::parse_tree_entries(input).expect("parse tree entries");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mode, 0o100644);
        assert_eq!(entries[0].path.as_os_str().as_bytes(), b"name-\xff");
    }
}
