use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

pub use riftri_git::SHIM_ACTIVE_ENV;
use riftri_git::{Git, GitError};
use serde::Serialize;
use thiserror::Error;

use crate::{
    AddWorktreeRequest, AddWorktreeResult, RemoveWorktreeRequest, RemoveWorktreeResult,
    WorktreeError, WorktreeMode, add_worktree, is_managed_worktree, remove_worktree,
    storage_accounting,
};

pub const ENABLED_CONFIG_KEY: &str = "riftri.enabled";
pub const BYPASS_ENV: &str = "RIFTRI_BYPASS";
pub const CACHE_DIR_ENV: &str = "RIFTRI_CACHE_DIR";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryActivation {
    pub repository: PathBuf,
    pub common_git_dir: PathBuf,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub enum GitProxyPlan {
    Passthrough,
    OptimizedAdd(AddWorktreeRequest),
    OptimizedRemove(RemoveWorktreeRequest),
}

#[derive(Debug)]
pub enum GitProxyOutcome {
    Passthrough(i32),
    OptimizedAdd(AddWorktreeResult),
    OptimizedRemove(RemoveWorktreeResult),
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

    let Some(activation) = activation_for_proxy(&context.repository)? else {
        return Ok(GitProxyPlan::Passthrough);
    };
    if !activation.enabled {
        return Ok(GitProxyPlan::Passthrough);
    }

    let Some(subcommand) = arguments.get(context.command_index + 1) else {
        return Ok(GitProxyPlan::Passthrough);
    };
    match subcommand.to_string_lossy().as_ref() {
        "add" => {
            if !context.optimization_compatible {
                return Err(unsupported(format!(
                    "Git invocation-level configuration is not supported by the optimized add path; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
                )));
            }
            parse_enabled_add(&context.repository, &arguments[context.command_index + 2..])
                .map(GitProxyPlan::OptimizedAdd)
        }
        "remove" => plan_enabled_remove(
            &context.repository,
            &arguments[context.command_index + 2..],
            context.optimization_compatible,
        ),
        "move" => {
            guard_managed_path_lifecycle(
                &context.repository,
                &arguments[context.command_index + 2..],
                "move",
            )?;
            Ok(GitProxyPlan::Passthrough)
        }
        "prune" => {
            guard_managed_prune(&activation)?;
            Ok(GitProxyPlan::Passthrough)
        }
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
        GitProxyPlan::OptimizedAdd(request) => {
            Ok(GitProxyOutcome::OptimizedAdd(add_worktree(request)?))
        }
        GitProxyPlan::OptimizedRemove(request) => {
            Ok(GitProxyOutcome::OptimizedRemove(remove_worktree(request)?))
        }
    }
}

pub fn execute_scoped_command(command: &[OsString]) -> Result<i32, ActivationError> {
    execute_scoped_command_from(command, None)
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
        .prefix("riftri-git-shim-")
        .tempdir()
        .map_err(|error| process_error(format!("create temporary Git shim directory: {error}")))?;
    install_git_shim(shim_directory.path(), &current_executable)?;

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
        .env(SHIM_ACTIVE_ENV, "1");
    if let Some(working_directory) = working_directory {
        child.current_dir(working_directory);
    }
    let status = child.status().map_err(|error| {
        process_error(format!(
            "start process-scoped command {}: {error}",
            Path::new(program).display()
        ))
    })?;
    Ok(exit_status_code(status))
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

#[cfg(unix)]
fn prepare_posix_shell_hook_inner() -> Result<String, ActivationError> {
    let real_git = locate_real_git()?;
    let current_executable = env::current_exe()
        .map_err(|error| shell_error(format!("locate the Riftri executable: {error}")))?;
    let shim_directory = shell_shim_directory()?;
    fs::create_dir_all(&shim_directory).map_err(|error| {
        shell_error(format!(
            "create shell shim directory {}: {error}",
            shim_directory.display()
        ))
    })?;
    set_private_directory_permissions(&shim_directory)?;
    install_durable_git_shim(&shim_directory, &current_executable)?;

    let shim_directory = posix_quote_path(&shim_directory)?;
    let real_git = posix_quote_path(&real_git)?;
    Ok(format!(
        "export {real_git_env}={real_git}\nexport {shim_active_env}='1'\ncase \":${{PATH-}}:\" in\n  *:{shim_directory}:*) ;;\n  *) export PATH={shim_directory}${{PATH:+\":$PATH\"}} ;;\nesac\n",
        real_git_env = riftri_git::REAL_GIT_ENV,
        shim_active_env = SHIM_ACTIVE_ENV,
    ))
}

#[cfg(not(unix))]
fn prepare_posix_shell_hook_inner() -> Result<String, ActivationError> {
    Err(shell_error(
        "the sh/bash/zsh hook is currently available only on Unix-like systems",
    ))
}

#[cfg(unix)]
fn shell_shim_directory() -> Result<PathBuf, ActivationError> {
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
    Ok(cache_root.join("shims/v1"))
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

struct CommandContext {
    repository: PathBuf,
    command_index: usize,
    optimization_compatible: bool,
}

fn command_context(current_directory: &Path, arguments: &[OsString]) -> Option<CommandContext> {
    let mut repository = current_directory.to_path_buf();
    let mut optimization_compatible = true;
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
        } else if argument == "--no-pager"
            || argument == "--paginate"
            || argument == "-p"
            || argument == "-P"
        {
            index += 1;
        } else if argument == "-c" {
            arguments.get(index + 1)?;
            optimization_compatible = false;
            index += 2;
        } else if argument.to_string_lossy().starts_with("--config-env=") {
            optimization_compatible = false;
            index += 1;
        } else if argument.to_string_lossy().starts_with('-') {
            return None;
        } else {
            return Some(CommandContext {
                repository,
                command_index: index,
                optimization_compatible,
            });
        }
    }
    None
}

fn parse_enabled_add(
    repository: &Path,
    arguments: &[OsString],
) -> Result<AddWorktreeRequest, ActivationError> {
    let mut mode = None;
    let mut positional = Vec::new();
    let mut options = true;
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
        } else if options && (argument == "--quiet" || argument == "--checkout") {
            // The optimized implementation is already quiet and always creates
            // a checked-out, clean result before returning.
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

    let mode = mode.ok_or_else(|| {
        unsupported(format!(
            "optimized add currently requires `-b <branch>` or `--detach`; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
        ))
    })?;
    if !(1..=2).contains(&positional.len()) {
        return Err(unsupported(
            "expected `git worktree add [options] <path> [<commit-ish>]`",
        ));
    }

    let destination = PathBuf::from(&positional[0]);
    let destination = if destination.is_absolute() {
        destination
    } else {
        repository.join(destination)
    };

    Ok(AddWorktreeRequest {
        repository: repository.to_path_buf(),
        destination,
        revision: positional
            .get(1)
            .cloned()
            .unwrap_or_else(|| OsString::from("HEAD")),
        mode,
        state_dir: None,
    })
}

fn parse_enabled_remove(
    repository: &Path,
    arguments: &[OsString],
) -> Result<Option<RemoveWorktreeRequest>, ActivationError> {
    let path = match arguments {
        [path] => path,
        [separator, path] if separator == "--" => path,
        _ => return Ok(None),
    };
    let destination = PathBuf::from(path);
    let destination = if destination.is_absolute() {
        destination
    } else {
        repository.join(destination)
    };
    if !is_managed_worktree(repository, &destination)? {
        return Ok(None);
    }
    Ok(Some(RemoveWorktreeRequest {
        repository: repository.to_path_buf(),
        destination,
        state_dir: None,
    }))
}

fn plan_enabled_remove(
    repository: &Path,
    arguments: &[OsString],
    optimization_compatible: bool,
) -> Result<GitProxyPlan, ActivationError> {
    if optimization_compatible {
        if let Some(request) = parse_enabled_remove(repository, arguments)? {
            return Ok(GitProxyPlan::OptimizedRemove(request));
        }
    }
    guard_managed_path_lifecycle(repository, arguments, "remove")?;
    Ok(GitProxyPlan::Passthrough)
}

fn guard_managed_path_lifecycle(
    repository: &Path,
    arguments: &[OsString],
    operation: &str,
) -> Result<(), ActivationError> {
    let Some(path) = positional_paths(arguments).first().copied() else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        repository.join(path)
    };
    if is_managed_worktree(repository, &path)? {
        return Err(unsupported(format!(
            "refusing `git worktree {operation}` options that would bypass the journal for managed Riftri worktree {}; use a supported Riftri lifecycle command instead",
            path.display()
        )));
    }
    Ok(())
}

fn guard_managed_prune(activation: &RepositoryActivation) -> Result<(), ActivationError> {
    let state_directory = activation.common_git_dir.join("riftri");
    if !state_directory.exists() {
        return Ok(());
    }
    let status = storage_accounting(&state_directory)?;
    if status.active_views > 0 || status.pending_adds > 0 || status.pending_removals > 0 {
        return Err(unsupported(
            "refusing `git worktree prune` while managed Riftri state exists because Git could change lifecycle metadata outside the Riftri journal",
        ));
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
        GitProxyPlan, disable_repository, enable_repository, plan_git_command,
        repository_activation,
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

        let GitProxyPlan::OptimizedAdd(request) =
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
        assert!(error.to_string().contains("requires `-b <branch>`"));
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
            GitProxyPlan::OptimizedAdd(_)
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
