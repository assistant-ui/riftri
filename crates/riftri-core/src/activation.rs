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
use riftri_git::{Git, GitError};
use serde::Serialize;
use thiserror::Error;

use crate::worktree::{managed_worktree_state_directory, repository_state_directories};
use crate::{
    AddWorktreeRequest, AddWorktreeResult, MoveWorktreeRequest, MoveWorktreeResult,
    PruneWorktreesRequest, PruneWorktreesResult, RemoveWorktreeRequest, RemoveWorktreeResult,
    WorktreeError, WorktreeMode, add_worktree, force_remove_worktree, move_worktree,
    prune_worktrees, remove_worktree, storage_accounting,
};

pub const ENABLED_CONFIG_KEY: &str = "riftri.enabled";
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
}

pub fn enable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    let status = repository_identity_with_git(&git, path)?;
    git.set_local_config(&status.repository, ENABLED_CONFIG_KEY, OsStr::new("true"))?;
    Ok(RepositoryActivation {
        enabled: true,
        ..status
    })
}

pub fn disable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    let status = repository_identity_with_git(&git, path)?;
    git.unset_local_config(&status.repository, ENABLED_CONFIG_KEY)?;
    Ok(RepositoryActivation {
        enabled: false,
        ..status
    })
}

pub fn repository_activation(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    activation_with_git(&Git::default(), path)
}

pub fn plan_git_command(
    current_directory: &Path,
    arguments: &[OsString],
) -> Result<GitProxyPlan, ActivationError> {
    if environment_truthy(BYPASS_ENV) {
        return Ok(GitProxyPlan::Passthrough);
    }

    let Some(context) = command_context(current_directory, arguments) else {
        return Ok(GitProxyPlan::Passthrough);
    };
    if arguments
        .get(context.command_index)
        .map(OsString::as_os_str)
        != Some(OsStr::new("worktree"))
    {
        return Ok(GitProxyPlan::Passthrough);
    }

    let repository = match context.git_directory.as_deref() {
        Some(git_directory) => Git::default()
            .worktree_root_from_git_dir(git_directory)?
            .unwrap_or(context.repository),
        None => context.repository,
    };
    let Some(activation) = activation_for_proxy(&repository)? else {
        return Ok(GitProxyPlan::Passthrough);
    };
    if !activation.enabled {
        return Ok(GitProxyPlan::Passthrough);
    }

    let Some(subcommand) = arguments.get(context.command_index + 1) else {
        return Ok(GitProxyPlan::Passthrough);
    };
    match subcommand.to_string_lossy().as_ref() {
        "add" => plan_enabled_add(
            &repository,
            &arguments[context.command_index + 2..],
            context.global_options,
        ),
        "remove" => plan_enabled_remove(
            &repository,
            &arguments[context.command_index + 2..],
            context.global_options.optimization_compatible(),
        ),
        "move" => plan_enabled_move(
            &repository,
            &arguments[context.command_index + 2..],
            context.global_options.optimization_compatible(),
        ),
        "prune" => plan_enabled_prune(
            &repository,
            &activation,
            &arguments[context.command_index + 2..],
            context.global_options,
        ),
        _ => Ok(GitProxyPlan::Passthrough),
    }
}

pub fn proxy_git_command(
    current_directory: &Path,
    arguments: &[OsString],
) -> Result<GitProxyOutcome, ActivationError> {
    match plan_git_command(current_directory, arguments)? {
        GitProxyPlan::Passthrough => Ok(GitProxyOutcome::Passthrough(exit_status_code(
            Git::default().passthrough(arguments)?,
        ))),
        GitProxyPlan::OptimizedAdd { request, quiet } => Ok(GitProxyOutcome::OptimizedAdd {
            result: add_worktree(request)?,
            quiet,
        }),
        GitProxyPlan::OptimizedRemove(request) => {
            Ok(GitProxyOutcome::OptimizedRemove(remove_worktree(request)?))
        }
        GitProxyPlan::OptimizedForceRemove(request) => Ok(GitProxyOutcome::OptimizedRemove(
            force_remove_worktree(request)?,
        )),
        GitProxyPlan::OptimizedMove(request) => {
            Ok(GitProxyOutcome::OptimizedMove(move_worktree(request)?))
        }
        GitProxyPlan::OptimizedPrune(request) => {
            Ok(GitProxyOutcome::OptimizedPrune(prune_worktrees(request)?))
        }
    }
}

