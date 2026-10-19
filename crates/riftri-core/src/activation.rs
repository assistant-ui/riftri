use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(target_os = "linux")]
use std::{fs::File, io};

pub use riftri_git::SHIM_ACTIVE_ENV;
use riftri_git::termination::TerminationError;
use riftri_git::{Git, GitError, RepositoryInfo};
use serde::Serialize;
use thiserror::Error;

use crate::worktree::{
    git_command_root, managed_worktree_state_directory_in, move_worktree_inspected,
    prune_worktrees_inspected, remove_worktree_inspected, repository_state_directories_with_git,
};
use crate::{
    AddWorktreeRequest, AddWorktreeResult, MoveWorktreeRequest, MoveWorktreeResult,
    PruneWorktreesRequest, PruneWorktreesResult, RemoveWorktreeRequest, RemoveWorktreeResult,
    WorktreeError, WorktreeMode, add_worktree, force_remove_worktree, move_worktree,
    prune_worktrees, remove_worktree, storage_accounting,
};

pub const ENABLED_CONFIG_KEY: &str = "riftri.enabled";

/// The only Git subcommand Riftri claims from the proxy.
const WORKTREE_SUBCOMMAND: &str = "worktree";

/// Git's own grace period for `git worktree prune`, which `git gc` runs
/// internally. Riftri pins it so Git never prunes a managed worktree outside
/// the journal; see `protect_worktrees_from_git_prune`.
pub const WORKTREE_PRUNE_EXPIRE_CONFIG_KEY: &str = "gc.worktreePruneExpire";
const WORKTREE_PRUNE_EXPIRE_NEVER: &str = "never";
pub const BYPASS_ENV: &str = "RIFTRI_BYPASS";
pub const CACHE_DIR_ENV: &str = "RIFTRI_CACHE_DIR";
/// Environment variable naming the ephemeral `riftri exec` shim directory.
pub const PROCESS_SHIM_DIR_ENV: &str = "RIFTRI_PROCESS_SHIM_DIR";
/// Name prefix shared by every ephemeral `riftri exec` shim directory.
pub const PROCESS_SHIM_DIR_PREFIX: &str = "riftri-git-shim-";
/// Marker file inside every Riftri shim directory recording the real Git path
/// captured when the shim was created, so a shim whose environment was
/// stripped can still identify itself and delegate to the real Git.
const REAL_GIT_MARKER_FILE: &str = "riftri-real-git";
/// Environment variable recording the durable shell-hook shim directory,
/// so status and deactivation keep working after the shell changes directory.
const SHELL_SHIM_DIR_ENV: &str = "RIFTRI_SHELL_SHIM_DIR";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryActivation {
    pub repository: PathBuf,
    pub common_git_dir: PathBuf,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellActivationStatus {
    pub active: bool,
    pub bypass: bool,
    pub marker_set: bool,
    pub shim_first_on_path: bool,
    pub shim_directory: PathBuf,
    pub real_git: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum GitProxyPlan {
    Passthrough,
    OptimizedAdd {
        request: AddWorktreeRequest,
        quiet: bool,
    },
    OptimizedRemove(RemoveWorktreeRequest),
    OptimizedForceRemove(RemoveWorktreeRequest),
    OptimizedMove(MoveWorktreeRequest),
    OptimizedPrune(PruneWorktreesRequest),
}

#[derive(Debug)]
pub enum GitProxyOutcome {
    Passthrough(i32),
    OptimizedAdd {
        result: AddWorktreeResult,
        quiet: bool,
    },
    OptimizedRemove(RemoveWorktreeResult),
    OptimizedMove(MoveWorktreeResult),
    OptimizedPrune(PruneWorktreesResult),
}

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    Worktree(#[from] WorktreeError),

    #[error("repository-scoped activation requires a non-bare Git worktree")]
    BareRepository,

    #[error("unsupported enabled Git worktree command: {0}")]
    UnsupportedWorktreeCommand(String),

    #[error("process-scoped activation failed: {0}")]
    Process(String),

    #[error("invalid worktree binding: {0}")]
    WorktreeBinding(String),

    #[error("shell activation failed: {0}")]
    Shell(String),

    #[error(
        "Riftri requires Git {minimum} or later, but {} reports `{version}`; upgrade Git, or run Git without Riftri",
        .git.display()
    )]
    UnsupportedGitVersion {
        git: PathBuf,
        version: String,
        minimum: &'static str,
    },
}

pub fn enable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    require_supported_git(&git)?;
    let status = repository_identity_with_git(&git, path)?;
    git.set_local_config(
        &status.common_git_dir,
        ENABLED_CONFIG_KEY,
        OsStr::new("true"),
    )?;
    protect_worktrees_from_git_prune(&git, &status.common_git_dir)?;
    Ok(RepositoryActivation {
        enabled: true,
        ..status
    })
}

pub fn disable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    let status = repository_identity_with_git(&git, path)?;
    git.unset_local_config(&status.common_git_dir, ENABLED_CONFIG_KEY)?;
    release_worktree_prune_protection(&git, &status.common_git_dir)?;
    Ok(RepositoryActivation {
        enabled: false,
        ..status
    })
}

/// Refuse to enable a repository or to start a Git shim scope on a Git older
/// than [`riftri_git::MINIMUM_GIT_VERSION`].
///
/// The shim decides on its hot path, without starting Git, that an alias
/// cannot replace a command in [`COMMON_GIT_BUILTINS`]. That holds only on a
/// Git where every one of those commands is a builtin: on an older Git, an
/// `alias.switch` that runs `worktree remove --force` would reach Git
/// unplanned. Checking the version once, when the scope records the real Git
/// it delegates to, keeps that guarantee without adding a Git process to any
/// delegated command. Riftri's lifecycle commands already fail on an older
/// Git, so the refusal only makes that failure early and explicit.
fn require_supported_git(git: &Git) -> Result<(), ActivationError> {
    let info = git.detect()?;
    if info.meets_minimum_version() {
        return Ok(());
    }
    Err(ActivationError::UnsupportedGitVersion {
        git: info.command,
        version: info.version,
        minimum: riftri_git::MINIMUM_GIT_VERSION_TEXT,
    })
}

/// Stop Git from pruning managed worktrees behind Riftri's back.
///
/// `git gc` runs `git worktree prune --expire <gc.worktreePruneExpire>`
/// internally, and `gc.auto` fires that from ordinary commands like
/// `git commit`. Those inner calls resolve `git` from Git's own exec-path, so
/// they never re-enter the shim and never reach the journaled prune the proxy
/// would otherwise plan. Dropping a managed worktree's admin files that way
/// leaves a stale add journal behind and pins its retained base until the user
/// runs `riftri repair`. Pinning the expiry to `never` closes every one of
/// those routes at the source, including config aliases and future Git
/// options, because it is Git itself that declines to prune.
///
/// A value the user already chose is left alone: this only writes the key when
/// the repository has no local setting of its own.
fn protect_worktrees_from_git_prune(git: &Git, repository: &Path) -> Result<(), ActivationError> {
    if git
        .local_config_value(repository, WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)?
        .is_some()
    {
        return Ok(());
    }
    git.set_local_config(
        repository,
        WORKTREE_PRUNE_EXPIRE_CONFIG_KEY,
        OsStr::new(WORKTREE_PRUNE_EXPIRE_NEVER),
    )?;
    Ok(())
}

/// Undo [`protect_worktrees_from_git_prune`], but only when the recorded value
/// is still the one Riftri wrote. A value the user has since changed is theirs.
fn release_worktree_prune_protection(git: &Git, repository: &Path) -> Result<(), ActivationError> {
    let Some(value) = git.local_config_value(repository, WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)? else {
        return Ok(());
    };
    if value == WORKTREE_PRUNE_EXPIRE_NEVER.as_bytes() {
        git.unset_local_config(repository, WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)?;
    }
    Ok(())
}

pub fn repository_activation(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    activation_with_git(&Git::default(), path)
}

pub fn plan_git_command(
    current_directory: &Path,
    arguments: &[OsString],
) -> Result<GitProxyPlan, ActivationError> {
    plan_git_command_recording(current_directory, arguments, &mut None)
}

/// The directory the proxy inspected while planning, and that inspection.
type InspectedRepository = (PathBuf, RepositoryInfo);

/// [`plan_git_command`], also handing back the repository inspection an
/// optimized remove, move, or prune was planned from. That inspection already
/// health-checked HEAD, and the lifecycle command reuses it instead of
/// inspecting the same directory again.
fn plan_git_command_recording(
    current_directory: &Path,
    arguments: &[OsString],
    inspected: &mut Option<InspectedRepository>,
) -> Result<GitProxyPlan, ActivationError> {
    if environment_truthy(BYPASS_ENV) {
        return Ok(GitProxyPlan::Passthrough);
    }

    plan_git_command_inner(current_directory, arguments, true, inspected)
}

fn plan_git_command_inner(
    current_directory: &Path,
    arguments: &[OsString],
    expand_aliases: bool,
    inspected: &mut Option<InspectedRepository>,
) -> Result<GitProxyPlan, ActivationError> {
    // More than one reading exists only behind an unrecognized global option,
    // which already rules out the optimized paths, so the first reading that
    // is not ordinary Git is a refusal and decides the plan.
    for context in command_contexts(current_directory, arguments) {
        let plan = plan_command_reading(
            current_directory,
            arguments,
            context,
            expand_aliases,
            inspected,
        )?;
        if !matches!(plan, GitProxyPlan::Passthrough) {
            return Ok(plan);
        }
    }
    Ok(GitProxyPlan::Passthrough)
}

fn plan_command_reading(
    current_directory: &Path,
    arguments: &[OsString],
    context: CommandContext,
    expand_aliases: bool,
    inspected: &mut Option<InspectedRepository>,
) -> Result<GitProxyPlan, ActivationError> {
    if arguments
        .get(context.command_index)
        .map(OsString::as_os_str)
        != Some(OsStr::new(WORKTREE_SUBCOMMAND))
    {
        if !expand_aliases {
            return Ok(GitProxyPlan::Passthrough);
        }
        return match expand_alias_chain(&context, arguments, true) {
            // Re-plan the command Git will actually run. Aliases are not
            // expanded recursively, so one pass is enough and terminates.
            Ok(Some(expanded)) => {
                plan_git_command_inner(current_directory, &expanded, false, inspected)
            }
            Ok(None) => Ok(GitProxyPlan::Passthrough),
            // An alias that cannot be resolved might run any worktree
            // command, so only a repository that is provably absent or never
            // opted in is left to Git, which then reports the failure itself.
            Err(ActivationError::Git(error)) => {
                let location = context
                    .git_directory
                    .as_deref()
                    .map_or(context.repository.as_path(), git_directory_location);
                match uninspected_for_proxy(&Git::default(), location, error)? {
                    ProxyRepository::Unhealthy {
                        enabled: true,
                        error,
                    } => Err(error.into()),
                    _ => Ok(GitProxyPlan::Passthrough),
                }
            }
            Err(error) => Err(error),
        };
    }

    // Only the lifecycle subcommands are ever planned or refused below, so
    // any other one is Git's alone. Deciding that before the repository is
    // inspected saves `git worktree list` and friends three to four Git
    // processes, and changes no outcome: every later branch passes a
    // non-lifecycle subcommand through, healthy repository or not.
    let subcommand = arguments.get(context.command_index + 1);
    if !subcommand.is_some_and(|subcommand| is_lifecycle_subcommand(subcommand.as_encoded_bytes()))
    {
        return Ok(GitProxyPlan::Passthrough);
    }

    let git = Git::default();
    let root = match context.git_directory.as_deref() {
        Some(git_directory) => git
            .worktree_root_from_git_dir(git_directory)
            .map_err(|error| (git_directory, error)),
        None => Ok(None),
    };
    let (repository, proxy) = match root {
        Ok(root) => {
            // Without a live working tree to stand in for it, a repository
            // named by `--git-dir` is planned from that directory, which is
            // where Riftri runs Git for a bare repository.
            let repository = root
                .or(context.git_directory)
                .unwrap_or_else(|| context.repository.clone());
            let proxy = activation_for_proxy(&repository)?;
            (repository, proxy)
        }
        Err((git_directory, error)) => {
            let proxy = uninspected_for_proxy(&git, git_directory_location(git_directory), error)?;
            (context.repository.clone(), proxy)
        }
    };
    // Two directories from here on. `repository` is inside the repository
    // Git opens and is where Riftri runs its own Git commands. `directory`
    // is where the caller's Git runs, and Git resolves relative worktree
    // paths there. They differ under `--git-dir`: keying path arguments on
    // `repository` let `git --git-dir=<repository>/.git worktree remove
    // --force ./view` from elsewhere be checked against the wrong path.
    let directory = context.repository.as_path();
    let (activation, repository_info) = match proxy {
        ProxyRepository::Absent => return Ok(GitProxyPlan::Passthrough),
        ProxyRepository::Inspected(activation, repository_info) => (activation, repository_info),
        ProxyRepository::Unhealthy { enabled, error } => {
            let subcommand = arguments
                .get(context.command_index + 1)
                .map(|subcommand| subcommand.to_string_lossy().into_owned())
                .unwrap_or_default();
            if enabled && matches!(subcommand.as_str(), "add" | "remove" | "move" | "prune") {
                return Err(unsupported(format!(
                    "refusing `git worktree {subcommand}` because the repository could not be inspected ({error}); repair the repository first, or set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
                )));
            }
            // Read-only worktree commands, and repositories that never opted
            // in, stay ordinary Git even when unhealthy.
            return Ok(GitProxyPlan::Passthrough);
        }
    };
    if !activation.enabled {
        return Ok(GitProxyPlan::Passthrough);
    }

    let Some(subcommand) = arguments.get(context.command_index + 1) else {
        return Ok(GitProxyPlan::Passthrough);
    };
    let plan = match subcommand.to_string_lossy().as_ref() {
        "add" => plan_enabled_add(
            &repository,
            directory,
            &arguments[context.command_index + 2..],
            context.global_options,
        )?,
        "remove" => plan_enabled_remove(
            &repository,
            &repository_info,
            directory,
            &arguments[context.command_index + 2..],
            context.global_options.optimization_compatible(),
        )?,
        "move" => plan_enabled_move(
            &repository,
            &repository_info,
            directory,
            &arguments[context.command_index + 2..],
            context.global_options.optimization_compatible(),
        )?,
        "prune" => plan_enabled_prune(
            &repository,
            &repository_info,
            &arguments[context.command_index + 2..],
            context.global_options,
        )?,
        _ => GitProxyPlan::Passthrough,
    };
    // An optimized request names `repository`, the very directory just
    // inspected, so the lifecycle command can reuse the inspection.
    if matches!(
        plan,
        GitProxyPlan::OptimizedRemove(_)
            | GitProxyPlan::OptimizedForceRemove(_)
            | GitProxyPlan::OptimizedMove(_)
            | GitProxyPlan::OptimizedPrune(_)
    ) {
        *inspected = Some((repository, repository_info));
    }
    Ok(plan)
}

pub fn proxy_git_command(
    current_directory: &Path,
    arguments: &[OsString],
) -> Result<GitProxyOutcome, ActivationError> {
    let mut inspected = None;
    let plan = plan_git_command_recording(current_directory, arguments, &mut inspected)?;
    // Reuse the planning inspection only for a request that names the very
    // directory it inspected; anything else is inspected afresh.
    let mut inspected_for = |repository: &Path| {
        inspected
            .take()
            .filter(|(path, _): &InspectedRepository| path == repository)
            .map(|(_, info)| info)
    };
    match plan {
        GitProxyPlan::Passthrough => Ok(GitProxyOutcome::Passthrough(exit_status_code(
            Git::default().passthrough(arguments)?,
        ))),
        GitProxyPlan::OptimizedAdd { request, quiet } => match add_worktree(request) {
            Ok(result) => Ok(GitProxyOutcome::OptimizedAdd { result, quiet }),
            // `HEAD` has no commit, so there is nothing to clone. The refusal
            // comes before anything is recorded or created, so let Git decide,
            // as it would without Riftri: a new repository gets an orphan
            // worktree, and anything else gets Git's own error.
            Err(WorktreeError::UnbornHead(_)) => Ok(GitProxyOutcome::Passthrough(
                exit_status_code(Git::default().passthrough(arguments)?),
            )),
            Err(error) => Err(error.into()),
        },
        GitProxyPlan::OptimizedRemove(request) => Ok(GitProxyOutcome::OptimizedRemove(
            match inspected_for(&request.repository) {
                Some(repository) => remove_worktree_inspected(request, repository, false)?,
                None => remove_worktree(request)?,
            },
        )),
        GitProxyPlan::OptimizedForceRemove(request) => Ok(GitProxyOutcome::OptimizedRemove(
            match inspected_for(&request.repository) {
                Some(repository) => remove_worktree_inspected(request, repository, true)?,
                None => force_remove_worktree(request)?,
            },
        )),
        GitProxyPlan::OptimizedMove(request) => Ok(GitProxyOutcome::OptimizedMove(
            match inspected_for(&request.repository) {
                Some(repository) => move_worktree_inspected(request, repository)?,
                None => move_worktree(request)?,
            },
        )),
        GitProxyPlan::OptimizedPrune(request) => Ok(GitProxyOutcome::OptimizedPrune(
            match inspected_for(&request.repository) {
                Some(repository) => prune_worktrees_inspected(request, repository)?,
                None => prune_worktrees(request)?,
            },
        )),
    }
}

pub fn execute_scoped_command(command: &[OsString]) -> Result<i32, ActivationError> {
    execute_scoped_command_from(command, None)
}

/// Install a root-owned, set-user-ID copy of the current Riftri executable
/// that exposes only the internal OverlayFS mount protocol when elevated.
#[cfg(target_os = "linux")]
pub fn install_overlayfs_helper(replace: bool) -> Result<PathBuf, ActivationError> {
    // SAFETY: geteuid takes no arguments, cannot fail, and only reads the
    // calling process's effective user ID.
    if unsafe { libc::geteuid() } != 0 {
        return Err(process_error(
            "installing the OverlayFS helper requires root; run `sudo riftri overlayfs install-helper`",
        ));
    }

    let destination = PathBuf::from(riftri_storage::OverlayFsMounter::DEFAULT_HELPER_PATH);
    let parent = destination
        .parent()
        .ok_or_else(|| process_error("the OverlayFS helper destination has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        process_error(format!(
            "create helper installation directory {}: {error}",
            parent.display()
        ))
    })?;
    validate_root_install_directory(parent)?;

    match fs::symlink_metadata(&destination) {
        Ok(_metadata) if !replace => {
            return Err(process_error(format!(
                "{} already exists; rerun with --replace after verifying the installed helper",
                destination.display()
            )));
        }
        Ok(metadata) => {
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(process_error(format!(
                    "refusing to replace unsafe helper path {}",
                    destination.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(process_error(format!(
                "inspect helper destination {}: {error}",
                destination.display()
            )));
        }
    }

    let source = env::current_exe()
        .map_err(|error| process_error(format!("locate the Riftri executable: {error}")))?;
    let source_metadata = fs::symlink_metadata(&source).map_err(|error| {
        process_error(format!(
            "inspect Riftri executable {}: {error}",
            source.display()
        ))
    })?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(process_error(format!(
            "Riftri executable is not a regular file: {}",
            source.display()
        )));
    }

    let mut source_file = File::open(&source).map_err(|error| {
        process_error(format!(
            "open Riftri executable {}: {error}",
            source.display()
        ))
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        process_error(format!(
            "create temporary helper in {}: {error}",
            parent.display()
        ))
    })?;
    io::copy(&mut source_file, temporary.as_file_mut()).map_err(|error| {
        process_error(format!(
            "copy Riftri helper into {}: {error}",
            parent.display()
        ))
    })?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o4755))
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| process_error(format!("secure temporary helper: {error}")))?;

    if replace {
        temporary.persist(&destination)
    } else {
        temporary.persist_noclobber(&destination)
    }
    .map_err(|error| {
        process_error(format!(
            "install helper at {}: {}",
            destination.display(),
            error.error
        ))
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| process_error(format!("sync helper installation: {error}")))?;
    Ok(destination)
}

#[cfg(not(target_os = "linux"))]
pub fn install_overlayfs_helper(_replace: bool) -> Result<PathBuf, ActivationError> {
    Err(process_error(
        "the OverlayFS helper is available only on Linux",
    ))
}

#[cfg(target_os = "linux")]
fn validate_root_install_directory(directory: &Path) -> Result<(), ActivationError> {
    let canonical = fs::canonicalize(directory).map_err(|error| {
        process_error(format!(
            "resolve helper installation directory {}: {error}",
            directory.display()
        ))
    })?;
    if canonical != directory {
        return Err(process_error(format!(
            "helper installation directory resolves through another path: {}",
            directory.display()
        )));
    }
    let mut current = Some(directory);
    while let Some(path) = current {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            process_error(format!(
                "inspect helper directory {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(process_error(format!(
                "helper installation path must contain only root-owned, non-writable real directories: {}",
                path.display()
            )));
        }
        current = path.parent();
    }
    Ok(())
}

/// Run any command from an exact, live Git worktree root while keeping Git
/// interception scoped to that child process and its descendants.
pub fn execute_scoped_command_in_worktree(
    worktree: &Path,
    command: &[OsString],
) -> Result<i32, ActivationError> {
    let worktree = resolve_worktree_binding(worktree)?;
    execute_scoped_command_from(command, Some(&worktree))
}

