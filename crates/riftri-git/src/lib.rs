//! Interaction with the user's installed Git executable.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use std::process::Stdio;

use serde::Serialize;
use thiserror::Error;

pub mod termination;

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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitAttribute {
    pub path: PathBuf,
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

/// A private temporary index holding one exact tree for attribute queries.
///
/// The index is populated by a single `git read-tree` and removed with this
/// value; every query against it is read-only, so one index can serve both
/// the isolated and the effective attribute passes.
pub struct TreeAttributeIndex {
    /// Owns the temporary directory backing `index` for this value's lifetime.
    _temporary: tempfile::TempDir,
    index: PathBuf,
}

/// Effective values and conditional includes discovered in one configuration read.
#[derive(Debug, Default)]
pub struct ConfigValues {
    pub values: BTreeMap<String, Vec<u8>>,
    pub has_conditional_includes: bool,
}

/// One source contributing a value to Git's effective configuration.
///
/// The fields remain raw because an origin can contain a native path and a
/// configuration value is not required to be UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValueOrigin {
    pub scope: Vec<u8>,
    pub origin: Vec<u8>,
    pub value: Vec<u8>,
}

/// Sparse-checkout state of an existing worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SparseCheckoutState {
    /// `core.sparseCheckout` is on, so the worktree materializes a subset.
    pub enabled: bool,
    /// `core.sparseCheckoutCone` is on, so the selection is a directory list.
    pub cone: bool,
    /// The cone directory list, empty unless `cone` is set.
    pub directories: Vec<String>,
}

/// Branch behavior for a new linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeHead<'a> {
    NewBranch(&'a OsStr),
    ExistingBranch(&'a OsStr),
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
    /// Git listed this worktree with neither `branch`, `detached`, nor `bare`:
    /// its HEAD file exists but cannot be resolved (empty, garbage, or an
    /// empty symref target — classic crash and power-loss shapes). Git still
    /// registers the worktree and prints a null `HEAD` for it. Callers must
    /// never treat such a worktree as clean or operable; unrelated operations
    /// should skip it and surface a diagnostic instead of failing.
    pub head_unresolvable: bool,
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

    #[error("cannot run Git in {}: the directory does not exist", path.display())]
    WorkingDirectoryMissing { path: PathBuf },

    #[error("{} is not a Git repository", path.display())]
    RepositoryAbsent { path: PathBuf },

    #[error("Git command failed ({arguments}): {}", command_failure_detail(.disposition, .stderr))]
    CommandFailed {
        arguments: String,
        stderr: String,
        /// How the process ended: an exit code, or the terminating signal.
        disposition: String,
    },

    #[error(
        "Git's index lock {} exists: another Git process is using this worktree, or one was \
         killed while holding the lock; once no Git process is running there, remove {} and retry",
        .lock.display(),
        .lock.display()
    )]
    IndexLocked { lock: PathBuf },

    #[error(
        "could not peel {revision}: {base} resolves to {target}, but that object is unreadable; \
         the repository object store may be corrupt (try `git fsck`)"
    )]
    UnreadableObject {
        revision: String,
        base: String,
        target: String,
    },

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

    #[error("could not create temporary Git state: {source}")]
    TemporaryState {
        #[source]
        source: std::io::Error,
    },
}

/// A handle to the real Git executable.
#[derive(Debug, Clone)]
pub struct Git {
    command: PathBuf,
    #[cfg(test)]
    process_attempts: Arc<AtomicUsize>,
}

/// One operation-scoped `cat-file --batch` process. Call `finish` to validate
/// EOF and the exit status. Dropping an unfinished reader kills and reaps it.
pub struct SmallBlobReader {
    command: PathBuf,
    child: Option<std::process::Child>,
    input: Option<std::process::ChildStdin>,
    output: std::io::BufReader<std::process::ChildStdout>,
    stderr: std::fs::File,
}