pub fn execute_scoped_command(command: &[OsString]) -> Result<i32, ActivationError> {
    execute_scoped_command_from(command, None)
}

/// Install a root-owned, set-user-ID copy of the current Riftri executable
/// that exposes only the internal OverlayFS mount protocol when elevated.
#[cfg(target_os = "linux")]
pub fn install_overlayfs_helper(replace: bool) -> Result<PathBuf, ActivationError> {
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
        TerminationError::Disposition { .. } => process_error(error.to_string()),
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
    let repository = git.inspect_repository(&canonical).map_err(|error| {
        worktree_binding_error(format!("inspect {}: {error}", canonical.display()))
    })?;
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

    let registered = git.list_worktrees(&root)?.into_iter().any(|worktree| {
        !worktree.bare && fs::canonicalize(&worktree.path).is_ok_and(|path| path == canonical)
    });
    if !registered {
        return Err(worktree_binding_error(format!(
            "{} is not a live entry in Git's worktree inventory",
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
        "_riftri_shim={shim_directory}\n_riftri_process_shim=${{{process_shim_env}-}}\n_riftri_remaining=${{PATH-}}\n_riftri_clean_path=\n_riftri_separator=\nwhile :; do\n  case \"$_riftri_remaining\" in\n    *:*) _riftri_entry=${{_riftri_remaining%%:*}}; _riftri_remaining=${{_riftri_remaining#*:}}; _riftri_more=1 ;;\n    *) _riftri_entry=$_riftri_remaining; _riftri_remaining=; _riftri_more=0 ;;\n  esac\n  _riftri_keep=1\n  [ \"$_riftri_entry\" = \"$_riftri_shim\" ] && _riftri_keep=0\n  [ -n \"$_riftri_process_shim\" ] && [ \"$_riftri_entry\" = \"$_riftri_process_shim\" ] && _riftri_keep=0\n  case \"$_riftri_entry\" in *{process_shim_prefix}*) _riftri_keep=0 ;; esac\n  if [ \"$_riftri_keep\" = 1 ]; then\n    _riftri_clean_path=${{_riftri_clean_path}}${{_riftri_separator}}${{_riftri_entry}}\n    _riftri_separator=:\n  fi\n  [ \"$_riftri_more\" = 0 ] && break\ndone\nexport PATH=$_riftri_clean_path\nunset {real_git_env} {shim_active_env} {shell_shim_dir_env} {process_shim_env}\nunset _riftri_shim _riftri_process_shim _riftri_remaining _riftri_clean_path _riftri_separator _riftri_entry _riftri_more _riftri_keep\n",
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
        "$_riftriShim = {shim_directory}\n$_riftriProcessShim = $env:{process_shim_env}\n$_riftriPath = @($env:PATH -split ';' | Where-Object {{ $_ -ne $_riftriShim -and (-not $_riftriProcessShim -or $_ -ne $_riftriProcessShim) -and $_ -notlike '*{process_shim_prefix}*' }})\n$env:PATH = $_riftriPath -join ';'\nRemove-Item Env:{real_git_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{shim_active_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{shell_shim_dir_env} -ErrorAction SilentlyContinue\nRemove-Item Env:{process_shim_env} -ErrorAction SilentlyContinue\nRemove-Variable _riftriShim, _riftriProcessShim, _riftriPath -ErrorAction SilentlyContinue\n",
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
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
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
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn activation_with_git(git: &Git, path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let mut activation = repository_identity_with_git(git, path)?;
    activation.enabled = git
        .local_config_bool(&activation.repository, ENABLED_CONFIG_KEY)?
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

fn activation_for_proxy(path: &Path) -> Result<Option<RepositoryActivation>, ActivationError> {
    let git = Git::default();
    let repository = match git.inspect_repository(path) {
        Ok(repository) => repository,
        Err(_) => return Ok(None),
    };
    let Some(root) = repository.root else {
        return Ok(None);
    };
    let enabled = git
        .local_config_bool(&root, ENABLED_CONFIG_KEY)?
        .unwrap_or(false);
    Ok(Some(RepositoryActivation {
        repository: root,
        common_git_dir: repository.identity.common_git_dir,
        enabled,
    }))
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
    repository: PathBuf,
    git_directory: Option<PathBuf>,
    command_index: usize,
    global_options: GlobalOptionScope,
}

fn command_context(current_directory: &Path, arguments: &[OsString]) -> Option<CommandContext> {
    let mut repository = current_directory.to_path_buf();
    let mut work_tree = None;
    let mut git_directory = None;
    let mut global_options = GlobalOptionScope::None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "-C" {
            let directory = arguments.get(index + 1)?;
            let directory = Path::new(directory);
            repository = if directory.is_absolute() {
                directory.to_path_buf()
            } else {
                repository.join(directory)
            };
            index += 2;
        } else if argument == "--git-dir" || argument == "--work-tree" {
            let value = arguments.get(index + 1)?;
            if argument == "--work-tree" {
                work_tree = Some(value.clone());
            } else {
                git_directory = Some(value.clone());
            }
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if let Some(value) = option_value(argument, "--git-dir=") {
            git_directory = Some(value);
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if let Some(value) = option_value(argument, "--work-tree=") {
            work_tree = Some(value);
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if argument == "--no-pager"
            || argument == "--paginate"
            || argument == "-p"
            || argument == "-P"
        {
            index += 1;
        } else if argument == "-c" {
            arguments.get(index + 1)?;
            global_options.observe(GlobalOptionScope::Significant);
            index += 2;
        } else if argument.to_string_lossy().starts_with("--config-env=") {
            global_options.observe(GlobalOptionScope::Significant);
            index += 1;
        } else if argument == "--namespace" {
            arguments.get(index + 1)?;
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
            || argument.as_encoded_bytes().starts_with(b"--exec-path=")
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
            return None;
        } else {
            let work_tree = work_tree.map(|path| resolve_command_path(&repository, &path));
            let git_directory = git_directory.map(|path| resolve_command_path(&repository, &path));
            let git_directory = work_tree.is_none().then_some(git_directory).flatten();
            return Some(CommandContext {
                repository: work_tree.unwrap_or(repository),
                git_directory,
                command_index: index,
                global_options,
            });
        }
    }
    None
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
        GlobalOptionScope::None => parse_enabled_add(repository, arguments)
            .map(|(request, quiet)| GitProxyPlan::OptimizedAdd { request, quiet }),
    }
}

fn parse_enabled_add(
    repository: &Path,
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
        } else if options && argument == "--detach" {
            set_mode(&mut mode, WorktreeMode::Detached)?;
        } else if options && argument == "--quiet" {
            quiet = true;
        } else if options && argument == "--checkout" {
            // Optimized adds always create a checked-out, clean result.
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

    let destination = PathBuf::from(&positional[0]);
    let destination = if destination.is_absolute() {
        destination
    } else {
        repository.join(destination)
    };

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

fn parse_enabled_remove(
    repository: &Path,
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
    let destination = Git::default().resolve_worktree_path(repository, path)?;
    let Some(state_directory) = managed_worktree_state_directory(repository, &destination)? else {
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
    arguments: &[OsString],
    optimization_compatible: bool,
) -> Result<GitProxyPlan, ActivationError> {
    if optimization_compatible
        && let Some((request, force)) = parse_enabled_remove(repository, arguments)?
    {
        return Ok(if force {
            GitProxyPlan::OptimizedForceRemove(request)
        } else {
            GitProxyPlan::OptimizedRemove(request)
        });
    }
    guard_managed_path_lifecycle(repository, arguments, "remove")?;
    Ok(GitProxyPlan::Passthrough)
}

fn parse_enabled_move(
    repository: &Path,
    arguments: &[OsString],
) -> Result<Option<MoveWorktreeRequest>, ActivationError> {
    let (source, destination) = match arguments {
        [source, destination] => (source, destination),
        [separator, source, destination] if separator == "--" => (source, destination),
        _ => return Ok(None),
    };
    let source = Git::default().resolve_worktree_path(repository, source)?;
    let Some(state_directory) = managed_worktree_state_directory(repository, &source)? else {
        return Ok(None);
    };
    Ok(Some(MoveWorktreeRequest {
        repository: repository.to_path_buf(),
        source,
        destination: resolve_command_path(repository, destination),
        state_dir: Some(state_directory),
    }))
}

fn plan_enabled_move(
    repository: &Path,
    arguments: &[OsString],
    optimization_compatible: bool,
) -> Result<GitProxyPlan, ActivationError> {
    if optimization_compatible && let Some(request) = parse_enabled_move(repository, arguments)? {
        return Ok(GitProxyPlan::OptimizedMove(request));
    }
    guard_managed_path_lifecycle(repository, arguments, "move")?;
    Ok(GitProxyPlan::Passthrough)
}

/// `git worktree prune` options that only report what a prune would do.
///
/// `-n`/`--dry-run` is Git's own "do not remove anything; just report what it
/// would remove", and the verbosity and help options change nothing either, so
/// real Git can answer all of them without touching lifecycle metadata.
fn is_read_only_prune_option(argument: &OsString) -> bool {
    matches!(
        argument.to_str(),
        Some("-n" | "--dry-run" | "-v" | "--verbose" | "-h" | "--help")
    )
}

fn plan_enabled_prune(
    repository: &Path,
    activation: &RepositoryActivation,
    arguments: &[OsString],
    global_options: GlobalOptionScope,
) -> Result<GitProxyPlan, ActivationError> {
    if !arguments.is_empty() && arguments.iter().all(is_read_only_prune_option) {
        return Ok(GitProxyPlan::Passthrough);
    }

    let mut managed_states = Vec::new();
    for state_directory in repository_state_directories(&activation.repository)? {
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
    if arguments.is_empty() {
        // A checkout-neutral global option cannot change what a prune removes,
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
    arguments: &[OsString],
    operation: &str,
) -> Result<(), ActivationError> {
    let Some(path) = positional_paths(arguments).first().copied() else {
        return Ok(());
    };
    let path = Git::default().resolve_worktree_path(repository, path)?;
    if managed_worktree_state_directory(repository, &path)?.is_some() {
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
    for directory in env::split_paths(&path) {
        for candidate in git_executable_candidates(&directory) {
            if is_executable_file(&candidate) {
                return candidate.canonicalize().map_err(|error| {
                    process_error(format!(
                        "resolve Git executable {}: {error}",
                        candidate.display()
                    ))
                });
            }
        }
    }
    Err(process_error(
        "could not find the real Git executable on PATH",
    ))
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
        for option in ["-n", "--dry-run", "-v", "--verbose", "-h", "--help"] {
            let arguments = [
                OsString::from("worktree"),
                OsString::from("prune"),
                OsString::from(option),
            ];

            assert!(
                matches!(
                    plan_git_command(fixture.path(), &arguments).expect("plan read-only prune"),
                    GitProxyPlan::Passthrough
                ),
                "`worktree prune {option}` was not delegated to Git"
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
        assert!(script.contains(&format!("*{PROCESS_SHIM_DIR_PREFIX}*")));
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

    /// A shim with a stripped environment must resolve the real Git without
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

    fn repository_fixture() -> tempfile::TempDir {
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

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .expect("start Git fixture command");
        assert!(status.success(), "git {arguments:?} failed");
    }
}