fn execute_scoped_command_from(
    command: &[OsString],
    working_directory: Option<&Path>,
) -> Result<i32, ActivationError> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| process_error("no process-scoped command was supplied"))?;
    let real_git = locate_real_git()?;
    require_supported_git(&Git::new(&real_git))?;
    let current_executable = env::current_exe()
        .map_err(|error| process_error(format!("locate the Riftri executable: {error}")))?;
    let shim_directory = tempfile::Builder::new()
        .prefix(PROCESS_SHIM_DIR_PREFIX)
        .tempdir()
        .map_err(|error| process_error(format!("create temporary Git shim directory: {error}")))?;
    install_git_shim(shim_directory.path(), &current_executable)?;
    record_real_git_marker(shim_directory.path(), &real_git)?;

    let existing_path = env::var_os("PATH").unwrap_or_default();
    let scoped_path = env::join_paths(
        std::iter::once(shim_directory.path().to_path_buf())
            .chain(env::split_paths(&existing_path)),
    )
    .map_err(|error| process_error(format!("construct process-scoped PATH: {error}")))?;
    let mut child = Command::new(program);
    child
        .args(arguments)
        .env("PATH", scoped_path)
        .env(riftri_git::REAL_GIT_ENV, real_git)
        .env(PROCESS_SHIM_DIR_ENV, shim_directory.path())
        .env(SHIM_ACTIVE_ENV, "1");
    if let Some(working_directory) = working_directory {
        child.current_dir(working_directory);
    }
    let status = wait_for_scoped_child(&mut child, program)?;
    Ok(exit_status_code(status))
}

/// Run the scoped command to completion while honoring the termination
/// contract implemented by [`riftri_git::termination`]: termination signals
/// delivered to Riftri are forwarded to the scoped command or left for the
/// command to decide, the command starts from the signal dispositions Riftri
/// itself inherited, Riftri keeps waiting so the temporary Git shim is
/// removed, and the command's exit status is propagated unchanged.
fn wait_for_scoped_child(
    command: &mut Command,
    program: &OsStr,
) -> Result<ExitStatus, ActivationError> {
    riftri_git::termination::run_forwarding_terminations(command).map_err(|error| match error {
        TerminationError::Disposition { .. } | TerminationError::AlreadyWaiting => {
            process_error(error.to_string())
        }
        TerminationError::Spawn(source) => process_error(format!(
            "start process-scoped command {}: {source}",
            Path::new(program).display()
        )),
        TerminationError::Wait(source) => process_error(format!(
            "wait for process-scoped command {}: {source}",
            Path::new(program).display()
        )),
    })
}

fn resolve_worktree_binding(requested: &Path) -> Result<PathBuf, ActivationError> {
    if requested.as_os_str().is_empty() {
        return Err(worktree_binding_error("path cannot be empty"));
    }
    let canonical = fs::canonicalize(requested).map_err(|error| {
        worktree_binding_error(format!("resolve {}: {error}", requested.display()))
    })?;
    if !canonical.is_dir() {
        return Err(worktree_binding_error(format!(
            "{} is not a directory",
            canonical.display()
        )));
    }

    let git = Git::default();
    // Only "not a repository" and "inside a Git directory" are about the
    // requested path; any other Git failure (a missing or broken Git) is
    // operational and stays one. Git has no worktree root to report from
    // inside a non-bare repository's Git directory and fails the inspection
    // there, so that case is identified on the failure path, where the extra
    // Git process costs a valid binding nothing.
    let repository = match git.inspect_repository(&canonical) {
        Ok(repository) => repository,
        Err(error @ GitError::RepositoryAbsent { .. }) => {
            return Err(worktree_binding_error(format!(
                "inspect {}: {error}",
                canonical.display()
            )));
        }
        Err(error) => {
            if git.inside_git_directory(&canonical).unwrap_or(false) {
                return Err(worktree_binding_error(format!(
                    "{} is inside a Git directory, not a worktree",
                    canonical.display()
                )));
            }
            return Err(ActivationError::Git(error));
        }
    };
    let root = repository.root.ok_or_else(|| {
        worktree_binding_error(format!("{} is a bare repository", canonical.display()))
    })?;
    let root = fs::canonicalize(&root).map_err(|error| {
        worktree_binding_error(format!(
            "resolve Git worktree root {}: {error}",
            root.display()
        ))
    })?;
    if root != canonical {
        return Err(worktree_binding_error(format!(
            "{} is inside {}, but --worktree must name the exact Git worktree root",
            canonical.display(),
            root.display()
        )));
    }

    let registered = git.list_worktrees(&root)?.into_iter().find(|worktree| {
        !worktree.bare && fs::canonicalize(&worktree.path).is_ok_and(|path| path == canonical)
    });
    let Some(registered) = registered else {
        return Err(worktree_binding_error(format!(
            "{} is not a live entry in Git's worktree inventory",
            canonical.display()
        )));
    };
    if registered.head_unresolvable {
        return Err(worktree_binding_error(format!(
            "Git cannot resolve the worktree HEAD of {}; run `git worktree repair` first",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Prepare a durable Git shim and render Bourne-compatible shell code that
/// activates it for the current shell and every child process.
pub fn prepare_posix_shell_hook() -> Result<String, ActivationError> {
    prepare_posix_shell_hook_inner()
}

/// Render Bourne-compatible code that removes Riftri from the current shell.
/// The caller must explicitly evaluate the returned code in that shell.
pub fn prepare_posix_shell_deactivation() -> Result<String, ActivationError> {
    prepare_posix_shell_deactivation_inner()
}

/// Prepare a durable Git shim and render PowerShell code that activates it for
/// the current session and every child process.
pub fn prepare_powershell_hook() -> Result<String, ActivationError> {
    prepare_powershell_hook_inner()
}

/// Render PowerShell code that removes Riftri from the current session. The
/// caller must explicitly evaluate the returned code in PowerShell.
pub fn prepare_powershell_deactivation() -> Result<String, ActivationError> {
    prepare_powershell_deactivation_inner()
}

/// Report whether the current process inherited a complete, usable shell hook.
pub fn shell_activation_status() -> Result<ShellActivationStatus, ActivationError> {
    shell_activation_status_inner()
}

#[cfg(unix)]
fn prepare_posix_shell_hook_inner() -> Result<String, ActivationError> {
    let real_git = locate_real_git()?;
    require_supported_git(&Git::new(&real_git))?;
    let current_executable = env::current_exe()
        .map_err(|error| shell_error(format!("locate the Riftri executable: {error}")))?;
    let shim_directory = shell_shim_directory()?;
    ensure_real_shell_shim_directory(&shim_directory)?;
    set_private_directory_permissions(&shim_directory)?;
    install_durable_git_shim(&shim_directory, &current_executable)?;
    record_real_git_marker(&shim_directory, &real_git)?;

    let shim_directory = posix_quote_path(&shim_directory)?;
    let real_git = posix_quote_path(&real_git)?;
    Ok(format!(
        "export {real_git_env}={real_git}\nexport {shim_active_env}='1'\nexport {shell_shim_dir_env}={shim_directory}\ncase \"${{PATH-}}\" in\n  {shim_directory}|{shim_directory}:*) ;;\n  *) export PATH={shim_directory}${{PATH:+\":$PATH\"}} ;;\nesac\n",
        real_git_env = riftri_git::REAL_GIT_ENV,
        shim_active_env = SHIM_ACTIVE_ENV,
        shell_shim_dir_env = SHELL_SHIM_DIR_ENV,
    ))
}

#[cfg(not(unix))]
fn prepare_posix_shell_hook_inner() -> Result<String, ActivationError> {
    Err(shell_error(
        "the sh/bash/zsh hook is currently available only on Unix-like systems",
    ))
}

#[cfg(unix)]
fn prepare_posix_shell_deactivation_inner() -> Result<String, ActivationError> {
    let shim_directory = posix_quote_path(&shell_shim_directory()?)?;
    Ok(format!(
        "_riftri_shim={shim_directory}\n_riftri_process_shim=${{{process_shim_env}-}}\n_riftri_remaining=${{PATH-}}\n_riftri_clean_path=\n_riftri_separator=\nwhile :; do\n  case \"$_riftri_remaining\" in\n    *:*) _riftri_entry=${{_riftri_remaining%%:*}}; _riftri_remaining=${{_riftri_remaining#*:}}; _riftri_more=1 ;;\n    *) _riftri_entry=$_riftri_remaining; _riftri_remaining=; _riftri_more=0 ;;\n  esac\n  _riftri_keep=1\n  [ \"$_riftri_entry\" = \"$_riftri_shim\" ] && _riftri_keep=0\n  [ -n \"$_riftri_process_shim\" ] && [ \"$_riftri_entry\" = \"$_riftri_process_shim\" ] && _riftri_keep=0\n  case \"${{_riftri_entry##*/}}\" in {process_shim_prefix}*) _riftri_keep=0 ;; esac\n  if [ \"$_riftri_keep\" = 1 ]; then\n    _riftri_clean_path=${{_riftri_clean_path}}${{_riftri_separator}}${{_riftri_entry}}\n    _riftri_separator=:\n  fi\n  [ \"$_riftri_more\" = 0 ] && break\ndone\nexport PATH=$_riftri_clean_path\nunset {real_git_env} {shim_active_env} {shell_shim_dir_env} {process_shim_env}\nunset _riftri_shim _riftri_process_shim _riftri_remaining _riftri_clean_path _riftri_separator _riftri_entry _riftri_more _riftri_keep\n",
        real_git_env = riftri_git::REAL_GIT_ENV,
        shim_active_env = SHIM_ACTIVE_ENV,
        shell_shim_dir_env = SHELL_SHIM_DIR_ENV,
        process_shim_env = PROCESS_SHIM_DIR_ENV,
        process_shim_prefix = PROCESS_SHIM_DIR_PREFIX,
    ))
}

#[cfg(not(unix))]
fn prepare_posix_shell_deactivation_inner() -> Result<String, ActivationError> {
    Err(shell_error(
        "shell deactivation for sh/bash/zsh is currently available only on Unix-like systems",
    ))
}

#[cfg(target_os = "windows")]
fn prepare_powershell_hook_inner() -> Result<String, ActivationError> {
    let real_git = locate_real_git()?;
    require_supported_git(&Git::new(&real_git))?;
    let current_executable = env::current_exe()
        .map_err(|error| shell_error(format!("locate the Riftri executable: {error}")))?;
    let shim_directory = shell_shim_directory()?;
    ensure_real_shell_shim_directory(&shim_directory)?;
    set_private_directory_permissions(&shim_directory)?;
    install_durable_git_shim(&shim_directory, &current_executable)?;
    record_real_git_marker(&shim_directory, &real_git)?;

    let shim_directory = powershell_quote_path(&shim_directory)?;
    let real_git = powershell_quote_path(&real_git)?;
    Ok(format!(
        "$env:{real_git_env} = {real_git}\n$env:{shim_active_env} = '1'\n$env:{shell_shim_dir_env} = {shim_directory}\n$_riftriShim = {shim_directory}\n$_riftriPath = @($env:PATH -split ';' | Where-Object {{ $_ -ne $_riftriShim }})\n$env:PATH = (@($_riftriShim) + $_riftriPath) -join ';'\nRemove-Variable _riftriShim, _riftriPath -ErrorAction SilentlyContinue\n",
        real_git_env = riftri_git::REAL_GIT_ENV,
        shim_active_env = SHIM_ACTIVE_ENV,
        shell_shim_dir_env = SHELL_SHIM_DIR_ENV,
    ))
}

#[cfg(not(target_os = "windows"))]
fn prepare_powershell_hook_inner() -> Result<String, ActivationError> {
    Err(shell_error(
        "the PowerShell hook is currently available only on Windows",
    ))
}

#[cfg(target_os = "windows")]
fn prepare_powershell_deactivation_inner() -> Result<String, ActivationError> {
    let shim_directory = powershell_quote_path(&shell_shim_directory()?)?;
    Ok(format!(
        "$_riftriShim = {shim_directory}\n$_riftriProcessShim = $env:{process_shim_env}\n$_riftriPath = @($env:PATH -split ';' | Where-Object {{ $_ -ne $_riftriShim -and (-not $_riftriProcessShim -or $_ -ne $_riftriProcessShim) -and ($_ -split '[\\\\/]')[-1] -notlike '{process_shim_prefix}*' }})\n$env:PATH = $_riftriPath -join ';'\nRemove-Item Env:{real_git_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{shim_active_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{shell_shim_dir_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{process_shim_env} -ErrorAction SilentlyContinue\nRemove-Variable _riftriShim, _riftriProcessShim, _riftriPath -ErrorAction SilentlyContinue\n",
        real_git_env = riftri_git::REAL_GIT_ENV,
        shim_active_env = SHIM_ACTIVE_ENV,
        shell_shim_dir_env = SHELL_SHIM_DIR_ENV,
        process_shim_env = PROCESS_SHIM_DIR_ENV,
        process_shim_prefix = PROCESS_SHIM_DIR_PREFIX,
    ))
}

#[cfg(not(target_os = "windows"))]
fn prepare_powershell_deactivation_inner() -> Result<String, ActivationError> {
    Err(shell_error(
        "PowerShell deactivation is currently available only on Windows",
    ))
}

#[cfg(any(unix, target_os = "windows"))]
fn shell_activation_status_inner() -> Result<ShellActivationStatus, ActivationError> {
    let first_path = env::var_os("PATH").and_then(|path| env::split_paths(&path).next());
    // A process scope owns an ephemeral shim, not the durable shell cache.
    // Only select its marker while that exact directory remains first on PATH;
    // a subsequently evaluated shell hook can legitimately supersede it.
    let process_shim = env::var_os(PROCESS_SHIM_DIR_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && first_path.as_ref() == Some(path));
    let shim_directory = match process_shim {
        Some(path) => path,
        None => shell_shim_directory()?,
    };
    let marker_set = environment_truthy(SHIM_ACTIVE_ENV);
    let shim_first_on_path = first_path.is_some_and(|path| path == shim_directory);
    let real_git = env::var_os(riftri_git::REAL_GIT_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let active = marker_set
        && shim_first_on_path
        && shim_executable_paths(&shim_directory)
            .iter()
            .any(|path| is_executable_file(path))
        && real_git.as_deref().is_some_and(is_executable_file);
    Ok(ShellActivationStatus {
        active,
        bypass: environment_truthy(BYPASS_ENV),
        marker_set,
        shim_first_on_path,
        shim_directory,
        real_git,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
fn shell_activation_status_inner() -> Result<ShellActivationStatus, ActivationError> {
    Err(shell_error("shell status is unavailable on this platform"))
}

#[cfg(unix)]
fn shim_executable_paths(directory: &Path) -> Vec<PathBuf> {
    vec![directory.join("git")]
}

#[cfg(target_os = "windows")]
fn shim_executable_paths(directory: &Path) -> Vec<PathBuf> {
    vec![directory.join("git.exe")]
}

#[cfg(any(unix, target_os = "windows"))]
fn shell_shim_directory() -> Result<PathBuf, ActivationError> {
    if let Some(path) = env::var_os(SHELL_SHIM_DIR_ENV).filter(|path| !path.is_empty()) {
        let directory = PathBuf::from(path);
        if !directory.is_absolute() {
            return Err(shell_error(format!(
                "{SHELL_SHIM_DIR_ENV} must be absolute"
            )));
        }
        return Ok(directory);
    }
    let cache_root = env::var_os(CACHE_DIR_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(default_cache_directory)?;
    let cache_root = if cache_root.is_absolute() {
        cache_root
    } else {
        env::current_dir()
            .map_err(|error| shell_error(format!("resolve current directory: {error}")))?
            .join(cache_root)
    };
    Ok(cache_root.join("shims").join("v1"))
}

#[cfg(target_os = "macos")]
fn default_cache_directory() -> Result<PathBuf, ActivationError> {
    let home = env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| shell_error("HOME is not set; cannot choose a shell shim directory"))?;
    Ok(PathBuf::from(home).join("Library/Caches/riftri"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_cache_directory() -> Result<PathBuf, ActivationError> {
    if let Some(cache) = env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(cache).join("riftri"));
    }
    let home = env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            shell_error("HOME and XDG_CACHE_HOME are not set; cannot choose a shell shim directory")
        })?;
    Ok(PathBuf::from(home).join(".cache/riftri"))
}

#[cfg(target_os = "windows")]
fn default_cache_directory() -> Result<PathBuf, ActivationError> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            shell_error("LOCALAPPDATA is not set; cannot choose a shell shim directory")
        })?;
    Ok(PathBuf::from(local_app_data).join("Riftri").join("Cache"))
}

#[cfg(unix)]
fn set_private_directory_permissions(directory: &Path) -> Result<(), ActivationError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
        shell_error(format!(
            "secure shell shim directory {}: {error}",
            directory.display()
        ))
    })
}

#[cfg(target_os = "windows")]
fn set_private_directory_permissions(_directory: &Path) -> Result<(), ActivationError> {
    Ok(())
}

#[cfg(any(unix, target_os = "windows"))]
fn ensure_real_shell_shim_directory(directory: &Path) -> Result<(), ActivationError> {
    let shims = directory
        .parent()
        .ok_or_else(|| shell_error("shell shim directory has no parent"))?;
    let cache_root = shims
        .parent()
        .ok_or_else(|| shell_error("shell shim cache root has no parent"))?;
    fs::create_dir_all(cache_root).map_err(|error| {
        shell_error(format!(
            "create shell shim cache root {}: {error}",
            cache_root.display()
        ))
    })?;
    require_real_shell_directory(cache_root)?;
    ensure_real_shell_directory(shims)?;
    ensure_real_shell_directory(directory)
}

#[cfg(any(unix, target_os = "windows"))]
fn ensure_real_shell_directory(directory: &Path) -> Result<(), ActivationError> {
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(shell_error(format!(
                "create shell shim directory {}: {error}",
                directory.display()
            )));
        }
    }
    require_real_shell_directory(directory)
}