impl SmallBlobReader {
    /// Read one bounded batch in request order, retaining at most
    /// `objects.len() * max_bytes` body bytes. Errors invalidate the session.
    pub fn read(
        &mut self,
        objects: &[ObjectId],
        max_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, GitError> {
        use std::io::Write;
        let result = (|| {
            let input = self
                .input
                .as_mut()
                .ok_or_else(|| invalid_blob_batch("closed session"))?;
            let mut blobs = Vec::with_capacity(objects.len());
            for object in objects {
                writeln!(input, "{}", object.as_str()).map_err(|source| GitError::WriteInput {
                    command: self.command.clone(),
                    source,
                })?;
                blobs.push(read_batch_blob(&mut self.output, object, max_bytes)?);
            }
            Ok(blobs)
        })();
        if result.is_err() {
            self.abort();
        }
        result
    }

    /// Close input and require no trailing response bytes and a successful exit.
    pub fn finish(mut self) -> Result<(), GitError> {
        use std::io::{Read, Seek, SeekFrom};
        if self.child.is_none() {
            return Err(invalid_blob_batch("closed session"));
        }
        drop(self.input.take());
        let mut extra = [0];
        if self
            .output
            .read(&mut extra)
            .map_err(|error| invalid_blob_batch(error.to_string()))?
            != 0
        {
            return Err(invalid_blob_batch("unexpected trailing response bytes"));
        }
        let status = self
            .child
            .as_mut()
            .expect("live session")
            .wait()
            .map_err(|source| GitError::Wait {
                command: self.command.clone(),
                source,
            })?;
        self.child.take();
        if !status.success() {
            self.stderr
                .seek(SeekFrom::Start(0))
                .map_err(|source| GitError::TemporaryState { source })?;
            let mut diagnostic = Vec::new();
            (&mut self.stderr)
                .take(64 * 1024)
                .read_to_end(&mut diagnostic)
                .map_err(|source| GitError::TemporaryState { source })?;
            return Err(command_failed(
                &[OsString::from("cat-file"), OsString::from("--batch")],
                &Output {
                    status,
                    stdout: Vec::new(),
                    stderr: diagnostic,
                },
            ));
        }
        Ok(())
    }

    fn abort(&mut self) {
        drop(self.input.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SmallBlobReader {
    fn drop(&mut self) {
        self.abort();
    }
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
            #[cfg(test)]
            process_attempts: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    fn process_attempts(&self) -> usize {
        self.process_attempts.load(Ordering::Relaxed)
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
    ///
    /// The caller is the Riftri Git shim, so the real Git started here is a
    /// grandchild of whatever sent a termination signal at the shim. Waiting
    /// under [`termination::run_forwarding_terminations`] is what keeps that
    /// signal travelling: without it the shim would die instantly and leave a
    /// long-running Git — a `clone` or `fetch` still writing to the working
    /// tree — reparented to init with nothing left to stop it.
    pub fn passthrough(&self, arguments: &[OsString]) -> Result<ExitStatus, GitError> {
        let mut command = Command::new(&self.command);
        command.args(arguments);
        termination::run_forwarding_terminations(&mut command).map_err(|error| match error {
            termination::TerminationError::Wait(source) => GitError::Wait {
                command: self.command.clone(),
                source,
            },
            termination::TerminationError::Spawn(source) => GitError::Start {
                command: self.command.clone(),
                source,
            },
            error @ (termination::TerminationError::Disposition { .. }
            | termination::TerminationError::AlreadyWaiting) => GitError::Start {
                command: self.command.clone(),
                source: std::io::Error::other(error.to_string()),
            },
        })
    }

    /// Inspect a normal, linked, unborn, detached, or bare repository.
    pub fn inspect_repository(&self, path: &Path) -> Result<RepositoryInfo, GitError> {
        self.inspect_repository_inner(path, false)
    }

    /// Inspect a repository and resolve HEAD's commit and tree together.
    ///
    /// Lifecycle callers that need both values use one structured `git show`
    /// instead of resolving the commit and then starting Git again for its
    /// tree. Callers that need only the commit keep the narrower
    /// [`Self::inspect_repository`] path.
    pub fn inspect_repository_with_head_tree(
        &self,
        path: &Path,
    ) -> Result<RepositoryInfo, GitError> {
        self.inspect_repository_inner(path, true)
    }

    fn inspect_repository_inner(
        &self,
        path: &Path,
        include_head_tree: bool,
    ) -> Result<RepositoryInfo, GitError> {
        // One invocation answers both identity questions; each Git subprocess
        // costs more in spawn and startup than in work. The bare flag is a
        // fixed `true`/`false` first line, so everything after it stays
        // parseable as one path even when that path contains newlines — which
        // is also why no second path query may join this call: two variable
        // paths in newline-separated output cannot be told apart.
        let output = match self.run(
            Some(path),
            &[
                "rev-parse",
                "--is-bare-repository",
                "--path-format=absolute",
                "--git-common-dir",
            ],
        ) {
            Ok(output) => output,
            Err(error) => {
                // Standing outside any repository is a caller mistake, usually
                // the wrong working directory, not a Git failure to inspect.
                // `repository_absent` forces the C locale, so the distinction
                // never depends on translated error text, and it deliberately
                // separates "no repository here" from "inside an unhealthy
                // one" — the latter must keep its original diagnostic. The
                // extra subprocess runs only on the failure path.
                if self.repository_absent(path).unwrap_or(false) {
                    return Err(GitError::RepositoryAbsent {
                        path: path.to_path_buf(),
                    });
                }
                return Err(error);
            }
        };
        let (flag, remainder) = {
            let bytes = &output.stdout;
            let newline = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or_else(|| GitError::InvalidOutput {
                    context: "repository identity",
                    detail: "expected a bare flag line and the common Git directory".to_owned(),
                })?;
            (&bytes[..newline], &bytes[newline + 1..])
        };
        let is_bare = match flag {
            b"true" => true,
            b"false" => false,
            value => {
                return Err(GitError::InvalidOutput {
                    context: "bare repository flag",
                    detail: format!(
                        "expected true or false, got {:?}",
                        String::from_utf8_lossy(value)
                    ),
                });
            }
        };
        // Mirror `run_path`: exactly one trailing newline belongs to Git, the
        // rest of the bytes belong to the path.
        let common_git_dir = remainder.strip_suffix(b"\n").unwrap_or(remainder);
        if common_git_dir.is_empty() {
            return Err(GitError::InvalidOutput {
                context: "common Git directory",
                detail: "path was empty".to_owned(),
            });
        }
        let common_git_dir =
            PathBuf::from(os_string_from_git(common_git_dir, "common Git directory")?);
        let root = if is_bare {
            None
        } else {
            Some(self.run_path(
                Some(path),
                &["rev-parse", "--path-format=absolute", "--show-toplevel"],
                "working-tree root",
            )?)
        };

        // Resolving HEAD's commit does double duty: it is the value `riftri
        // doctor` reports and the probe that surfaces a corrupt object store
        // as an inspection failure (relied on to fail lifecycle commands
        // closed on an unhealthy repository). Most callers do not need its
        // tree, while add can request both from one Git process.
        let (head_commit, head_tree) = if include_head_tree {
            match self.resolve_optional_head_revision(path)? {
                Some(resolved) => (Some(resolved.commit), Some(resolved.tree)),
                None => (None, None),
            }
        } else {
            (self.resolve_optional_object(path, "HEAD^{commit}")?, None)
        };
        Ok(RepositoryInfo {
            root,
            identity: RepositoryIdentity { common_git_dir },
            is_bare,
            head_commit,
            head_tree,
            // Deliberately not probed here: cleanliness costs a full
            // `git status` traversal, this runs on the path of every lifecycle
            // operation, and only `doctor` ever reads it. Callers that need it
            // ask through `inspect_repository_for_report`.
            clean: None,
        })
    }

    /// Inspect a repository and additionally probe working-tree cleanliness.
    ///
    /// Separate from [`Self::inspect_repository`] because that probe is a full
    /// `git status` traversal which every lifecycle operation would otherwise
    /// pay for and discard.
    /// Inspect a repository and additionally resolve the report-only fields
    /// that `inspect_repository` skips — working-tree cleanliness and HEAD's
    /// tree. Commit and tree resolution share one Git subprocess; cleanliness
    /// remains a separate full traversal used only by `riftri doctor`.
    pub fn inspect_repository_for_report(&self, path: &Path) -> Result<RepositoryInfo, GitError> {
        let mut info = self.inspect_repository_with_head_tree(path)?;
        if !info.is_bare {
            // `worktree_is_clean` passes `--untracked-files=all`, which
            // overrides a `status.showUntrackedFiles=no` that would otherwise
            // hide untracked content.
            info.clean = Some(self.worktree_is_clean(path)?);
        } else {
            // Preserve the report contract: cleanliness and tree are
            // worktree-only diagnostics even though the combined resolution
            // made the bare repository's tree available at no extra cost.
            info.head_tree = None;
        }
        Ok(info)
    }

    /// Resolve `revision` to exact commit and tree IDs using Git's semantics.
    pub fn resolve_revision(
        &self,
        path: &Path,
        revision: &OsStr,
    ) -> Result<ResolvedRevision, GitError> {
        // `git show` reads its argument as a revision walk, so it would list
        // the commits of a range (`A..B`, `A...B`) or a negation (`^A`)
        // rather than refuse them as `rev-parse --verify` and Git's own
        // `worktree add` do. Only those spellings take the strict two-process
        // path; the appended `^{commit}` already defeats `^@`, `^!` and `^-`.
        if must_resolve_before_peeling(revision) {
            let object = self.resolve_required_object(path, revision, "")?;
            let arguments = resolved_revision_arguments(OsStr::new(object.as_str()));
            let output = self.run_os(Some(path), &arguments)?;
            return parse_resolved_revision_output(&output.stdout);
        }
        if is_revision_walk(revision) {
            let commit = self.resolve_required_object(path, revision, "^{commit}")?;
            return self.resolve_commit_tree(path, commit);
        }
        let arguments = resolved_revision_arguments(revision);
        let output = self.run_os(Some(path), &arguments)?;
        parse_resolved_revision_output(&output.stdout)
    }

    /// Resolve the tree for a commit ID the caller already obtained from Git.
    ///
    /// Repository inspection resolves `HEAD^{commit}` both as a report value
    /// and as an object-store health check. Callers adding from that exact
    /// `HEAD` can reuse the validated commit while still asking Git for its
    /// tree, avoiding a duplicate commit lookup without trusting a ref name.
    pub fn resolve_commit_tree(
        &self,
        path: &Path,
        commit: ObjectId,
    ) -> Result<ResolvedRevision, GitError> {
        let tree = self.resolve_required_object(path, OsStr::new(commit.as_str()), "^{tree}")?;
        Ok(ResolvedRevision { commit, tree })
    }

    /// Resolve a revision the *caller* named, returning `Ok(None)` when that
    /// name resolves to nothing.
    ///
    /// `resolve_revision` reports every failure as a Git command failure, so a
    /// misspelled branch, or `HEAD` in a repository with no commits, reached
    /// the caller as an operational error that retrying might fix (#425).
    /// Telling the two apart follows `resolve_optional_object`: on the failure
    /// path only, probe the unpeeled name with `--verify --quiet`, which exits
    /// `1` with no output when the name resolves to nothing. If the name *does*
    /// resolve, peeling failed for another reason — it names a tree or blob, or
    /// its object is unreadable in a damaged store — and the original error is
    /// kept, so corruption is never reported as a caller mistake.
    ///
    /// Only for revisions the caller supplied. `HEAD` inside an existing
    /// worktree failing to resolve is a crash shape, not a typo, and keeps
    /// going through `resolve_revision`.
    pub fn resolve_requested_revision(
        &self,
        path: &Path,
        revision: &OsStr,
    ) -> Result<Option<ResolvedRevision>, GitError> {
        // A leading `^` is a negation, never a commit; even `rev-parse
        // --verify` accepts it and prints `^<oid>` back.
        if revision.as_encoded_bytes().first() == Some(&b'^') {
            return Ok(None);
        }
        match self.resolve_revision(path, revision) {
            Ok(resolved) => Ok(Some(resolved)),
            Err(error @ GitError::CommandFailed { .. }) => {
                let arguments = [
                    OsString::from("rev-parse"),
                    OsString::from("--verify"),
                    OsString::from("--quiet"),
                    OsString::from("--end-of-options"),
                    revision.to_os_string(),
                ];
                let probe = self.output_os(Some(path), &arguments)?;
                if probe.status.code() == Some(1) || self.names_a_non_commit(path, revision)? {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Whether `revision` resolves to an object that is not a commit even
    /// after peeling tags: a tree, a blob, or a tag of one. Like an unknown
    /// name, that is a caller mistake rather than a Git failure. Anything this
    /// probe cannot establish (a corrupt object store, say) answers `false`,
    /// keeping the caller's original error.
    fn names_a_non_commit(&self, path: &Path, revision: &OsStr) -> Result<bool, GitError> {
        let mut peeled = if must_resolve_before_peeling(revision) {
            match self.resolve_optional_object_os(path, revision)? {
                Some(object) => OsString::from(object.as_str()),
                None => return Ok(false),
            }
        } else {
            revision.to_os_string()
        };
        peeled.push("^{}");
        let object = self.output_os(
            Some(path),
            &[
                OsString::from("rev-parse"),
                OsString::from("--verify"),
                OsString::from("--quiet"),
                OsString::from("--end-of-options"),
                peeled,
            ],
        )?;
        if !object.status.success() {
            return Ok(false);
        }
        let object = String::from_utf8_lossy(&object.stdout).trim().to_owned();
        let kind = self.output_os(
            Some(path),
            &[
                OsString::from("cat-file"),
                OsString::from("-t"),
                OsString::from(object),
            ],
        )?;
        Ok(kind.status.success() && String::from_utf8_lossy(&kind.stdout).trim() != "commit")
    }

    /// Return Git's stable, NUL-delimited worktree inventory.
    ///
    /// `git worktree list` reads each worktree's administrative files one at a
    /// time and exits 128 when a concurrent `git worktree remove` deletes one
    /// in between ("failed to read '.git/worktrees/<name>/locked'"). Listing is
    /// read-only, so a failure is retried briefly before it is reported:
    /// otherwise any Riftri command could fail because another worktree was
    /// being removed at that moment.
    pub fn list_worktrees(&self, path: &Path) -> Result<Vec<WorktreeInfo>, GitError> {
        const ATTEMPTS: u32 = 4;
        let mut attempt = 1;
        loop {
            match self.run(Some(path), &["worktree", "list", "--porcelain", "-z"]) {
                Ok(output) => return parse_worktree_porcelain(&output.stdout),
                Err(GitError::CommandFailed { .. }) if attempt < ATTEMPTS => {
                    std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Resolve Git's unique path suffix before falling back to a filesystem path.
    pub fn resolve_worktree_path(
        &self,
        repository: &Path,
        selector: &OsStr,
    ) -> Result<PathBuf, GitError> {
        let path = repository.join(selector);
        let suffix = selector.as_encoded_bytes();
        if suffix.is_empty() {
            return Ok(path);
        }
        let config = self.run(
            Some(repository),
            &[
                "config",
                "--bool",
                "--default=false",
                "--get",
                "core.ignorecase",
            ],
        )?;
        let ignore_case = cfg!(windows) || trim_line_endings(&config.stdout) == b"true";
        let is_separator = |byte: u8| byte == b'/' || (cfg!(windows) && byte == b'\\');
        let worktrees = self.list_worktrees(repository)?;
        let mut matches = worktrees.into_iter().filter(|worktree| {
            let bytes = worktree.path.as_os_str().as_encoded_bytes();
            let Some(start) = bytes.len().checked_sub(suffix.len()) else {
                return false;
            };
            (start == 0 || is_separator(bytes[start - 1]))
                && bytes[start..].iter().zip(suffix).all(|(left, right)| {
                    left == right
                        || (ignore_case && left.eq_ignore_ascii_case(right))
                        || (cfg!(windows) && is_separator(*left) && is_separator(*right))
                })
        });
        match (matches.next(), matches.next()) {
            (Some(worktree), None) => Ok(worktree.path),
            // Git tries the literal path when the suffix is absent or ambiguous.
            _ => Ok(path),
        }
    }

    /// Resolve an explicit Git directory to one live non-bare worktree root.
    ///
    /// A linked worktree's Git directory does not sit below that worktree, so
    /// callers must use Git's own inventory instead of inferring a root from
    /// the administrative directory path.
    pub fn worktree_root_from_git_dir(&self, git_dir: &Path) -> Result<Option<PathBuf>, GitError> {
        let arguments = [
            OsString::from("--git-dir"),
            git_path_argument(git_dir),
            OsString::from("worktree"),
            OsString::from("list"),
            OsString::from("--porcelain"),
            OsString::from("-z"),
        ];
        let output = self.run_os(None, &arguments)?;
        let worktrees = parse_worktree_porcelain(&output.stdout)?;
        if worktrees.iter().all(|worktree| worktree.bare) {
            return Ok(None);
        }
        worktrees
            .into_iter()
            .find(|worktree| !worktree.bare && worktree.path.is_dir())
            .map(|worktree| worktree.path)
            .map(Some)
            .ok_or_else(|| GitError::InvalidOutput {
                context: "Git worktree inventory",
                detail: format!(
                    "{} did not identify a live non-bare worktree",
                    git_dir.display()
                ),
            })
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

    /// Return the uncompressed size of one exact blob object.
    pub fn blob_size(&self, path: &Path, object: &ObjectId) -> Result<u64, GitError> {
        let size = self.run_text(
            Some(path),
            &["cat-file", "-s", object.as_str()],
            "blob size",
        )?;
        size.parse::<u64>()
            .map_err(|error| GitError::InvalidOutput {
                context: "blob size",
                detail: error.to_string(),
            })
    }

    /// Read the exact bytes of one blob object without consulting checkout
    /// filters or the mutable working tree.
    pub fn read_blob(&self, path: &Path, object: &ObjectId) -> Result<Vec<u8>, GitError> {
        let arguments = [
            OsString::from("cat-file"),
            OsString::from("blob"),
            OsString::from(object.as_str()),
        ];
        Ok(self.run_os(Some(path), &arguments)?.stdout)
    }

    /// Read sizes without reading blob bodies. Exact IDs keep this protocol
    /// independent of filenames; preserve request order and duplicate IDs.
    pub fn blob_sizes(&self, path: &Path, objects: &[ObjectId]) -> Result<Vec<u64>, GitError> {
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let input = objects
            .iter()
            .map(|object| format!("{}\n", object.as_str()))
            .collect::<String>();
        let output = self.run_os_with_input(
            Some(path),
            &[OsString::from("cat-file"), OsString::from("--batch-check")],
            &[],
            input.as_bytes(),
        )?;
        parse_blob_sizes(&output.stdout, objects)
    }

    /// Read small exact blobs through one Git process. Validate each header
    /// before allocating/reading its body. Requests are sequential so neither
    /// pipe can fill while the other end waits; no checkout filters are run.
    pub fn read_small_blobs(
        &self,
        path: &Path,
        objects: &[ObjectId],
        max_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, GitError> {
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let mut reader = self.small_blob_reader(path)?;
        let blobs = reader.read(objects, max_bytes)?;
        reader.finish()?;
        Ok(blobs)
    }

    /// Start a reader reusable across bounded batches within one operation.
    pub fn small_blob_reader(&self, path: &Path) -> Result<SmallBlobReader, GitError> {
        let stderr = tempfile::tempfile().map_err(|source| GitError::TemporaryState { source })?;
        let arguments = [OsString::from("cat-file"), OsString::from("--batch")];
        let mut command = Command::new(&self.command);
        command
            .args(&arguments)
            .current_dir(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A file avoids a full stderr pipe deadlocking a response read.
            .stderr(
                stderr
                    .try_clone()
                    .map_err(|source| GitError::TemporaryState { source })?,
            );
        #[cfg(test)]
        self.process_attempts.fetch_add(1, Ordering::Relaxed);
        let mut child = command
            .spawn()
            .map_err(|source| self.start_error(Some(path), source))?;
        let input = child.stdin.take();
        let output = child.stdout.take();
        let (Some(input), Some(output)) = (input, output) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid_blob_batch("missing child pipes"));
        };
        Ok(SmallBlobReader {
            command: self.command.clone(),
            child: Some(child),
            input: Some(input),
            output: std::io::BufReader::new(output),
            stderr,
        })
    }

    /// Return the installed Git LFS version line, or `None` when the standard
    /// `git lfs version` command is unavailable or unhealthy.
    pub fn lfs_version(&self, path: &Path) -> Result<Option<Vec<u8>>, GitError> {
        let arguments = [OsString::from("lfs"), OsString::from("version")];
        let output = self.output_os(Some(path), &arguments)?;
        if !output.status.success() {
            return Ok(None);
        }
        let version = trim_line_endings(&output.stdout);
        if version.is_empty() {
            return Err(GitError::InvalidOutput {
                context: "git lfs version",
                detail: "version output was empty".to_owned(),
            });
        }
        Ok(Some(version.to_vec()))
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

    /// Read every conditional target, including conditions that do not match.
    /// Nested includes and unreadable targets cannot satisfy the key allowlist.
    pub fn conditional_config_has_only(
        &self,
        path: &Path,
        allowed_keys: &[&str],
    ) -> Result<bool, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--show-origin"),
            OsString::from("--path"),
            OsString::from("--get-regexp"),
            OsString::from(r"^includeif\..*\.path$"),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(true);
        }
        if !output.status.success() {
            return Err(command_failed(&arguments, &output));
        }
        let mut records = output.stdout.split(|byte| *byte == 0);
        while let Some(origin) = records.next().filter(|record| !record.is_empty()) {
            let Some(value) = records
                .next()
                .and_then(|record| record.splitn(2, |byte| *byte == b'\n').nth(1))
            else {
                return Err(GitError::InvalidOutput {
                    context: "conditional configuration",
                    detail: "missing include path".to_owned(),
                });
            };
            let mut target = PathBuf::from(os_string_from_git(value, "include path")?);
            if !target.is_absolute() {
                let Some(origin) = origin.strip_prefix(b"file:") else {
                    return Ok(false);
                };
                let origin = PathBuf::from(os_string_from_git(origin, "configuration origin")?);
                let Some(parent) = origin.parent() else {
                    return Ok(false);
                };
                target = parent.join(target);
            }
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                return Ok(false);
            };
            let arguments = [
                OsString::from("config"),
                OsString::from("--file"),
                git_path_argument(&Path::new(".").join(name)),
                OsString::from("--no-includes"),
                OsString::from("--null"),
                OsString::from("--name-only"),
                OsString::from("--list"),
            ];
            let output = self.output_os(Some(&path.join(parent)), &arguments)?;
            if !output.status.success()
                || output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|key| !key.is_empty())
                    .any(|key| {
                        !allowed_keys
                            .iter()
                            .any(|allowed| key.eq_ignore_ascii_case(allowed.as_bytes()))
                    })
            {
                return Ok(false);
            }
        }
        Ok(true)
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

    /// Expand one captured configuration value using Git's pathname rules.
    ///
    /// Path-valued configuration can use forms such as `~/hooks` and
    /// `%(prefix)/hooks`. Let Git expand those forms so callers do not need to
    /// duplicate platform- and installation-specific parsing. Passing the
    /// captured value back through a command-line configuration entry avoids
    /// re-reading a value that could have changed since the caller's snapshot.
    pub fn expand_config_path(&self, value: &OsStr) -> Result<PathBuf, GitError> {
        let mut setting = OsString::from("riftri.path=");
        setting.push(value);
        let arguments = [
            OsString::from("-c"),
            setting,
            OsString::from("config"),
            OsString::from("--path"),
            OsString::from("--null"),
            OsString::from("--get"),
            OsString::from("riftri.path"),
        ];
        let output = self.run_os(None, &arguments)?;
        let value = output.stdout.strip_suffix(&[0]).unwrap_or(&output.stdout);
        Ok(PathBuf::from(os_string_from_git(
            value,
            "path configuration",
        )?))
    }

    /// Read simple `section.variable` keys and detect conditional includes in one
    /// Git process, with normal precedence and the same raw values as `config_value`.
    ///
    /// Section and variable names are case-insensitive and returned lowercase.
    /// Subsection names keep their case in selectors and returned keys.
    /// This accepts a conservative ASCII subset, including subsection keys
    /// such as `filter.Mixed.clean`. Missing keys are absent. Empty values and
    /// implicit booleans are present with empty bytes, as with `--get`.
    /// The result is operation-local: no answers are cached between calls.
    pub fn config_values(&self, path: &Path, keys: &[&str]) -> Result<ConfigValues, GitError> {
        if keys.is_empty() {
            return Ok(ConfigValues::default());
        }
        let keys = keys
            .iter()
            .map(|key| normalize_config_key(key))
            .collect::<Result<Vec<_>, _>>()?;
        // Validation above excludes regexp metacharacters other than the one
        // separating dot. Anchor the allowlist so unrelated settings cannot match.
        let pattern = format!(
            r"^({}|includeif\..*\.path)$",
            keys.iter()
                .map(|key| key.replace('.', r"\."))
                .collect::<Vec<_>>()
                .join("|")
        );
        let arguments = [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--get-regexp"),
            OsString::from(pattern),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            parse_config_values(&output.stdout, &keys)
        } else if output.status.code() == Some(1) && output.stdout.is_empty() {
            Ok(ConfigValues::default())
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Read every source contributing to one effective configuration key.
    ///
    /// Git returns records in precedence order. This is intended for
    /// failure-only diagnostics: normal configuration reads should continue
    /// using `config_values` so successful operations need only one process.
    pub fn config_value_origins(
        &self,
        path: &Path,
        key: &str,
    ) -> Result<Vec<ConfigValueOrigin>, GitError> {
        let key = normalize_config_key(key)?;
        let arguments = [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--show-origin"),
            OsString::from("--show-scope"),
            OsString::from("--get-all"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            parse_config_value_origins(&output.stdout)
        } else if output.status.code() == Some(1) && output.stdout.is_empty() {
            Ok(Vec::new())
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

    /// Read every repository-local path value using Git's path parser while
    /// preserving native path units.
    pub fn local_config_paths(&self, path: &Path, key: &str) -> Result<Vec<PathBuf>, GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--null"),
            OsString::from("--path"),
            OsString::from("--get-all"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    os_string_from_git(value, "repository-local path configuration")
                        .map(PathBuf::from)
                })
                .collect()
        } else if output.status.code() == Some(1) {
            Ok(Vec::new())
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

    /// Append one repository-local path value through Git.
    pub fn add_local_config_path(
        &self,
        path: &Path,
        key: &str,
        value: &Path,
    ) -> Result<(), GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--add"),
            OsString::from(key),
            value.as_os_str().to_os_string(),
        ];
        self.run_os(Some(path), &arguments)?;
        Ok(())
    }

    /// Remove one exact repository-local configuration value while preserving
    /// other values for the same key.
    pub fn unset_local_config_value(
        &self,
        path: &Path,
        key: &str,
        value: &OsStr,
    ) -> Result<(), GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--fixed-value"),
            OsString::from("--unset-all"),
            OsString::from(key),
            value.to_os_string(),
        ];
        self.run_os(Some(path), &arguments)?;
        Ok(())
    }

    /// Remove a repository-local configuration key. Missing keys are accepted.
    pub fn unset_local_config(&self, path: &Path, key: &str) -> Result<(), GitError> {
        let arguments = [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--unset-all"),
            OsString::from(key),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() || output.status.code() == Some(5) {
            // `git config --unset-all` exits 5 when the key has no matching
            // value. Accept that documented missing-key disposition directly
            // instead of probing the key in a separate process first.
            Ok(())
        } else {
            Err(command_failed(&arguments, &output))
        }
    }

    /// Resolve the repository-specific attributes file through Git so linked
    /// worktrees use the correct common Git directory.
    pub fn info_attributes_path(&self, path: &Path) -> Result<PathBuf, GitError> {
        let common_git_dir = self.run_path(
            Some(path),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "common Git directory",
        )?;
        // Git's absolute --git-path can resolve a symlink at the final path,
        // hiding it from callers that need to inspect attributes without following it.
        Ok(common_git_dir.join("info/attributes"))
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
        self.attributes_for_paths_with_environment(path, paths, &[], &[])
    }

    /// Load an exact tree into a private temporary index once so multiple
    /// attribute queries can share it instead of re-running `git read-tree`.
    ///
    /// `read-tree` runs with only `GIT_INDEX_FILE` set, exactly as each
    /// attribute query previously ran it for its own private index; attribute
    /// isolation is applied per query by `check-attr`, which only reads the
    /// index, so sharing one index cannot leak state between queries.
    pub fn tree_attribute_index(
        &self,
        path: &Path,
        tree: &ObjectId,
    ) -> Result<TreeAttributeIndex, GitError> {
        let temporary = tempfile::Builder::new()
            .prefix("riftri-attributes-")
            .tempdir()
            .map_err(|source| GitError::TemporaryState { source })?;
        let index = temporary.path().join("index");
        let index_environment = [(OsStr::new("GIT_INDEX_FILE"), index.as_os_str())];
        let read_tree = [OsString::from("read-tree"), OsString::from(tree.as_str())];
        self.run_os_with_env(Some(path), &read_tree, &index_environment)?;
        Ok(TreeAttributeIndex {
            _temporary: temporary,
            index,
        })
    }

    /// Return attributes for an exact tree using Git's normal external
    /// attribute precedence.
    pub fn effective_attributes_for_tree_paths(
        &self,
        path: &Path,
        tree: &ObjectId,
        paths: &[PathBuf],
    ) -> Result<Vec<GitAttribute>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let index = self.tree_attribute_index(path, tree)?;
        self.effective_attributes_for_index(path, &index, paths)
    }

    /// Return attributes from an exact tree while disabling global and system
    /// attribute files. Callers must separately reject `.git/info/attributes`,
    /// which Git intentionally gives highest precedence and cannot disable.
    pub fn in_tree_attributes_for_paths(
        &self,
        path: &Path,
        tree: &ObjectId,
        paths: &[PathBuf],
    ) -> Result<Vec<GitAttribute>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let index = self.tree_attribute_index(path, tree)?;
        self.in_tree_attributes_for_index(path, &index, paths)
    }

    /// Return attributes for an already indexed tree using Git's normal
    /// external attribute precedence.
    pub fn effective_attributes_for_index(
        &self,
        path: &Path,
        index: &TreeAttributeIndex,
        paths: &[impl AsRef<Path>],
    ) -> Result<Vec<GitAttribute>, GitError> {
        let input = attribute_stdin(paths);
        self.effective_attributes_for_index_with_input(path, index, paths, input.as_deref())
    }

    fn effective_attributes_for_index_with_input(
        &self,
        path: &Path,
        index: &TreeAttributeIndex,
        paths: &[impl AsRef<Path>],
        input: Option<&[u8]>,
    ) -> Result<Vec<GitAttribute>, GitError> {
        let index_environment = [(OsStr::new("GIT_INDEX_FILE"), index.index.as_os_str())];
        self.attributes_with_input(path, paths, &[], &index_environment, input)
    }

    /// Return attributes from an already indexed tree while disabling global
    /// and system attribute files. Callers must separately reject
    /// `.git/info/attributes`, which Git intentionally gives highest
    /// precedence and cannot disable.
    pub fn in_tree_attributes_for_index(
        &self,
        path: &Path,
        index: &TreeAttributeIndex,
        paths: &[impl AsRef<Path>],
    ) -> Result<Vec<GitAttribute>, GitError> {
        let input = attribute_stdin(paths);
        self.in_tree_attributes_for_index_with_input(path, index, paths, input.as_deref())
    }

    /// Query isolated in-tree attributes, then effective attributes, reusing
    /// only the immutable encoded path input. Environments remain separate.
    /// Skip the isolated query only when the caller proved the tree has no
    /// `.gitattributes`; the effective query must still detect external rules.
    pub fn attribute_pair_for_index(
        &self,
        path: &Path,
        index: &TreeAttributeIndex,
        paths: &[impl AsRef<Path>],
        include_in_tree: bool,
    ) -> Result<(Vec<GitAttribute>, Vec<GitAttribute>), GitError> {
        let input = attribute_stdin(paths);
        let in_tree = if include_in_tree {
            self.in_tree_attributes_for_index_with_input(path, index, paths, input.as_deref())?
        } else {
            Vec::new()
        };
        let effective =
            self.effective_attributes_for_index_with_input(path, index, paths, input.as_deref())?;
        Ok((in_tree, effective))
    }

    fn in_tree_attributes_for_index_with_input(
        &self,
        path: &Path,
        index: &TreeAttributeIndex,
        paths: &[impl AsRef<Path>],
        input: Option<&[u8]>,
    ) -> Result<Vec<GitAttribute>, GitError> {
        #[cfg(unix)]
        let null_device = OsStr::new("/dev/null");
        #[cfg(not(unix))]
        let null_device = OsStr::new("NUL");
        let mut attributes_override = OsString::from("core.attributesFile=");
        attributes_override.push(null_device);
        let arguments = [OsString::from("-c"), attributes_override];
        let environment = [
            (OsStr::new("GIT_INDEX_FILE"), index.index.as_os_str()),
            (OsStr::new("GIT_ATTR_NOSYSTEM"), OsStr::new("1")),
            (OsStr::new("GIT_CONFIG_NOSYSTEM"), OsStr::new("1")),
            (OsStr::new("GIT_CONFIG_GLOBAL"), null_device),
            (OsStr::new("GIT_CONFIG_SYSTEM"), null_device),
        ];
        self.attributes_with_input(path, paths, &arguments, &environment, input)
    }

    fn attributes_for_paths_with_environment(
        &self,
        path: &Path,
        paths: &[impl AsRef<Path>],
        argument_prefix: &[OsString],
        environment: &[(&OsStr, &OsStr)],
    ) -> Result<Vec<GitAttribute>, GitError> {
        let input = attribute_stdin(paths);
        self.attributes_with_input(path, paths, argument_prefix, environment, input.as_deref())
    }

    fn attributes_with_input(
        &self,
        path: &Path,
        paths: &[impl AsRef<Path>],
        argument_prefix: &[OsString],
        environment: &[(&OsStr, &OsStr)],
        input: Option<&[u8]>,
    ) -> Result<Vec<GitAttribute>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(input) = input {
            let mut arguments = argument_prefix.to_vec();
            arguments.extend([
                OsString::from("check-attr"),
                OsString::from("--cached"),
                OsString::from("--all"),
                OsString::from("-z"),
                OsString::from("--stdin"),
            ]);
            let output = self.run_os_with_input(Some(path), &arguments, environment, input)?;
            parse_attribute_records(&output.stdout)
        } else {
            // Preserve the native-argument path for non-Unicode Windows input
            // instead of replacing unpaired UTF-16 surrogates lossily. Exact
            // tree paths decoded from Git are UTF-8 and use the fast path.
            self.attributes_for_paths_as_arguments(path, paths, argument_prefix, environment)
        }
    }

    fn attributes_for_paths_as_arguments(
        &self,
        path: &Path,
        paths: &[impl AsRef<Path>],
        argument_prefix: &[OsString],
        environment: &[(&OsStr, &OsStr)],
    ) -> Result<Vec<GitAttribute>, GitError> {
        let mut attributes = Vec::new();
        for chunk in paths.chunks(128) {
            let mut arguments = argument_prefix.to_vec();
            arguments.extend([
                OsString::from("check-attr"),
                OsString::from("--cached"),
                OsString::from("--all"),
                OsString::from("-z"),
                OsString::from("--"),
            ]);
            arguments.extend(
                chunk
                    .iter()
                    .map(|entry| entry.as_ref().as_os_str().to_os_string()),
            );
            let output = self.run_os_with_env(Some(path), &arguments, environment)?;
            attributes.extend(parse_attribute_records(&output.stdout)?);
        }
        Ok(attributes)
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
        self.materialize_tree_with_config(repository, tree, destination, temporary_index, &[])
    }

    /// Materialize using captured checkout configuration and only attributes
    /// from the exact tree. Mutable repository/global attributes and filters
    /// cannot enter this private Git administrative directory.
    pub fn materialize_tree_with_config(
        &self,
        repository: &Path,
        tree: &ObjectId,
        destination: &Path,
        temporary_index: &Path,
        configuration: &[(String, Vec<u8>)],
    ) -> Result<(), GitError> {
        self.materialize_tree_inner(
            repository,
            tree,
            destination,
            temporary_index,
            configuration,
            &[],
        )
    }

    /// Materialize a cone-mode sparse view of the exact tree. Git's own
    /// `sparse-checkout set --cone` computes the sparse patterns and
    /// skip-worktree bits inside the isolated administrative directory, and
    /// `checkout-index` then writes only the entries Git left active.
    pub fn materialize_sparse_tree_with_config(
        &self,
        repository: &Path,
        tree: &ObjectId,
        destination: &Path,
        temporary_index: &Path,
        configuration: &[(String, Vec<u8>)],
        sparse_directories: &[String],
    ) -> Result<(), GitError> {
        self.materialize_tree_inner(
            repository,
            tree,
            destination,
            temporary_index,
            configuration,
            sparse_directories,
        )
    }

    fn materialize_tree_inner(
        &self,
        repository: &Path,
        tree: &ObjectId,
        destination: &Path,
        temporary_index: &Path,
        configuration: &[(String, Vec<u8>)],
        sparse_directories: &[String],
    ) -> Result<(), GitError> {
        let output = self.run(
            Some(repository),
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
                "--show-object-format",
            ],
        )?;
        let (objects, format) = parse_checkout_storage(&output.stdout)?;
        let isolated = tempfile::Builder::new()
            .prefix("riftri-checkout-")
            .tempdir()
            .map_err(|source| GitError::TemporaryState { source })?;
        let git_dir = isolated.path().join("git");
        let template = isolated.path().join("empty-template");
        std::fs::create_dir(&template).map_err(|source| GitError::TemporaryState { source })?;
        let temporary_index = git_path_argument(temporary_index);
        let work_tree = git_path_argument(destination);
        #[cfg(unix)]
        let null_device = OsStr::new("/dev/null");
        #[cfg(not(unix))]
        let null_device = OsStr::new("NUL");
        let environment = [
            (OsStr::new("GIT_DIR"), git_dir.as_os_str()),
            (OsStr::new("GIT_WORK_TREE"), work_tree.as_os_str()),
            (OsStr::new("GIT_INDEX_FILE"), temporary_index.as_os_str()),
            (OsStr::new("GIT_OBJECT_DIRECTORY"), objects.as_os_str()),
            (OsStr::new("GIT_ATTR_NOSYSTEM"), OsStr::new("1")),
            (OsStr::new("GIT_CONFIG_GLOBAL"), null_device),
            (OsStr::new("GIT_CONFIG_SYSTEM"), null_device),
            (OsStr::new("GIT_CONFIG_NOSYSTEM"), OsStr::new("1")),
        ];
        let mut prefix = Vec::new();
        for setting in [
            "core.autocrlf=false",
            "core.symlinks=true",
            "core.ignorecase=false",
            "core.precomposeunicode=false",
        ] {
            prefix.extend([OsString::from("-c"), OsString::from(setting)]);
        }
        for (key, value) in configuration {
            let mut setting = OsString::from(format!("{key}="));
            setting.push(os_string_from_git(
                value,
                "captured checkout configuration",
            )?);
            prefix.extend([OsString::from("-c"), setting]);
        }
        let mut attributes = OsString::from("core.attributesFile=");
        attributes.push(null_device);
        prefix.extend([OsString::from("-c"), attributes]);
        let run = |arguments: &[OsString]| -> Result<(), GitError> {
            let mut command = Command::new(&self.command);
            // Remove all inherited Git overrides, including config injection,
            // worktree/attribute sources, and another repository's common dir.
            for (name, _) in std::env::vars_os() {
                if name
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("GIT_")
                {
                    command.env_remove(name);
                }
            }
            command
                .args(&prefix)
                .args(arguments)
                .envs(environment.iter().copied())
                .current_dir(destination);
            if arguments.first().is_some_and(|argument| argument == "init") {
                for name in [
                    "GIT_DIR",
                    "GIT_WORK_TREE",
                    "GIT_INDEX_FILE",
                    "GIT_OBJECT_DIRECTORY",
                ] {
                    command.env_remove(name);
                }
            }
            let output = command
                .output()
                .map_err(|source| self.start_error(Some(destination), source))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(command_failed(arguments, &output))
            }
        };
        let mut template_argument = OsString::from("--template=");
        template_argument.push(git_path_argument(&template));
        run(&[
            OsString::from("init"),
            OsString::from("--bare"),
            OsString::from("--quiet"),
            OsString::from(format!("--object-format={format}")),
            template_argument,
            git_path_argument(&git_dir),
        ])?;
        let read_tree = [OsString::from("read-tree"), OsString::from(tree.as_str())];
        run(&read_tree)?;

        if !sparse_directories.is_empty() {
            // Real Git computes the cone patterns and applies skip-worktree
            // bits to the isolated index; checkout-index below honors them.
            let mut sparse = vec![
                OsString::from("sparse-checkout"),
                OsString::from("set"),
                OsString::from("--cone"),
                OsString::from("--"),
            ];
            sparse.extend(sparse_directories.iter().map(OsString::from));
            run(&sparse)?;
        }

        let mut prefix = git_path_argument(destination);
        prefix.push(std::path::MAIN_SEPARATOR.to_string());
        let checkout = [
            OsString::from("checkout-index"),
            OsString::from("--all"),
            OsString::from("--force"),
            OsString::from("--prefix"),
            prefix,
        ];
        run(&checkout)?;
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
        let revision = match head {
            WorktreeHead::NewBranch(branch) => {
                arguments.push(OsString::from("-b"));
                arguments.push(branch.to_os_string());
                revision
            }
            WorktreeHead::ExistingBranch(branch) => branch,
            WorktreeHead::Detached => {
                arguments.push(OsString::from("--detach"));
                revision
            }
        };
        arguments.push(git_path_argument(destination));
        arguments.push(revision.to_os_string());
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Stable, read-only Git state for forced-removal consent. Keep structured
    /// index entries rather than stat-cache bytes, which ordinary status may refresh.
    pub fn worktree_removal_state(&self, worktree: &Path) -> Result<Vec<u8>, GitError> {
        let mut state = Vec::new();
        for arguments in [
            &["rev-parse", "--verify", "HEAD"][..],
            &["ls-files", "--stage", "-v", "--full-name", "-z"],
            &[
                "diff",
                "--cached",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-color",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                // Omitting intent-to-add entries distinguishes them from
                // fully staged empty blobs (ls-files shows the same object ID).
                "--ita-invisible-in-index",
                "HEAD",
                "--",
            ],
        ] {
            let output = self.run(Some(worktree), arguments)?;
            state.extend_from_slice(&(output.stdout.len() as u64).to_le_bytes());
            state.extend_from_slice(&output.stdout);
        }
        let arguments = ["symbolic-ref", "--quiet", "HEAD"];
        let symbolic = self.output(Some(worktree), &arguments)?;
        if !symbolic.status.success() && symbolic.status.code() != Some(1) {
            return Err(command_failed(&arguments.map(OsString::from), &symbolic));
        }
        state.extend_from_slice(&(symbolic.stdout.len() as u64).to_le_bytes());
        state.extend_from_slice(&symbolic.stdout);
        Ok(state)
    }

    /// Inspect staged state without confusing a not-yet-created index with
    /// staged deletion of the whole tree. Both intent-to-add representations
    /// are compared so an empty tracked blob cannot hide an intent-only entry.
    pub fn worktree_index_has_changes(&self, worktree: &Path) -> Result<bool, GitError> {
        let index = self.worktree_index_path(worktree)?;
        match std::fs::symlink_metadata(&index) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(GitError::TemporaryState { source }),
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(GitError::InvalidOutput {
                    context: "worktree index",
                    detail: "index is not a regular file".to_owned(),
                });
            }
            Ok(_) => {}
        }
        for intent in ["--ita-visible-in-index", "--ita-invisible-in-index"] {
            let args = [
                "diff",
                "--cached",
                "--quiet",
                "--no-ext-diff",
                "--no-textconv",
                intent,
                "HEAD",
                "--",
            ];
            let output = self.output(Some(worktree), &args)?;
            match output.status.code() {
                Some(0) => {}
                Some(1) => return Ok(true),
                _ => return Err(command_failed(&args.map(OsString::from), &output)),
            }
        }
        Ok(false)
    }

    fn worktree_index_path(&self, worktree: &Path) -> Result<PathBuf, GitError> {
        self.run_path(
            Some(worktree),
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            "worktree index path",
        )
    }

    /// Build a missing index with real Git in a temporary file, then install
    /// it without replacing any index a user or another Git process created.
    /// Recovery must never reset an already populated worktree index.
    pub fn initialize_missing_worktree_index(
        &self,
        worktree: &Path,
        sparse_directories: &[String],
    ) -> Result<bool, GitError> {
        let index = self.worktree_index_path(worktree)?;
        match std::fs::symlink_metadata(&index) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(GitError::TemporaryState { source }),
        }
        let parent = index.parent().ok_or_else(|| GitError::InvalidOutput {
            context: "worktree index path",
            detail: "index has no parent directory".to_owned(),
        })?;
        let temporary = tempfile::Builder::new()
            .prefix("riftri-recovery-index-")
            .tempdir_in(parent)
            .map_err(|source| GitError::TemporaryState { source })?;
        let temporary_index = temporary.path().join("index");
        let environment = [(OsStr::new("GIT_INDEX_FILE"), temporary_index.as_os_str())];
        let run = |args: &[&str]| {
            self.run_os_with_env(
                Some(worktree),
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                &environment,
            )
        };
        if !sparse_directories.is_empty() {
            let mut args = vec!["sparse-checkout", "set", "--cone", "--"];
            args.extend(sparse_directories.iter().map(String::as_str));
            run(&args)?;
        }
        run(&["reset", "--mixed", "--quiet", "HEAD"])?;
        if !sparse_directories.is_empty() {
            run(&["sparse-checkout", "reapply"])?;
        }
        // Windows FlushFileBuffers requires a handle opened for writing.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary_index)
            .and_then(|file| file.sync_all())
            .map_err(|source| GitError::TemporaryState { source })?;
        let temporary_index = tempfile::TempPath::try_from_path(temporary_index)
            .map_err(|source| GitError::TemporaryState { source })?;
        match temporary_index.persist_noclobber(&index) {
            Ok(()) => Ok(true),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(GitError::TemporaryState {
                source: error.error,
            }),
        }
    }

    /// Populate the linked worktree index from HEAD without writing files.
    ///
    /// SAFETY ARGUMENT: this intentionally issues no separate
    /// `update-index --refresh`. The staged index contents are fully
    /// determined by `reset --mixed HEAD`; a refresh only rewrites cached
    /// stat data and reports paths whose contents differ. Every caller pairs
    /// this call with `worktree_is_clean`, whose `git status` performs the
    /// same full stat-and-content comparison against the freshly written
    /// index and fails closed on any divergence, so corruption detection and
    /// the clean-creation guarantee are unchanged while each add saves one
    /// Git process and one full worktree traversal.
    pub fn synchronize_worktree_index(&self, worktree: &Path) -> Result<(), GitError> {
        self.run(Some(worktree), &["reset", "--mixed", "--quiet", "HEAD"])?;
        Ok(())
    }

    /// Enable per-worktree cone sparse checkout for the listed directories and
    /// populate the linked worktree index from HEAD. `sparse-checkout set`
    /// stores the sparse configuration in the worktree-scoped Git
    /// configuration, exactly as running the command by hand would, and
    /// `sparse-checkout reapply` restores the skip-worktree bits after the
    /// index is rebuilt.
    /// Read the sparse-checkout state Git would copy into a new worktree.
    ///
    /// `git worktree add` inherits the current worktree's sparse cone, so an
    /// add issued from inside a sparse worktree produces a sparse worktree
    /// even though nothing on the command line asked for one. Riftri reads the
    /// same state in order to reproduce that behavior deliberately, with an
    /// immutable-base key that matches what it actually materializes.
    ///
    /// Only cone mode reports directories. Non-cone sparse checkouts are
    /// reported as enabled without a directory list, leaving the caller to
    /// refuse rather than guess at pattern semantics.
    pub fn sparse_checkout_state(&self, worktree: &Path) -> Result<SparseCheckoutState, GitError> {
        let config = self.config_values(
            worktree,
            &["core.sparsecheckout", "core.sparsecheckoutcone"],
        )?;
        self.sparse_checkout_state_with_config(worktree, &config)
    }

    /// Read sparse-checkout state from an operation-local configuration
    /// snapshot that the caller already needs for checkout compatibility.
    ///
    /// Keeping the directory-list query here preserves Git as the source of
    /// truth for active cone selections while avoiding a second configuration
    /// process on callers that captured the relevant keys together.
    pub fn sparse_checkout_state_with_config(
        &self,
        worktree: &Path,
        config: &ConfigValues,
    ) -> Result<SparseCheckoutState, GitError> {
        let enabled_value = |key: &str| {
            config
                .values
                .get(key)
                .is_some_and(|value| value.eq_ignore_ascii_case(b"true"))
        };
        if !enabled_value("core.sparsecheckout") {
            return Ok(SparseCheckoutState::default());
        }
        if !enabled_value("core.sparsecheckoutcone") {
            return Ok(SparseCheckoutState {
                enabled: true,
                cone: false,
                directories: Vec::new(),
            });
        }
        let output = self.run(Some(worktree), &["sparse-checkout", "list"])?;
        let mut directories = Vec::new();
        for line in output.stdout.split(|byte| *byte == b'\n') {
            let line = trim_line_endings(line);
            if line.is_empty() {
                continue;
            }
            directories.push(
                std::str::from_utf8(line)
                    .map_err(|error| GitError::InvalidOutput {
                        context: "git sparse-checkout list",
                        detail: error.to_string(),
                    })?
                    .to_owned(),
            );
        }
        Ok(SparseCheckoutState {
            enabled: true,
            cone: true,
            directories,
        })
    }

    pub fn synchronize_sparse_worktree_index(
        &self,
        worktree: &Path,
        sparse_directories: &[String],
    ) -> Result<(), GitError> {
        let mut arguments = vec!["sparse-checkout", "set", "--cone", "--"];
        arguments.extend(sparse_directories.iter().map(String::as_str));
        self.run(Some(worktree), &arguments)?;
        self.run(Some(worktree), &["reset", "--mixed", "--quiet", "HEAD"])?;
        self.run(Some(worktree), &["sparse-checkout", "reapply"])?;
        // As with a full worktree, the caller's mandatory clean `git status`
        // performs the stat-and-content refresh and fails closed on any
        // divergence. The sparse commands above still establish the exact
        // cone configuration and skip-worktree bits.
        Ok(())
    }

    /// Refresh index stat data without changing staged entries or worktree files.
    /// The branch `HEAD` names, without its `refs/heads/` prefix, or `None`
    /// for a detached `HEAD`.
    pub fn symbolic_head_branch(&self, path: &Path) -> Result<Option<Vec<u8>>, GitError> {
        let arguments = ["symbolic-ref", "--quiet", "HEAD"];
        let output = self.output(Some(path), &arguments)?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(command_failed(&arguments.map(OsString::from), &output));
        }
        let name = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        Ok(Some(
            name.strip_prefix(b"refs/heads/").unwrap_or(name).to_vec(),
        ))
    }

    /// Whether the repository has any reference at all, which tells an empty
    /// repository apart from one whose current branch is merely unborn.
    pub fn has_any_reference(&self, path: &Path) -> Result<bool, GitError> {
        let output = self.run(Some(path), &["for-each-ref", "--count=1", "--format=x"])?;
        Ok(!output.stdout.is_empty())
    }

    pub fn refresh_worktree_index(&self, worktree: &Path) -> Result<(), GitError> {
        match self.run(Some(worktree), &["update-index", "-q", "--refresh"]) {
            Ok(_) => Ok(()),
            // `-q` also silences Git's own "Unable to create index.lock"
            // report, which would otherwise leave a bare exit code.
            Err(error @ GitError::CommandFailed { .. }) => {
                match self.worktree_index_path(worktree) {
                    Ok(index) => {
                        let mut lock = index.into_os_string();
                        lock.push(".lock");
                        let lock = PathBuf::from(lock);
                        if std::fs::symlink_metadata(&lock).is_ok() {
                            Err(GitError::IndexLocked { lock })
                        } else {
                            Err(error)
                        }
                    }
                    Err(_) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn worktree_is_clean(&self, worktree: &Path) -> Result<bool, GitError> {
        let output = self.run(
            Some(worktree),
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        Ok(output.stdout.is_empty())
    }

    /// Whether Git refuses to remove `worktree` without `--force` because it
    /// contains submodules: its administrative directory holds a `modules`
    /// store, or its index has a gitlink whose directory is checked out. This
    /// mirrors Git's own check, which Git applies only while the worktree
    /// directory exists.
    pub fn worktree_has_submodules(&self, worktree: &Path) -> Result<bool, GitError> {
        let admin = self.run_path(
            Some(worktree),
            &["rev-parse", "--absolute-git-dir"],
            "worktree administrative directory",
        )?;
        if admin.join("modules").is_dir() {
            return Ok(true);
        }
        let output = self.run(
            Some(worktree),
            &["ls-files", "--stage", "--full-name", "-z"],
        )?;
        for entry in output.stdout.split(|byte| *byte == 0) {
            if !entry.starts_with(b"160000 ") {
                continue;
            }
            let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
                return Err(GitError::InvalidOutput {
                    context: "gitlink index entry",
                    detail: "entry has no path".to_owned(),
                });
            };
            let path = os_string_from_git(&entry[tab + 1..], "gitlink path")?;
            if worktree.join(path).join(".git").exists() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Return whether replacing a checkout from its exact tree would discard
    /// no tracked, untracked, or ignored files.
    pub fn worktree_is_pristine(&self, worktree: &Path) -> Result<bool, GitError> {
        let output = self.run(
            Some(worktree),
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ],
        )?;
        Ok(output.stdout.is_empty())
    }

    /// Remove a linked worktree through Git's normal dirty-worktree checks.
    pub fn remove_worktree(&self, repository: &Path, worktree: &Path) -> Result<(), GitError> {
        let arguments = [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--"),
            git_path_argument(worktree),
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
            OsString::from("--"),
            git_path_argument(worktree),
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
            git_path_argument(source),
            git_path_argument(destination),
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
        // Without `--verify`, show-ref returns 1 with no output for a missing
        // ref and prints the object ID for a match, so existence and target
        // resolution share one process. Its pattern is a suffix match, though:
        // `refs/heads/topic` also matches `refs/remotes/origin/refs/heads/topic`
        // and `refs/heads/refs/heads/topic`. So only the line naming exactly
        // this ref counts. A refname cannot contain a space, so each
        // `<oid> <refname>` line splits unambiguously.
        let arguments = [OsString::from("show-ref"), reference.clone()];
        let output = self.output_os(Some(repository), &arguments)?;
        match output.status.code() {
            Some(0) => {
                let reference = reference.as_encoded_bytes();
                output
                    .stdout
                    .split(|byte| *byte == b'\n')
                    .filter_map(|line| {
                        let space = line.iter().position(|byte| *byte == b' ')?;
                        Some((&line[..space], &line[space + 1..]))
                    })
                    .find(|(_, name)| *name == reference)
                    .map(|(object, _)| parse_object_bytes(object))
                    .transpose()
            }
            Some(1) => Ok(None),
            _ => Err(command_failed(&arguments, &output)),
        }
    }

    /// Remove the lock on a registered worktree, whether or not its directory
    /// still exists.
    pub fn unlock_worktree(&self, repository: &Path, worktree: &Path) -> Result<(), GitError> {
        let arguments = [
            OsString::from("worktree"),
            OsString::from("unlock"),
            git_path_argument(worktree),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
    }

    /// Point `branch` at `new` only if it still points at `expected`, as one
    /// compare-and-swap reference update. Used to pin a branch the current
    /// operation just created back to the commit it resolved before mutation.
    pub fn move_branch_if_unchanged(
        &self,
        repository: &Path,
        branch: &OsStr,
        new: &ObjectId,
        expected: &ObjectId,
    ) -> Result<(), GitError> {
        let mut reference = OsString::from("refs/heads/");
        reference.push(branch);
        let arguments = [
            OsString::from("update-ref"),
            OsString::from("--no-deref"),
            OsString::from("-m"),
            OsString::from("riftri: pin new branch to the resolved start point"),
            reference,
            OsString::from(new.as_str()),
            OsString::from(expected.as_str()),
        ];
        self.run_os(Some(repository), &arguments)?;
        Ok(())
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

    /// `rev-parse --verify --quiet` without peeling: the object `revision`
    /// names, or `None` when it names nothing.
    fn resolve_optional_object_os(
        &self,
        path: &Path,
        revision: &OsStr,
    ) -> Result<Option<ObjectId>, GitError> {
        let arguments = [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from("--end-of-options"),
            revision.to_os_string(),
        ];
        let output = self.output_os(Some(path), &arguments)?;
        match output.status.code() {
            Some(0) => parse_object_output(&output.stdout).map(Some),
            Some(1) => Ok(None),
            _ => Err(command_failed(&arguments, &output)),
        }
    }

    fn resolve_optional_object(
        &self,
        path: &Path,
        revision: &str,
    ) -> Result<Option<ObjectId>, GitError> {
        let arguments = [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            revision,
        ];
        let output = self.output(Some(path), &arguments)?;
        if output.status.success() {
            return parse_object_output(&output.stdout).map(Some);
        }
        // `--verify --quiet` exits 1 both when the revision does not exist
        // (an unborn HEAD) and when the ref exists but its object cannot be
        // read (a corrupt object store); stderr is identical either way. Any
        // other disposition — a signal death included — is a real failure
        // and must never be reported as an absent revision.
        if output.status.code() != Some(1) {
            return Err(command_failed(&arguments.map(OsString::from), &output));
        }
        // Distinguish the two exit-1 states with one cheap follow-up that
        // runs only on this already-failing path: resolving the unpeeled
        // base needs no object read, so it succeeds over a corrupt store
        // but still fails in a genuinely unborn repository.
        let Some((base, _)) = revision.split_once("^{") else {
            return Ok(None);
        };
        let base_arguments = ["rev-parse", "--verify", "--quiet", "--end-of-options", base];
        let probe = self.output(Some(path), &base_arguments)?;
        if probe.status.success() {
            return Err(GitError::UnreadableObject {
                revision: revision.to_owned(),
                base: base.to_owned(),
                target: String::from_utf8_lossy(trim_line_endings(&probe.stdout)).into_owned(),
            });
        }
        if probe.status.code() == Some(1) {
            return Ok(None);
        }
        Err(command_failed(&base_arguments.map(OsString::from), &probe))
    }

    /// Resolve HEAD's commit and tree in one process, preserving the same
    /// unborn-versus-corrupt distinction as `resolve_optional_object`.
    fn resolve_optional_head_revision(
        &self,
        path: &Path,
    ) -> Result<Option<ResolvedRevision>, GitError> {
        let arguments = resolved_revision_arguments(OsStr::new("HEAD"));
        let output = self.output_os(Some(path), &arguments)?;
        if output.status.success() {
            return parse_resolved_revision_output(&output.stdout).map(Some);
        }

        // `git show` has no quiet missing-revision disposition. Reuse the
        // established optional-object probe only on this failure path: it
        // returns None for an unborn HEAD, reports an unreadable commit as
        // corruption, and lets every other failure retain the original show
        // diagnostic.
        match self.resolve_optional_object(path, "HEAD^{commit}")? {
            None => Ok(None),
            Some(_) => Err(command_failed(&arguments, &output)),
        }
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
        let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
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

    /// Whether `path` lies outside any Git repository, as opposed to inside
    /// an unhealthy one. `rev-parse --git-dir` needs repository discovery but
    /// no object reads, so it succeeds in a repository whose object store is
    /// damaged and fails outside one. The C locale is forced so the
    /// classification never depends on translated error text.
    pub fn repository_absent(&self, path: &Path) -> Result<bool, GitError> {
        // A path that does not exist, or is not a directory, cannot lie inside
        // a repository — and Git cannot even be started there. The operating
        // system reports a bad working directory as the *program* being
        // missing, so without this check a mistyped repository path surfaced
        // as "could not start Git command" and sent people looking for a
        // broken Git installation (#422).
        if !path.is_dir() {
            return Ok(true);
        }
        let arguments = [OsString::from("rev-parse"), OsString::from("--git-dir")];
        let output = self.output_os_with_env(
            Some(path),
            &arguments,
            &[
                (OsStr::new("LC_ALL"), OsStr::new("C")),
                (OsStr::new("LANGUAGE"), OsStr::new("")),
            ],
        )?;
        Ok(!output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("not a git repository"))
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
        #[cfg(test)]
        self.process_attempts.fetch_add(1, Ordering::Relaxed);
        let mut command = Command::new(&self.command);
        command.args(arguments).envs(environment.iter().copied());

        if let Some(path) = path {
            command.current_dir(path);
        }

        command
            .output()
            .map_err(|source| self.start_error(path, source))
    }

    /// A spawn fails when its working directory is missing, which Unix reports
    /// as a missing program and Windows as an invalid directory name. Either
    /// way the directory is the cause, so the Git executable is not blamed.
    fn start_error(&self, path: Option<&Path>, source: std::io::Error) -> GitError {
        match path {
            Some(path) if !path.is_dir() => GitError::WorkingDirectoryMissing {
                path: path.to_path_buf(),
            },
            _ => GitError::Start {
                command: self.command.clone(),
                source,
            },
        }
    }

    fn run_os_with_input(
        &self,
        path: Option<&Path>,
        arguments: &[OsString],
        environment: &[(&OsStr, &OsStr)],
        input: &[u8],
    ) -> Result<Output, GitError> {
        use std::io::Write;

        #[cfg(test)]
        self.process_attempts.fetch_add(1, Ordering::Relaxed);
        let mut command = Command::new(&self.command);
        command
            .args(arguments)
            .envs(environment.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            command.current_dir(path);
        }

        let mut child = command
            .spawn()
            .map_err(|source| self.start_error(path, source))?;
        let mut stdin = child.stdin.take().ok_or_else(|| GitError::InvalidOutput {
            context: "Git command input",
            detail: "piped standard input was unavailable".to_owned(),
        })?;
        // The scoped writer borrows reusable input while output is drained
        // concurrently. It is joined before returning, including wait errors.
        let (output, write_result) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output().map_err(|source| GitError::Wait {
                command: self.command.clone(),
                source,
            });
            let write_result = writer.join().map_err(|_| GitError::InvalidOutput {
                context: "Git command input",
                detail: "input writer thread panicked".to_owned(),
            });
            Ok::<_, GitError>((output?, write_result?))
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

fn attribute_stdin(paths: &[impl AsRef<Path>]) -> Option<Vec<u8>> {
    let mut input = Vec::new();
    for path in paths {
        let path = path.as_ref();
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes()
        };
        #[cfg(not(unix))]
        let bytes = path.to_str()?.as_bytes();
        input.extend_from_slice(bytes);
        input.push(0);
    }
    Some(input)
}

fn invalid_blob_batch(detail: impl Into<String>) -> GitError {
    GitError::InvalidOutput {
        context: "bounded blob batch",
        detail: detail.into(),
    }
}

fn read_batch_blob(
    reader: &mut impl std::io::BufRead,
    object: &ObjectId,
    max_bytes: usize,
) -> Result<Vec<u8>, GitError> {
    use std::io::{BufRead, Read};
    let mut header = Vec::new();
    reader
        .take(128)
        .read_until(b'\n', &mut header)
        .map_err(|error| invalid_blob_batch(error.to_string()))?;
    let line = std::str::from_utf8(&header)
        .map_err(|_| invalid_blob_sizes())?
        .strip_suffix('\n')
        .ok_or_else(invalid_blob_sizes)?;
    let size = parse_blob_size_line(line, object)?;
    if size > max_bytes as u64 {
        return Err(invalid_blob_batch(format!(
            "blob {} is {size} bytes; limit is {max_bytes}",
            object.as_str()
        )));
    }
    let mut bytes = vec![0; size as usize];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| invalid_blob_batch(error.to_string()))?;
    let mut delimiter = [0];
    reader
        .read_exact(&mut delimiter)
        .map_err(|error| invalid_blob_batch(error.to_string()))?;
    if delimiter != *b"\n" {
        return Err(invalid_blob_batch("missing blob body delimiter"));
    }
    Ok(bytes)
}

fn invalid_blob_sizes() -> GitError {
    GitError::InvalidOutput {
        context: "blob size batch",
        detail: "expected one matching blob ID, type and size per request".to_owned(),
    }
}

fn parse_blob_size_line(line: &str, object: &ObjectId) -> Result<u64, GitError> {
    let mut fields = line.split(' ');
    let (Some(id), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next()) else {
        return Err(invalid_blob_sizes());
    };
    if fields.next().is_some()
        || id != object.as_str()
        || kind != "blob"
        || size.is_empty()
        || !size.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_blob_sizes());
    }
    size.parse().map_err(|_| invalid_blob_sizes())
}

fn parse_blob_sizes(bytes: &[u8], objects: &[ObjectId]) -> Result<Vec<u64>, GitError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid_blob_sizes())?;
    let lines = text
        .strip_suffix('\n')
        .ok_or_else(invalid_blob_sizes)?
        .split('\n');
    // Preserve whole-response count validation before parsing individual lines.
    if lines.clone().count() != objects.len() {
        return Err(invalid_blob_sizes());
    }
    lines
        .zip(objects)
        .map(|(line, object)| parse_blob_size_line(line, object))
        .collect()
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

    let mut fields = input[..input.len() - 1].split(|byte| *byte == 0);
    // Validate the complete shape first to retain malformed-output error
    // precedence, without allocating a vector of every intermediate field.
    let field_count = fields.clone().count();
    if field_count % 3 != 0 {
        return Err(GitError::InvalidOutput {
            context: "Git attribute records",
            detail: "output did not contain path/name/value triples".to_owned(),
        });
    }

    let mut attributes = Vec::with_capacity(field_count / 3);
    while let Some(path) = fields.next() {
        let name = fields.next().expect("validated attribute triple");
        let value = fields.next().expect("validated attribute triple");
        if path.is_empty() || name.is_empty() {
            return Err(GitError::InvalidOutput {
                context: "Git attribute record",
                detail: "path and attribute name must not be empty".to_owned(),
            });
        }
        attributes.push(GitAttribute {
            path: PathBuf::from(os_string_from_git(path, "attribute path")?),
            name: name.to_vec(),
            value: value.to_vec(),
        });
    }
    Ok(attributes)
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
        let head_states = usize::from(partial.detached)
            + usize::from(partial.bare)
            + usize::from(partial.branch.is_some());
        if head_states > 1 {
            return Err(GitError::InvalidOutput {
                context: "worktree porcelain",
                detail: format!(
                    "{} did not have exactly one of branch, detached, or bare",
                    path.display()
                ),
            });
        }
        // Zero of the three is real Git output, not a protocol violation: a
        // worktree whose HEAD file cannot be resolved (empty, garbage, or an
        // empty symref target) is listed with a null HEAD and no state
        // attribute. Represent it instead of failing so one corrupt worktree
        // cannot poison every listing of the repository.
        worktrees.push(WorktreeInfo {
            path,
            head: partial.head,
            branch: partial.branch,
            detached: partial.detached,
            bare: partial.bare,
            head_unresolvable: head_states == 0,
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

/// Whether a peel suffix cannot be appended to `revision`: in `:/<text>` it
/// would join the search text, and in `<rev>:<path>` the path. Those spellings
/// are resolved to an object first and peeled after. No refname contains `:`.
fn must_resolve_before_peeling(revision: &OsStr) -> bool {
    revision.as_encoded_bytes().contains(&b':')
}

/// Whether `git show` would parse `revision` as a range or a negation. No
/// refname contains `..` or starts with `^`, so a name that does is either one
/// of those or something `rev-parse --verify` must judge.
fn is_revision_walk(revision: &OsStr) -> bool {
    let bytes = revision.as_encoded_bytes();
    bytes.first() == Some(&b'^') || bytes.windows(2).any(|pair| pair == b"..")
}

fn resolved_revision_arguments(revision: &OsStr) -> [OsString; 8] {
    let mut expression = revision.to_os_string();
    expression.push("^{commit}");
    [
        OsString::from("show"),
        OsString::from("--no-patch"),
        OsString::from("--no-notes"),
        OsString::from("--no-show-signature"),
        OsString::from("--format=format:%H%x00%T%x00"),
        OsString::from("--end-of-options"),
        expression,
        OsString::from("--"),
    ]
}

fn parse_resolved_revision_output(bytes: &[u8]) -> Result<ResolvedRevision, GitError> {
    let mut fields = bytes.split(|byte| *byte == 0);
    let commit = fields.next().unwrap_or_default();
    let tree = fields.next().unwrap_or_default();
    if fields.next() != Some(&[][..]) || fields.next().is_some() {
        return Err(GitError::InvalidOutput {
            context: "resolved revision",
            detail: "expected exactly two NUL-delimited object IDs".to_owned(),
        });
    }
    Ok(ResolvedRevision {
        commit: parse_object_bytes(commit)?,
        tree: parse_object_bytes(tree)?,
    })
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

/// Parse the object directory followed by the object format from one
/// `rev-parse` invocation. Splitting at the final newline preserves every byte
/// in a native path that itself contains newlines; the format is Git-owned
/// ASCII and always occupies the final line.
fn parse_checkout_storage(output: &[u8]) -> Result<(PathBuf, String), GitError> {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    let separator = output
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(|| GitError::InvalidOutput {
            context: "checkout object storage",
            detail: "expected an object directory and object format".to_owned(),
        })?;
    let object_directory = &output[..separator];
    if object_directory.is_empty() {
        return Err(GitError::InvalidOutput {
            context: "object directory",
            detail: "path was empty".to_owned(),
        });
    }
    let format = utf8_line(&output[separator + 1..], "object format")?;
    if format.is_empty() {
        return Err(GitError::InvalidOutput {
            context: "object format",
            detail: "value was empty".to_owned(),
        });
    }
    Ok((
        PathBuf::from(os_string_from_git(object_directory, "object directory")?),
        format,
    ))
}

fn trim_line_endings(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn git_path_argument(path: &Path) -> OsString {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const VERBATIM_UNC: &[u16] = &[
            b'\\' as u16,
            b'\\' as u16,
            b'?' as u16,
            b'\\' as u16,
            b'U' as u16,
            b'N' as u16,
            b'C' as u16,
            b'\\' as u16,
        ];

        let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.starts_with(VERBATIM_UNC) {
            let mut normalized = vec![b'\\' as u16, b'\\' as u16];
            normalized.extend_from_slice(&wide[VERBATIM_UNC.len()..]);
            return OsString::from_wide(&normalized);
        }
        if wide.starts_with(VERBATIM) {
            return OsString::from_wide(&wide[VERBATIM.len()..]);
        }
    }

    path.as_os_str().to_os_string()
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

fn parse_config_values(output: &[u8], keys: &[String]) -> Result<ConfigValues, GitError> {
    let invalid = || GitError::InvalidOutput {
        context: "configuration values",
        detail: "expected NUL-terminated records for the requested keys".to_owned(),
    };
    let mut config = ConfigValues::default();
    if output.is_empty() {
        return Ok(config);
    }
    let records = output.strip_suffix(&[0]).ok_or_else(invalid)?;
    for record in records.split(|byte| *byte == 0) {
        // Git emits key\nvalue\0, or key\0 for an implicit boolean. Split only
        // the first newline: values may themselves contain newlines or non-UTF-8.
        let (name, value) = match record.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&record[..index], &record[index + 1..]),
            None => (record, &[][..]),
        };
        if name.starts_with(b"includeif.") && name.ends_with(b".path") {
            config.has_conditional_includes = true;
            continue;
        }
        let key = keys
            .iter()
            .find(|key| key.as_bytes() == name)
            .ok_or_else(invalid)?;
        // --get-regexp returns all occurrences in Git's precedence order;
        // --get returns the last one. Preserve that exact behavior.
        config.values.insert(key.clone(), value.to_vec());
    }
    Ok(config)
}

fn normalize_config_key(key: &str) -> Result<String, GitError> {
    let components = key.split('.').collect::<Vec<_>>();
    let valid = components.len() >= 2
        && components.iter().all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && components
            .last()
            .and_then(|variable| variable.as_bytes().first())
            .is_some_and(u8::is_ascii_alphabetic);
    if !valid {
        return Err(GitError::InvalidOutput {
            context: "configuration keys",
            detail: "reads require simple section.variable keys".to_owned(),
        });
    }
    let mut key = key.to_owned();
    key[..components[0].len()].make_ascii_lowercase();
    let variable = key.rfind('.').expect("validated configuration key") + 1;
    key[variable..].make_ascii_lowercase();
    Ok(key)
}

fn parse_config_value_origins(output: &[u8]) -> Result<Vec<ConfigValueOrigin>, GitError> {
    let invalid = || GitError::InvalidOutput {
        context: "configuration value origins",
        detail: "expected NUL-terminated scope, origin, and value fields".to_owned(),
    };
    if output.is_empty() {
        return Ok(Vec::new());
    }
    let records = output.strip_suffix(&[0]).ok_or_else(invalid)?;
    let fields = records.split(|byte| *byte == 0).collect::<Vec<_>>();
    let (records, remainder) = fields.as_slice().as_chunks::<3>();
    if !remainder.is_empty() {
        return Err(invalid());
    }
    records
        .iter()
        .map(|record| {
            if record[0].is_empty() || record[1].is_empty() {
                return Err(invalid());
            }
            Ok(ConfigValueOrigin {
                scope: record[0].to_vec(),
                origin: record[1].to_vec(),
                value: record[2].to_vec(),
            })
        })
        .collect()
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
        disposition: describe_exit_disposition(output.status),
    }
}

/// Render trimmed stderr with the exit disposition appended, or the
/// disposition alone, so a signal-killed Git that wrote nothing to stderr
/// still explains itself instead of ending the message at a bare colon.
fn command_failure_detail(disposition: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        disposition.to_owned()
    } else {
        format!("{stderr} ({disposition})")
    }
}

/// Describe how a Git process ended: its exit code when it exited, or on
/// Unix the terminating signal, following the 128+signal convention used by
/// the activation layer for propagating such deaths.
fn describe_exit_disposition(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            return match signal_name(signal) {
                Some(name) => format!("killed by signal {signal} ({name})"),
                None => format!("killed by signal {signal}"),
            };
        }
    }
    "terminated without an exit code".to_owned()
}

#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::{TempDir, tempdir};

    use super::{Git, WorktreeHead, parse_attribute_records, parse_worktree_porcelain};

    struct RepositoryFixture {
        directory: TempDir,
    }

    #[test]
    fn small_blob_session_reuses_one_process_across_bounded_batches() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        let revision = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let object = git.list_tree(fixture.path(), &revision.tree).unwrap()[0]
            .object_id
            .clone();
        let before = git
            .process_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        let mut reader = git.small_blob_reader(fixture.path()).unwrap();
        for count in [128, 128, 1] {
            assert_eq!(
                reader.read(&vec![object.clone(); count], 8).unwrap(),
                vec![b"tracked\n".to_vec(); count]
            );
        }
        reader.finish().unwrap();
        assert_eq!(
            git.process_attempts
                .load(std::sync::atomic::Ordering::Relaxed)
                - before,
            1
        );
    }

    #[test]
    fn small_blob_session_rejects_later_oversized_reads_and_cannot_be_reused() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        let revision = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let object = git.list_tree(fixture.path(), &revision.tree).unwrap()[0]
            .object_id
            .clone();
        let mut reader = git.small_blob_reader(fixture.path()).unwrap();
        reader.read(std::slice::from_ref(&object), 8).unwrap();
        assert!(reader.read(std::slice::from_ref(&object), 7).is_err());
        assert!(reader.child.is_none());
        assert!(reader.read(&[object], 8).is_err());
        assert!(reader.finish().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn small_blob_session_finish_rejects_trailing_data_and_failed_exit() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let script = fixture.path().join("git-batch");
        let id = super::ObjectId::parse("a".repeat(40)).unwrap();
        for ending in ["printf extra", "printf failed >&2; exit 7"] {
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\nwhile read request; do printf '{} blob 1\\nx\\n'; done\n{ending}\n",
                    id.as_str()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            let mut reader = Git::new(&script).small_blob_reader(fixture.path()).unwrap();
            assert_eq!(
                reader.read(std::slice::from_ref(&id), 1).unwrap(),
                vec![b"x".to_vec()]
            );
            assert!(reader.finish().is_err());
        }
    }

    #[test]
    fn bounded_blob_batch_checks_limits_and_preserves_order() {
        let fixture = RepositoryFixture::committed();
        let handle = Git::default();
        let revision = handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let object = handle.list_tree(fixture.path(), &revision.tree).unwrap()[0]
            .object_id
            .clone();
        let before = handle
            .process_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            handle
                .read_small_blobs(fixture.path(), &[object.clone(), object.clone()], 8)
                .unwrap(),
            vec![b"tracked\n".to_vec(); 2]
        );
        assert_eq!(
            handle
                .process_attempts
                .load(std::sync::atomic::Ordering::Relaxed)
                - before,
            1
        );
        assert!(
            handle
                .read_small_blobs(fixture.path(), &[object], 7)
                .is_err()
        );
        assert!(
            handle
                .read_small_blobs(fixture.path(), &[revision.tree], 1024)
                .is_err()
        );
        let missing = super::ObjectId::parse("0".repeat(40)).unwrap();
        assert!(
            handle
                .read_small_blobs(fixture.path(), &[missing], 1024)
                .is_err()
        );
        let before = handle
            .process_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            handle
                .read_small_blobs(fixture.path(), &[], 1024)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle
                .process_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            before
        );
    }

    #[test]
    #[ignore = "manual release-mode process-reuse benchmark"]
    fn reports_small_blob_session_latency() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        let revision = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let object = git.list_tree(fixture.path(), &revision.tree).unwrap()[0]
            .object_id
            .clone();
        let ids = vec![object; 10_000];
        for round in 0..4 {
            for reuse in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let before = git
                    .process_attempts
                    .load(std::sync::atomic::Ordering::Relaxed);
                let start = std::time::Instant::now();
                let mut reader = reuse.then(|| git.small_blob_reader(fixture.path()).unwrap());
                for batch in ids.chunks(128) {
                    let blobs = match &mut reader {
                        Some(reader) => reader.read(batch, 1024).unwrap(),
                        None => git.read_small_blobs(fixture.path(), batch, 1024).unwrap(),
                    };
                    assert_eq!(blobs, vec![b"tracked\n".to_vec(); batch.len()]);
                }
                if let Some(reader) = reader {
                    reader.finish().unwrap();
                }
                let elapsed = start.elapsed();
                let starts = git
                    .process_attempts
                    .load(std::sync::atomic::Ordering::Relaxed)
                    - before;
                assert_eq!(starts, if reuse { 1 } else { 79 });
                eprintln!(
                    "round={round} reuse={reuse} starts={starts} microseconds={}",
                    elapsed.as_micros()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_blob_session_reaps_the_child() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        let reader = git.small_blob_reader(fixture.path()).unwrap();
        let pid = reader.child.as_ref().unwrap().id();
        drop(reader);
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn bounded_blob_decoder_refuses_bad_headers_bodies_and_delimiters() {
        let id = super::ObjectId::parse("a".repeat(40)).unwrap();
        let decode =
            |bytes: Vec<u8>| super::read_batch_blob(&mut std::io::Cursor::new(bytes), &id, 8);
        for data in [
            Vec::new(),
            vec![b'x'; 1000],
            format!("{} missing\n", id.as_str()).into_bytes(),
            format!("{} tree 1\nx\n", id.as_str()).into_bytes(),
            format!("{} blob 3\nab", id.as_str()).into_bytes(),
            format!("{} blob 3\nabc!", id.as_str()).into_bytes(),
            format!("{} blob 1\nx\n", "b".repeat(40)).into_bytes(),
        ] {
            assert!(decode(data).is_err());
        }
        let header = format!("{} blob 1000000000\n", id.as_str());
        let mut oversized = std::io::Cursor::new(format!("{header}body-must-not-be-read"));
        assert!(super::read_batch_blob(&mut oversized, &id, 8).is_err());
        assert_eq!(oversized.position(), header.len() as u64);
        let mut binary = format!("{} blob 3\n", id.as_str()).into_bytes();
        binary.extend_from_slice(b"a\0\n\n");
        assert_eq!(decode(binary).unwrap(), b"a\0\n");
        assert_eq!(
            decode(format!("{} blob 0\n\n", id.as_str()).into_bytes()).unwrap(),
            b""
        );
        let sha256 = super::ObjectId::parse("b".repeat(64)).unwrap();
        let mut body = std::io::Cursor::new(format!("{} blob 1\nx\n", sha256.as_str()));
        assert_eq!(super::read_batch_blob(&mut body, &sha256, 1).unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn oversized_blob_batch_kills_and_reaps_the_child() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let script = fixture.path().join("git-oversized");
        let pid_file = fixture.path().join("pid");
        let id = super::ObjectId::parse("a".repeat(40)).unwrap();
        fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > \"{}\"\nread request\nprintf '{} blob 1000000000\\n'\nexec sleep 30\n", pid_file.display(), id.as_str())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let handle = Git::new(&script);
        let started = std::time::Instant::now();
        assert!(
            handle
                .read_small_blobs(fixture.path(), &[id], 1024)
                .is_err()
        );
        assert!(
            started.elapsed().as_secs() < 10,
            "failed to terminate the oversized response producer"
        );
        let pid = fs::read_to_string(pid_file).unwrap();
        assert!(
            !Command::new("kill")
                .args(["-0", pid.trim()])
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn blob_sizes_batch_preserves_order_duplicates_and_rejects_non_blobs() {
        let fixture = RepositoryFixture::committed();
        let handle = Git::default();
        let revision = handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let entries = handle.list_tree(fixture.path(), &revision.tree).unwrap();
        let object = entries[0].object_id.clone();
        fs::write(fixture.path().join("other.txt"), "abc").unwrap();
        let other = Command::new("git")
            .args(["hash-object", "-w", "other.txt"])
            .current_dir(fixture.path())
            .output()
            .unwrap();
        assert!(other.status.success());
        let other =
            super::ObjectId::parse(String::from_utf8(other.stdout).unwrap().trim()).unwrap();
        let before = handle
            .process_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            handle
                .blob_sizes(fixture.path(), &[object.clone(), other, object])
                .unwrap(),
            vec![8, 3, 8]
        );
        assert_eq!(
            handle
                .process_attempts
                .load(std::sync::atomic::Ordering::Relaxed)
                - before,
            1
        );
        assert!(handle.blob_sizes(fixture.path(), &[revision.tree]).is_err());
        let missing = super::ObjectId::parse("0".repeat(40)).unwrap();
        assert!(handle.blob_sizes(fixture.path(), &[missing]).is_err());
        let before = handle
            .process_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(handle.blob_sizes(fixture.path(), &[]).unwrap().is_empty());
        assert_eq!(
            handle
                .process_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            before
        );
    }

    #[test]
    fn blob_size_parser_preserves_exact_spacing_and_record_counts() {
        let id = super::ObjectId::parse("a".repeat(40)).unwrap();
        for size in ["0", "0001", "18446744073709551615"] {
            let line = format!("{} blob {size}\n", id.as_str());
            assert_eq!(
                super::parse_blob_sizes(line.as_bytes(), std::slice::from_ref(&id)).unwrap(),
                vec![size.parse::<u64>().unwrap()]
            );
        }
        for suffix in [
            " blob 1 \n",
            "  blob 1\n",
            " blob  1\n",
            " blob +1\n",
            " blob 1\r\n",
            " blob 1\n\n",
            "\tblob 1\n",
        ] {
            let line = format!("{}{suffix}", id.as_str());
            assert!(
                super::parse_blob_sizes(line.as_bytes(), std::slice::from_ref(&id)).is_err(),
                "{line:?}"
            );
        }
        let two = format!("{} blob 7\n{} blob 0\n", id.as_str(), id.as_str());
        assert_eq!(
            super::parse_blob_sizes(two.as_bytes(), &[id.clone(), id.clone()]).unwrap(),
            vec![7, 0]
        );
        assert!(super::parse_blob_sizes(two.as_bytes(), std::slice::from_ref(&id)).is_err());
        assert!(super::parse_blob_sizes(b"", &[]).is_err());
    }

    #[test]
    fn blob_sizes_batch_rejects_malformed_protocol() {
        let sha256 = super::ObjectId::parse("a".repeat(64)).unwrap();
        assert_eq!(
            super::parse_blob_sizes(
                format!("{} blob 0\n", sha256.as_str()).as_bytes(),
                &[sha256]
            )
            .unwrap(),
            vec![0]
        );
        let id = super::ObjectId::parse("a".repeat(40)).unwrap();
        for text in [
            String::new(),
            format!("{} blob 1", id.as_str()),
            format!("{} blob -1\n", id.as_str()),
            format!("{} tree 1\n", id.as_str()),
            format!("{} missing\n", id.as_str()),
            format!("{} blob 18446744073709551616\n", id.as_str()),
            format!("{} blob 1\nextra\n", id.as_str()),
            format!("{} blob 1\n", "b".repeat(40)),
        ] {
            assert!(
                super::parse_blob_sizes(text.as_bytes(), std::slice::from_ref(&id)).is_err(),
                "{text:?}"
            );
        }
    }

    #[test]
    #[ignore = "manual paired size-query benchmark; no host-sensitive timing gate"]
    fn reports_blob_size_batch_latency() {
        let fixture = RepositoryFixture::committed();
        let handle = Git::default();
        let revision = handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap();
        let object = handle.list_tree(fixture.path(), &revision.tree).unwrap()[0]
            .object_id
            .clone();
        let objects = vec![object; 64];
        for round in 0..4 {
            for batch in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let started = std::time::Instant::now();
                let sizes = if batch {
                    handle.blob_sizes(fixture.path(), &objects).unwrap()
                } else {
                    objects
                        .iter()
                        .map(|id| handle.blob_size(fixture.path(), id).unwrap())
                        .collect()
                };
                assert_eq!(sizes, vec![8; 64]);
                println!(
                    "blob-size round={round} batch={batch} elapsed_us={}",
                    started.elapsed().as_micros()
                );
            }
        }
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

    #[test]
    fn checkout_storage_parser_preserves_newlines_in_the_object_path() {
        let (objects, format) =
            super::parse_checkout_storage(b"/tmp/repository\nname/.git/objects\nsha256\n")
                .expect("parse checkout storage");
        assert_eq!(objects, Path::new("/tmp/repository\nname/.git/objects"));
        assert_eq!(format, "sha256");
        assert!(super::parse_checkout_storage(b"objects-only\n").is_err());
        assert!(super::parse_checkout_storage(b"\nsha1\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn checkout_storage_parser_preserves_non_utf8_object_paths() {
        use std::os::unix::ffi::OsStringExt;

        let (objects, format) =
            super::parse_checkout_storage(b"/tmp/objects-\xff\nsha1\n").unwrap();
        assert_eq!(
            objects,
            PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/objects-\xff".to_vec()))
        );
        assert_eq!(format, "sha1");
    }

    /// `git worktree list` dies when a concurrent removal deletes a
    /// worktree's administrative file mid-listing. The inventory is retried,
    /// so one such race no longer fails the caller.
    #[cfg(unix)]
    #[test]
    fn worktree_listing_retries_a_listing_that_raced_a_removal() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::tempdir().expect("fixture");
        let repository = fixture.path().join("repository");
        fs::create_dir(&repository).expect("create repository");
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .expect("init");
        assert!(init.success());
        let real = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git");
        let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
        let marker = fixture.path().join("already-failed");
        let stand_in = fixture.path().join("git");
        fs::write(
            &stand_in,
            format!(
                "#!/bin/sh\nif [ \"$1 $2\" = \"worktree list\" ] && [ ! -e '{marker}' ]; then\n  : > '{marker}'\n  echo \"fatal: failed to read '.git/worktrees/gone/locked': No such file or directory\" >&2\n  exit 128\nfi\nexec '{real}' \"$@\"\n",
                marker = marker.display()
            ),
        )
        .expect("write stand-in");
        fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).expect("chmod");

        let git = super::Git::new(&stand_in);
        let listed = retry_while_wrapper_is_busy(|| git.list_worktrees(&repository))
            .expect("the retried listing succeeds");

        assert!(marker.exists(), "the first listing did fail");
        assert_eq!(listed.len(), 1, "{listed:?}");
    }

    #[test]
    fn recovery_initializes_only_a_missing_worktree_index() {
        let fixture = RepositoryFixture::committed();
        let parent = tempdir().unwrap();
        let worktree = parent.path().join("view");
        git(
            fixture.path(),
            &[
                "worktree",
                "add",
                "--no-checkout",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let git_handle = Git::default();
        assert!(!git_handle.worktree_index_has_changes(&worktree).unwrap());
        assert!(
            git_handle
                .initialize_missing_worktree_index(&worktree, &[])
                .unwrap()
        );
        let index = git_handle.worktree_index_path(&worktree).unwrap();
        let before = fs::read(&index).unwrap();
        assert!(
            !git_handle
                .initialize_missing_worktree_index(&worktree, &[])
                .unwrap()
        );
        assert_eq!(fs::read(&index).unwrap(), before);
        assert!(!git_handle.worktree_index_has_changes(&worktree).unwrap());
        git(
            &worktree,
            &["update-index", "--force-remove", "tracked.txt"],
        );
        let staged = fs::read(&index).unwrap();
        assert!(git_handle.worktree_index_has_changes(&worktree).unwrap());
        assert!(
            !git_handle
                .initialize_missing_worktree_index(&worktree, &[])
                .unwrap()
        );
        assert_eq!(fs::read(&index).unwrap(), staged);
    }

    #[test]
    fn reads_exact_blob_bytes_and_size() {
        let fixture = RepositoryFixture::committed();
        let object = Command::new("git")
            .args(["rev-parse", "HEAD:tracked.txt"])
            .current_dir(fixture.path())
            .output()
            .expect("read object ID");
        assert!(object.status.success());
        let object = String::from_utf8(object.stdout).expect("UTF-8 object ID");
        let object = super::ObjectId::parse(object.trim()).expect("object ID");
        let git = Git::default();

        assert_eq!(git.blob_size(fixture.path(), &object).unwrap(), 8);
        assert_eq!(
            git.read_blob(fixture.path(), &object).unwrap(),
            b"tracked\n"
        );
    }

    #[test]
    fn removal_state_works_through_a_private_overlay_pointer() {
        let fixture = RepositoryFixture::committed();
        let views = tempdir().unwrap();
        let linked = views.path().join("linked");
        let upper = views.path().join("upper");
        let output = Command::new("git")
            .current_dir(fixture.path())
            .args(["worktree", "add", "--detach"])
            .arg(&linked)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        fs::create_dir(&upper).unwrap();
        fs::copy(linked.join(".git"), upper.join(".git")).unwrap();
        let git = Git::default();
        assert_eq!(
            git.worktree_removal_state(&linked).unwrap(),
            git.worktree_removal_state(&upper).unwrap()
        );
    }

    #[test]
    fn removal_state_distinguishes_intent_to_add_from_staged_empty_content() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join("empty.txt"), []).unwrap();
        let git = Git::default();
        git.run(
            Some(fixture.path()),
            &["add", "--intent-to-add", "empty.txt"],
        )
        .unwrap();
        let intent = git.worktree_removal_state(fixture.path()).unwrap();
        git.run(Some(fixture.path()), &["add", "empty.txt"])
            .unwrap();
        let staged = git.worktree_removal_state(fixture.path()).unwrap();
        assert_ne!(
            intent, staged,
            "staging must invalidate force consent even when bytes match"
        );
    }

    #[test]
    fn batch_configuration_reads_lfs_subsection_keys() {
        let fixture = RepositoryFixture::unborn();
        git(
            fixture.path(),
            &["config", "filter.lfs.clean", "git-lfs clean -- %f"],
        );
        git(
            fixture.path(),
            &["config", "filter.lfs.process", "git-lfs filter-process"],
        );
        let values = Git::default()
            .config_values(fixture.path(), &["filter.lfs.clean", "filter.lfs.process"])
            .expect("read LFS configuration")
            .values;

        assert_eq!(
            values.get("filter.lfs.clean").map(Vec::as_slice),
            Some(&b"git-lfs clean -- %f"[..])
        );
        assert_eq!(
            values.get("filter.lfs.process").map(Vec::as_slice),
            Some(&b"git-lfs filter-process"[..])
        );
    }

    #[test]
    fn batched_configuration_preserves_subsection_case() {
        let fixture = RepositoryFixture::unborn();
        for (key, value) in [
            ("filter.Mixed.clean", "cat"),
            ("filter.mixed.clean", "lowercase"),
            ("filter.Mixed.Part.clean", "dotted"),
        ] {
            git(fixture.path(), &["config", key, value]);
        }
        let git = Git::default();
        let values = git
            .config_values(
                fixture.path(),
                &[
                    "FILTER.Mixed.CLEAN",
                    "filter.mixed.clean",
                    "Filter.Mixed.Part.Clean",
                    "filter.MIXED.clean",
                ],
            )
            .unwrap()
            .values;
        assert_eq!(
            values,
            std::collections::BTreeMap::from([
                ("filter.Mixed.clean".to_owned(), b"cat".to_vec()),
                ("filter.mixed.clean".to_owned(), b"lowercase".to_vec()),
                ("filter.Mixed.Part.clean".to_owned(), b"dotted".to_vec()),
            ])
        );
        for (key, value) in values {
            assert_eq!(git.config_value(fixture.path(), &key).unwrap(), Some(value));
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

    #[cfg(target_os = "windows")]
    #[test]
    fn removes_windows_verbatim_prefixes_from_git_path_arguments() {
        assert_eq!(
            super::git_path_argument(Path::new(r"\\?\C:\repo\view")),
            OsStr::new(r"C:\repo\view")
        );
        assert_eq!(
            super::git_path_argument(Path::new(r"\\?\UNC\server\share\view")),
            OsStr::new(r"\\server\share\view")
        );
        assert_eq!(
            super::git_path_argument(Path::new(r"C:\repo\view")),
            OsStr::new(r"C:\repo\view")
        );
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
        // An unborn HEAD leaves the commit unresolved; the tree is report-only.
        assert!(repository.head_commit.is_none());
        assert!(repository.head_tree.is_none());
        let reported = Git::default()
            .inspect_repository_for_report(fixture.path())
            .expect("inspect for report");
        assert!(reported.head_tree.is_none());
        assert_eq!(reported.clean, Some(true));
    }

    /// `git status --porcelain` honours `status.showUntrackedFiles`, which is
    /// a commonly recommended setting for large repositories. Without an
    /// explicit `--untracked-files=all` this probe reported a working tree
    /// holding untracked content as clean, and `riftri doctor` repeated that.
    #[test]
    fn repository_cleanliness_ignores_show_untracked_files_configuration() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        fs::write(fixture.path().join("untracked.txt"), "private\n").expect("write untracked file");

        for value in ["no", "normal", "all"] {
            git.set_local_config(
                fixture.path(),
                "status.showUntrackedFiles",
                OsStr::new(value),
            )
            .expect("configure untracked-file reporting");
            assert_eq!(
                git.inspect_repository_for_report(fixture.path())
                    .expect("inspect repository")
                    .clean,
                Some(false),
                "status.showUntrackedFiles={value} must not hide untracked content"
            );
        }
    }

    /// Remove the loose object file backing `object_id`, clearing the
    /// read-only permission Git leaves on loose objects first so the
    /// deletion also works on Windows.
    fn delete_loose_object(repository: &Path, object_id: &str) {
        let object = repository
            .join(".git/objects")
            .join(&object_id[..2])
            .join(&object_id[2..]);
        let mut permissions = fs::metadata(&object)
            .expect("loose object metadata")
            .permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).expect("make loose object writable");
        fs::remove_file(&object).expect("delete loose object");
    }

    #[test]
    fn a_corrupt_object_store_is_not_reported_as_an_unborn_head() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.path())
            .output()
            .expect("read HEAD object ID");
        assert!(head.status.success());
        let head = String::from_utf8(head.stdout)
            .expect("UTF-8 object ID")
            .trim()
            .to_owned();
        delete_loose_object(fixture.path(), &head);

        let error = git
            .inspect_repository(fixture.path())
            .expect_err("a corrupt object store must fail inspection");
        match &error {
            super::GitError::UnreadableObject {
                revision,
                base,
                target,
            } => {
                assert_eq!(revision, "HEAD^{commit}");
                assert_eq!(base, "HEAD");
                assert_eq!(target, &head);
            }
            other => panic!("expected an unreadable-object error, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("unreadable"), "{message}");
        assert!(message.contains("corrupt"), "{message}");
        assert!(!message.contains("unborn"), "{message}");
    }

    #[test]
    fn git_in_a_missing_directory_names_the_directory_not_git() {
        let parent = tempdir().expect("temporary directory");
        let missing = parent.path().join("moved-away");

        let error = Git::default()
            .list_worktrees(&missing)
            .expect_err("Git cannot run in a missing directory");

        // The OS reports a bad working directory as a missing program; the
        // error must not send people to debug their Git installation.
        let message = error.to_string();
        assert!(
            matches!(&error, super::GitError::WorkingDirectoryMissing { path } if path == &missing),
            "{message}"
        );
        assert!(message.contains("does not exist"), "{message}");
        assert!(!message.contains("could not start Git"), "{message}");
    }

    #[test]
    fn an_index_refresh_blocked_by_a_stale_lock_names_the_lock() {
        let fixture = RepositoryFixture::committed();
        // Git takes the lock only when the refresh must rewrite the index, so
        // make the cached stat data stale; otherwise newer Git succeeds.
        fs::File::options()
            .write(true)
            .open(fixture.path().join("tracked.txt"))
            .expect("open tracked file")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .expect("age the tracked file");
        let lock = fixture.path().join(".git/index.lock");
        fs::write(&lock, b"").expect("leave a stale index lock");

        let error = Git::default()
            .refresh_worktree_index(fixture.path())
            .expect_err("a held index lock must stop the refresh");

        // `update-index -q` exits 128 without a word; the error must still
        // say which lock blocks it and how to clear it.
        let message = error.to_string();
        assert!(message.contains("no Git process"), "{message}");
        let super::GitError::IndexLocked { lock: reported } = &error else {
            panic!("expected the lock to be named: {message}");
        };
        // Git spells paths its own way (forward slashes on Windows).
        assert_eq!(
            fs::canonicalize(reported).expect("resolve reported lock"),
            fs::canonicalize(&lock).expect("resolve lock path"),
            "{message}"
        );
        assert!(lock.exists(), "Riftri must never remove Git's lock itself");

        fs::remove_file(&lock).expect("clear the lock");
        Git::default()
            .refresh_worktree_index(fixture.path())
            .expect("refresh succeeds once the lock is gone");
    }

    #[test]
    fn command_failures_append_the_exit_code_to_stderr() {
        let fixture = RepositoryFixture::committed();
        let error = Git::default()
            .run(
                Some(fixture.path()),
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    "no-such-revision",
                ],
            )
            .expect_err("an unknown revision must fail");
        let message = error.to_string();
        assert!(message.contains("(exit code 128)"), "{message}");
        assert!(message.contains("fatal:"), "{message}");
    }

    #[cfg(unix)]
    fn self_killing_git_stand_in(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let stand_in = directory.join("killed-git");
        fs::write(&stand_in, "#!/bin/sh\nkill -9 $$\n").expect("write Git stand-in");
        let mut permissions = fs::metadata(&stand_in)
            .expect("stand-in metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stand_in, permissions).expect("mark stand-in executable");
        stand_in
    }

    #[cfg(unix)]
    #[test]
    fn command_failures_name_the_terminating_signal() {
        let directory = tempdir().expect("temporary directory");
        let git = Git::new(self_killing_git_stand_in(directory.path()));
        let error = retry_while_wrapper_is_busy(|| {
            git.run(Some(directory.path()), &["status", "--porcelain=v1"])
        })
        .expect_err("a signal death must be an error");
        let message = error.to_string();
        assert!(
            message.contains("killed by signal 9 (SIGKILL)"),
            "{message}"
        );
        assert!(!message.trim_end().ends_with(':'), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn a_signal_killed_probe_is_not_reported_as_an_absent_revision() {
        let directory = tempdir().expect("temporary directory");
        let git = Git::new(self_killing_git_stand_in(directory.path()));
        let error = retry_while_wrapper_is_busy(|| {
            git.resolve_optional_object(directory.path(), "HEAD^{commit}")
        })
        .expect_err("a signal death must not resolve to an absent revision");
        let message = error.to_string();
        assert!(
            message.contains("killed by signal 9 (SIGKILL)"),
            "{message}"
        );
    }

    #[test]
    fn resolves_commit_and_tree_ids_in_a_normal_repository() {
        let fixture = RepositoryFixture::committed();
        let git = Git::default();

        let attempts_before = git.process_attempts();
        let resolved = git
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve HEAD");
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "one structured Git call must resolve both object IDs"
        );
        // `inspect_repository` resolves HEAD's commit; the tree comes from the
        // report method.
        let repository = git
            .inspect_repository(fixture.path())
            .expect("inspect repository");
        assert_eq!(repository.head_commit, Some(resolved.commit.clone()));
        assert!(repository.head_tree.is_none());
        let repository_with_tree = git
            .inspect_repository_with_head_tree(fixture.path())
            .expect("inspect repository with HEAD tree");
        assert_eq!(
            repository_with_tree.head_commit,
            Some(resolved.commit.clone())
        );
        assert_eq!(repository_with_tree.head_tree, Some(resolved.tree.clone()));
        let reported = git
            .inspect_repository_for_report(fixture.path())
            .expect("inspect for report");
        assert_eq!(reported.head_commit, Some(resolved.commit));
        assert_eq!(reported.head_tree, Some(resolved.tree));
        assert_eq!(reported.clean, Some(true));
    }

    #[test]
    fn resolved_revision_parser_requires_exact_nul_delimited_ids() {
        let oid = b"0123456789012345678901234567890123456789";
        let mut valid = Vec::new();
        valid.extend_from_slice(oid);
        valid.push(0);
        valid.extend_from_slice(oid);
        valid.push(0);
        let resolved = super::parse_resolved_revision_output(&valid).expect("parse exact record");
        assert_eq!(
            resolved.commit.as_str(),
            "0123456789012345678901234567890123456789"
        );
        assert_eq!(
            resolved.tree.as_str(),
            "0123456789012345678901234567890123456789"
        );

        valid.extend_from_slice(b"unexpected");
        assert!(
            super::parse_resolved_revision_output(&valid).is_err(),
            "trailing output must fail closed"
        );
        assert!(
            super::parse_resolved_revision_output(oid).is_err(),
            "missing delimiters must fail closed"
        );
    }

    /// Spawning a freshly written script can fail with ETXTBSY when a
    /// concurrent test forks while the writer's descriptor is still duplicated
    /// into a not-yet-exec'd child. A busy failure means the script never ran,
    /// so retrying the whole attempt is safe, and one success proves the racy
    /// descriptor is gone for the rest of the test.
    #[cfg(unix)]
    fn retry_while_wrapper_is_busy<T>(
        mut attempt: impl FnMut() -> Result<T, crate::GitError>,
    ) -> Result<T, crate::GitError> {
        let mut retries = 0;
        loop {
            match attempt() {
                Err(crate::GitError::Start { ref source, .. })
                    if source.kind() == std::io::ErrorKind::ExecutableFileBusy && retries < 50 =>
                {
                    retries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => break result,
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolves_tree_from_commit_even_when_the_revision_moves() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = RepositoryFixture::committed();
        let original = Git::default()
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("original revision");
        fs::write(fixture.path().join("tracked.txt"), "new tree\n").expect("change tree");
        git(fixture.path(), &["commit", "-am", "second", "--quiet"]);
        let wrapper = fixture.path().join("moving-git");
        fs::write(&wrapper, "#!/bin/sh\nif [ \"$1\" = show ]; then\n  git \"$@\" || exit\n  git update-ref refs/heads/moving HEAD\nelse\n  exec git \"$@\"\nfi\n")
            .expect("write Git wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("executable");

        let moving_git = Git::new(wrapper);
        let resolved = retry_while_wrapper_is_busy(|| {
            // Re-pin the moving ref so the scenario is intact even if a retry
            // follows a partial resolution.
            git(
                fixture.path(),
                &["branch", "--force", "moving", original.commit.as_str()],
            );
            moving_git.resolve_revision(fixture.path(), OsStr::new("moving"))
        })
        .expect("resolve moving ref");
        assert_eq!(resolved.commit, original.commit);
        assert_eq!(resolved.tree, original.tree);
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

    #[cfg(unix)]
    #[test]
    fn materialization_ignores_mutable_repository_attributes_and_filters() {
        let fixture = RepositoryFixture::committed();
        let git_client = Git::default();
        let tree = git_client
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap()
            .tree;
        // These inputs can appear after core has validated the checkout profile.
        git(
            fixture.path(),
            &[
                "config",
                "filter.changed.smudge",
                "sed s/tracked/poisoned/g",
            ],
        );
        fs::write(
            fixture.path().join(".git/info/attributes"),
            "*.txt filter=changed\n",
        )
        .unwrap();
        let output = tempdir().unwrap();
        let index_dir = tempdir().unwrap();
        git_client
            .materialize_tree(
                fixture.path(),
                &tree,
                output.path(),
                &index_dir.path().join("index"),
            )
            .unwrap();
        assert_eq!(
            fs::read(output.path().join("tracked.txt")).unwrap(),
            b"tracked\n"
        );
    }

    #[test]
    fn materialization_uses_captured_line_endings() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join(".gitattributes"), "*.txt text\n").unwrap();
        git(fixture.path(), &["add", ".gitattributes"]);
        git(
            fixture.path(),
            &["commit", "--quiet", "-m", "text attributes"],
        );
        let client = Git::default();
        let tree = client
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap()
            .tree;
        let captured = vec![("core.eol".to_owned(), b"lf".to_vec())];
        git(fixture.path(), &["config", "core.eol", "crlf"]);
        git(fixture.path(), &["config", "core.autocrlf", "true"]);
        let output = tempdir().unwrap();
        let index = tempdir().unwrap();
        client
            .materialize_tree_with_config(
                fixture.path(),
                &tree,
                output.path(),
                &index.path().join("index"),
                &captured,
            )
            .unwrap();
        assert_eq!(
            fs::read(output.path().join("tracked.txt")).unwrap(),
            b"tracked\n"
        );
    }

    /// `remove_worktree` has always passed `--`; the force variant did not, so
    /// a selector beginning with `-` was parsed as a bundle of short options
    /// instead of a worktree. Force is the path that bypasses Git's own
    /// dirty-worktree checks, so it is the one that must be built most
    /// defensively.
    #[test]
    fn force_removal_accepts_a_worktree_selector_beginning_with_a_dash() {
        let fixture = RepositoryFixture::committed();
        let linked = fixture.path().join("-dash");
        let git = Git::default();

        git.add_worktree_no_checkout(
            fixture.path(),
            &linked,
            OsStr::new("HEAD"),
            WorktreeHead::NewBranch(OsStr::new("feature/dash")),
        )
        .expect("add no-checkout worktree");
        assert!(linked.join(".git").is_file());

        // Relative, so the argument Git receives keeps its leading dash.
        git.remove_worktree_force(fixture.path(), Path::new("-dash"))
            .expect("force-remove a dash-leading worktree");
        assert!(!linked.exists());
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
        let attempts_before = git.process_attempts();
        assert_eq!(
            git.local_branch_target(fixture.path(), OsStr::new("feature/suppressed"))
                .expect("read branch"),
            Some(revision.commit.clone())
        );
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "one show-ref call must both detect and resolve an exact local branch"
        );

        fs::write(linked.join("tracked.txt"), "tracked\n").expect("materialize linked file");
        let attempts_before = git.process_attempts();
        git.synchronize_worktree_index(&linked)
            .expect("synchronize index");
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "index synchronization must stay a single Git invocation; the \
             paired clean check performs the refresh"
        );
        assert!(git.worktree_is_clean(&linked).expect("check clean"));

        fs::write(linked.join("tracked.txt"), "changed\n").expect("modify linked file");
        assert!(!git.worktree_is_clean(&linked).expect("check dirty"));
        git.refresh_worktree_index(&linked)
            .expect("refresh index while worktree is dirty");
        assert!(!git.worktree_is_clean(&linked).expect("still dirty"));
        fs::write(linked.join("tracked.txt"), "tracked\n").expect("restore linked file");
        assert!(git.worktree_is_clean(&linked).expect("check restored"));
        assert!(git.worktree_is_pristine(&linked).expect("check pristine"));

        fs::write(fixture.path().join(".git/info/exclude"), "ignored\n")
            .expect("configure ignored fixture path");
        fs::write(linked.join("ignored"), "private build output\n").expect("write ignored file");
        assert!(git.worktree_is_clean(&linked).expect("ignored stays clean"));
        assert!(
            !git.worktree_is_pristine(&linked)
                .expect("ignored is not safe to discard")
        );
        fs::remove_file(linked.join("ignored")).expect("remove ignored file");
        assert!(git.worktree_is_pristine(&linked).expect("pristine again"));

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
    fn colon_revisions_are_peeled_after_they_resolve() {
        // Appending `^{commit}` turned `:/second` into a search for the text
        // `second^{commit}` and `HEAD:tracked.txt` into a path ending in it,
        // so Git's own message search failed and a blob was reported as a
        // Git failure rather than as a revision that names no commit.
        let fixture = RepositoryFixture::committed();
        git(
            fixture.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "second"],
        );
        let git_runner = Git::default();
        let head = git_runner
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve HEAD");

        assert_eq!(
            git_runner
                .resolve_requested_revision(fixture.path(), OsStr::new(":/second"))
                .expect("search commit messages"),
            Some(head)
        );
        assert_eq!(
            git_runner
                .resolve_requested_revision(fixture.path(), OsStr::new("HEAD:tracked.txt"))
                .expect("classify a blob"),
            None
        );
        assert_eq!(
            git_runner
                .resolve_requested_revision(fixture.path(), OsStr::new(":/no such message"))
                .expect("classify a failed search"),
            None
        );
    }

    #[test]
    fn requested_ranges_and_negations_name_no_commit() {
        // `git show` walks these, so `HEAD~1..HEAD` once resolved to HEAD
        // where Git's own `worktree add` refuses it as an invalid reference.
        let fixture = RepositoryFixture::committed();
        git(
            fixture.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "second"],
        );
        let git_runner = Git::default();
        for revision in ["HEAD~1..HEAD", "HEAD~1...HEAD", "HEAD...HEAD~1", "^HEAD~1"] {
            assert_eq!(
                git_runner
                    .resolve_requested_revision(fixture.path(), OsStr::new(revision))
                    .expect("classify a walk expression"),
                None,
                "{revision} must not resolve to a single commit"
            );
        }

        let attempts_before = git_runner.process_attempts();
        let head = git_runner
            .resolve_requested_revision(fixture.path(), OsStr::new("HEAD~1"))
            .expect("resolve an ordinary revision");
        assert!(head.is_some());
        assert_eq!(
            git_runner.process_attempts() - attempts_before,
            1,
            "an ordinary revision must still resolve in one process"
        );
    }

    #[test]
    fn local_branch_lookup_ignores_refs_that_only_end_with_the_branch_ref() {
        // show-ref patterns match ref-name suffixes, so these matched
        // `refs/heads/topic`: an absent branch read as present, and a present
        // one printed two object IDs and failed to parse.
        let fixture = RepositoryFixture::committed();
        let head = Git::default()
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve HEAD")
            .commit;
        git(
            fixture.path(),
            &["update-ref", "refs/remotes/origin/refs/heads/topic", "HEAD"],
        );
        git(
            fixture.path(),
            &["update-ref", "refs/heads/refs/heads/topic", "HEAD"],
        );
        let git_runner = Git::default();

        assert_eq!(
            git_runner
                .local_branch_target(fixture.path(), OsStr::new("topic"))
                .expect("query a branch only other refs end with"),
            None
        );

        git(fixture.path(), &["branch", "topic", "HEAD"]);
        let attempts_before = git_runner.process_attempts();
        assert_eq!(
            git_runner
                .local_branch_target(fixture.path(), OsStr::new("topic"))
                .expect("resolve a branch that other refs end with"),
            Some(head)
        );
        assert_eq!(
            git_runner.process_attempts() - attempts_before,
            1,
            "the exact match must still need only one show-ref"
        );
    }

    #[test]
    fn local_branch_lookup_does_not_match_a_nested_ref_prefix() {
        let fixture = RepositoryFixture::committed();
        git(fixture.path(), &["branch", "topic/child", "HEAD"]);
        let git = Git::default();

        let attempts_before = git.process_attempts();
        assert_eq!(
            git.local_branch_target(fixture.path(), OsStr::new("topic"))
                .expect("query absent branch prefix"),
            None
        );
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "an absent exact branch must remain one lookup"
        );
    }

    #[test]
    fn sparse_index_synchronization_leaves_refresh_to_the_paired_clean_check() {
        let fixture = RepositoryFixture::committed();
        fs::create_dir(fixture.path().join("selected")).expect("create selected directory");
        fs::create_dir(fixture.path().join("omitted")).expect("create omitted directory");
        fs::write(fixture.path().join("selected/file.txt"), "selected\n")
            .expect("write selected file");
        fs::write(fixture.path().join("omitted/file.txt"), "omitted\n")
            .expect("write omitted file");
        git(fixture.path(), &["add", "--", "selected", "omitted"]);
        git(
            fixture.path(),
            &["commit", "--quiet", "-m", "sparse fixture"],
        );

        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent.path().join("sparse");
        let git = Git::default();
        git.add_worktree_no_checkout(
            fixture.path(),
            &linked,
            OsStr::new("HEAD"),
            WorktreeHead::NewBranch(OsStr::new("feature/sparse-suppressed")),
        )
        .expect("add no-checkout worktree");
        fs::create_dir(linked.join("selected")).expect("materialize selected directory");
        fs::write(linked.join("tracked.txt"), "tracked\n").expect("materialize root file");
        fs::write(linked.join("selected/file.txt"), "selected\n")
            .expect("materialize selected file");

        let attempts_before = git.process_attempts();
        git.synchronize_sparse_worktree_index(&linked, &["selected".to_owned()])
            .expect("synchronize sparse index");
        assert_eq!(
            git.process_attempts() - attempts_before,
            3,
            "sparse index synchronization needs set, reset, and reapply; the paired clean check performs the refresh"
        );
        assert!(git.worktree_is_clean(&linked).expect("check clean"));
        let tags = git
            .run_text(Some(&linked), &["ls-files", "-t"], "sparse index tags")
            .expect("read sparse index tags");
        assert!(tags.contains("H selected/file.txt"), "{tags}");
        assert!(tags.contains("S omitted/file.txt"), "{tags}");

        fs::write(linked.join("selected/file.txt"), "changed\n").expect("modify selected file");
        assert!(!git.worktree_is_clean(&linked).expect("check dirty"));
        git.remove_worktree_force(fixture.path(), &linked)
            .expect("remove linked worktree");
        git.delete_branch_force(fixture.path(), OsStr::new("feature/sparse-suppressed"))
            .expect("delete rollback branch");
    }

    #[test]
    fn creates_suppressed_checkout_for_an_existing_branch() {
        let fixture = RepositoryFixture::committed();
        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent.path().join("existing");
        let git_client = Git::default();
        git(fixture.path(), &["branch", "feature/existing"]);

        git_client
            .add_worktree_no_checkout(
                fixture.path(),
                &linked,
                OsStr::new("feature/existing"),
                WorktreeHead::ExistingBranch(OsStr::new("feature/existing")),
            )
            .expect("add existing branch without checkout");

        assert!(linked.join(".git").is_file());
        assert!(!linked.join("tracked.txt").exists());
        assert_eq!(
            git_client
                .run_text(Some(&linked), &["branch", "--show-current"], "branch")
                .expect("read linked branch"),
            "feature/existing"
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

    #[cfg(unix)]
    #[test]
    fn reads_path_configuration_with_gits_expansion_rules() {
        let fixture = RepositoryFixture::committed();
        git(
            fixture.path(),
            &["config", "core.hooksPath", "~/riftri-hooks"],
        );
        let git = Git::default();

        assert_eq!(
            git.config_value(fixture.path(), "core.hooksPath")
                .expect("read raw hooks path")
                .as_deref(),
            Some(b"~/riftri-hooks".as_slice())
        );
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set for Git tests"));
        assert_eq!(
            git.expand_config_path(OsStr::new("~/riftri-hooks"))
                .expect("expand hooks path"),
            home.join("riftri-hooks")
        );
        assert_eq!(
            git.expand_config_path(OsStr::new("relative/hooks"))
                .expect("preserve relative hooks path"),
            PathBuf::from("relative/hooks")
        );
    }

    #[test]
    fn conditional_targets_preserve_native_paths_and_reject_missing_files() {
        let fixture = RepositoryFixture::committed();
        let included = fixture.path().join("identity-é");
        fs::write(&included, "[user]\n email = work@example.invalid\n").unwrap();
        let git = Git::default();
        assert!(
            !git.config_values(fixture.path(), &["user.email"])
                .unwrap()
                .has_conditional_includes
        );
        git.set_local_config(
            fixture.path(),
            "includeIf.gitdir:never.path",
            included.canonicalize().unwrap().as_os_str(),
        )
        .unwrap();
        assert!(
            git.config_values(fixture.path(), &["user.email"])
                .unwrap()
                .has_conditional_includes
        );
        assert!(
            git.conditional_config_has_only(fixture.path(), &["user.email"])
                .unwrap()
        );
        fs::remove_file(included).unwrap();
        assert!(
            !git.conditional_config_has_only(fixture.path(), &["user.email"])
                .unwrap()
        );
    }

    #[test]
    fn batches_configuration_in_one_process_without_changing_values() {
        let fixture = RepositoryFixture::unborn();
        let keys = [
            "riftri-test.one",
            "riftri-test.two",
            "riftri-test.three",
            "riftri-test.four",
            "riftri-test.five",
            "riftri-test.six",
            "riftri-test.seven",
            "riftri-test.eight",
            "riftri-test.nine",
            "riftri-test.ten",
            "riftri-test.eleven",
        ];
        for key in keys {
            git(fixture.path(), &["config", key, "same answer"]);
        }
        let git = Git::default();
        let before = git.process_attempts();
        let values = git
            .config_values(fixture.path(), &keys)
            .expect("batch config")
            .values;
        assert_eq!(git.process_attempts() - before, 1);
        for key in keys {
            assert_eq!(
                values.get(key).cloned(),
                git.config_value(fixture.path(), key).unwrap()
            );
        }
    }

    #[test]
    #[ignore = "manual release-mode benchmark; reports timings without a host-load-sensitive threshold"]
    fn reports_batched_configuration_read_latency() {
        let fixture = RepositoryFixture::unborn();
        let git = Git::default();
        let keys = [
            "core.attributesfile",
            "core.sparsecheckout",
            "core.sparsecheckoutcone",
            "core.autocrlf",
            "core.eol",
            "core.symlinks",
            "core.filemode",
            "core.ignorecase",
            "core.precomposeunicode",
            "core.protecthfs",
            "core.protectntfs",
        ];
        let expected = keys
            .iter()
            .filter_map(|key| {
                git.config_value(fixture.path(), key)
                    .unwrap()
                    .map(|value| (key.to_string(), value))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            git.config_values(fixture.path(), &keys).unwrap().values,
            expected
        );
        let mut individual_seconds = Vec::new();
        let mut batched_seconds = Vec::new();
        for round in 0..30 {
            for batched in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let attempts = git.process_attempts();
                let started = std::time::Instant::now();
                let values = if batched {
                    git.config_values(fixture.path(), &keys).unwrap().values
                } else {
                    keys.iter()
                        .filter_map(|key| {
                            git.config_value(fixture.path(), key)
                                .unwrap()
                                .map(|value| (key.to_string(), value))
                        })
                        .collect()
                };
                let seconds = started.elapsed().as_secs_f64();
                assert_eq!(values, expected);
                assert_eq!(
                    git.process_attempts() - attempts,
                    if batched { 1 } else { keys.len() }
                );
                if batched {
                    batched_seconds.push(seconds);
                } else {
                    individual_seconds.push(seconds);
                }
            }
        }
        println!(
            "{{\"individual_seconds\":{individual_seconds:?},\"batched_seconds\":{batched_seconds:?}}}"
        );
    }

    #[test]
    fn batched_configuration_preserves_raw_empty_implicit_and_duplicate_values() {
        let fixture = RepositoryFixture::unborn();
        let config = fixture.path().join(".git/config");
        let mut bytes = fs::read(&config).unwrap();
        bytes.extend_from_slice(b"\n[Riftri-Test]\n duplicate = first\n DUPLICATE = last\n empty =\n implicit\n multiline = \"one\\ntwo\\tthree\"\n raw = \"raw-\xff\"\n false = false\n");
        fs::write(&config, bytes).unwrap();
        let before = fs::read(&config).unwrap();
        let keys = [
            "RIFTRI-TEST.DUPLICATE",
            "riftri-test.empty",
            "riftri-test.implicit",
            "riftri-test.multiline",
            "riftri-test.raw",
            "riftri-test.false",
            "riftri-test.missing",
        ];
        let git = Git::default();
        let values = git.config_values(fixture.path(), &keys).unwrap().values;
        for key in keys {
            assert_eq!(
                values.get(&key.to_ascii_lowercase()).cloned(),
                git.config_value(fixture.path(), key).unwrap(),
                "{key}"
            );
        }
        assert_eq!(values["riftri-test.duplicate"], b"last");
        assert_eq!(values["riftri-test.empty"], b"");
        assert_eq!(values["riftri-test.implicit"], b"");
        assert_eq!(values["riftri-test.multiline"], b"one\ntwo\tthree");
        assert_eq!(values["riftri-test.raw"], b"raw-\xff");
        assert_eq!(values["riftri-test.false"], b"false");
        assert!(!values.contains_key("riftri-test.missing"));
        assert_eq!(
            fs::read(config).unwrap(),
            before,
            "read must not alter config"
        );
    }

    #[test]
    fn configuration_value_origins_preserve_structured_raw_records() {
        let fixture = RepositoryFixture::unborn();
        let git = Git::default();
        git.set_local_config(fixture.path(), "core.autocrlf", OsStr::new("true"))
            .unwrap();

        let origins = git
            .config_value_origins(fixture.path(), "core.autocrlf")
            .expect("read config origins");
        assert!(origins.iter().any(|origin| {
            origin.scope == b"local"
                && origin.origin == b"file:.git/config"
                && origin.value == b"true"
        }));

        assert_eq!(
            super::parse_config_value_origins(
                b"system\0file:/etc/gitconfig\0false\0local\0file:.git/conf\xffg\0true\xff\0"
            )
            .unwrap(),
            vec![
                super::ConfigValueOrigin {
                    scope: b"system".to_vec(),
                    origin: b"file:/etc/gitconfig".to_vec(),
                    value: b"false".to_vec(),
                },
                super::ConfigValueOrigin {
                    scope: b"local".to_vec(),
                    origin: b"file:.git/conf\xffg".to_vec(),
                    value: b"true\xff".to_vec(),
                },
            ]
        );
    }

    #[test]
    fn configuration_value_origin_parser_rejects_incomplete_records() {
        for output in [
            b"local\0file:.git/config\0true".as_slice(),
            b"local\0file:.git/config\0".as_slice(),
            b"local\0file:.git/config\0true\0extra\0".as_slice(),
        ] {
            assert!(
                super::parse_config_value_origins(output).is_err(),
                "{output:?}"
            );
        }
    }

    #[test]
    fn batched_configuration_handles_missing_exact_keys_and_fresh_reads() {
        let fixture = RepositoryFixture::unborn();
        let git = Git::default();
        assert!(
            git.config_values(fixture.path(), &[])
                .unwrap()
                .values
                .is_empty()
        );
        assert_eq!(git.process_attempts(), 0);
        git.set_local_config(fixture.path(), "riftri-test.other", OsStr::new("unrelated"))
            .unwrap();
        git.set_local_config(
            fixture.path(),
            "riftri-test.valueExtra",
            OsStr::new("unrelated"),
        )
        .unwrap();
        let values = git
            .config_values(fixture.path(), &["riftri-test.value"])
            .unwrap()
            .values;
        assert!(values.is_empty());
        git.set_local_config(fixture.path(), "riftri-test.value", OsStr::new("first"))
            .unwrap();
        let first = git
            .config_values(fixture.path(), &["riftri-test.value"])
            .unwrap()
            .values;
        git.set_local_config(fixture.path(), "riftri-test.value", OsStr::new("changed"))
            .unwrap();
        let next = git
            .config_values(fixture.path(), &["riftri-test.value"])
            .unwrap()
            .values;
        assert_eq!(first["riftri-test.value"], b"first");
        assert_eq!(next["riftri-test.value"], b"changed");
    }

    #[test]
    fn batched_configuration_rejects_invalid_selectors_and_git_failures() {
        let fixture = RepositoryFixture::unborn();
        let git = Git::default();
        for key in [
            "",
            "core",
            "core.",
            ".value",
            "core.*",
            "core.value|user.name",
            "filter..clean",
            "core.1value",
        ] {
            assert!(
                git.config_values(fixture.path(), &[key]).is_err(),
                "{key:?}"
            );
        }
        assert_eq!(git.process_attempts(), 0);
        fs::write(fixture.path().join(".git/config"), "[broken\n").unwrap();
        assert!(git.config_value(fixture.path(), "core.filemode").is_err());
        assert!(
            git.config_values(fixture.path(), &["core.filemode"])
                .is_err()
        );
        let unavailable = Git::new(fixture.path().join("missing-git"));
        assert!(matches!(
            unavailable.config_values(fixture.path(), &["core.filemode"]),
            Err(super::GitError::Start { .. })
        ));
    }

    #[test]
    fn batched_configuration_parser_rejects_incomplete_or_unrequested_records() {
        let keys = ["core.eol".to_owned()];
        for output in [
            b"core.eol\nlf".as_slice(),
            b"\0",
            b"core.eol\nlf\0\0",
            b"user.name\nvalue\0",
            b"core.eol\nlf\0broken\0",
        ] {
            assert!(
                super::parse_config_values(output, &keys).is_err(),
                "{output:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn batched_configuration_preserves_scope_include_and_worktree_precedence() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = RepositoryFixture::committed();
        fs::write(
            fixture.path().join("global-config"),
            "[riftri-test]\n scope = global\n global = yes\n[riftri]\n enabled = true\n",
        )
        .unwrap();
        fs::write(
            fixture.path().join("included-config"),
            "[riftri-test]\n included = yes\n order = included\n",
        )
        .unwrap();
        git(fixture.path(), &["config", "riftri-test.scope", "local"]);
        git(
            fixture.path(),
            &["config", "riftri-test.order", "before-include"],
        );
        git(
            fixture.path(),
            &["config", "include.path", "../included-config"],
        );
        // `git config --add` inserts into the existing section, which is still
        // before the include. Append explicitly to exercise a later override.
        let config_path = fixture.path().join(".git/config");
        let config = fs::read_to_string(&config_path).unwrap();
        fs::write(
            &config_path,
            format!("{config}\n[riftri-test]\n order = after-include\n"),
        )
        .unwrap();
        git(
            fixture.path(),
            &["config", "extensions.worktreeConfig", "true"],
        );
        let linked = fixture.path().join("linked");
        git(
            fixture.path(),
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        git(
            &linked,
            &["config", "--worktree", "riftri-test.scope", "worktree"],
        );
        let wrapper = fixture.path().join("scoped-git");
        fs::write(&wrapper, "#!/bin/sh\nfixture=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd) || exit\nexport GIT_CONFIG_NOSYSTEM=1\nexport GIT_CONFIG_GLOBAL=\"$fixture/global-config\"\nexport GIT_CONFIG_COUNT=1\nexport GIT_CONFIG_KEY_0=riftri-test.command\nexport GIT_CONFIG_VALUE_0=environment\nexec git -c riftri-test.command=command-line \"$@\"\n").unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let scoped = Git::new(wrapper);
        retry_while_wrapper_is_busy(|| scoped.config_value(fixture.path(), "riftri.enabled"))
            .expect("warm up the scoped Git wrapper");
        let keys = [
            "riftri-test.scope",
            "riftri-test.global",
            "riftri-test.included",
            "riftri-test.order",
            "riftri-test.command",
            "riftri.enabled",
        ];
        for repository in [fixture.path(), linked.as_path()] {
            let values = scoped.config_values(repository, &keys).unwrap().values;
            for key in keys {
                assert_eq!(
                    values.get(key).cloned(),
                    scoped.config_value(repository, key).unwrap(),
                    "{key}"
                );
            }
            assert_eq!(values["riftri-test.global"], b"yes");
            assert_eq!(values["riftri-test.included"], b"yes");
            assert_eq!(values["riftri-test.order"], b"after-include");
            assert_eq!(values["riftri-test.command"], b"command-line");
            assert_eq!(values["riftri.enabled"], b"true");
            assert_eq!(
                scoped
                    .local_config_bool(repository, "riftri.enabled")
                    .unwrap(),
                None,
                "global consent must not become local consent"
            );
        }
        assert_eq!(
            scoped.config_values(fixture.path(), &keys).unwrap().values["riftri-test.scope"],
            b"local"
        );
        assert_eq!(
            scoped.config_values(&linked, &keys).unwrap().values["riftri-test.scope"],
            b"worktree"
        );
        git(
            fixture.path(),
            &["worktree", "remove", linked.to_str().unwrap()],
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

        let attempts_before = git.process_attempts();
        git.unset_local_config(fixture.path(), "riftri.enabled")
            .expect("disable repository");
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "removing a present key should not require a separate existence query"
        );
        assert_eq!(
            git.local_config_value(fixture.path(), "riftri.enabled")
                .expect("read removed local configuration"),
            None
        );
        let attempts_before = git.process_attempts();
        git.unset_local_config(fixture.path(), "riftri.enabled")
            .expect("missing local configuration is already removed");
        assert_eq!(
            git.process_attempts() - attempts_before,
            1,
            "a missing key should use the same direct removal attempt"
        );

        let first = fixture.path().join("state one");
        let second = fixture.path().join("state-two");
        git.add_local_config_path(fixture.path(), "riftri.stateDirectory", &first)
            .expect("register first state path");
        git.add_local_config_path(fixture.path(), "riftri.stateDirectory", &second)
            .expect("register second state path");
        assert_eq!(
            git.local_config_paths(fixture.path(), "riftri.stateDirectory")
                .expect("read registered state paths"),
            vec![first.clone(), second.clone()]
        );

        git.unset_local_config_value(fixture.path(), "riftri.stateDirectory", first.as_os_str())
            .expect("remove one registered state path");
        assert_eq!(
            git.local_config_paths(fixture.path(), "riftri.stateDirectory")
                .expect("read remaining registered state paths"),
            vec![second]
        );
    }

    #[test]
    fn detects_external_attributes_for_tree_paths() {
        let fixture = RepositoryFixture::committed();
        let attributes = fixture.path().join(".git/info/attributes");
        fs::write(&attributes, "*.txt riftri-test\n").expect("write info attributes");
        let git = Git::default();

        assert_eq!(
            git.info_attributes_path(fixture.path())
                .expect("resolve info attributes")
                .canonicalize()
                .expect("canonical Git attributes path"),
            attributes
                .canonicalize()
                .expect("canonical fixture attributes path"),
        );

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

    #[test]
    fn reads_attributes_from_the_requested_exact_tree() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join(".gitattributes"), "*.txt text eol=lf\n")
            .expect("write tree attributes");
        git(fixture.path(), &["add", "--", ".gitattributes"]);
        git(
            fixture.path(),
            &["commit", "--quiet", "-m", "attributes tree"],
        );
        let git_handle = Git::default();
        let attributed_tree = git_handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve attributed tree")
            .tree;
        git(fixture.path(), &["reset", "--hard", "--quiet", "HEAD^"]);

        let attributes = git_handle
            .in_tree_attributes_for_paths(
                fixture.path(),
                &attributed_tree,
                &[Path::new("tracked.txt").to_path_buf()],
            )
            .expect("read exact-tree attributes");

        assert!(
            attributes
                .iter()
                .any(|attribute| attribute.name == b"text" && attribute.value == b"set")
        );
        assert!(
            attributes
                .iter()
                .any(|attribute| attribute.name == b"eol" && attribute.value == b"lf")
        );
    }

    #[test]
    fn isolates_in_tree_attributes_from_configured_external_attributes() {
        let fixture = RepositoryFixture::committed();
        let external = fixture.path().join("external-attributes");
        fs::write(&external, "*.txt filter=external\n").expect("write external attributes");
        git(
            fixture.path(),
            &[
                "config",
                "core.attributesFile",
                external.to_str().expect("UTF-8 temporary path"),
            ],
        );
        let git_handle = Git::default();
        let tree = git_handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve tree")
            .tree;
        let paths = [Path::new("tracked.txt").to_path_buf()];

        let effective = git_handle
            .effective_attributes_for_tree_paths(fixture.path(), &tree, &paths)
            .expect("read effective attributes");
        let in_tree = git_handle
            .in_tree_attributes_for_paths(fixture.path(), &tree, &paths)
            .expect("read isolated attributes");

        assert!(
            effective
                .iter()
                .any(|attribute| { attribute.name == b"filter" && attribute.value == b"external" })
        );
        assert!(in_tree.is_empty());
    }

    #[test]
    fn shared_tree_index_preserves_isolation_and_runs_read_tree_once() {
        let fixture = RepositoryFixture::committed();
        let external = fixture.path().join("external-attributes");
        fs::write(&external, "*.txt filter=external\n").expect("write external attributes");
        git(
            fixture.path(),
            &[
                "config",
                "core.attributesFile",
                external.to_str().expect("UTF-8 temporary path"),
            ],
        );
        let git_handle = Git::default();
        let tree = git_handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .expect("resolve tree")
            .tree;
        let paths = (0..300)
            .map(|index| PathBuf::from(format!("日本語 path-{index}.txt")))
            .collect::<Vec<_>>();
        let borrowed = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();

        let attempts_before = git_handle.process_attempts();
        let index = git_handle
            .tree_attribute_index(fixture.path(), &tree)
            .expect("load shared tree index");
        let in_tree = git_handle
            .in_tree_attributes_for_index(fixture.path(), &index, &borrowed)
            .expect("read isolated attributes");
        let effective = git_handle
            .effective_attributes_for_index(fixture.path(), &index, &borrowed)
            .expect("read effective attributes");
        assert_eq!(
            git_handle.process_attempts() - attempts_before,
            3,
            "both attribute passes must share one read-tree invocation"
        );

        assert!(
            effective
                .iter()
                .any(|attribute| { attribute.name == b"filter" && attribute.value == b"external" })
        );
        assert!(in_tree.is_empty());

        assert_eq!(
            in_tree,
            git_handle
                .in_tree_attributes_for_paths(fixture.path(), &tree, &paths)
                .expect("read isolated attributes through the wrapper")
        );
        assert_eq!(
            effective,
            git_handle
                .effective_attributes_for_tree_paths(fixture.path(), &tree, &paths)
                .expect("read effective attributes through the wrapper")
        );
    }

    #[test]
    fn paired_attribute_queries_preserve_isolation_native_paths_and_query_counts() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
        git(fixture.path(), &["add", ".gitattributes"]);
        git(fixture.path(), &["commit", "-m", "attributes"]);
        let external = fixture.path().join("external-attributes");
        fs::write(&external, "*.txt filter=external\n").unwrap();
        git(
            fixture.path(),
            &["config", "core.attributesFile", external.to_str().unwrap()],
        );
        let handle = Git::default();
        let tree = handle
            .resolve_revision(fixture.path(), OsStr::new("HEAD"))
            .unwrap()
            .tree;
        let index = handle.tree_attribute_index(fixture.path(), &tree).unwrap();
        let mut paths = (0..1000)
            .map(|i| PathBuf::from(format!("{}/日本語-{i}.txt", "d".repeat(240))))
            .collect::<Vec<_>>();
        paths.push(PathBuf::from("-tab\tline\n.txt"));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            paths.push(PathBuf::from(OsString::from_vec(
                b"native-\xff.txt".to_vec(),
            )));
        }
        let expected_in_tree = handle
            .in_tree_attributes_for_index(fixture.path(), &index, &paths)
            .unwrap();
        let expected_effective = handle
            .effective_attributes_for_index(fixture.path(), &index, &paths)
            .unwrap();
        assert_ne!(expected_in_tree, expected_effective);
        let borrowed = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let before = handle.process_attempts();
        let (in_tree, effective) = handle
            .attribute_pair_for_index(fixture.path(), &index, &borrowed, true)
            .unwrap();
        assert_eq!(handle.process_attempts() - before, 2);
        assert_eq!(in_tree, expected_in_tree);
        assert_eq!(effective, expected_effective);
        let before = handle.process_attempts();
        let (in_tree, effective) = handle
            .attribute_pair_for_index(fixture.path(), &index, &borrowed, false)
            .unwrap();
        assert_eq!(handle.process_attempts() - before, 1);
        assert!(in_tree.is_empty());
        assert_eq!(effective, expected_effective);
        let before = handle.process_attempts();
        assert_eq!(
            handle
                .attribute_pair_for_index(fixture.path(), &index, &borrowed[..0], true)
                .unwrap(),
            (vec![], vec![])
        );
        assert_eq!(handle.process_attempts(), before);
    }

    #[test]
    fn borrowed_command_input_preserves_git_failure_when_pipe_closes_early() {
        let fixture = RepositoryFixture::committed();
        let handle = Git::default();
        let input = vec![b'x'; 1024 * 1024];
        for _ in 0..2 {
            let error = handle
                .run_os_with_input(
                    Some(fixture.path()),
                    &[OsString::from("riftri-nonexistent-command")],
                    &[],
                    &input,
                )
                .unwrap_err();
            assert!(
                matches!(error, super::GitError::CommandFailed { .. }),
                "{error}"
            );
        }
        assert!(input.iter().all(|byte| *byte == b'x'));
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

    #[test]
    fn attribute_parser_preserves_error_precedence_and_empty_values() {
        // The whole record shape is checked before individual empty fields.
        let error = parse_attribute_records(b"\0text\0value\0extra\0").unwrap_err();
        assert!(matches!(
            error,
            super::GitError::InvalidOutput {
                context: "Git attribute records",
                ..
            }
        ));
        let error = parse_attribute_records(b"\0text\0value\0").unwrap_err();
        assert!(matches!(
            error,
            super::GitError::InvalidOutput {
                context: "Git attribute record",
                ..
            }
        ));
        let records =
            parse_attribute_records(b"a\0text\0\0a\0text\0auto\0b\0binary\0set\0").unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].path, PathBuf::from("a"));
        assert_eq!(records[0].value, b"");
        assert_eq!(records[1].value, b"auto");
        assert_eq!(records[2].name, b"binary");
    }

    #[test]
    fn attribute_parser_matches_legacy_for_small_inputs() {
        fn legacy(input: &[u8]) -> Result<Vec<super::GitAttribute>, super::GitError> {
            use super::GitError;
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
                .split(|b| *b == 0)
                .collect::<Vec<_>>();
            if fields.len() % 3 != 0 {
                return Err(GitError::InvalidOutput {
                    context: "Git attribute records",
                    detail: "output did not contain path/name/value triples".to_owned(),
                });
            }
            fields
                .as_chunks::<3>()
                .0
                .iter()
                .map(|[path, name, value]| {
                    if path.is_empty() || name.is_empty() {
                        return Err(GitError::InvalidOutput {
                            context: "Git attribute record",
                            detail: "path and attribute name must not be empty".to_owned(),
                        });
                    }
                    Ok(super::GitAttribute {
                        path: PathBuf::from(super::os_string_from_git(path, "attribute path")?),
                        name: name.to_vec(),
                        value: value.to_vec(),
                    })
                })
                .collect()
        }
        for length in 0..=7_u32 {
            for mut number in 0..3_usize.pow(length) {
                let input = (0..length)
                    .map(|_| {
                        let byte = [0, b'a', 255][number % 3];
                        number /= 3;
                        byte
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    parse_attribute_records(&input).map_err(|e| format!("{e:?}")),
                    legacy(&input).map_err(|e| format!("{e:?}")),
                    "input: {input:?}"
                );
            }
        }
    }

    #[test]
    fn attribute_parser_handles_large_record_sets_without_losing_duplicates() {
        let input = b"path\0text\0auto\0".repeat(100_000);
        let attributes = parse_attribute_records(&input).unwrap();
        assert_eq!(attributes.len(), 100_000);
        assert!(
            attributes
                .iter()
                .all(|a| a.path == std::path::Path::new("path")
                    && a.name == b"text"
                    && a.value == b"auto")
        );
    }

    #[test]
    fn checks_large_attribute_path_sets_with_one_git_process() {
        let fixture = RepositoryFixture::committed();
        let git = Git::new("git");
        let paths = (0..300)
            .map(|index| PathBuf::from(format!("path-{index}.txt")))
            .collect::<Vec<_>>();

        let attempts_before = git.process_attempts();
        let attributes = git
            .paths_have_effective_attributes(fixture.path(), &paths)
            .expect("check attributes");

        assert!(!attributes);
        assert_eq!(git.process_attempts() - attempts_before, 1);
    }

    #[test]
    fn attribute_stdin_preserves_long_unicode_and_delimited_paths() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
        git(fixture.path(), &["add", ".gitattributes"]);
        let handle = Git::default();
        // These need not exist: check-attr evaluates indexed patterns. The
        // aggregate would exceed the Windows command-line limit if sent as argv.
        let mut paths = (0..300)
            .map(|i| PathBuf::from(format!("{}/日本語 space-{i}.txt", "d".repeat(240))))
            .collect::<Vec<_>>();
        paths.extend([
            PathBuf::from("-leading.txt"),
            PathBuf::from("tab\tline\n.txt"),
        ]);
        let before = handle.process_attempts();
        let records = handle
            .effective_attributes_for_paths(fixture.path(), &paths)
            .unwrap();
        assert_eq!(handle.process_attempts() - before, 1);
        assert_eq!(records.len(), paths.len() * 2);
        for (path, records) in paths.iter().zip(records.as_chunks::<2>().0) {
            assert!(records.iter().all(|record| &record.path == path));
            assert!(
                records
                    .iter()
                    .any(|r| r.name == b"text" && r.value == b"set")
            );
            assert!(records.iter().any(|r| r.name == b"eol" && r.value == b"lf"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn attribute_stdin_keeps_unpaired_utf16_on_the_native_argument_path() {
        use std::os::windows::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_wide(&[0xd800]));
        assert!(super::attribute_stdin(&[path]).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn attribute_stdin_keeps_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"bad-\xff.txt".to_vec()));
        assert_eq!(
            super::attribute_stdin(&[path.as_path()]).unwrap(),
            b"bad-\xff.txt\0"
        );
        assert_eq!(super::attribute_stdin(&[path]).unwrap(), b"bad-\xff.txt\0");
    }

    #[test]
    #[ignore = "path preparation microbenchmark; no wall-clock threshold"]
    fn reports_borrowed_attribute_paths_latency() {
        let paths = (0..100_000)
            .map(|i| {
                PathBuf::from(format!(
                    "packages/package-{i}/src/日本語 long tracked file.txt"
                ))
            })
            .collect::<Vec<_>>();
        let expected = super::attribute_stdin(&paths).unwrap();
        for round in 0..6 {
            for borrowed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                let input = if borrowed {
                    let prepared = std::hint::black_box(&paths)
                        .iter()
                        .map(PathBuf::as_path)
                        .collect::<Vec<_>>();
                    super::attribute_stdin(&prepared).unwrap()
                } else {
                    let prepared = std::hint::black_box(&paths).clone();
                    super::attribute_stdin(&prepared).unwrap()
                };
                let elapsed = start.elapsed();
                assert_eq!(input, expected);
                println!(
                    "path-preparation round={round} borrowed={borrowed} paths={} elapsed_us={}",
                    paths.len(),
                    elapsed.as_micros()
                );
            }
        }
    }

    #[test]
    #[ignore = "paired input encoding microbenchmark; no wall-clock threshold"]
    fn reports_reused_attribute_input_latency() {
        let paths = (0..100_000)
            .map(|i| {
                PathBuf::from(format!(
                    "packages/package-{i}/src/日本語 long tracked file.txt"
                ))
            })
            .collect::<Vec<_>>();
        let expected = super::attribute_stdin(&paths).unwrap();
        for round in 0..6 {
            for reuse in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                let bytes = if reuse {
                    let input = super::attribute_stdin(std::hint::black_box(&paths)).unwrap();
                    std::hint::black_box(input.as_slice()).len()
                        + std::hint::black_box(input.as_slice()).len()
                } else {
                    let first = super::attribute_stdin(std::hint::black_box(&paths)).unwrap();
                    let bytes = std::hint::black_box(first.as_slice()).len();
                    drop(first);
                    let second = super::attribute_stdin(std::hint::black_box(&paths)).unwrap();
                    bytes + std::hint::black_box(second.as_slice()).len()
                };
                let elapsed = start.elapsed();
                assert_eq!(bytes, expected.len() * 2);
                println!(
                    "attribute-input round={round} reuse={reuse} paths={} elapsed_us={}",
                    paths.len(),
                    elapsed.as_micros()
                );
            }
        }
    }

    #[test]
    #[ignore = "paired attribute query benchmark; no wall-clock threshold"]
    fn reports_attribute_stdin_latency() {
        let fixture = RepositoryFixture::committed();
        fs::write(fixture.path().join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
        git(fixture.path(), &["add", ".gitattributes"]);
        let handle = Git::default();
        let paths = (0..10000)
            .map(|i| PathBuf::from(format!("path-{i}.txt")))
            .collect::<Vec<_>>();
        let expected = handle
            .effective_attributes_for_paths(fixture.path(), &paths)
            .unwrap();
        for round in 0..4 {
            for stdin in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let attempts = handle.process_attempts();
                let start = std::time::Instant::now();
                let records = if stdin {
                    handle
                        .effective_attributes_for_paths(fixture.path(), &paths)
                        .unwrap()
                } else {
                    handle
                        .attributes_for_paths_as_arguments(fixture.path(), &paths, &[], &[])
                        .unwrap()
                };
                let elapsed = start.elapsed();
                assert_eq!(records, expected);
                let starts = handle.process_attempts() - attempts;
                assert_eq!(starts, if stdin { 1 } else { 79 });
                println!(
                    "attribute-query round={round} stdin={stdin} git_starts={starts} elapsed_us={}",
                    elapsed.as_micros()
                );
            }
        }
    }

    /// The identity probe reads a bare flag and the common Git directory from
    /// one invocation, split at the first newline. A repository path that
    /// itself contains a newline is the case that split must not corrupt —
    /// and the reason no second path query may ever join that invocation.
    #[cfg(unix)]
    #[test]
    fn inspects_a_repository_whose_path_contains_a_newline() {
        let parent = tempdir().expect("temporary directory");
        let repository = parent.path().join("line one\nline two");
        fs::create_dir(&repository).expect("create repository directory");
        git(&repository, &["init", "--quiet"]);

        let inspected = Git::default()
            .inspect_repository(&repository)
            .expect("inspect repository with newline in its path");

        assert!(!inspected.is_bare);
        assert_eq!(
            inspected
                .identity
                .common_git_dir
                .canonicalize()
                .expect("canonical common directory"),
            repository
                .join(".git")
                .canonicalize()
                .expect("canonical .git")
        );
        assert_eq!(
            inspected
                .root
                .as_deref()
                .expect("working-tree root")
                .canonicalize()
                .expect("canonical root"),
            repository.canonicalize().expect("canonical repository")
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

    #[test]
    fn resolves_a_linked_git_directory_through_the_worktree_inventory() {
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
        let linked_git_directory = git
            .run_path(
                Some(&linked),
                &["rev-parse", "--path-format=absolute", "--git-dir"],
                "linked Git directory",
            )
            .expect("resolve linked Git directory");

        let root = git
            .worktree_root_from_git_dir(&linked_git_directory)
            .expect("resolve worktree root")
            .expect("non-bare worktree root");

        assert_eq!(
            root.canonicalize().expect("canonical resolved root"),
            fixture.path().canonicalize().expect("canonical main root")
        );
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

    #[test]
    fn porcelain_parser_tolerates_a_worktree_without_resolvable_head() {
        // Exact shape git 2.50.1 emits when a linked worktree's HEAD file is
        // empty, garbage, or an empty symref target: a null HEAD and none of
        // branch, detached, or bare.
        let input = b"worktree /tmp/main\0HEAD 0123456789abcdef0123456789abcdef01234567\0branch refs/heads/main\0\0worktree /tmp/corrupt\0HEAD 0000000000000000000000000000000000000000\0\0";

        let worktrees = parse_worktree_porcelain(input).expect("parse porcelain");

        assert_eq!(worktrees.len(), 2);
        assert!(!worktrees[0].head_unresolvable);
        assert_eq!(
            worktrees[0].branch.as_deref(),
            Some(b"refs/heads/main".as_slice())
        );
        let corrupt = &worktrees[1];
        assert_eq!(corrupt.path, Path::new("/tmp/corrupt"));
        assert!(corrupt.head_unresolvable);
        assert!(corrupt.head.is_none());
        assert!(corrupt.branch.is_none());
        assert!(!corrupt.detached);
        assert!(!corrupt.bare);
    }

    #[test]
    fn lists_a_worktree_whose_head_file_is_corrupt() {
        let fixture = RepositoryFixture::committed();
        let linked_parent = tempdir().expect("linked parent");
        let linked = linked_parent.path().join("corrupt-head");
        let linked_string = linked.to_str().expect("UTF-8 fixture path");
        git(
            fixture.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "corrupt-head",
                linked_string,
            ],
        );
        fs::write(
            fixture.path().join(".git/worktrees/corrupt-head/HEAD"),
            "garbage\n",
        )
        .expect("corrupt linked worktree HEAD");

        let worktrees = Git::default()
            .list_worktrees(fixture.path())
            .expect("a corrupt linked worktree must not poison the listing");

        assert_eq!(worktrees.len(), 2);
        assert!(!worktrees[0].head_unresolvable);
        let corrupt = worktrees
            .iter()
            .find(|worktree| {
                worktree.path.canonicalize().ok().as_deref()
                    == linked.canonicalize().ok().as_deref()
            })
            .expect("corrupt worktree stays listed");
        assert!(corrupt.head_unresolvable);
        assert!(corrupt.head.is_none());
        assert!(corrupt.branch.is_none());
        assert!(!corrupt.detached);
        assert!(!corrupt.bare);
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
