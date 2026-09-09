use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

pub use riftri_git::SHIM_ACTIVE_ENV;
use riftri_git::{Git, GitError};
use serde::Serialize;
use thiserror::Error;

use crate::{AddWorktreeRequest, AddWorktreeResult, WorktreeError, WorktreeMode, add_worktree};

pub const ENABLED_CONFIG_KEY: &str = "riftri.enabled";
pub const BYPASS_ENV: &str = "RIFTRI_BYPASS";

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
}

#[derive(Debug)]
pub enum GitProxyOutcome {
    Passthrough(i32),
    OptimizedAdd(AddWorktreeResult),
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
    if subcommand != "add" {
        return Ok(GitProxyPlan::Passthrough);
    }

    if !context.optimization_compatible {
        return Err(unsupported(format!(
            "Git invocation-level configuration is not supported by the optimized add path; set {BYPASS_ENV}=1 for an explicit ordinary-Git operation"
        )));
    }

    parse_enabled_add(&context.repository, &arguments[context.command_index + 2..])
        .map(GitProxyPlan::OptimizedAdd)
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
    }
}

pub fn execute_scoped_command(command: &[OsString]) -> Result<i32, ActivationError> {
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
    let status = Command::new(program)
        .args(arguments)
        .env("PATH", scoped_path)
        .env(riftri_git::REAL_GIT_ENV, real_git)
        .env(SHIM_ACTIVE_ENV, "1")
        .status()
        .map_err(|error| {
            process_error(format!(
                "start process-scoped command {}: {error}",
                Path::new(program).display()
            ))
        })?;
    Ok(exit_status_code(status))
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