#[cfg(any(unix, target_os = "windows"))]
fn require_real_shell_directory(directory: &Path) -> Result<(), ActivationError> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        shell_error(format!(
            "inspect shell shim directory {}: {error}",
            directory.display()
        ))
    })?;
    #[cfg(unix)]
    let unsafe_kind = metadata.file_type().is_symlink() || !metadata.is_dir();
    #[cfg(target_os = "windows")]
    let unsafe_kind = {
        use std::os::windows::fs::MetadataExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    if unsafe_kind {
        return Err(shell_error(format!(
            "shell shim path {} is not a real directory",
            directory.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn install_durable_git_shim(
    directory: &Path,
    current_executable: &Path,
) -> Result<PathBuf, ActivationError> {
    let destination = directory.join("git");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| shell_error(format!("system clock error: {error}")))?
        .as_nanos();
    let temporary = directory.join(format!(".git-{}-{nonce}.tmp", std::process::id()));
    std::os::unix::fs::symlink(current_executable, &temporary).map_err(|error| {
        shell_error(format!(
            "create shell Git shim {}: {error}",
            temporary.display()
        ))
    })?;
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(shell_error(format!(
            "activate shell Git shim {}: {error}",
            destination.display()
        )));
    }
    Ok(destination)
}

#[cfg(target_os = "windows")]
fn install_durable_git_shim(
    directory: &Path,
    current_executable: &Path,
) -> Result<PathBuf, ActivationError> {
    use std::io::Write;

    let source_metadata = fs::symlink_metadata(current_executable).map_err(|error| {
        shell_error(format!(
            "inspect Riftri executable {}: {error}",
            current_executable.display()
        ))
    })?;
    if !source_metadata.is_file() || is_windows_reparse_point(&source_metadata) {
        return Err(shell_error(format!(
            "Riftri executable is not a regular non-reparse file: {}",
            current_executable.display()
        )));
    }

    let destination = directory.join("git.exe");
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (!metadata.is_file() || is_windows_reparse_point(&metadata))
    {
        return Err(shell_error(format!(
            "shell Git shim is not a regular non-reparse file: {}",
            destination.display()
        )));
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(".git-")
        .suffix(".tmp")
        .tempfile_in(directory)
        .map_err(|error| shell_error(format!("create temporary shell Git shim: {error}")))?;
    let mut source = fs::File::open(current_executable).map_err(|error| {
        shell_error(format!(
            "open Riftri executable {}: {error}",
            current_executable.display()
        ))
    })?;
    std::io::copy(&mut source, temporary.as_file_mut())
        .map_err(|error| shell_error(format!("copy shell Git shim: {error}")))?;
    temporary
        .flush()
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| shell_error(format!("sync shell Git shim: {error}")))?;
    let (_, temporary_path) = temporary.keep().map_err(|error| {
        shell_error(format!("retain temporary shell Git shim: {}", error.error))
    })?;
    if let Err(error) = windows_replace_file(&temporary_path, &destination) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(destination)
}

#[cfg(target_os = "windows")]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(target_os = "windows")]
fn windows_replace_file(source: &Path, destination: &Path) -> Result<(), ActivationError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source_wide.push(0);
    let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination_wide.push(0);
    // SAFETY: both path buffers are NUL-terminated UTF-16. The source is a
    // closed, same-directory temporary file and the destination was rejected
    // above if it was not a regular non-reparse file.
    let succeeded = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(shell_error(format!(
            "activate shell Git shim {}: {}",
            destination.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn posix_quote_path(path: &Path) -> Result<String, ActivationError> {
    let value = path
        .to_str()
        .ok_or_else(|| shell_error(format!("shell path is not valid UTF-8: {}", path.display())))?;
    if value.contains(':') {
        return Err(shell_error(format!(
            "shell shim path cannot contain a colon: {}",
            path.display()
        )));
    }
    Ok(super::quote_shell_argument(value))
}

#[cfg(target_os = "windows")]
fn powershell_quote_path(path: &Path) -> Result<String, ActivationError> {
    let value = path.to_str().ok_or_else(|| {
        shell_error(format!(
            "shell path is not valid Unicode: {}",
            path.display()
        ))
    })?;
    if value.contains(';') {
        return Err(shell_error(format!(
            "shell shim path cannot contain a semicolon: {}",
            path.display()
        )));
    }
    Ok(super::quote_shell_argument(value))
}

fn activation_with_git(git: &Git, path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let mut activation = repository_identity_with_git(git, path)?;
    activation.enabled = git
        .local_config_bool(&activation.common_git_dir, ENABLED_CONFIG_KEY)?
        .unwrap_or(false);
    Ok(activation)
}

fn repository_identity_with_git(
    git: &Git,
    path: &Path,
) -> Result<RepositoryActivation, ActivationError> {
    let repository = git.inspect_repository(path)?;
    if repository.is_bare {
        return Err(ActivationError::BareRepository);
    }
    let root = repository.root.ok_or(ActivationError::BareRepository)?;
    Ok(RepositoryActivation {
        repository: root,
        common_git_dir: repository.identity.common_git_dir,
        enabled: false,
    })
}

/// What the proxy is standing in when it plans a `git worktree` command.
enum ProxyRepository {
    /// No repository here at all: every command is ordinary Git.
    Absent,
    /// A healthy repository, inspected, with the inspection itself.
    Inspected(RepositoryActivation, RepositoryInfo),
    /// A repository exists but could not be inspected. For an enabled
    /// repository this must fail closed on lifecycle commands: treating the
    /// error as "no repository" let `git worktree remove --force <managed>`
    /// run against a corrupted repository outside the journal.
    Unhealthy { enabled: bool, error: GitError },
}

fn activation_for_proxy(path: &Path) -> Result<ProxyRepository, ActivationError> {
    let git = Git::default();
    let repository = match git.inspect_repository(path) {
        Ok(repository) => repository,
        Err(error) => return uninspected_for_proxy(&git, path, error),
    };
    // A bare repository has no working-tree root, but Riftri manages its
    // worktrees from its Git directory, so it is planned like any other.
    let Some(root) = git_command_root(&repository).map(Path::to_path_buf) else {
        return Ok(ProxyRepository::Absent);
    };
    // Read the opt-in from the common Git directory, not from `root`:
    // `GIT_WORK_TREE` or `core.worktree` can place the working tree outside
    // the repository, even inside one that never enabled Riftri.
    let enabled =
        match git.local_config_bool(&repository.identity.common_git_dir, ENABLED_CONFIG_KEY) {
            Ok(enabled) => enabled.unwrap_or(false),
            // An unprovable opt-out fails closed here too.
            Err(error) => {
                return Ok(ProxyRepository::Unhealthy {
                    enabled: true,
                    error,
                });
            }
        };
    Ok(ProxyRepository::Inspected(
        RepositoryActivation {
            repository: root,
            common_git_dir: repository.identity.common_git_dir.clone(),
            enabled,
        },
        repository,
    ))
}

/// A directory inside the repository `--git-dir` names. The option may name
/// a linked worktree's `.git` file, which sits in the worktree it belongs to.
fn git_directory_location(git_directory: &Path) -> &Path {
    match git_directory.parent() {
        Some(parent) if git_directory.is_file() => parent,
        _ => git_directory,
    }
}

/// Classify a location where a Git inspection failed with `error`.
fn uninspected_for_proxy(
    git: &Git,
    path: &Path,
    error: GitError,
) -> Result<ProxyRepository, ActivationError> {
    if git.repository_absent(path)? {
        return Ok(ProxyRepository::Absent);
    }
    // Repository discovery works even over a damaged object store, so the
    // opt-in flag is still readable; a repository that never enabled Riftri
    // stays ordinary Git. When even the flag cannot be read, claim
    // enablement: an unprovable opt-out fails closed.
    let enabled = git
        .local_config_bool(path, ENABLED_CONFIG_KEY)
        .map(|enabled| enabled.unwrap_or(false))
        .unwrap_or(true);
    Ok(ProxyRepository::Unhealthy { enabled, error })
}

/// How the global Git options in front of `worktree` constrain optimization.
///
/// Riftri separates options that can change what Git would produce from
/// options that only affect reporting or locking, because the first class must
/// be refused while the second only has to keep Riftri from claiming that an
/// optimized result reproduces the requested invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalOptionScope {
    /// No invocation-level option that Riftri has to account for.
    None,
    /// Only options that cannot change the content of a created worktree.
    CheckoutNeutral,
    /// At least one option that can change what Git would produce.
    Significant,
}

impl GlobalOptionScope {
    fn observe(&mut self, scope: Self) {
        if scope == Self::Significant || *self == Self::None {
            *self = scope;
        }
    }

    fn optimization_compatible(self) -> bool {
        self == Self::None
    }
}

struct CommandContext {
    /// The directory Git runs in after every `-C`, which is where it looks for
    /// the repository unless `--git-dir` names one. `--work-tree` is absent on
    /// purpose: it moves the checked-out files, never the repository.
    repository: PathBuf,
    git_directory: Option<PathBuf>,
    command_index: usize,
    global_options: GlobalOptionScope,
    /// Aliases defined for this one invocation via `-c alias.<name>=<body>`
    /// or `--config-env alias.<name>=<variable>`. Git gives these the highest
    /// configuration precedence, so an alias planner that only reads the
    /// repository's stored configuration never sees them — which let
    /// `git -c 'alias.x=worktree remove --force' x <managed>` reach real Git
    /// unclassified. Names are lowercased (configuration keys are
    /// ASCII-case-insensitive) and later entries win at lookup time. Bodies
    /// stay in the platform's argument encoding because Git runs them whether
    /// or not they are UTF-8.
    invocation_aliases: Vec<(String, Vec<u8>)>,
}

/// Record a `key=value` invocation-configuration assignment when the key
/// names an alias. Entries Git would refuse to use — a missing `=`, an empty
/// name, an empty body — are skipped: Git errors on them at lookup, so
/// passthrough stays safe.
fn record_invocation_alias(aliases: &mut Vec<(String, Vec<u8>)>, assignment: &OsStr) {
    let Some((key, body)) = split_assignment(assignment) else {
        return;
    };
    let (key, body) = (key.as_encoded_bytes(), body.as_encoded_bytes());
    if key.len() <= 6 || !key[..6].eq_ignore_ascii_case(b"alias.") {
        return;
    }
    let Ok(name) = std::str::from_utf8(&key[6..]) else {
        return;
    };
    if name.is_empty() || body.is_empty() {
        return;
    }
    aliases.push((name.to_ascii_lowercase(), body.to_vec()));
}

/// Split a `key=value` argument at its first `=` without requiring either
/// side to be UTF-8.
fn split_assignment(assignment: &OsStr) -> Option<(&OsStr, &OsStr)> {
    let encoded = assignment.as_encoded_bytes();
    let separator = encoded.iter().position(|byte| *byte == b'=')?;
    // SAFETY: both splits border the ASCII `=`, which is a valid boundary in
    // the platform-independent encoded representation.
    Some(unsafe {
        (
            OsStr::from_encoded_bytes_unchecked(&encoded[..separator]),
            OsStr::from_encoded_bytes_unchecked(&encoded[separator + 1..]),
        )
    })
}

/// An alias word as the argument Git passes to the command it runs. Unix
/// arguments are bytes, so every word converts and the planner checks the
/// exact path Git will see.
#[cfg(unix)]
fn alias_argument(word: Vec<u8>) -> Result<OsString, ActivationError> {
    use std::os::unix::ffi::OsStringExt;

    Ok(OsString::from_vec(word))
}

/// An alias word as the argument Git passes to the command it runs. Windows
/// arguments are UTF-16 and Git for Windows applies its own conversion to
/// bytes that are not UTF-8, so the path it would touch cannot be known here
/// and a worktree alias carrying such a word is refused.
#[cfg(not(unix))]
fn alias_argument(word: Vec<u8>) -> Result<OsString, ActivationError> {
    String::from_utf8(word).map(OsString::from).map_err(|_| {
        unsupported(format!(
            "refusing a Git alias for `git worktree` whose arguments are not valid UTF-8 because the path Git would use cannot be determined; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
        ))
    })
}

/// Record a `--config-env` `<name>=<variable>` assignment when the name is an
/// alias, taking the body from the named environment variable. An unset
/// variable is skipped: Git itself errors on it at lookup.
fn record_environment_alias(aliases: &mut Vec<(String, Vec<u8>)>, assignment: &OsStr) {
    if let Some((key, variable)) = split_assignment(assignment)
        && let Some(body) = env::var_os(variable)
    {
        let mut assignment = key.to_os_string();
        assignment.push("=");
        assignment.push(body);
        record_invocation_alias(aliases, &assignment);
    }
}

/// Git commands common enough that resolving an alias for them would add a
/// configuration read to the hot path. Git never lets an alias shadow a
/// built-in command that is not deprecated, and none of these is, so skipping
/// the lookup for them is correct as well as cheap. Every entry is a builtin in
/// every Git from [`riftri_git::MINIMUM_GIT_VERSION`] on, which a shim scope
/// checks before it starts: `switch` and `restore`, for example, became
/// builtins only in Git 2.23, so on an older Git an alias of that name would
/// run. Programs Git runs from its exec-path (`git-bisect` before Git 2.40,
/// `git-submodule`) are not builtins and stay out. This is a shortcut, not the
/// full set: a built-in missing from the list costs that one read, and
/// [`expand_worktree_alias`] asks Git for the real set before it trusts an
/// alias that leads to a worktree command.
const COMMON_GIT_BUILTINS: &[&str] = &[
    "add",
    "am",
    "apply",
    "archive",
    "blame",
    "branch",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-ref-format",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "commit-tree",
    "config",
    "count-objects",
    "describe",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "fetch",
    "for-each-ref",
    "format-patch",
    "fsck",
    "gc",
    "grep",
    "hash-object",
    "help",
    "init",
    "interpret-trailers",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "merge",
    "merge-base",
    "mktree",
    "mv",
    "name-rev",
    "notes",
    "patch-id",
    "pull",
    "push",
    "read-tree",
    "rebase",
    "reflog",
    "remote",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "shortlog",
    "show",
    "show-ref",
    "stash",
    "status",
    "switch",
    "symbolic-ref",
    "tag",
    "update-index",
    "update-ref",
    "var",
    "version",
    WORKTREE_SUBCOMMAND,
    "write-tree",
];

/// Whether `git worktree <word>` is one of the lifecycle subcommands Riftri
/// plans. Every other subcommand is left to Git unplanned.
///
/// Git reads the subcommand only from the argument right after `worktree` and
/// matches it exactly: no abbreviation, no case folding, and no option or
/// `--` in front of it (`git worktree -q add` and `git worktree -- add` both
/// stop with a usage error). Git 2.36 compares `argv[1]` with `strcmp`, and
/// later releases parse it with `OPT_SUBCOMMAND`, which behaves the same way
/// for these inputs (probed on Git 2.50.1). So no other word in that position
/// can reach a lifecycle subcommand.
fn is_lifecycle_subcommand(word: &[u8]) -> bool {
    matches!(word, b"add" | b"remove" | b"move" | b"prune")
}

/// [`expand_alias_chain`] for the exact command Git runs, whatever its
/// worktree subcommand.
#[cfg(test)]
fn expand_worktree_alias(
    context: &CommandContext,
    arguments: &[OsString],
) -> Result<Option<Vec<OsString>>, ActivationError> {
    expand_alias_chain(context, arguments, false)
}

/// Resolve a `git` alias that ultimately runs `git worktree ...`.
///
/// The proxy compares the subcommand token to `worktree` literally, so an alias
/// such as `alias.wtp = worktree prune -v` reaches real Git unclassified and
/// performs exactly the lifecycle change the spelled-out command refuses. Git
/// expands these internally, so nothing re-enters the shim.
///
/// Shell aliases (`!command`) are deliberately left alone: they spawn a new
/// process that resolves `git` through `PATH`, so any worktree command inside
/// one already re-enters the proxy on its own.
///
/// With `lifecycle_only`, an alias that reaches any other worktree subcommand
/// is reported as `None`, which the planner passes through, as it would the
/// full expansion: the planner acts only on lifecycle subcommands. That skips
/// the Git process that asks whether a builtin earlier in the chain would run
/// instead, which can only turn the outcome into a passthrough as well.
fn expand_alias_chain(
    context: &CommandContext,
    arguments: &[OsString],
    lifecycle_only: bool,
) -> Result<Option<Vec<OsString>>, ActivationError> {
    let Some(token) = arguments.get(context.command_index) else {
        return Ok(None);
    };
    let Some(name) = token.to_str() else {
        return Ok(None);
    };
    // Git chains alias-to-alias expansions itself, to any depth, and stops
    // only on a loop. Each inner body's arguments run before the outer
    // body's remainder. Follow the chain the same way; on a cycle, pass
    // through: Git fatals on the loop itself, so nothing reaches a worktree
    // command. `seen` also bounds this loop, because every further hop needs
    // an alias name that has not been expanded yet. Every stop that is not the
    // `worktree` token is equally safe to pass through: Git either runs a
    // builtin, errors on an unknown name, or executes a shell alias that
    // re-enters the proxy on its own.
    let mut name = name.to_owned();
    let mut invocation_aliases = context.invocation_aliases.clone();
    let mut options: Vec<OsString> = Vec::new();
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        if name.is_empty() || COMMON_GIT_BUILTINS.contains(&name.as_str()) {
            return Ok(None);
        }
        // Git detects a loop by exact name, and the shadowing check below
        // needs the names as typed. Alias lookup ignores case, so a name
        // that differs only in case resolves to the same body and repeats.
        if !seen.insert(name.clone()) {
            return Ok(None);
        }
        let lowered = name.to_ascii_lowercase();
        let value = if let Some((_, body)) = invocation_aliases
            .iter()
            .rev()
            .find(|(alias, _)| *alias == lowered)
        {
            body.clone()
        } else {
            // Git reads the alias from the repository it opens, so an explicit
            // `--git-dir` outranks the directory the command runs in.
            let Some(value) = Git::default().config_value_in(
                &context.repository,
                context.git_directory.as_deref(),
                &format!("alias.{name}"),
            )?
            else {
                return Ok(None);
            };
            value
        };
        // Git tests the first byte and splits the body untrimmed, so
        // trimming here would plan a command Git never runs.
        if value.starts_with(b"!") {
            return Ok(None);
        }
        let Ok(mut words) = split_alias_words(&value) else {
            // Git refuses the same body ("bad alias string"), so passthrough
            // dies in Git with nothing executed.
            return Ok(None);
        };
        // Git runs its global option parser over the body before it reads
        // the command, and that parser stops at the first word that does not
        // start with `-`, so only such a body has options to find.
        if words.first().is_some_and(|word| word.starts_with(b"-")) {
            let mut body_options = words
                .iter()
                .cloned()
                .map(alias_argument)
                .collect::<Result<Vec<_>, _>>()?;
            // Find the command the way the outer command line does. Git dies
            // on an option it does not know, so the first reading is the only
            // one, and with no command behind the options it dies as well.
            let Some(body) = command_contexts(&context.repository, &body_options)
                .into_iter()
                .next()
            else {
                return Ok(None);
            };
            body_options.truncate(body.command_index);
            if !git_runs_alias_options(&body_options) {
                return Ok(None);
            }
            // The options stay in front of the command Git will run, where
            // planning weighs them like any other global option, and a body
            // can define the alias a later hop names.
            options.append(&mut body_options);
            invocation_aliases.extend(body.invocation_aliases);
            words.drain(..body.command_index);
        }
        let mut words = words.into_iter();
        let Some(first) = words.next() else {
            return Ok(None);
        };
        let mut tail: Vec<Vec<u8>> = words.collect();
        tail.append(&mut pending);
        pending = tail;
        if first == WORKTREE_SUBCOMMAND.as_bytes() {
            // The word after `worktree` in the command Git will run: the
            // body's next word, or else the caller's first argument.
            let subcommand = match pending.first() {
                Some(word) => word.as_slice(),
                None => arguments
                    .get(context.command_index + 1)
                    .map_or(&[][..], |argument| argument.as_encoded_bytes()),
            };
            if lifecycle_only && !is_lifecycle_subcommand(subcommand) {
                return Ok(None);
            }
            // Git runs a builtin or a `git-<name>` program before it looks
            // for an alias of that name, so a chain that passes through one
            // never reaches this worktree command. A deprecated builtin is
            // the exception: Git prefers its alias, so it is not in the set.
            // Only Git knows the full set, and asking costs a process, so the
            // question waits until here, the one outcome it can change.
            let commands = Git::default().commands_run_before_aliases()?;
            if commands.iter().any(|command| seen.contains(command)) {
                return Ok(None);
            }
            let mut expanded = arguments[..context.command_index].to_vec();
            expanded.append(&mut options);
            expanded.push(OsString::from(WORKTREE_SUBCOMMAND));
            for word in pending {
                expanded.push(alias_argument(word)?);
            }
            expanded.extend_from_slice(&arguments[context.command_index + 1..]);
            return Ok(Some(expanded));
        }
        // Git rejects an alias key with a byte outside ASCII, so a command
        // word that is not UTF-8 cannot name another alias.
        let Ok(first) = String::from_utf8(first) else {
            return Ok(None);
        };
        name = first;
    }
}

/// Whether Git's locale-independent `isspace` accepts the byte: space, tab,
/// line feed, and carriage return, and nothing else. Vertical tab, form feed,
/// and the bytes of Unicode spaces such as U+00A0 are ordinary word bytes.
fn is_git_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether Git runs an alias whose body starts with these global options.
///
/// Verified against git 2.53.0: Git dies ("alias '<name>' changes environment
/// variables") on every option that sets `envchanged` in its `handle_options`,
/// and the remaining ones print usage or a path and exit, so only these reach
/// the command behind them.
fn git_runs_alias_options(options: &[OsString]) -> bool {
    let mut options = options.iter();
    while let Some(option) = options.next() {
        let runs = if option == "-p" || option == "--paginate" {
            true
        } else if option == "-c" || option == "--config-env" {
            options.next().is_some()
        } else if option == "-C" {
            options.next().is_some_and(|directory| directory.is_empty())
        } else {
            let option = option.as_encoded_bytes();
            option.starts_with(b"--exec-path=") || option.starts_with(b"--config-env=")
        };
        if !runs {
            return false;
        }
    }
    true
}

/// Split an alias body exactly the way Git's own `split_cmdline` does,
/// verified against git 2.53.0: Git splits bytes, so the words are bytes too;
/// a run of Git whitespace ends one word and starts the next, so the body is
/// never trimmed and a leading or trailing run yields an empty first or last
/// word; single quotes are fully literal; inside double quotes and in the
/// open, a backslash makes the next byte literal (`l\og` is `log`, `"a\tb"`
/// is `atb`). `Err` mirrors the inputs Git itself refuses (an unclosed quote
/// or a trailing backslash) so the caller can pass the command through and
/// let Git fail it identically, with no room for a parse that Git accepts and
/// Riftri reads differently.
fn split_alias_words(value: &[u8]) -> Result<Vec<Vec<u8>>, ()> {
    #[derive(PartialEq)]
    enum State {
        Open,
        SingleQuoted,
        DoubleQuoted,
    }
    let mut state = State::Open;
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut bytes = value.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match state {
            State::SingleQuoted => match byte {
                b'\'' => state = State::Open,
                other => word.push(other),
            },
            State::DoubleQuoted => match byte {
                b'"' => state = State::Open,
                b'\\' => word.push(bytes.next().ok_or(())?),
                other => word.push(other),
            },
            State::Open => match byte {
                b'\'' => state = State::SingleQuoted,
                b'"' => state = State::DoubleQuoted,
                b'\\' => word.push(bytes.next().ok_or(())?),
                byte if is_git_space(byte) => {
                    words.push(std::mem::take(&mut word));
                    while bytes.next_if(|next| is_git_space(*next)).is_some() {}
                }
                other => word.push(other),
            },
        }
    }
    if state != State::Open {
        return Err(());
    }
    words.push(word);
    Ok(words)
}

/// Every reading of where the global options end and the command begins.
///
/// There is one reading unless the word after an unrecognized global option
/// could be that option's separate value; then that word is the command in
/// one reading and the scan continues past it for the other. A global option
/// that is missing its value ends the scan: Git refuses the invocation.
fn command_contexts(current_directory: &Path, arguments: &[OsString]) -> Vec<CommandContext> {
    let mut contexts = Vec::new();
    let mut repository = current_directory.to_path_buf();
    let mut git_directory = None;
    let mut global_options = GlobalOptionScope::None;
    let mut invocation_aliases = Vec::new();
    let mut possible_value_index = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "-C" {
            let Some(directory) = arguments.get(index + 1) else {
                break;
            };
            let directory = Path::new(directory);
            repository = if directory.is_absolute() {
                directory.to_path_buf()
            } else {
                repository.join(directory)
            };
            index += 2;
        } else if argument == "--git-dir" || argument == "--work-tree" {
            let Some(value) = arguments.get(index + 1) else {
                break;
            };
            if argument == "--git-dir" {
                git_directory = Some(value.clone());
            }
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if let Some(value) = option_value(argument, "--git-dir=") {
            git_directory = Some(value);
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if argument == "--no-pager"
            || argument == "--paginate"
            || argument == "-p"
            || argument == "-P"
        {
            index += 1;
        } else if argument == "-c" {
            let Some(assignment) = arguments.get(index + 1) else {
                break;
            };
            record_invocation_alias(&mut invocation_aliases, assignment);
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if argument == "--config-env" {
            let Some(assignment) = arguments.get(index + 1) else {
                break;
            };
            record_environment_alias(&mut invocation_aliases, assignment);
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if let Some(assignment) = option_value(argument, "--config-env=") {
            record_environment_alias(&mut invocation_aliases, &assignment);
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if argument == "--namespace"
            || argument == "--attr-source"
            || argument == "--shallow-file"
        {
            // Git takes the next word as this option's value, whatever it
            // spells, so it must never be read as the command.
            if arguments.get(index + 1).is_none() {
                break;
            }
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if argument == "--no-optional-locks"
            || argument == "--no-advice"
            || argument == "--literal-pathspecs"
        {
            // Advice suppression and optional-lock avoidance never change what
            // a checkout contains, and `git worktree` takes no pathspec, so
            // literal pathspec matching cannot change it either. IDEs pass
            // `--no-optional-locks` on every Git call, so refusing these would
            // break ordinary editor integration.
            global_options.observe(GlobalOptionScope::CheckoutNeutral);
            index += 1;
        } else if argument.to_string_lossy().starts_with("--namespace=")
            || argument.as_encoded_bytes().starts_with(b"--attr-source=")
            || argument.as_encoded_bytes().starts_with(b"--exec-path=")
            || argument.as_encoded_bytes().starts_with(b"--work-tree=")
            || argument == "--no-replace-objects"
            || argument == "--no-lazy-fetch"
            || argument == "--glob-pathspecs"
            || argument == "--noglob-pathspecs"
            || argument == "--icase-pathspecs"
            || argument == "--bare"
        {
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if argument.to_string_lossy().starts_with('-') {
            // An option this parser does not know. Treating it as "not our
            // business" would fail *open*: `git --future-option worktree
            // remove <managed>` would reach real Git with no lifecycle guard,
            // even though the same command without the option is refused, and
            // an unrecognized *subcommand* option already fails closed. Record
            // it as significant, which disqualifies the optimized paths and
            // leaves the managed-path guard in charge, and keep scanning. Its
            // arity is unknown, so unless a value is attached with `=` the
            // next word may be its value.
            if !argument.as_encoded_bytes().contains(&b'=') {
                possible_value_index = Some(index + 1);
            }
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else {
            let git_directory = git_directory
                .as_deref()
                .map(|path| resolve_command_path(&repository, path));
            contexts.push(CommandContext {
                repository: repository.clone(),
                git_directory,
                command_index: index,
                global_options,
                invocation_aliases: invocation_aliases.clone(),
            });
            if possible_value_index != Some(index) {
                break;
            }
            index += 1;
        }
    }
    contexts
}

fn resolve_command_path(base: &Path, value: &OsStr) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn option_value(argument: &OsStr, prefix: &str) -> Option<OsString> {
    let encoded = argument.as_encoded_bytes();
    let value = encoded.strip_prefix(prefix.as_bytes())?;
    // SAFETY: the split occurs immediately after an ASCII prefix, which is a
    // valid boundary in the platform-independent encoded representation.
    Some(unsafe { OsStr::from_encoded_bytes_unchecked(value) }.to_os_string())
}

/// `-h` and `--help` ask Git to print usage and exit without touching the
/// repository, exactly like the `remove` and `list` subcommands Riftri already
/// delegates. Intercepting them would leave `worktree add` as the one
/// subcommand whose documentation is unreachable inside an enabled repository.
fn requests_git_help(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .take_while(|argument| *argument != "--")
        .any(|argument| argument == "-h" || argument == "--help")
}

fn plan_enabled_add(
    repository: &Path,
    directory: &Path,
    arguments: &[OsString],
    global_options: GlobalOptionScope,
) -> Result<GitProxyPlan, ActivationError> {
    if requests_git_help(arguments) {
        return Ok(GitProxyPlan::Passthrough);
    }
    match global_options {
        // An option Riftri cannot reproduce must not be silently dropped, and
        // an add is not safe to hand to ordinary Git once Riftri would have
        // optimized it, so this stays a visible refusal.
        GlobalOptionScope::Significant => Err(unsupported(format!(
            "Git invocation-level configuration is not supported by the optimized add path; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
        ))),
        // Nothing about the request is unreproducible, but Riftri also gains
        // nothing by claiming the option: ordinary Git honors it exactly.
        GlobalOptionScope::CheckoutNeutral => Ok(GitProxyPlan::Passthrough),
        GlobalOptionScope::None => parse_enabled_add(repository, directory, arguments)
            .map(|(request, quiet)| GitProxyPlan::OptimizedAdd { request, quiet }),
    }
}

fn parse_enabled_add(
    repository: &Path,
    directory: &Path,
    arguments: &[OsString],
) -> Result<(AddWorktreeRequest, bool), ActivationError> {
    let mut mode = None;
    let mut positional = Vec::new();
    let mut options = true;
    let mut quiet = false;
    let mut index = 0;

    while let Some(argument) = arguments.get(index) {
        if options && argument == "--" {
            options = false;
        } else if options && argument == "-b" {
            let branch = arguments
                .get(index + 1)
                .ok_or_else(|| unsupported("-b requires a new branch name"))?;
            set_mode(&mut mode, WorktreeMode::NewBranch(branch.clone()))?;
            index += 1;
        } else if options && matches!(argument.to_str(), Some("--detach" | "-d")) {
            set_mode(&mut mode, WorktreeMode::Detached)?;
        } else if options && matches!(argument.to_str(), Some("--quiet" | "-q")) {
            quiet = true;
        } else if options && argument == "--no-quiet" {
            quiet = false;
        } else if options && argument == "--checkout" {
            // Optimized adds always create a checked-out, clean result.
        } else if options
            && matches!(
                argument.to_str(),
                Some("--no-lock" | "--no-force" | "--no-orphan" | "--no-guess-remote")
            )
        {
            // Explicit defaults: each names what the optimized add already does
            // — no lock, no force, a branch with history, and no remote
            // guessing. `--no-track` is deliberately absent: new branches follow
            // Git's tracking rules, so it would change the result.
        } else if options && is_short_option_cluster(argument) {
            // Git's parser also takes short options bundled (`-qd`) and a value
            // stuck to its option (`-bname`); refusing them made
            // `git worktree add -qb task …` fail under interception (#437).
            // Every letter is held to the same rules as its spaced form.
            let cluster = argument.to_str().ok_or_else(|| {
                unsupported(format!(
                    "write short options separately, as in `-b <branch>`, when a value is not valid UTF-8; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
                ))
            })?;
            for (offset, letter) in cluster.char_indices().skip(1) {
                match letter {
                    'q' => quiet = true,
                    'd' => set_mode(&mut mode, WorktreeMode::Detached)?,
                    'b' => {
                        let stuck = &cluster[offset + letter.len_utf8()..];
                        let branch = if stuck.is_empty() {
                            index += 1;
                            arguments
                                .get(index)
                                .cloned()
                                .ok_or_else(|| unsupported("-b requires a new branch name"))?
                        } else {
                            OsString::from(stuck)
                        };
                        set_mode(&mut mode, WorktreeMode::NewBranch(branch))?;
                        break;
                    }
                    other => {
                        return Err(unsupported(format!(
                            "option -{other} is not supported by the optimized add path; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
                        )));
                    }
                }
            }
        } else if options
            && (argument == "--sparse"
                || argument == "--sparse-dir"
                || argument.to_string_lossy().starts_with("--sparse-dir="))
        {
            return Err(unsupported(format!(
                "sparse worktrees are only available through the explicit `riftri worktree add --sparse-dir` interface; intercepted Git adds cannot request a sparse view yet. Set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
            )));
        } else if options && argument.to_string_lossy().starts_with('-') {
            return Err(unsupported(format!(
                "option {} is not supported by the optimized add path; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(argument.clone());
        }
        index += 1;
    }

    if !(1..=2).contains(&positional.len()) {
        return Err(unsupported(
            "expected `git worktree add [options] <path> [<commit-ish>]`",
        ));
    }

    let revision = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| OsString::from("HEAD"));
    let mode = match mode {
        Some(mode) => mode,
        // `git worktree add <path> <commit-ish>` without a mode flag only
        // checks out a branch when `<commit-ish>` names an existing local
        // branch. For a tag, a raw commit, `HEAD`, or a remote-tracking ref
        // real Git detaches or creates a DWIM tracking branch instead, neither
        // of which the optimized path reproduces. Decide that here, from one
        // ref lookup, rather than letting the request fail deep inside the add
        // after several Git processes have already run.
        None if positional.len() == 2 => {
            if Git::default()
                .local_branch_target(repository, &revision)?
                .is_none()
            {
                let revision = revision.to_string_lossy().into_owned();
                return Err(unsupported(format!(
                    "optimized add checks out an existing local branch, and `{revision}` is not one; ordinary Git would create a detached or remote-tracking worktree instead. Use `--detach` to check out `{revision}` detached, `-b <new-branch>` to create a branch, or set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
                )));
            }
            WorktreeMode::ExistingBranch(revision.clone())
        }
        None => {
            return Err(unsupported(format!(
                "optimized add requires an existing local branch, `-b <new-branch>`, or `--detach`; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
            )));
        }
    };

    let destination = resolve_command_path(directory, &positional[0]);

    Ok((
        AddWorktreeRequest {
            repository: repository.to_path_buf(),
            destination,
            revision,
            mode,
            state_dir: None,
            sparse_directories: Vec::new(),
        },
        quiet,
    ))
}

/// Whether `argument` is a cluster of short options, such as `-qd` or
/// `-bname`: one dash followed by more than one character. `-` alone and
/// long options (`--…`) are not clusters.
fn is_short_option_cluster(argument: &OsStr) -> bool {
    let bytes = argument.as_encoded_bytes();
    bytes.len() > 2 && bytes[0] == b'-' && bytes[1] != b'-'
}

fn parse_enabled_remove(
    repository: &Path,
    repository_info: &RepositoryInfo,
    directory: &Path,
    arguments: &[OsString],
) -> Result<Option<(RemoveWorktreeRequest, bool)>, ActivationError> {
    let mut force = false;
    let mut options = true;
    let mut positional = Vec::new();
    for argument in arguments {
        if options && argument == "--" {
            options = false;
        } else if options && matches!(argument.to_str(), Some("--force" | "-f")) {
            force = true;
        } else if options && argument.to_string_lossy().starts_with('-') {
            return Ok(None);
        } else {
            positional.push(argument);
        }
    }
    let [path] = positional.as_slice() else {
        return Ok(None);
    };
    let destination = Git::default().resolve_worktree_path(repository, directory, path)?;
    let Some(state_directory) = managed_worktree_state_directory_in(repository_info, &destination)?
    else {
        return Ok(None);
    };
    Ok(Some((
        RemoveWorktreeRequest {
            repository: repository.to_path_buf(),
            destination,
            state_dir: Some(state_directory),
        },
        force,
    )))
}

fn plan_enabled_remove(
    repository: &Path,
    repository_info: &RepositoryInfo,
    directory: &Path,
    arguments: &[OsString],
    optimization_compatible: bool,
) -> Result<GitProxyPlan, ActivationError> {
    if optimization_compatible
        && let Some((request, force)) =
            parse_enabled_remove(repository, repository_info, directory, arguments)?
    {
        return Ok(if force {
            GitProxyPlan::OptimizedForceRemove(request)
        } else {
            GitProxyPlan::OptimizedRemove(request)
        });
    }
    guard_managed_path_lifecycle(repository, repository_info, directory, arguments, "remove")?;
    Ok(GitProxyPlan::Passthrough)
}

fn parse_enabled_move(
    repository: &Path,
    repository_info: &RepositoryInfo,
    directory: &Path,
    arguments: &[OsString],
) -> Result<Option<MoveWorktreeRequest>, ActivationError> {
    let (source, destination) = match arguments {
        [source, destination] => (source, destination),
        [separator, source, destination] if separator == "--" => (source, destination),
        _ => return Ok(None),
    };
    let source = Git::default().resolve_worktree_path(repository, directory, source)?;
    let Some(state_directory) = managed_worktree_state_directory_in(repository_info, &source)?
    else {
        return Ok(None);
    };
    Ok(Some(MoveWorktreeRequest {
        repository: repository.to_path_buf(),
        source,
        destination: resolve_command_path(directory, destination),
        state_dir: Some(state_directory),
    }))
}

fn plan_enabled_move(
    repository: &Path,
    repository_info: &RepositoryInfo,
    directory: &Path,
    arguments: &[OsString],
    optimization_compatible: bool,
) -> Result<GitProxyPlan, ActivationError> {
    if optimization_compatible
        && let Some(request) =
            parse_enabled_move(repository, repository_info, directory, arguments)?
    {
        return Ok(GitProxyPlan::OptimizedMove(request));
    }
    guard_managed_path_lifecycle(repository, repository_info, directory, arguments, "move")?;
    Ok(GitProxyPlan::Passthrough)
}

/// `git worktree prune` options that only report what a prune would do.
///
/// `-n`/`--dry-run` is Git's own "do not remove anything; just report what it
/// would remove", and the verbosity and help options change nothing either, so
/// real Git can answer all of them without touching lifecycle metadata.
/// `-v`/`--verbose` is deliberately absent: for `git worktree prune` it is a
/// verbose *prune*, not a report, so it must never bypass the journaled path.
fn is_prune_help_option(argument: &OsString) -> bool {
    matches!(argument.to_str(), Some("-h" | "--help"))
}

fn is_prune_dry_run_option(argument: &OsString) -> bool {
    matches!(argument.to_str(), Some("-n" | "--dry-run"))
}

fn is_prune_verbose_option(argument: &OsString) -> bool {
    matches!(argument.to_str(), Some("-v" | "--verbose"))
}

/// Every documented `git worktree prune` option, so a dry run combined only
/// with recognized options can delegate while anything unknown fails closed.
fn is_recognized_prune_argument(argument: &OsString) -> bool {
    if is_prune_help_option(argument)
        || is_prune_dry_run_option(argument)
        || is_prune_verbose_option(argument)
    {
        return true;
    }
    matches!(argument.to_str(), Some("--expire"))
        || argument
            .to_str()
            .is_some_and(|argument| argument.starts_with("--expire="))
}

fn plan_enabled_prune(
    repository: &Path,
    repository_info: &RepositoryInfo,
    arguments: &[OsString],
    global_options: GlobalOptionScope,
) -> Result<GitProxyPlan, ActivationError> {
    // Help always delegates: git prints usage and touches nothing.
    if arguments.iter().any(is_prune_help_option) {
        return Ok(GitProxyPlan::Passthrough);
    }
    // A dry run reports without mutating, and in git it wins over `-v` and
    // `--expire`; delegate only when every other argument is a documented
    // prune option (an `--expire` value may follow its flag), so an unknown
    // option still fails closed below.
    if arguments.iter().any(is_prune_dry_run_option) {
        let mut expecting_expire_value = false;
        let recognized = arguments.iter().all(|argument| {
            if expecting_expire_value {
                expecting_expire_value = false;
                return true;
            }
            if argument.to_str() == Some("--expire") {
                expecting_expire_value = true;
                return true;
            }
            is_recognized_prune_argument(argument)
        });
        if recognized && !expecting_expire_value {
            return Ok(GitProxyPlan::Passthrough);
        }
    }

    let mut managed_states = Vec::new();
    for state_directory in repository_state_directories_with_git(&Git::default(), repository_info)?
    {
        let status = storage_accounting(&state_directory)?;
        if status.active_views > 0
            || status.pending_adds > 0
            || status.pending_removals > 0
            || status.pending_moves > 0
            || status.pending_prunes > 0
            || !status.diagnostic_issues.is_empty()
        {
            managed_states.push(state_directory);
        }
    }
    if managed_states.is_empty() {
        return Ok(GitProxyPlan::Passthrough);
    }
    if arguments.is_empty() || arguments.iter().all(is_prune_verbose_option) {
        // A verbose bare prune removes exactly what a bare prune removes, so
        // it takes the same journaled path (without the listing). A
        // checkout-neutral global option cannot change what a prune removes,
        // so the journaled prune still reproduces the requested invocation.
        if global_options != GlobalOptionScope::Significant {
            return Ok(GitProxyPlan::OptimizedPrune(PruneWorktreesRequest {
                repository: repository.to_path_buf(),
                state_dir: managed_states.into_iter().next(),
            }));
        }
        return Err(unsupported(format!(
            "refusing `git worktree prune` under invocation-level Git configuration while managed Riftri state exists because Git could change lifecycle metadata outside the Riftri journal; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
        )));
    }
    let options = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Err(unsupported(format!(
        "refusing `git worktree prune {options}` while managed Riftri state exists because Git could change lifecycle metadata outside the Riftri journal; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
    )))
}

fn guard_managed_path_lifecycle(
    repository: &Path,
    repository_info: &RepositoryInfo,
    directory: &Path,
    arguments: &[OsString],
    operation: &str,
) -> Result<(), ActivationError> {
    let Some(path) = positional_paths(arguments).first().copied() else {
        return Ok(());
    };
    let path = Git::default().resolve_worktree_path(repository, directory, path)?;
    if managed_worktree_state_directory_in(repository_info, &path)?.is_some() {
        return Err(unsupported(format!(
            "refusing `git worktree {operation}` options that would bypass the journal for managed Riftri worktree {}; use a supported Riftri lifecycle command instead",
            path.display()
        )));
    }
    Ok(())
}

fn positional_paths(arguments: &[OsString]) -> Vec<&OsStr> {
    let mut options = true;
    arguments
        .iter()
        .filter_map(|argument| {
            if options && argument == "--" {
                options = false;
                None
            } else if options && argument.to_string_lossy().starts_with('-') {
                None
            } else {
                Some(argument.as_os_str())
            }
        })
        .collect()
}

fn set_mode(
    current: &mut Option<WorktreeMode>,
    requested: WorktreeMode,
) -> Result<(), ActivationError> {
    if current.is_some() {
        return Err(unsupported(
            "choose exactly one of `-b <branch>` or `--detach`",
        ));
    }
    *current = Some(requested);
    Ok(())
}

fn unsupported(message: impl Into<String>) -> ActivationError {
    ActivationError::UnsupportedWorktreeCommand(message.into())
}

fn environment_truthy(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
    })
}

/// Report whether the inherited shim environment is complete enough for
/// optimized interception: the activation marker must be set and the recorded
/// real Git executable must still be present.
pub fn shim_environment_complete() -> bool {
    env::var_os(SHIM_ACTIVE_ENV).is_some()
        && env::var_os(riftri_git::REAL_GIT_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .as_deref()
            .is_some_and(is_executable_file)
}

/// Report whether the current `git`-named invocation still looks like a
/// Riftri shim even though the activation marker is gone: either a process
/// scope recorded its shim directory, or the first `git` resolved from `PATH`
/// is a Riftri shim (identified by its real-Git marker or because it resolves
/// to the currently running executable).
pub fn stripped_shim_scope_detected() -> bool {
    if env::var_os(PROCESS_SHIM_DIR_ENV).is_some_and(|path| !path.is_empty()) {
        return true;
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let current_executable = canonical_current_executable();
    for directory in env::split_paths(&path) {
        let candidates = git_executable_candidates(&directory);
        if !candidates.iter().any(|path| is_executable_file(path)) {
            continue;
        }
        if directory.join(REAL_GIT_MARKER_FILE).is_file() {
            return true;
        }
        return candidates.iter().any(|candidate| {
            current_executable.as_deref().is_some_and(|executable| {
                fs::canonicalize(candidate).is_ok_and(|candidate| candidate == executable)
            })
        });
    }
    false
}

/// Delegate a `git`-named invocation to the real Git executable although the
/// shim environment is missing or inconsistent. A broken shim must never
/// answer as Riftri or intercept anything, so this performs a plain
/// passthrough with inherited standard streams and exit status — and, like
/// every other shim delegation, under the termination contract, so a signal
/// aimed at the shim cannot orphan the real Git it started.
pub fn delegate_stripped_shim_invocation(arguments: &[OsString]) -> Result<i32, ActivationError> {
    let real_git = resolve_real_git_for_stripped_shim().ok_or_else(|| {
        process_error(
            "the Riftri Git shim lost its environment and could not locate the real Git \
             executable; run `riftri shell deactivate <shell>` in an activated shell or exit \
             the `riftri exec` session, then retry",
        )
    })?;
    let mut command = Command::new(&real_git);
    command.args(arguments);
    let status =
        riftri_git::termination::run_forwarding_terminations(&mut command).map_err(|error| {
            process_error(format!(
                "delegate to the real Git executable {}: {error}",
                real_git.display()
            ))
        })?;
    Ok(exit_status_code(status))
}

/// Record the captured real Git path next to a shim so the shim keeps a
/// delegation target even when its environment is stripped later. The marker
/// is written atomically because durable shell shim directories are shared by
/// concurrently activating shells.
fn record_real_git_marker(directory: &Path, real_git: &Path) -> Result<(), ActivationError> {
    let Some(contents) = real_git_marker_bytes(real_git) else {
        // A non-representable path only loses the stripped-environment
        // fallback; PATH re-resolution still works, so do not fail activation.
        return Ok(());
    };
    let nonce = std::process::id();
    let temporary = directory.join(format!(".{REAL_GIT_MARKER_FILE}-{nonce}.tmp"));
    fs::write(&temporary, contents).map_err(|error| {
        process_error(format!(
            "record real Git path in {}: {error}",
            directory.display()
        ))
    })?;
    let destination = directory.join(REAL_GIT_MARKER_FILE);
    fs::rename(&temporary, &destination).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        process_error(format!(
            "activate real Git marker {}: {error}",
            destination.display()
        ))
    })
}

#[cfg(unix)]
fn real_git_marker_bytes(real_git: &Path) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    Some(real_git.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn real_git_marker_bytes(real_git: &Path) -> Option<Vec<u8>> {
    real_git.to_str().map(|path| path.as_bytes().to_vec())
}

#[cfg(unix)]
fn read_real_git_marker(directory: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let contents = fs::read(directory.join(REAL_GIT_MARKER_FILE)).ok()?;
    Some(PathBuf::from(OsString::from_vec(contents)))
}

#[cfg(not(unix))]
fn read_real_git_marker(directory: &Path) -> Option<PathBuf> {
    let contents = fs::read(directory.join(REAL_GIT_MARKER_FILE)).ok()?;
    Some(PathBuf::from(String::from_utf8(contents).ok()?))
}

fn canonical_current_executable() -> Option<PathBuf> {
    env::current_exe().and_then(fs::canonicalize).ok()
}

/// Locate a real Git executable for a shim whose environment was stripped:
/// prefer whatever the environment still records, then the path baked into a
/// shim directory at creation, and finally a `PATH` walk that skips every
/// Riftri shim directory so the shim can never select itself.
fn resolve_real_git_for_stripped_shim() -> Option<PathBuf> {
    resolve_stripped_real_git(
        env::var_os(riftri_git::REAL_GIT_ENV),
        env::var_os(PROCESS_SHIM_DIR_ENV).map(PathBuf::from),
        env::var_os("PATH"),
        canonical_current_executable(),
    )
}

fn resolve_stripped_real_git(
    recorded_real_git: Option<OsString>,
    process_shim_directory: Option<PathBuf>,
    path: Option<OsString>,
    current_executable: Option<PathBuf>,
) -> Option<PathBuf> {
    let not_self = |candidate: &Path| {
        current_executable.as_deref().is_none_or(|executable| {
            fs::canonicalize(candidate).is_ok_and(|candidate| candidate != executable)
        })
    };

    if let Some(recorded) = recorded_real_git
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| is_executable_file(path) && not_self(path))
    {
        return Some(recorded);
    }
    if let Some(baked) = process_shim_directory
        .as_deref()
        .and_then(read_real_git_marker)
        .filter(|path| is_executable_file(path) && not_self(path))
    {
        return Some(baked);
    }

    for directory in env::split_paths(path.as_deref()?) {
        if process_shim_directory
            .as_deref()
            .is_some_and(|shim| shim == directory)
        {
            continue;
        }
        if directory.join(REAL_GIT_MARKER_FILE).is_file() {
            // Another shim directory: its baked marker names the real Git,
            // while its own `git` entry must never be executed.
            if let Some(baked) = read_real_git_marker(&directory)
                .filter(|path| is_executable_file(path) && not_self(path))
            {
                return Some(baked);
            }
            continue;
        }
        if let Some(candidate) = git_executable_candidates(&directory)
            .into_iter()
            .find(|candidate| is_executable_file(candidate) && not_self(candidate))
        {
            return Some(candidate);
        }
    }
    None
}

fn locate_real_git() -> Result<PathBuf, ActivationError> {
    if let Some(command) = env::var_os(SHIM_ACTIVE_ENV)
        .and_then(|_| env::var_os(riftri_git::REAL_GIT_ENV))
        .filter(|path| !path.is_empty())
    {
        return Ok(PathBuf::from(command));
    }

    let path =
        env::var_os("PATH").ok_or_else(|| process_error("PATH is not set; cannot locate Git"))?;
    real_git_on_path(
        &path,
        env::current_exe()
            .ok()
            .and_then(|executable| fs::canonicalize(executable).ok()),
    )
    .ok_or_else(|| process_error("could not find the real Git executable on PATH"))
}

/// Walk `PATH` for the Git executable that later calls are delegated to.
///
/// This records the binary every subsequent Git call runs, so
/// it must never select a shim: a shell whose durable shim is still on `PATH`
/// but whose marker variable has been cleared would otherwise bake Riftri in as
/// the real Git, and every Git command would then re-enter the shim forever.
/// [`resolve_stripped_real_git`] already guards its own walk this way.
fn real_git_on_path(path: &OsStr, current_executable: Option<PathBuf>) -> Option<PathBuf> {
    let not_self = |candidate: &Path| {
        current_executable.as_deref().is_none_or(|executable| {
            fs::canonicalize(candidate).is_ok_and(|candidate| candidate != executable)
        })
    };
    for directory in env::split_paths(path) {
        if directory.join(REAL_GIT_MARKER_FILE).is_file() {
            // A shim directory: its baked marker names the real Git, while its
            // own `git` entry must never be executed.
            if let Some(baked) = read_real_git_marker(&directory)
                .filter(|path| is_executable_file(path) && not_self(path))
            {
                return Some(baked);
            }
            continue;
        }
        if let Some(candidate) = git_executable_candidates(&directory)
            .into_iter()
            .find(|candidate| is_executable_file(candidate) && not_self(candidate))
        {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn git_executable_candidates(directory: &Path) -> Vec<PathBuf> {
    vec![directory.join("git")]
}

#[cfg(target_os = "windows")]
fn git_executable_candidates(directory: &Path) -> Vec<PathBuf> {
    vec![directory.join("git.exe"), directory.join("git.cmd")]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn install_git_shim(directory: &Path, current_executable: &Path) -> Result<(), ActivationError> {
    std::os::unix::fs::symlink(current_executable, directory.join("git"))
        .map_err(|error| process_error(format!("install process-scoped Git shim: {error}")))
}

#[cfg(target_os = "windows")]
fn install_git_shim(directory: &Path, current_executable: &Path) -> Result<(), ActivationError> {
    let destination = directory.join("git.exe");
    match std::fs::hard_link(current_executable, &destination) {
        Ok(()) => Ok(()),
        Err(_) => std::fs::copy(current_executable, &destination)
            .map(|_| ())
            .map_err(|error| process_error(format!("install process-scoped Git shim: {error}"))),
    }
}

#[cfg(unix)]
fn exit_status_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

#[cfg(not(unix))]
fn exit_status_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn process_error(message: impl Into<String>) -> ActivationError {
    ActivationError::Process(message.into())
}

fn worktree_binding_error(message: impl Into<String>) -> ActivationError {
    ActivationError::WorktreeBinding(message.into())
}

fn shell_error(message: impl Into<String>) -> ActivationError {
    ActivationError::Shell(message.into())
}

#[cfg(all(test, unix))]
#[path = "activation_hegel_tests.rs"]
mod hegel_tests;

#[cfg(all(test, unix))]
#[path = "activation_add_hegel_tests.rs"]
mod add_hegel_tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "activation_shim_hegel_tests.rs"]
mod shim_hegel_tests;

// Creating a managed worktree needs a native copy-on-write backend.
#[cfg(all(
    test,
    any(
        target_os = "macos",
        all(feature = "native-cow-integration", target_os = "linux")
    )
))]
#[path = "activation_lifecycle_hegel_tests.rs"]
mod lifecycle_hegel_tests;

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{
        BYPASS_ENV, GitProxyPlan, disable_repository, enable_repository, plan_git_command,
        repository_activation,
    };
    #[cfg(unix)]
    use super::{
        PROCESS_SHIM_DIR_ENV, PROCESS_SHIM_DIR_PREFIX, REAL_GIT_MARKER_FILE, SHELL_SHIM_DIR_ENV,
        prepare_posix_shell_deactivation_inner, record_real_git_marker, resolve_stripped_real_git,
    };
    use crate::WorktreeMode;

    #[test]
    fn repository_enable_is_local_and_reversible() {
        let fixture = repository_fixture();

        let enabled = enable_repository(fixture.path()).expect("enable repository");
        assert!(enabled.enabled);
        assert!(
            repository_activation(fixture.path())
                .expect("read activation")
                .enabled
        );

        let disabled = disable_repository(fixture.path()).expect("disable repository");
        assert!(!disabled.enabled);
        assert!(
            !repository_activation(fixture.path())
                .expect("read activation")
                .enabled
        );
    }

    #[cfg(unix)]
    #[test]
    fn activation_preserves_trailing_line_endings_in_repository_paths() {
        let fixture = tempdir().expect("temporary directory");
        let neighbor = fixture.path().join("repository");
        fs::create_dir(&neighbor).expect("create neighboring repository");
        git(&neighbor, &["init", "--quiet"]);
        let client = riftri_git::Git::default();

        for suffix in ["\n", "\r", "\r\n"] {
            let repository = fixture.path().join(format!("repository{suffix}"));
            fs::create_dir(&repository).expect("create repository");
            git(&repository, &["init", "--quiet"]);

            enable_repository(&repository).expect("enable selected repository");
            assert_eq!(
                client
                    .local_config_bool(&repository, "riftri.enabled")
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                client
                    .local_config_bool(&neighbor, "riftri.enabled")
                    .unwrap(),
                None
            );

            git(&neighbor, &["config", "riftri.enabled", "true"]);
            disable_repository(&repository).expect("disable selected repository");
            assert_eq!(
                client
                    .local_config_bool(&repository, "riftri.enabled")
                    .unwrap(),
                None
            );
            assert_eq!(
                client
                    .local_config_bool(&neighbor, "riftri.enabled")
                    .unwrap(),
                Some(true)
            );
            git(&neighbor, &["config", "--unset", "riftri.enabled"]);
        }
    }

    /// `git gc` prunes worktrees itself — and `gc.auto` fires it from ordinary
    /// commands like `git commit`. Those inner calls resolve `git` from Git's
    /// own exec-path, so they never re-enter the shim and never reach the
    /// journaled prune the proxy plans. Enabling a repository must pin Git's
    /// own expiry so Git declines to prune managed worktrees at the source.
    #[test]
    fn enabling_pins_gits_worktree_prune_expiry_and_disabling_restores_it() {
        let client = riftri_git::Git::default();
        let fixture = repository_fixture();
        let repository = fixture.path();

        assert_eq!(
            client
                .local_config_value(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)
                .unwrap(),
            None
        );
        enable_repository(repository).expect("enable repository");
        assert_eq!(
            client
                .local_config_value(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)
                .unwrap()
                .as_deref(),
            Some(super::WORKTREE_PRUNE_EXPIRE_NEVER.as_bytes())
        );

        disable_repository(repository).expect("disable repository");
        assert_eq!(
            client
                .local_config_value(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)
                .unwrap(),
            None,
            "disabling must remove the value Riftri wrote"
        );
    }

    /// An expiry the user chose is theirs: enabling must not overwrite it, and
    /// disabling must not delete it.
    #[test]
    fn worktree_prune_protection_preserves_a_user_chosen_expiry() {
        let client = riftri_git::Git::default();
        let fixture = repository_fixture();
        let repository = fixture.path();
        let chosen = OsStr::new("2.weeks.ago");
        client
            .set_local_config(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY, chosen)
            .expect("record a user-chosen expiry");

        enable_repository(repository).expect("enable repository");
        assert_eq!(
            client
                .local_config_value(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)
                .unwrap()
                .as_deref(),
            Some(b"2.weeks.ago".as_slice()),
            "enabling must not overwrite an expiry the user chose"
        );

        disable_repository(repository).expect("disable repository");
        assert_eq!(
            client
                .local_config_value(repository, super::WORKTREE_PRUNE_EXPIRE_CONFIG_KEY)
                .unwrap()
                .as_deref(),
            Some(b"2.weeks.ago".as_slice()),
            "disabling must not delete an expiry the user chose"
        );
    }

    #[test]
    fn enabled_standard_add_is_planned_as_an_optimized_worktree() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let arguments = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from("feature/activation"),
            OsString::from("../activated-view"),
            OsString::from("HEAD"),
        ];

        let GitProxyPlan::OptimizedAdd { request, .. } =
            plan_git_command(fixture.path(), &arguments).expect("plan Git command")
        else {
            panic!("enabled worktree add was not optimized");
        };
        assert_eq!(
            request.destination,
            fixture.path().join("../activated-view")
        );
        assert_eq!(request.revision, OsStr::new("HEAD"));
        assert_eq!(
            request.mode,
            WorktreeMode::NewBranch(OsString::from("feature/activation"))
        );
    }

    #[test]
    fn enabled_existing_branch_add_is_planned_as_an_optimized_worktree() {
        let fixture = repository_fixture();
        git(fixture.path(), &["branch", "feature/existing"]);
        enable_repository(fixture.path()).expect("enable repository");
        let arguments = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("../activated-view"),
            OsString::from("feature/existing"),
        ];

        let GitProxyPlan::OptimizedAdd { request, .. } =
            plan_git_command(fixture.path(), &arguments).expect("plan Git command")
        else {
            panic!("enabled existing-branch add was not optimized");
        };
        assert_eq!(request.revision, OsStr::new("feature/existing"));
        assert_eq!(
            request.mode,
            WorktreeMode::ExistingBranch(OsString::from("feature/existing"))
        );
    }

    /// Git's option parser accepts short forms, bundled short options, a value
    /// stuck to `-b`, and explicit defaults. A script is as likely to write
    /// `git worktree add -q …` as the spelled-out form; interception refused
    /// every one of these (#437).
    #[test]
    fn enabled_add_accepts_gits_standard_option_syntax() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let new_branch = |name: &str| WorktreeMode::NewBranch(OsString::from(name));
        let cases: Vec<(&[&str], WorktreeMode, bool)> = vec![
            (&["-q", "-b", "a", "../v"], new_branch("a"), true),
            (&["-d", "../v"], WorktreeMode::Detached, false),
            (&["-d", "../v", "HEAD"], WorktreeMode::Detached, false),
            (&["-bstuck", "../v"], new_branch("stuck"), false),
            (&["-qb", "bundled", "../v"], new_branch("bundled"), true),
            (&["-qbglued", "../v"], new_branch("glued"), true),
            (&["-qd", "../v"], WorktreeMode::Detached, true),
            (&["--no-quiet", "-b", "nq", "../v"], new_branch("nq"), false),
            // As in Git, the last of `-q` and `--no-quiet` wins.
            (
                &["-q", "--no-quiet", "-b", "last", "../v"],
                new_branch("last"),
                false,
            ),
            (
                &[
                    "--no-lock",
                    "--no-force",
                    "--no-orphan",
                    "--no-guess-remote",
                    "-b",
                    "defaults",
                    "../v",
                ],
                new_branch("defaults"),
                false,
            ),
            (&["../v", "-q", "-b", "after"], new_branch("after"), true),
        ];
        for (options, expected_mode, expected_quiet) in cases {
            let mut arguments = vec![OsString::from("worktree"), OsString::from("add")];
            arguments.extend(options.iter().map(OsString::from));
            let plan = plan_git_command(fixture.path(), &arguments)
                .unwrap_or_else(|error| panic!("{options:?} was refused: {error}"));
            let GitProxyPlan::OptimizedAdd { request, quiet } = plan else {
                panic!("{options:?} was not optimized");
            };
            assert_eq!(request.mode, expected_mode, "{options:?}");
            assert_eq!(quiet, expected_quiet, "{options:?}");
            assert_eq!(
                request.destination,
                fixture.path().join("../v"),
                "{options:?}"
            );
        }
    }

    /// Bundling must not smuggle in an option the optimized add does not
    /// support, and `--no-track` is not an inert default: new branches follow
    /// Git's tracking rules, so it would have to reach Git, like `--track`.
    #[test]
    fn enabled_add_still_refuses_unsupported_options_inside_clusters() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        for options in [
            &["-qf", "../v", "main"][..],
            &["-qB", "reset", "../v"],
            &["-dq", "-B", "reset", "../v"],
            &["--no-track", "-b", "x", "../v"],
        ] {
            let mut arguments = vec![OsString::from("worktree"), OsString::from("add")];
            arguments.extend(options.iter().map(OsString::from));
            let error = plan_git_command(fixture.path(), &arguments)
                .err()
                .unwrap_or_else(|| panic!("{options:?} must be refused"));
            assert!(
                error.to_string().contains("not supported"),
                "{options:?}: {error}"
            );
        }
    }

    #[test]
    fn disabled_repository_passes_the_same_add_to_git() {
        let fixture = repository_fixture();
        let arguments = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from("feature/ordinary"),
            OsString::from("../ordinary-view"),
        ];

        assert!(matches!(
            plan_git_command(fixture.path(), &arguments).expect("plan Git command"),
            GitProxyPlan::Passthrough
        ));
    }

    #[test]
    fn enabled_add_refuses_an_ambiguous_head_mode() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let arguments = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("../ambiguous-view"),
        ];

        let error = plan_git_command(fixture.path(), &arguments)
            .expect_err("ambiguous worktree add must fail");
        assert!(
            error
                .to_string()
                .contains("requires an existing local branch")
        );
    }

    #[test]
    fn enabled_add_refuses_a_sparse_request_with_a_precise_diagnostic() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        for sparse_argument in [
            "--sparse",
            "--sparse-dir",
            "--sparse-dir=crates/riftri-core",
        ] {
            let arguments = [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from(sparse_argument),
                OsString::from("-b"),
                OsString::from("feature/sparse"),
                OsString::from("../sparse-view"),
            ];

            let error = plan_git_command(fixture.path(), &arguments)
                .expect_err("intercepted sparse add must fail before any state exists");
            let message = error.to_string();
            assert!(
                message.contains("riftri worktree add --sparse-dir"),
                "{sparse_argument}: {message}"
            );
            assert!(
                message.contains("cannot request a sparse view"),
                "{sparse_argument}: {message}"
            );
        }
    }

    #[test]
    fn enabled_add_honors_git_dash_c_repository_selection() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let current = fixture.path().parent().expect("fixture parent");
        let arguments = [
            OsString::from("-C"),
            fixture.path().as_os_str().to_os_string(),
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            OsString::from("../detached-view"),
            OsString::from("HEAD"),
        ];

        assert!(matches!(
            plan_git_command(current, &arguments).expect("plan -C Git command"),
            GitProxyPlan::OptimizedAdd { .. }
        ));
    }

    /// `--work-tree` changes where Git looks for checked-out files, never
    /// which repository it opens: that is still found from the current
    /// directory. The planner used to key on the option's value instead, so
    /// naming any path outside the enabled repository turned its commands
    /// into ordinary Git.
    #[test]
    fn work_tree_options_do_not_select_the_planned_repository() {
        let enabled = repository_fixture();
        enable_repository(enabled.path()).expect("enable repository");
        let unenabled = repository_fixture();
        let add = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            OsString::from("../detached-view"),
        ];
        let with_work_tree = |work_tree: &Path, attached: bool| {
            let mut arguments = if attached {
                let mut option = OsString::from("--work-tree=");
                option.push(work_tree);
                vec![option]
            } else {
                vec![OsString::from("--work-tree"), work_tree.into()]
            };
            arguments.extend(add.iter().cloned());
            arguments
        };

        for attached in [true, false] {
            for work_tree in [
                unenabled.path().to_path_buf(),
                unenabled.path().join("tracked.txt"),
                unenabled.path().join("missing"),
            ] {
                let error = plan_git_command(enabled.path(), &with_work_tree(&work_tree, attached))
                    .expect_err("an enabled repository must keep planning its own adds");
                assert!(
                    error.to_string().contains("invocation-level configuration"),
                    "{}: {error}",
                    work_tree.display()
                );
            }
            assert!(
                matches!(
                    plan_git_command(unenabled.path(), &with_work_tree(enabled.path(), attached))
                        .expect("plan in the repository that never opted in"),
                    GitProxyPlan::Passthrough
                ),
                "a work tree elsewhere must not opt the current repository in"
            );
        }
    }

    /// The lifecycle half of the same rule: with the planner keyed on the
    /// `--work-tree` value, `git --work-tree=<anything else> worktree remove
    /// --force <managed>` reached real Git, which deleted the view outside
    /// the removal journal.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn work_tree_options_cannot_bypass_the_managed_lifecycle_guard() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        git(
            fixture.path(),
            &["config", "alias.drop", "worktree remove --force"],
        );
        let destination = fixture.path().join("guarded-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/guarded")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let other = repository_fixture();

        for work_tree in [
            other.path().to_path_buf(),
            other.path().join("tracked.txt"),
            other.path().join("missing"),
        ] {
            let mut option = OsString::from("--work-tree=");
            option.push(&work_tree);
            for (command, refusal) in [
                (
                    &["worktree", "remove", "--force", "guarded-view"][..],
                    "managed Riftri worktree",
                ),
                (
                    &["worktree", "move", "guarded-view", "moved-view"][..],
                    "managed Riftri worktree",
                ),
                (&["drop", "guarded-view"][..], "managed Riftri worktree"),
                (&["worktree", "prune"][..], "managed Riftri state"),
            ] {
                for git_directory in [None, Some("--git-dir=.git")] {
                    let arguments = git_directory
                        .map(OsString::from)
                        .into_iter()
                        .chain([option.clone()])
                        .chain(command.iter().map(OsString::from))
                        .collect::<Vec<_>>();
                    let error = plan_git_command(fixture.path(), &arguments)
                        .expect_err("a work tree elsewhere must not bypass the guard");
                    assert!(
                        error.to_string().contains(refusal),
                        "{arguments:?}: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn enabled_add_refuses_invocation_config_instead_of_ignoring_it() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let arguments = [
            OsString::from("-c"),
            OsString::from("core.autocrlf=true"),
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            OsString::from("../configured-view"),
            OsString::from("HEAD"),
        ];

        let error = plan_git_command(fixture.path(), &arguments)
            .expect_err("invocation config must not be ignored");
        assert!(error.to_string().contains("invocation-level configuration"));
    }

    /// `worktree add` is the only subcommand Riftri intercepts eagerly, so its
    /// usage text has to stay reachable exactly like `remove -h` and `list -h`.
    #[test]
    fn enabled_add_delegates_help_requests_to_git() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        for help in ["--help", "-h"] {
            let arguments = [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from(help),
            ];

            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan help request"),
                    GitProxyPlan::Passthrough
                ),
                "`worktree add {help}` was not delegated to Git"
            );
        }
    }

    /// A `--` separator ends option parsing, so a later `--help` is a path.
    #[test]
    fn enabled_add_treats_help_after_a_separator_as_a_path() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let arguments = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            OsString::from("--"),
            OsString::from("--help"),
        ];

        let GitProxyPlan::OptimizedAdd { request, .. } =
            plan_git_command(fixture.path(), &arguments).expect("plan separated add")
        else {
            panic!("`worktree add --detach -- --help` was not an optimized add");
        };
        assert_eq!(request.destination, fixture.path().join("--help"));
    }

    /// The proxy compares the subcommand token to `worktree` literally, so a
    /// config alias used to reach real Git unclassified and perform exactly the
    /// lifecycle change the spelled-out command refuses — Git expands aliases
    /// internally, so nothing re-enters the shim. Planning the command Git will
    /// actually run keeps the alias working *and* journaled.
    // Creating a managed worktree needs a native copy-on-write backend, so
    // this runs where `worktree.rs`'s own tests run.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn worktree_aliases_are_planned_as_the_command_git_will_run() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("aliased-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/aliased")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");

        git(
            fixture.path(),
            &["config", "alias.wtp", "worktree prune -v"],
        );
        assert!(
            matches!(
                plan_git_command(fixture.path(), &[OsString::from("wtp")]).expect("plan alias"),
                GitProxyPlan::OptimizedPrune(_)
            ),
            "an alias for `worktree prune` was not journaled"
        );

        // A quoted alias body splits the way Git splits it, and arguments after
        // the alias name still apply.
        git(
            fixture.path(),
            &["config", "alias.wtrm", "worktree 'remove'"],
        );
        assert!(
            matches!(
                plan_git_command(
                    fixture.path(),
                    &[OsString::from("wtrm"), destination.clone().into_os_string()],
                )
                .expect("plan alias"),
                GitProxyPlan::OptimizedRemove(_)
            ),
            "an alias for `worktree remove` was not journaled"
        );
        assert!(
            matches!(
                plan_git_command(
                    fixture.path(),
                    &[
                        OsString::from("wtrm"),
                        OsString::from("--force"),
                        destination.into_os_string(),
                    ],
                )
                .expect("plan alias"),
                GitProxyPlan::OptimizedForceRemove(_)
            ),
            "arguments after the alias name were not applied"
        );
    }

    /// Git looks for an alias only when the name is neither a builtin nor a
    /// `git-<name>` program, so an alias with such a name never runs. The
    /// planner used to expand it anyway: with `alias.gc = worktree remove
    /// --force`, `git gc <managed>` was planned as a forced removal of a
    /// worktree the user never asked to remove, while real Git runs `gc`.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn aliases_cannot_shadow_the_commands_git_runs_first() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("shadowed-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/shadowed")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let destination = destination.to_str().expect("UTF-8 fixture path");

        let force_remove = "worktree remove --force";
        git(fixture.path(), &["config", "alias.gc", force_remove]);
        git(fixture.path(), &["config", "alias.prune", "worktree prune"]);
        // `git-submodule` is a program in Git's exec-path, not a builtin.
        git(fixture.path(), &["config", "alias.submodule", force_remove]);
        // A chain stops at the first name Git runs itself.
        git(fixture.path(), &["config", "alias.viagc", "gc"]);
        for arguments in [
            vec!["gc", destination],
            vec!["prune"],
            vec!["submodule", destination],
            vec!["viagc", destination],
            vec![
                "-c",
                "alias.fsck=worktree remove --force",
                "fsck",
                destination,
            ],
        ] {
            let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan shadowed alias"),
                    GitProxyPlan::Passthrough
                ),
                "{arguments:?} was planned as the alias Git never runs"
            );
        }

        // Git lets an alias replace a deprecated builtin, so these are
        // aliases too: probed on git 2.53.0, `git -c alias.whatchanged='worktree
        // list' whatchanged` lists worktrees.
        let deprecated = Command::new("git")
            .arg("--list-cmds=deprecated")
            .output()
            .expect("list deprecated commands");
        let deprecated = String::from_utf8(deprecated.stdout).expect("UTF-8 command names");
        for name in deprecated.lines() {
            git(
                fixture.path(),
                &["config", &format!("alias.{name}"), force_remove],
            );
            assert!(
                matches!(
                    plan_git_command(
                        fixture.path(),
                        &[OsString::from(name), OsString::from(destination)],
                    )
                    .expect("plan alias of a deprecated builtin"),
                    GitProxyPlan::OptimizedForceRemove(_)
                ),
                "`git {name}` runs its alias, which was not planned"
            );
        }

        // A name Git does not run itself is still an alias.
        git(fixture.path(), &["config", "alias.wtrm", force_remove]);
        assert!(matches!(
            plan_git_command(
                fixture.path(),
                &[OsString::from("wtrm"), OsString::from(destination)],
            )
            .expect("plan alias"),
            GitProxyPlan::OptimizedForceRemove(_)
        ));
    }

    /// Every builtin in Git 2.36, the oldest supported release, as listed by
    /// the `commands[]` table in its `git.c`. Entries in
    /// [`super::COMMON_GIT_BUILTINS`] must come from here, because the shim
    /// skips their alias lookup on any Git a scope accepts.
    const GIT_2_36_BUILTINS: &[&str] = &[
        "add",
        "am",
        "annotate",
        "apply",
        "archive",
        "bisect--helper",
        "blame",
        "branch",
        "bugreport",
        "bundle",
        "cat-file",
        "check-attr",
        "check-ignore",
        "check-mailmap",
        "check-ref-format",
        "checkout",
        "checkout--worker",
        "checkout-index",
        "cherry",
        "cherry-pick",
        "clean",
        "clone",
        "column",
        "commit",
        "commit-graph",
        "commit-tree",
        "config",
        "count-objects",
        "credential",
        "credential-cache",
        "credential-cache--daemon",
        "credential-store",
        "describe",
        "diff",
        "diff-files",
        "diff-index",
        "diff-tree",
        "difftool",
        "env--helper",
        "fast-export",
        "fast-import",
        "fetch",
        "fetch-pack",
        "fmt-merge-msg",
        "for-each-ref",
        "for-each-repo",
        "format-patch",
        "fsck",
        "fsck-objects",
        "fsmonitor--daemon",
        "gc",
        "get-tar-commit-id",
        "grep",
        "hash-object",
        "help",
        "hook",
        "index-pack",
        "init",
        "init-db",
        "interpret-trailers",
        "log",
        "ls-files",
        "ls-remote",
        "ls-tree",
        "mailinfo",
        "mailsplit",
        "maintenance",
        "merge",
        "merge-base",
        "merge-file",
        "merge-index",
        "merge-ours",
        "merge-recursive",
        "merge-recursive-ours",
        "merge-recursive-theirs",
        "merge-subtree",
        "merge-tree",
        "mktag",
        "mktree",
        "multi-pack-index",
        "mv",
        "name-rev",
        "notes",
        "pack-objects",
        "pack-redundant",
        "pack-refs",
        "patch-id",
        "pickaxe",
        "prune",
        "prune-packed",
        "pull",
        "push",
        "range-diff",
        "read-tree",
        "rebase",
        "receive-pack",
        "reflog",
        "remote",
        "remote-ext",
        "remote-fd",
        "repack",
        "replace",
        "rerere",
        "reset",
        "restore",
        "rev-list",
        "rev-parse",
        "revert",
        "rm",
        "send-pack",
        "shortlog",
        "show",
        "show-branch",
        "show-index",
        "show-ref",
        "sparse-checkout",
        "stage",
        "stash",
        "status",
        "stripspace",
        "submodule--helper",
        "switch",
        "symbolic-ref",
        "tag",
        "unpack-file",
        "unpack-objects",
        "update-index",
        "update-ref",
        "update-server-info",
        "upload-archive",
        "upload-archive--writer",
        "upload-pack",
        "var",
        "verify-commit",
        "verify-pack",
        "verify-tag",
        "version",
        "whatchanged",
        "worktree",
        "write-tree",
    ];

    /// The shim skips the alias lookup for these names, which is correct only
    /// while Git runs each of them itself. Check that against the oldest
    /// supported Git and against the installed one: each must be a builtin,
    /// and none may be deprecated, because Git lets an alias replace a
    /// deprecated builtin. A Git without the `deprecated` listing has no
    /// deprecated builtins to exclude.
    #[test]
    fn skipped_alias_lookups_name_builtins_of_every_supported_git() {
        let list = |kind: &str| {
            let output = Command::new("git")
                .arg(format!("--list-cmds={kind}"))
                .output()
                .expect("list Git commands");
            if !output.status.success() {
                return Vec::new();
            }
            String::from_utf8(output.stdout)
                .expect("UTF-8 command names")
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let installed = list("builtins");
        assert!(installed.iter().any(|builtin| builtin == "worktree"));
        let deprecated = list("deprecated");
        for name in super::COMMON_GIT_BUILTINS {
            assert!(
                GIT_2_36_BUILTINS.contains(name),
                "`{name}` is not a builtin in Git 2.36"
            );
            assert!(
                installed.iter().any(|builtin| builtin == name),
                "`{name}` is not a builtin in the installed Git"
            );
            assert!(
                !deprecated.iter().any(|builtin| builtin == name),
                "`{name}` is deprecated in the installed Git, so an alias can replace it"
            );
        }
        for legacy in [
            "annotate",
            "init-db",
            "fsck-objects",
            "stage",
            "pickaxe",
            "show-branch",
            "whatchanged",
            "pack-redundant",
            "get-tar-commit-id",
        ] {
            assert!(
                !super::COMMON_GIT_BUILTINS.contains(&legacy),
                "`{legacy}` is a legacy name Git may deprecate"
            );
        }
        assert_eq!(
            riftri_git::MINIMUM_GIT_VERSION,
            (2, 36),
            "update GIT_2_36_BUILTINS with the minimum version"
        );
    }

    /// Alias resolution must not capture anything else. A shell alias spawns a
    /// process that resolves `git` through `PATH`, so a worktree command inside
    /// one already re-enters the proxy on its own and must be left alone.
    #[test]
    fn alias_expansion_leaves_other_commands_to_git() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        git(
            fixture.path(),
            &["config", "alias.shellwt", "!git worktree prune"],
        );
        git(fixture.path(), &["config", "alias.plain", "log --oneline"]);

        for argument in ["shellwt", "plain", "status", "definitely-not-a-command"] {
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &[OsString::from(argument)])
                        .expect("plan ordinary command"),
                    GitProxyPlan::Passthrough
                ),
                "`git {argument}` was not delegated to Git"
            );
        }
    }

    /// Git reads an alias from the repository `--git-dir` selects, not from
    /// the directory the command runs in.
    #[test]
    fn alias_lookup_follows_an_explicit_git_directory() {
        let fixture = repository_fixture();
        git(
            fixture.path(),
            &["config", "alias.wtrm", "worktree remove --force"],
        );
        let outside = tempdir().expect("directory outside the repository");
        let selection = [
            OsString::from("--git-dir"),
            fixture.path().join(".git").into_os_string(),
        ];
        let arguments = [&selection[..], &["wtrm".into(), "view".into()]].concat();
        let context = super::command_contexts(outside.path(), &arguments)
            .into_iter()
            .next()
            .expect("command context");

        assert_eq!(
            super::expand_worktree_alias(&context, &arguments).expect("expand alias"),
            Some(
                [
                    &selection[..],
                    &["worktree", "remove", "--force", "view"].map(OsString::from),
                ]
                .concat()
            )
        );
    }

    /// `record_invocation_alias` mirrors what Git accepts: entries Git would
    /// refuse at lookup are skipped so passthrough stays safe, and key case
    /// never matters.
    #[test]
    fn invocation_alias_recording_skips_what_git_refuses() {
        let mut aliases = Vec::new();
        for rejected in [
            "alias.noequals",  // Git: missing value
            "alias.=worktree", // empty name
            "alias.empty=",    // Git: empty alias refused at use
            "core.editor=vim", // not an alias
            "alias=worktree",  // no name segment at all
        ] {
            super::record_invocation_alias(&mut aliases, OsStr::new(rejected));
        }
        assert!(aliases.is_empty(), "{aliases:?}");

        super::record_invocation_alias(&mut aliases, OsStr::new("ALIAS.WtRm=worktree remove"));
        super::record_invocation_alias(&mut aliases, OsStr::new("alias.keep=log --oneline"));
        assert_eq!(
            aliases,
            [
                ("wtrm".to_owned(), b"worktree remove".to_vec()),
                ("keep".to_owned(), b"log --oneline".to_vec()),
            ]
        );
    }

    /// A one-shot alias supplied with `-c alias.<name>=…` (or `--config-env=`)
    /// outranks every stored configuration scope, so the planner must read it
    /// from the invocation itself. Before it did,
    /// `git -c 'alias.wtrm=worktree remove --force' wtrm <managed>` reached
    /// real Git unclassified and removed the view outside the journal.
    // Creating a managed worktree needs a native copy-on-write backend, so
    // this runs where `worktree.rs`'s own tests run.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn invocation_aliases_cannot_bypass_the_lifecycle_guard() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("one-shot-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/one-shot")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let destination = destination.into_os_string();

        let refused = |arguments: &[OsString]| {
            let error = plan_git_command(fixture.path(), arguments)
                .expect_err("a one-shot worktree alias must not bypass the guard");
            assert!(
                error.to_string().contains("managed Riftri worktree"),
                "{arguments:?}: {error}"
            );
        };

        refused(&[
            OsString::from("-c"),
            OsString::from("alias.wtrm=worktree remove --force"),
            OsString::from("wtrm"),
            destination.clone(),
        ]);
        // Configuration keys and the typed token are ASCII-case-insensitive.
        refused(&[
            OsString::from("-c"),
            OsString::from("ALIAS.WTRM=worktree remove --force"),
            OsString::from("WtRm"),
            destination.clone(),
        ]);
        // The last occurrence wins, exactly as Git resolves configuration.
        refused(&[
            OsString::from("-c"),
            OsString::from("alias.z=log --oneline"),
            OsString::from("-c"),
            OsString::from("alias.z=worktree remove --force"),
            OsString::from("z"),
            destination.clone(),
        ]);
        // `--config-env` takes the body from the named environment variable.
        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe {
            std::env::set_var("RIFTRI_TEST_INVOCATION_ALIAS", "worktree remove --force");
        }
        refused(&[
            OsString::from("--config-env=alias.ce=RIFTRI_TEST_INVOCATION_ALIAS"),
            OsString::from("ce"),
            destination.clone(),
        ]);

        // Shell aliases and non-worktree aliases still delegate to Git.
        for arguments in [
            vec![
                OsString::from("-c"),
                OsString::from("alias.sh=!git worktree prune"),
                OsString::from("sh"),
            ],
            vec![
                OsString::from("-c"),
                OsString::from("alias.st2=status --porcelain"),
                OsString::from("st2"),
            ],
        ] {
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan ordinary command"),
                    GitProxyPlan::Passthrough
                ),
                "{arguments:?} was not delegated to Git"
            );
        }
    }

    /// Delete the loose object behind `HEAD` so repository discovery still
    /// works while inspection fails — the shape a damaged object store has.
    fn corrupt_head_object(repository: &Path) {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository)
            .output()
            .expect("resolve HEAD");
        let oid = String::from_utf8(output.stdout).expect("object id");
        let oid = oid.trim();
        let object = repository
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&object)
                .expect("object metadata")
                .permissions();
            permissions.set_mode(permissions.mode() | 0o200);
            fs::set_permissions(&object, permissions).expect("make object writable");
        }
        fs::remove_file(&object).expect("remove loose object");
    }

    /// An inspection failure used to classify as "not a repository" and pass
    /// every command through, so `git worktree remove --force <managed>` ran
    /// against a corrupted repository outside the journal. An enabled but
    /// unhealthy repository must refuse the lifecycle subcommands instead —
    /// while read-only subcommands, repositories that never opted in, and
    /// plain non-repository directories all stay ordinary Git.
    #[test]
    fn unhealthy_repositories_fail_closed_for_lifecycle_commands() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        corrupt_head_object(fixture.path());

        for subcommand in ["add", "remove", "move", "prune"] {
            let arguments = [
                OsString::from(super::WORKTREE_SUBCOMMAND),
                OsString::from(subcommand),
                OsString::from("../somewhere"),
            ];
            let error = plan_git_command(fixture.path(), &arguments)
                .expect_err("an unhealthy enabled repository must refuse lifecycle commands");
            let message = error.to_string();
            assert!(
                message.contains("could not be inspected") && message.contains(BYPASS_ENV),
                "{subcommand}: {message}"
            );
        }
        assert!(
            matches!(
                plan_git_command(
                    fixture.path(),
                    &[
                        OsString::from(super::WORKTREE_SUBCOMMAND),
                        OsString::from("list")
                    ],
                )
                .expect("plan read-only subcommand"),
                GitProxyPlan::Passthrough
            ),
            "read-only worktree commands stay ordinary Git"
        );

        // A repository that never opted in stays ordinary Git even unhealthy.
        let unenabled = repository_fixture();
        corrupt_head_object(unenabled.path());
        assert!(matches!(
            plan_git_command(
                unenabled.path(),
                &[
                    OsString::from(super::WORKTREE_SUBCOMMAND),
                    OsString::from("remove"),
                    OsString::from("../view"),
                ],
            )
            .expect("plan in an unenabled unhealthy repository"),
            GitProxyPlan::Passthrough
        ));

        // Outside any repository, everything is ordinary Git.
        let elsewhere = tempdir().expect("plain directory");
        assert!(matches!(
            plan_git_command(
                elsewhere.path(),
                &[
                    OsString::from(super::WORKTREE_SUBCOMMAND),
                    OsString::from("remove"),
                    OsString::from("../view"),
                ],
            )
            .expect("plan outside a repository"),
            GitProxyPlan::Passthrough
        ));
    }

    /// Where Git cannot enter `-C` or open `--git-dir` there is nothing for
    /// Riftri to guard, in an enabled repository or outside one. The planner
    /// used to return its own inspection error instead of letting Git fail.
    #[test]
    fn repositories_git_cannot_open_stay_ordinary_git() {
        let elsewhere = tempdir().expect("plain directory");
        let enabled = repository_fixture();
        enable_repository(enabled.path()).expect("enable repository");

        for directory in [elsewhere.path(), enabled.path()] {
            fs::write(directory.join("file"), "").expect("write regular file");
            for arguments in [
                &["-C", "missing", "rm", "x"][..],
                &["-C", "missing", "definitely-not-a-command"][..],
                &["-C", "file", "mv", "a", "b"][..],
                &["--git-dir=missing", "worktree"][..],
                &["--git-dir=missing", "worktree", "list"][..],
                &["--git-dir", "missing", "worktree", "add", "../view"][..],
            ] {
                let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
                assert!(
                    matches!(
                        plan_git_command(directory, &arguments),
                        Ok(GitProxyPlan::Passthrough)
                    ),
                    "{arguments:?}"
                );
            }
        }
    }

    /// Letting Git answer for a repository it cannot open must not open the
    /// lifecycle guard: an enabled repository that exists but cannot be read
    /// still refuses, whether it is named by `--git-dir`, by a linked
    /// worktree's `.git` file, or reached through an alias.
    #[test]
    fn unreadable_enabled_repositories_fail_closed_through_git_dir_and_aliases() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        git(fixture.path(), &["config", "alias.wtp", "worktree prune"]);
        let linked = tempdir().expect("linked worktree parent");
        let view = linked.path().join("view");
        git(
            fixture.path(),
            &["worktree", "add", "--quiet", view.to_str().expect("view")],
        );
        let configuration = fixture.path().join(".git/config");
        let mut contents = fs::read(&configuration).expect("read configuration");
        contents.extend_from_slice(b"[broken\n");
        fs::write(&configuration, contents).expect("corrupt configuration");
        let plan = |arguments: &[&OsStr]| {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            plan_git_command(fixture.path(), &arguments)
        };
        let git_file = view.join(".git");

        plan(&[OsStr::new("wtp")]).expect_err("an unresolvable alias must not reach Git");
        // The alias lives in the repository `--git-dir` names, so that is the
        // repository whose opt-in decides, wherever the command runs.
        let outside = tempdir().expect("directory outside the repository");
        let git_directory = fixture.path().join(".git");
        plan_git_command(
            outside.path(),
            &[
                OsString::from("--git-dir"),
                git_directory.into_os_string(),
                OsString::from("wtp"),
            ],
        )
        .expect_err("an unresolvable alias behind --git-dir must not reach Git");
        for git_directory in [OsStr::new(".git"), git_file.as_os_str()] {
            let message = plan(&[
                OsStr::new("--git-dir"),
                git_directory,
                OsStr::new("worktree"),
                OsStr::new("prune"),
            ])
            .expect_err("an unreadable enabled repository must refuse lifecycle commands")
            .to_string();
            assert!(
                message.contains("could not be inspected") && message.contains(BYPASS_ENV),
                "{git_directory:?}: {message}"
            );
            assert!(
                matches!(
                    plan(&[
                        OsStr::new("--git-dir"),
                        git_directory,
                        OsStr::new("worktree"),
                        OsStr::new("list"),
                    ]),
                    Ok(GitProxyPlan::Passthrough)
                ),
                "{git_directory:?}: read-only worktree commands stay ordinary Git"
            );
        }
    }

    /// An enablement flag Git cannot parse is an unprovable opt-out, so
    /// lifecycle commands refuse while read-only ones stay ordinary Git.
    #[test]
    fn an_unreadable_enablement_flag_fails_closed_only_for_lifecycle_commands() {
        let fixture = repository_fixture();
        git(fixture.path(), &["config", "riftri.enabled", "maybe"]);
        let plan = |subcommand: &str| {
            plan_git_command(
                fixture.path(),
                &[
                    OsString::from(super::WORKTREE_SUBCOMMAND),
                    OsString::from(subcommand),
                ],
            )
        };

        assert!(matches!(plan("list"), Ok(GitProxyPlan::Passthrough)));
        let message = plan("prune")
            .expect_err("an unreadable flag must refuse lifecycle commands")
            .to_string();
        assert!(
            message.contains("could not be inspected") && message.contains(BYPASS_ENV),
            "{message}"
        );
    }

    /// Git's alias grammar accepts backslash escapes and chains alias to
    /// alias, so `alias.h = w\orktree remove` and a two-hop chain both reach
    /// a worktree lifecycle command. The planner used to read neither and
    /// passed them to real Git unclassified.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn escaped_and_chained_aliases_cannot_bypass_the_lifecycle_guard() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("grammar-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/grammar")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let destination = destination.into_os_string();

        git(
            fixture.path(),
            &["config", "alias.esc", r"w\orktree remove --force"],
        );
        git(fixture.path(), &["config", "alias.hop1", "hop2"]);
        git(
            fixture.path(),
            &["config", "alias.hop2", "worktree remove --force"],
        );

        // With no invocation-level options these plan as the journaled
        // removal — the same outcome a spelled-out `worktree remove --force`
        // gets — instead of slipping to real Git unclassified.
        for name in ["esc", "hop1"] {
            let arguments = [OsString::from(name), destination.clone()];
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan hidden alias"),
                    GitProxyPlan::OptimizedForceRemove(_)
                ),
                "`git {name}` was not planned as the journaled removal"
            );
        }

        // A body Git itself refuses still delegates: Git fatals on it, so
        // nothing executes.
        git(fixture.path(), &["config", "alias.badq", "worktree 'oops"]);
        assert!(matches!(
            plan_git_command(fixture.path(), &[OsString::from("badq")])
                .expect("plan refused-by-git alias"),
            GitProxyPlan::Passthrough
        ),);
    }

    /// Every row was probed against git 2.50.1 (its error text echoes the
    /// parsed word verbatim), not derived from documentation.
    #[test]
    fn alias_bodies_split_the_way_git_splits_them() {
        let split = |value: &str| {
            super::split_alias_words(value.as_bytes())
                .expect(value)
                .into_iter()
                .map(|word| String::from_utf8(word).expect(value))
                .collect::<Vec<_>>()
        };
        assert_eq!(split("worktree prune -v"), ["worktree", "prune", "-v"]);
        // A run of separators is one boundary, but Git does not trim: git
        // 2.53.0 traces this body as `'' worktree prune ''`.
        assert_eq!(split("  worktree   prune  "), ["", "worktree", "prune", ""]);
        assert_eq!(
            split("worktree 'remove' \"--force\""),
            ["worktree", "remove", "--force"]
        );
        assert_eq!(
            split("worktree add 'my dir'"),
            ["worktree", "add", "my dir"]
        );
        // A backslash makes the next character literal, in the open and inside
        // double quotes; single quotes are fully literal.
        assert_eq!(split(r"w\orktree l\ist"), ["worktree", "list"]);
        assert_eq!(split(r"log A\ B"), ["log", "A B"]);
        assert_eq!(
            split(r#"log "a\tb" "a\"b" "a\\b""#),
            ["log", "atb", "a\"b", r"a\b"]
        );
        assert_eq!(split(r"log 'a\tb'"), ["log", r"a\tb"]);
        assert_eq!(split("   "), ["", ""]);
        // Git refuses these ("bad alias string"), so the caller passes them
        // through and Git fails them identically.
        assert!(super::split_alias_words(b"log 'oops").is_err());
        assert!(super::split_alias_words(br#"log "oops"#).is_err());
        assert!(super::split_alias_words(br"log ends\").is_err());
    }

    /// Git's alias splitter separates on its own `isspace` only: space, tab,
    /// line feed, and carriage return. Rows probed against git 2.53.0 with
    /// `GIT_TRACE=1 git -c alias.zz=<body> zz`, which prints the split words.
    #[test]
    fn alias_bodies_split_on_gits_whitespace_only() {
        let split = |value: &str| {
            super::split_alias_words(value.as_bytes())
                .expect(value)
                .into_iter()
                .map(|word| String::from_utf8(word).expect(value))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            split("worktree\tremove\n--force\r\nview"),
            ["worktree", "remove", "--force", "view"]
        );
        for not_a_separator in ['\u{b}', '\u{c}', '\u{85}', '\u{a0}', '\u{2003}'] {
            let word = format!("no{not_a_separator}break");
            assert_eq!(
                split(&format!("worktree remove {word}")),
                ["worktree", "remove", word.as_str()],
                "{not_a_separator:?} separated an alias word"
            );
        }
        // Git does not trim a body. A leading separator leaves an empty
        // command, which Git refuses, and a trailing one adds an empty
        // argument.
        assert_eq!(split("worktree prune "), ["worktree", "prune", ""]);
        assert_eq!(split(" worktree prune"), ["", "worktree", "prune"]);
        assert_eq!(split("worktree '' prune"), ["worktree", "", "prune"]);
        // The split is over bytes: a byte that is not UTF-8 stays in its
        // word, and no byte above ASCII is a separator.
        assert_eq!(
            super::split_alias_words(b"worktree remove a\xffb\x85c\xa0d\x0be\x0cf"),
            Ok(vec![
                b"worktree".to_vec(),
                b"remove".to_vec(),
                b"a\xffb\x85c\xa0d\x0be\x0cf".to_vec(),
            ])
        );
    }

    /// The planner has to guard the path Git will act on. Splitting a body on
    /// Unicode whitespace turned a managed path containing U+00A0 into two
    /// words, so the guard checked the wrong one and a forced removal reached
    /// real Git. Trimming a body planned a lifecycle command where Git runs
    /// none: Git reads a leading space as an empty command and refuses it.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn alias_whitespace_cannot_move_the_lifecycle_guard_off_the_managed_path() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("no\u{a0}break");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/no-break")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        // Git's alias splitting treats a backslash as an escape, so a Windows
        // path needs doubled separators to stay unquoted and intact.
        let destination = destination
            .to_str()
            .expect("UTF-8 fixture path")
            .replace('\\', "\\\\");

        let error = plan_git_command(
            fixture.path(),
            &[
                OsString::from("-c"),
                OsString::from(format!("alias.wtrm=worktree remove --force {destination}")),
                OsString::from("wtrm"),
            ],
        )
        .expect_err("a no-break space in the managed path must not bypass the guard");
        assert!(
            error.to_string().contains("managed Riftri worktree"),
            "{error}"
        );

        // Git runs `worktree prune ''` here and fails on the extra argument,
        // so this must not become a journaled prune.
        git(
            fixture.path(),
            &["config", "alias.trailing", "worktree prune "],
        );
        let error = plan_git_command(fixture.path(), &[OsString::from("trailing")])
            .expect_err("a prune with an extra empty argument is not a bare prune");
        assert!(error.to_string().contains("worktree prune"), "{error}");

        // Git refuses the empty command, so nothing is removed.
        git(
            fixture.path(),
            &["config", "alias.leading", " worktree remove --force"],
        );
        assert!(
            matches!(
                plan_git_command(
                    fixture.path(),
                    &[OsString::from("leading"), OsString::from(destination)],
                )
                .expect("plan an alias Git refuses"),
                GitProxyPlan::Passthrough
            ),
            "a body Git refuses was planned as a lifecycle command"
        );
    }

    /// Chained expansion follows Git: each inner body's arguments run before
    /// the outer body's remainder, and a cycle stops expansion (Git fatals on
    /// the loop itself, so passthrough executes nothing).
    #[test]
    fn alias_chains_expand_in_git_order_and_cycles_stop() {
        let context = super::CommandContext {
            repository: std::path::PathBuf::from("."),
            git_directory: None,
            command_index: 0,
            global_options: super::GlobalOptionScope::Significant,
            invocation_aliases: vec![
                ("inner".to_owned(), "worktree remove".into()),
                ("outer".to_owned(), "inner --force".into()),
                ("loop1".to_owned(), "loop2".into()),
                ("loop2".to_owned(), "loop1".into()),
            ],
        };
        let arguments = [OsString::from("outer"), OsString::from("view")];
        let expanded = super::expand_worktree_alias(&context, &arguments)
            .expect("expand")
            .expect("outer resolves to a worktree command");
        assert_eq!(
            expanded,
            ["worktree", "remove", "--force", "view"].map(OsString::from)
        );

        let arguments = [OsString::from("loop1")];
        assert_eq!(
            super::expand_worktree_alias(&context, &arguments).expect("expand"),
            None
        );
    }

    /// A path inside a repository's Git directory is never a worktree root,
    /// so binding to it is the caller's mistake like any other wrong path. Git
    /// answers the root query there with a plain command failure, which used
    /// to surface as an operational Git error.
    #[test]
    fn a_worktree_binding_inside_a_git_directory_is_a_binding_refusal() {
        let fixture = repository_fixture();
        for inside in [".git", ".git/objects"] {
            let error = super::resolve_worktree_binding(&fixture.path().join(inside))
                .expect_err("a Git directory is not a worktree root");
            assert!(
                matches!(error, super::ActivationError::WorktreeBinding(_)),
                "{inside}: {error:?}"
            );
        }
    }

    /// Git follows an alias chain of any length and stops only on a loop, so
    /// a long chain must still be planned as the worktree command it ends in.
    /// A fixed hop limit used to pass anything longer to real Git unclassified.
    #[test]
    fn alias_chains_are_followed_as_far_as_git_follows_them() {
        const HOPS: usize = 64;
        let mut invocation_aliases: Vec<(String, Vec<u8>)> = (1..HOPS)
            .map(|hop| (format!("hop{hop}"), format!("hop{}", hop + 1).into_bytes()))
            .collect();
        invocation_aliases.push((format!("hop{HOPS}"), "worktree remove --force".into()));
        let context = super::CommandContext {
            repository: std::path::PathBuf::from("."),
            git_directory: None,
            command_index: 0,
            global_options: super::GlobalOptionScope::Significant,
            invocation_aliases,
        };
        let arguments = [OsString::from("hop1"), OsString::from("view")];
        assert_eq!(
            super::expand_worktree_alias(&context, &arguments).expect("expand"),
            Some(
                ["worktree", "remove", "--force", "view"]
                    .map(OsString::from)
                    .to_vec()
            )
        );
    }

    /// A Unix alias body is bytes, and Git runs it whether or not they are
    /// UTF-8. The planner used to give up on such a body and pass the command
    /// through, so the expansion has to carry the exact bytes Git will see,
    /// from every place an alias can be defined.
    #[cfg(unix)]
    #[test]
    fn non_utf8_alias_bodies_expand_to_the_bytes_git_runs() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let fixture = repository_fixture();
        let body = OsStr::from_bytes(b"worktree remove --force ../vi\xffew");
        let status = Command::new("git")
            .args([OsStr::new("config"), OsStr::new("alias.stored"), body])
            .current_dir(fixture.path())
            .status()
            .expect("store alias");
        assert!(status.success());
        let variable = OsStr::from_bytes(b"RIFTRI_TEST_NON_UTF8_ALIAS_\xff");
        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe {
            std::env::set_var(variable, body);
        }
        let assignment = |key: &str, value: &OsStr| {
            let mut assignment = OsString::from(key);
            assignment.push(value);
            assignment
        };

        for arguments in [
            vec![OsString::from("stored")],
            vec![
                OsString::from("-c"),
                assignment("alias.one=", body),
                OsString::from("one"),
            ],
            vec![
                assignment("--config-env=alias.env=", variable),
                OsString::from("env"),
            ],
            vec![
                OsString::from("--config-env"),
                assignment("alias.spaced=", variable),
                OsString::from("spaced"),
            ],
        ] {
            let context = super::command_contexts(fixture.path(), &arguments)
                .into_iter()
                .next()
                .expect("command context");
            let expanded = super::expand_worktree_alias(&context, &arguments)
                .expect("expand")
                .unwrap_or_else(|| panic!("{arguments:?} was not expanded"));
            assert_eq!(
                expanded[context.command_index..],
                [
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    OsString::from("--force"),
                    OsString::from_vec(b"../vi\xffew".to_vec()),
                ],
                "{arguments:?}"
            );
        }
    }

    /// macOS cannot create a non-UTF-8 path, so the end-to-end case uses a
    /// non-UTF-8 option value: real Git accepts `--expire=now\xff` and prunes.
    #[cfg(unix)]
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(feature = "native-cow-integration", target_os = "linux")
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn non_utf8_aliases_cannot_bypass_the_lifecycle_guard() {
        use std::os::unix::ffi::OsStringExt;

        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: fixture.path().join("bytes-view"),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/bytes")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");

        let error = plan_git_command(
            fixture.path(),
            &[
                OsString::from("-c"),
                OsString::from_vec(b"alias.px=worktree prune --expire=now\xff".to_vec()),
                OsString::from("px"),
            ],
        )
        .expect_err("a non-UTF-8 worktree alias must not bypass the guard");
        assert!(
            error.to_string().contains("refusing `git worktree prune"),
            "{error}"
        );
    }

    /// Git runs its global option parser over an expanded alias body, so
    /// `alias.pw = -p worktree remove --force` reaches the same lifecycle
    /// command as the bare body. The planner used to read `-p` as the command
    /// name and passed the removal to real Git unclassified.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn alias_bodies_with_global_options_cannot_bypass_the_lifecycle_guard() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("optioned-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/optioned")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let destination = destination.into_os_string();

        // Stored aliases whose options leave the invocation unchanged plan as
        // the journaled removal, alone and through a chain.
        git(
            fixture.path(),
            &["config", "alias.pw", "-p worktree remove --force"],
        );
        git(fixture.path(), &["config", "alias.hop", "--paginate pw"]);
        git(
            fixture.path(),
            &["config", "alias.here", "-C '' worktree remove --force"],
        );
        for name in ["pw", "hop", "here"] {
            let arguments = [OsString::from(name), destination.clone()];
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan optioned alias"),
                    GitProxyPlan::OptimizedForceRemove(_)
                ),
                "`git {name}` was not planned as the journaled removal"
            );
        }

        // Options the outer command line treats as significant keep that
        // meaning inside a body, so the managed-path guard refuses.
        let refused = |arguments: &[OsString]| {
            let error = plan_git_command(fixture.path(), arguments)
                .expect_err("an alias body with global options must not bypass the guard");
            assert!(
                error.to_string().contains("managed Riftri worktree"),
                "{arguments:?}: {error}"
            );
        };
        git(
            fixture.path(),
            &[
                "config",
                "alias.ep",
                "--exec-path=/nonexistent worktree remove --force",
            ],
        );
        // A body can define the alias its own command names.
        git(
            fixture.path(),
            &[
                "config",
                "alias.nested",
                "-c 'alias.inner=-p worktree remove --force' inner",
            ],
        );
        refused(&[OsString::from("ep"), destination.clone()]);
        refused(&[OsString::from("nested"), destination.clone()]);
        refused(&[
            OsString::from("-c"),
            OsString::from("alias.one=-p worktree remove --force"),
            OsString::from("one"),
            destination.clone(),
        ]);
        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe {
            std::env::set_var(
                "RIFTRI_TEST_OPTIONED_ALIAS",
                "--paginate worktree remove --force",
            );
        }
        refused(&[
            OsString::from("--config-env=alias.ce=RIFTRI_TEST_OPTIONED_ALIAS"),
            OsString::from("ce"),
            destination.clone(),
        ]);
        // Git also accepts the spaced `--config-env <name>=<variable>` form,
        // on the command line and at the start of a body.
        refused(&[
            OsString::from("--config-env"),
            OsString::from("alias.ce=RIFTRI_TEST_OPTIONED_ALIAS"),
            OsString::from("ce"),
            destination.clone(),
        ]);
        git(
            fixture.path(),
            &[
                "config",
                "alias.envhop",
                "--config-env alias.viaenv=RIFTRI_TEST_OPTIONED_ALIAS viaenv",
            ],
        );
        refused(&[OsString::from("envhop"), destination.clone()]);

        // Git dies on a body whose options change its environment, so these
        // still delegate and nothing executes.
        for body in [
            "-P worktree remove --force",
            "-C . worktree remove --force",
            "--no-optional-locks worktree remove --force",
        ] {
            git(fixture.path(), &["config", "alias.dies", body]);
            assert!(
                matches!(
                    plan_git_command(
                        fixture.path(),
                        &[OsString::from("dies"), destination.clone()]
                    )
                    .expect("plan refused-by-git alias"),
                    GitProxyPlan::Passthrough
                ),
                "`{body}` was not delegated to Git"
            );
        }
    }

    /// Real Git is the reference for which global options an alias body may
    /// start with: it must run a body exactly when the planner follows it.
    #[test]
    fn alias_body_options_match_what_git_accepts() {
        let fixture = repository_fixture();
        for options in [
            "",
            "-p",
            "--paginate",
            "-p --paginate",
            "--exec-path=/nonexistent",
            "-C ''",
            "-c riftri.probe=1",
            "--config-env=riftri.probe=PATH",
            "--config-env riftri.probe=PATH",
            "--attr-source HEAD",
            "--shallow-file shallow",
            "-P",
            "--no-pager",
            "-C .",
            "--git-dir=.git",
            "--git-dir .git",
            "--work-tree=.",
            "--work-tree .",
            "--namespace=probe",
            "--namespace probe",
            "--bare",
            "--no-optional-locks",
            "--no-advice",
            "--literal-pathspecs",
            "--glob-pathspecs",
            "--noglob-pathspecs",
            "--icase-pathspecs",
            "--no-replace-objects",
            "--no-lazy-fetch",
            "--attr-source=HEAD",
            "--list-cmds=main",
            "--exec-path",
            "--help",
            "--version",
            "--unknown-option",
            "-p -P",
        ] {
            let body = format!("{options} rev-parse --git-dir");
            let body = body.trim_start();
            let git_runs = std::process::Command::new("git")
                .args(["-c", &format!("alias.probe={body}"), "probe"])
                .current_dir(fixture.path())
                .output()
                .expect("run git");
            let git_runs = git_runs.status.success() && git_runs.stdout == b".git\n";
            let words: Vec<OsString> = if options.is_empty() {
                Vec::new()
            } else {
                super::split_alias_words(options.as_bytes())
                    .expect("split options")
                    .into_iter()
                    .map(|word| super::alias_argument(word).expect("UTF-8 option"))
                    .collect()
            };
            assert_eq!(super::git_runs_alias_options(&words), git_runs, "`{body}`");
        }
    }

    /// A bare repository has no working-tree root, and the planner used to
    /// read that as "no repository": from the bare directory every worktree
    /// command, guarded or not, was ordinary Git. Riftri manages such a
    /// repository from its Git directory, so the shim has to plan there too.
    #[test]
    fn an_enabled_bare_repository_plans_from_its_git_directory() {
        let fixture = repository_fixture();
        let parent = tempdir().expect("bare repository parent");
        let bare = parent.path().join("bare.git");
        git(
            parent.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                fixture.path().to_str().expect("UTF-8 fixture path"),
                "bare.git",
            ],
        );
        let add = ["worktree", "add", "--detach", "../view"].map(OsString::from);

        assert!(matches!(
            plan_git_command(&bare, &add).expect("plan before opting in"),
            GitProxyPlan::Passthrough
        ));
        // `riftri enable` needs a working tree, so it records the opt-in in
        // this configuration from one of the repository's linked worktrees.
        git(&bare, &["config", "riftri.enabled", "true"]);
        assert!(matches!(
            plan_git_command(&bare, &add).expect("plan from the bare directory"),
            GitProxyPlan::OptimizedAdd { .. }
        ));

        // `--git-dir` names the same repository from outside it, with no
        // linked worktree to stand in for it.
        let selected = [
            &[OsString::from("--git-dir"), bare.into_os_string()],
            &add[..],
        ]
        .concat();
        let error = plan_git_command(parent.path(), &selected)
            .expect_err("an enabled bare repository must plan its own adds");
        assert!(
            error.to_string().contains("invocation-level configuration"),
            "{error}"
        );
    }

    /// The planner keeps two directories apart: the repository, where Riftri
    /// runs its own Git commands, and the directory the caller's Git runs
    /// in, where Git resolves relative worktree paths. Every path argument
    /// of a planned request has to come from the second.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn worktree_path_arguments_resolve_in_the_directory_git_runs_in() {
        let parent = tempdir().expect("fixture parent");
        let parent = fs::canonicalize(parent.path()).expect("canonical fixture parent");
        let repository = parent.join("repository");
        fs::create_dir(&repository).expect("create repository");
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Riftri Tests"]);
        git(
            &repository,
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("write fixture");
        git(&repository, &["add", "--", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        enable_repository(&repository).expect("enable repository");
        let view = parent.join("view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: repository.clone(),
            destination: view.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::Detached,
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        let arguments = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        let inspected = riftri_git::Git::default()
            .inspect_repository(&repository)
            .expect("inspect repository");

        // From the repository root `./view` is not a worktree at all.
        assert!(
            super::parse_enabled_remove(
                &repository,
                &inspected,
                &repository,
                &arguments(&["./view"])
            )
            .expect("parse removal from the repository root")
            .is_none()
        );
        let (removal, _) =
            super::parse_enabled_remove(&repository, &inspected, &parent, &arguments(&["./view"]))
                .expect("parse removal")
                .expect("the managed view, named from its parent directory");
        assert_eq!(removal.repository, repository);
        assert_eq!(removal.destination, parent.join("./view"));

        let relocation = super::parse_enabled_move(
            &repository,
            &inspected,
            &parent,
            &arguments(&["./view", "./moved"]),
        )
        .expect("parse move")
        .expect("the managed view, named from its parent directory");
        assert_eq!(relocation.repository, repository);
        assert_eq!(relocation.source, parent.join("./view"));
        assert_eq!(relocation.destination, parent.join("./moved"));

        let (addition, _) =
            super::parse_enabled_add(&repository, &parent, &arguments(&["--detach", "./new"]))
                .expect("parse add");
        assert_eq!(addition.repository, repository);
        assert_eq!(addition.destination, parent.join("./new"));

        let error = super::guard_managed_path_lifecycle(
            &repository,
            &inspected,
            &parent,
            &arguments(&["--force", "./view"]),
            "remove",
        )
        .expect_err("the guard must check the path Git acts on");
        assert!(
            error.to_string().contains("managed Riftri worktree"),
            "{error}"
        );
    }

    /// IDE Git integrations pass `--no-optional-locks` on every invocation.
    /// None of these options can change what a checkout contains, so an add
    /// carrying one delegates to ordinary Git instead of failing.
    #[test]
    fn enabled_add_delegates_checkout_neutral_global_options() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        for option in ["--no-optional-locks", "--no-advice", "--literal-pathspecs"] {
            let arguments = [
                OsString::from(option),
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("--detach"),
                OsString::from("../neutral-view"),
                OsString::from("HEAD"),
            ];

            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan neutral add"),
                    GitProxyPlan::Passthrough
                ),
                "`git {option} worktree add` was not delegated to Git"
            );
        }
    }

    /// A global option this parser does not know must not fail *open*. It used
    /// to: the parse bailed out and the command reached real Git with no
    /// lifecycle guard, so `git --attr-source=HEAD worktree remove <managed>`
    /// (an option the parser did not know then) removed a managed worktree
    /// that the same command without the option refuses. Every future Git
    /// global option inherits that hole, so the unknown-token policy has to
    /// match the subcommand parser's, which already fails closed.
    // Creating a managed worktree needs a native copy-on-write backend, so
    // this runs where `worktree.rs`'s own tests run.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn unrecognized_global_options_do_not_bypass_the_managed_lifecycle_guard() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("guarded-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/guarded")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");

        // Both spellings: the value may be attached or separate, and the
        // parser cannot know an unknown option's arity.
        for option in [
            vec![OsString::from("--attr-source=HEAD")],
            vec![OsString::from("--attr-source"), OsString::from("HEAD")],
            vec![OsString::from("--riftri-unknown=HEAD")],
            vec![OsString::from("--riftri-unknown"), OsString::from("HEAD")],
        ] {
            let mut arguments = option.clone();
            arguments.extend([
                OsString::from(super::WORKTREE_SUBCOMMAND),
                OsString::from("remove"),
                destination.clone().into_os_string(),
            ]);

            let error = plan_git_command(fixture.path(), &arguments)
                .expect_err("an unknown global option must not bypass the guard");
            assert!(
                error.to_string().contains("managed Riftri worktree"),
                "{option:?}: {error}"
            );
        }
    }

    /// Git reads the word after `--attr-source`, `--config-env`, or
    /// `--shallow-file` as that option's value, and more options may follow
    /// it. The planner used to take the value for the command, so
    /// `git --attr-source HEAD -c 'alias.wtrm=worktree remove --force' wtrm
    /// <managed>` reached real Git unclassified. An option the planner does
    /// not know may or may not take a value, so both readings are guarded.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            all(
                feature = "native-cow-integration",
                any(target_os = "linux", target_os = "windows")
            )
        )),
        ignore = "requires a native copy-on-write worktree backend"
    )]
    #[test]
    fn spaced_global_option_values_are_not_read_as_the_command() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        let destination = fixture.path().join("spaced-view");
        crate::add_worktree(crate::AddWorktreeRequest {
            repository: fixture.path().to_path_buf(),
            destination: destination.clone(),
            revision: OsString::from("HEAD"),
            mode: crate::WorktreeMode::NewBranch(OsString::from("feature/spaced")),
            state_dir: None,
            sparse_directories: Vec::new(),
        })
        .expect("create managed worktree");
        git(
            fixture.path(),
            &["config", "alias.stored", "worktree remove --force"],
        );
        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe {
            std::env::set_var("RIFTRI_TEST_SPACED_ALIAS", "worktree remove --force");
        }

        let one_shot = "alias.wtrm=worktree remove --force";
        for prefix in [
            vec!["--attr-source", "HEAD", "-c", one_shot, "wtrm"],
            vec!["--attr-source", "HEAD", "stored"],
            vec!["--attr-source", "worktree", "worktree", "remove", "--force"],
            vec![
                "--config-env",
                "alias.wtrm=RIFTRI_TEST_SPACED_ALIAS",
                "wtrm",
            ],
            vec![
                "--config-env",
                "core.quotePath=RIFTRI_TEST_SPACED_ALIAS",
                "-c",
                one_shot,
                "wtrm",
            ],
            vec!["--shallow-file", "shallow", "-c", one_shot, "wtrm"],
            // Not a Git option today: the next word is its value in one
            // reading and the command in the other.
            vec!["--riftri-unknown", "value", "-c", one_shot, "wtrm"],
            vec!["--riftri-unknown", "value", "stored"],
            vec!["--riftri-unknown", "stored"],
            vec![
                "--riftri-unknown",
                "worktree",
                "worktree",
                "remove",
                "--force",
            ],
            vec!["--riftri-unknown", "worktree", "remove", "--force"],
            vec!["-X", "value", "--riftri-unknown", "value", "stored"],
        ] {
            let mut arguments: Vec<OsString> = prefix.iter().map(OsString::from).collect();
            arguments.push(destination.clone().into_os_string());

            let error = plan_git_command(fixture.path(), &arguments)
                .expect_err("a spaced global option value must not bypass the guard");
            assert!(
                error.to_string().contains("managed Riftri worktree"),
                "{prefix:?}: {error}"
            );
        }

        // The guard stays narrow: a value that merely spells `worktree`, or a
        // later argument that does, is not a lifecycle command.
        for arguments in [
            vec!["--attr-source", "worktree", "log"],
            vec!["--riftri-unknown", "worktree", "list"],
            vec!["--riftri-unknown", "value", "log", "--grep", "worktree"],
        ] {
            let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan ordinary command"),
                    GitProxyPlan::Passthrough
                ),
                "{arguments:?} was not delegated to Git"
            );
        }
    }

    /// Failing closed must stay narrow: an unknown global option may only
    /// change the outcome for a managed worktree in an enabled repository.
    #[test]
    fn unrecognized_global_options_still_delegate_ordinary_commands() {
        let fixture = repository_fixture();
        let unmanaged = fixture.path().join("plain-view");
        git(
            fixture.path(),
            &["worktree", "add", "--detach", unmanaged.to_str().unwrap()],
        );
        enable_repository(fixture.path()).expect("enable repository");

        for arguments in [
            // Not a `worktree` command at all.
            vec![OsString::from("--attr-source=HEAD"), OsString::from("log")],
            // Read-only `worktree` subcommand.
            vec![
                OsString::from("--attr-source=HEAD"),
                OsString::from(super::WORKTREE_SUBCOMMAND),
                OsString::from("list"),
            ],
            // A worktree Riftri does not manage.
            vec![
                OsString::from("--attr-source=HEAD"),
                OsString::from(super::WORKTREE_SUBCOMMAND),
                OsString::from("remove"),
                unmanaged.clone().into_os_string(),
            ],
            // `worktree` appearing as an option value rather than a subcommand.
            vec![
                OsString::from("--attr-source=HEAD"),
                OsString::from("log"),
                OsString::from("--grep"),
                OsString::from(super::WORKTREE_SUBCOMMAND),
            ],
        ] {
            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan ordinary command"),
                    GitProxyPlan::Passthrough
                ),
                "{arguments:?} was not delegated to Git"
            );
        }
    }

    /// Without a mode flag, only an existing local branch is checked out.
    /// Every other revision would make ordinary Git detach or create a
    /// tracking branch, so the refusal has to name that limitation before any
    /// Git process runs rather than surface as a missing-branch failure later.
    #[test]
    fn enabled_add_refuses_a_revision_that_is_not_a_local_branch() {
        let fixture = repository_fixture();
        git(fixture.path(), &["tag", "v1.0.0"]);
        enable_repository(fixture.path()).expect("enable repository");
        for revision in ["v1.0.0", "origin/main", "HEAD"] {
            let arguments = [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("../detached-candidate"),
                OsString::from(revision),
            ];

            let error = plan_git_command(fixture.path(), &arguments)
                .expect_err("a non-branch revision must be refused before any mutation");
            let message = error.to_string();
            assert!(
                message.contains("is not one") && message.contains(revision),
                "{revision}: {message}"
            );
            assert!(message.contains("--detach"), "{revision}: {message}");
            assert!(message.contains(BYPASS_ENV), "{revision}: {message}");
            assert!(
                !message.contains("existing local branch does not exist"),
                "{revision}: {message}"
            );
        }
    }

    /// `-n`/`--dry-run` is Git's own "show what would be pruned", and the
    /// verbosity and help options report just as harmlessly, so none of them
    /// may be refused as journal-bypassing mutations.
    #[test]
    fn enabled_prune_delegates_read_only_options_to_git() {
        let fixture = repository_fixture();
        enable_repository(fixture.path()).expect("enable repository");
        for options in [
            &["-n"][..],
            &["--dry-run"][..],
            &["-h"][..],
            &["--help"][..],
            &["--dry-run", "-v"][..],
            &["--dry-run", "--expire", "1.day.ago"][..],
            &["--dry-run", "--expire=1.day.ago"][..],
            &["--expire", "1.day.ago", "-h"][..],
        ] {
            let mut arguments = vec![OsString::from("worktree"), OsString::from("prune")];
            arguments.extend(options.iter().map(OsString::from));

            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan read-only prune"),
                    GitProxyPlan::Passthrough
                ),
                "`worktree prune {options:?}` was not delegated to Git"
            );
        }
    }

    /// Deactivation evaluated inside a `riftri exec` session must strip the
    /// process-scoped shim from `PATH` and drop its scope variable, not only
    /// the durable shell-hook shim.
    #[cfg(unix)]
    #[test]
    fn posix_deactivation_removes_process_scoped_shim_entries() {
        let script = prepare_posix_shell_deactivation_inner().expect("render deactivation code");
        assert!(script.contains(PROCESS_SHIM_DIR_ENV));
        // The sweep keys on each PATH entry's final component, not a raw
        // substring, so it no longer strips a directory that merely embeds the
        // prefix in a parent segment.
        assert!(script.contains(&format!("{PROCESS_SHIM_DIR_PREFIX}*")));
        assert!(script.contains("${_riftri_entry##*/}"));
        assert!(!script.contains(&format!("*{PROCESS_SHIM_DIR_PREFIX}*")));
        // Both shim scopes are torn down: the durable shell hook's variable and
        // the process-scoped one are unset in the same statement.
        let unset = script
            .lines()
            .find(|line| line.starts_with("unset RIFTRI_REAL_GIT "))
            .expect("deactivation unsets the shim variables");
        for variable in [
            "RIFTRI_SHIM_ACTIVE",
            SHELL_SHIM_DIR_ENV,
            PROCESS_SHIM_DIR_ENV,
        ] {
            assert!(unset.contains(variable), "{variable} is not unset: {unset}");
        }

        let shim = "/tmp/riftri-test/riftri-git-shim-abc123";
        let output = Command::new("sh")
            .args([
                "-c",
                "eval \"$RIFTRI_TEST_DEACTIVATION\"\n\
                 printf 'path=%s\\n' \"$PATH\"\n\
                 printf 'scope=%s\\n' \"${RIFTRI_PROCESS_SHIM_DIR-unset}\"\n\
                 printf 'marker=%s\\n' \"${RIFTRI_SHIM_ACTIVE-unset}\"\n\
                 printf 'real=%s\\n' \"${RIFTRI_REAL_GIT-unset}\"",
            ])
            .env("RIFTRI_TEST_DEACTIVATION", &script)
            .env("PATH", format!("{shim}:/usr/bin:/bin"))
            .env(PROCESS_SHIM_DIR_ENV, shim)
            .env(super::SHIM_ACTIVE_ENV, "1")
            .env(riftri_git::REAL_GIT_ENV, "/usr/bin/git")
            .output()
            .expect("evaluate deactivation in sh");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 shell output");
        assert!(stdout.contains("path=/usr/bin:/bin\n"), "{stdout}");
        assert!(stdout.contains("scope=unset"), "{stdout}");
        assert!(stdout.contains("marker=unset"), "{stdout}");
        assert!(stdout.contains("real=unset"), "{stdout}");
    }

    /// The process-shim sweep keys on each PATH entry's final component, so a
    /// user directory that merely embeds the shim prefix in a parent segment
    /// survives, while a stacked shim from a nested `riftri exec` — on PATH as
    /// its own `riftri-git-shim-*` directory but not the recorded scope
    /// variable — is still removed.
    #[cfg(unix)]
    #[test]
    fn posix_deactivation_preserves_unrelated_prefix_paths_and_strips_nested_shims() {
        let script = prepare_posix_shell_deactivation_inner().expect("render deactivation code");
        let unrelated = "/opt/riftri-git-shim-tools/bin";
        let outer_shim = "/tmp/outer/riftri-git-shim-outer1";
        let inner_shim = "/tmp/inner/riftri-git-shim-inner2";
        let output = Command::new("sh")
            .args([
                "-c",
                "eval \"$RIFTRI_TEST_DEACTIVATION\"\nprintf 'path=%s\\n' \"$PATH\"",
            ])
            .env("RIFTRI_TEST_DEACTIVATION", &script)
            .env(
                "PATH",
                format!("{inner_shim}:{outer_shim}:{unrelated}:/usr/bin:/bin"),
            )
            // Only the innermost shim is recorded as the process scope; the
            // outer stacked shim must still be stripped by the leaf-name match.
            .env(PROCESS_SHIM_DIR_ENV, inner_shim)
            .env(super::SHIM_ACTIVE_ENV, "1")
            .env(riftri_git::REAL_GIT_ENV, "/usr/bin/git")
            .output()
            .expect("evaluate deactivation in sh");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 shell output");
        assert!(
            stdout.contains(&format!("path={unrelated}:/usr/bin:/bin\n")),
            "unrelated prefix path must survive while both shims are stripped: {stdout}"
        );
    }

    /// A shim with a stripped environment must resolve the real Git without
    /// The `PATH` walk that bakes `RIFTRI_REAL_GIT` for `riftri exec` and the
    /// shell hooks must never select a shim. A shell whose durable shim is
    /// still on `PATH` but whose marker variable has been cleared would
    /// otherwise record Riftri as the real Git, and every later Git command
    /// would re-enter the shim forever.
    #[cfg(unix)]
    #[test]
    fn real_git_lookup_skips_shim_directories_and_never_selects_itself() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempdir().expect("fixture");
        let shim_directory = fixture.path().join("shim");
        let real_directory = fixture.path().join("real");
        fs::create_dir_all(&shim_directory).expect("create shim directory");
        fs::create_dir_all(&real_directory).expect("create real directory");
        for executable in [shim_directory.join("git"), real_directory.join("git")] {
            fs::write(&executable, "#!/bin/sh\n").expect("write executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("mark executable");
        }
        let real_git = real_directory.join("git");
        super::record_real_git_marker(&shim_directory, &real_git).expect("record marker");
        let path = std::env::join_paths([&shim_directory, &real_directory]).expect("join PATH");

        // The shim directory comes first, but its baked marker names the real
        // Git rather than its own entry.
        assert_eq!(
            super::real_git_on_path(&path, None),
            Some(real_git.clone()),
            "the shim's own `git` entry must never be selected"
        );

        // A markerless directory whose `git` is this very executable is skipped
        // in favour of the next entry.
        let self_directory = fixture.path().join("self");
        fs::create_dir_all(&self_directory).expect("create self directory");
        let running = fs::canonicalize(std::env::current_exe().expect("current executable"))
            .expect("canonical executable");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&running, self_directory.join("git")).expect("link self");
        let path = std::env::join_paths([&self_directory, &real_directory]).expect("join PATH");
        let selected = super::real_git_on_path(&path, Some(running)).expect("resolve real Git");
        assert_eq!(
            fs::canonicalize(&selected).ok(),
            fs::canonicalize(&real_git).ok(),
            "Riftri must never record itself as the real Git"
        );
    }

    /// ever selecting itself or another shim's `git` entry.
    #[cfg(unix)]
    #[test]
    fn stripped_shim_resolution_skips_shim_directories_and_uses_the_baked_marker() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempdir().expect("fixture");
        let shim_directory = fixture.path().join("riftri-git-shim-test");
        let real_directory = fixture.path().join("real");
        fs::create_dir_all(&shim_directory).expect("create shim directory");
        fs::create_dir_all(&real_directory).expect("create real directory");
        for executable in [shim_directory.join("git"), real_directory.join("git")] {
            fs::write(&executable, "#!/bin/sh\n").expect("write executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("mark executable");
        }
        let real_git = real_directory.join("git");
        record_real_git_marker(&shim_directory, &real_git).expect("record marker");
        assert!(shim_directory.join(REAL_GIT_MARKER_FILE).is_file());
        let path = std::env::join_paths([&shim_directory, &real_directory]).expect("join PATH");

        // The scope variable alone is enough to find the baked real Git.
        assert_eq!(
            resolve_stripped_real_git(None, Some(shim_directory.clone()), Some(path.clone()), None,),
            Some(real_git.clone())
        );

        // Without any scope variable, the PATH walk reads the marker of the
        // shim directory it skips instead of executing that shim.
        assert_eq!(
            resolve_stripped_real_git(None, None, Some(path.clone()), None),
            Some(real_git.clone())
        );

        // A still-present recorded environment value wins.
        assert_eq!(
            resolve_stripped_real_git(
                Some(real_git.clone().into_os_string()),
                None,
                Some(path.clone()),
                None,
            ),
            Some(real_git.clone())
        );

        // A markerless shim entry that resolves to the running executable is
        // skipped in favor of the next PATH entry.
        fs::remove_file(shim_directory.join(REAL_GIT_MARKER_FILE)).expect("remove marker");
        let canonical_shim_git =
            fs::canonicalize(shim_directory.join("git")).expect("canonical shim git");
        assert_eq!(
            resolve_stripped_real_git(None, None, Some(path.clone()), Some(canonical_shim_git)),
            Some(real_git.clone())
        );

        // With no real Git anywhere, resolution reports failure instead of
        // selecting the shim itself.
        let canonical_shim_git =
            fs::canonicalize(shim_directory.join("git")).expect("canonical shim git");
        let shim_only = std::env::join_paths([&shim_directory]).expect("join shim-only PATH");
        assert_eq!(
            resolve_stripped_real_git(None, None, Some(shim_only), Some(canonical_shim_git)),
            None
        );
    }

    pub(super) fn repository_fixture() -> tempfile::TempDir {
        let fixture = tempdir().expect("temporary directory");
        git(fixture.path(), &["init", "--quiet"]);
        git(fixture.path(), &["config", "user.name", "Riftri Tests"]);
        git(
            fixture.path(),
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(fixture.path(), &["config", "core.autocrlf", "false"]);
        fs::write(fixture.path().join("tracked.txt"), "tracked\n").expect("write fixture");
        git(fixture.path(), &["add", "--", "tracked.txt"]);
        git(fixture.path(), &["commit", "--quiet", "-m", "initial"]);
        fixture
    }

    pub(super) fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .expect("start Git fixture command");
        assert!(status.success(), "git {arguments:?} failed");
    }
}
