use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

mod setup;
mod ui;

// One presentation boundary; machine commands bypass it before any rendering.
macro_rules! outputln {
    () => { ui::print_line(format_args!("")) };
    ($($arg:tt)*) => { ui::print_line(format_args!($($arg)*)) };
}

// Machine output (JSON reports) skips the renderer but still exits cleanly when
// a reader closes the pipe early, matching outputln!'s broken-pipe handling.
macro_rules! machineln {
    ($($arg:tt)*) => { ui::print_machine(format_args!($($arg)*)) };
}

const CLI_EXAMPLES: &str = "\
Examples:
  riftri setup                             Create a worktree, then choose an agent
  riftri enable                            Opt the current repository in
  riftri worktree add ../feature -b f/x    Create a COW-backed worktree
  riftri worktree remove ../feature        Safely remove it again
  riftri gc --apply                        Delete unreferenced bases
  riftri doctor --json                     Inspect Git and storage support

Run `riftri <command> --help` for details on one command.";

const CLI_ENVIRONMENT: &str = "\
Environment:
  RIFTRI_BYPASS=1        Route one intercepted Git command to ordinary Git.
  RIFTRI_CACHE_DIR=PATH  Directory holding the shell-activation Git shim
                         (defaults to the platform cache directory).
  NO_COLOR=1            Disable terminal colors.
  RIFTRI_NO_ANIMATION=1 Disable animated progress.

Riftri sets RIFTRI_REAL_GIT and RIFTRI_SHIM_ACTIVE inside activated scopes.
Shell hooks also set RIFTRI_SHELL_SHIM_DIR to the absolute shim directory.
RIFTRI_REQUIRE_* variables only make the test suites fail instead of falling
back. See docs/agent-integration.md for the automation contract.";

#[derive(Debug, Parser)]
#[command(
    name = "riftri",
    version,
    about = "Lightweight Git workspaces for parallel development",
    styles = ui::help_styles(),
    after_help = CLI_EXAMPLES,
    after_long_help = format!("{CLI_EXAMPLES}\n\n{CLI_ENVIRONMENT}")
)]
struct Cli {
    /// Emit command failures as one machine-readable JSON receipt on stderr.
    #[arg(long, global = true)]
    json_errors: bool,

    /// Do not print lifecycle phase-progress lines on stderr.
    #[arg(long, global = true)]
    no_progress: bool,

    /// Use plain, line-oriented output and setup prompts (no terminal UI).
    #[arg(long, global = true)]
    plain: bool,

    /// Disable animated progress while keeping the terminal interface.
    #[arg(long, global = true)]
    no_animation: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Interactively create a COW worktree, then optionally open a coding agent.
    Setup {
        /// Existing repository to use; paths entered at prompts are relative to the caller.
        #[arg(long, default_value = ".")]
        repository: PathBuf,

        /// Proposed destination (skip the directory question).
        #[arg(long, value_name = "PATH")]
        destination: Option<PathBuf>,

        /// New branch at HEAD (skip the branch question; never resets an existing branch).
        #[arg(short = 'b', long, value_name = "BRANCH")]
        branch: Option<OsString>,
    },

    /// Enable optimized worktree creation for one repository.
    Enable {
        /// Repository to enable.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Repository to enable (alternative to the positional path).
        #[arg(long, value_name = "PATH", conflicts_with = "path")]
        repository: Option<PathBuf>,
    },

    /// Disable optimized worktree creation for one repository.
    Disable {
        /// Repository to disable.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Repository to disable (alternative to the positional path).
        #[arg(long, value_name = "PATH", conflicts_with = "path")]
        repository: Option<PathBuf>,
    },

    /// Run a command with process-scoped Git worktree interception.
    Exec {
        /// Start the command from this exact, registered Git worktree root.
        #[arg(long, value_name = "PATH")]
        worktree: Option<PathBuf>,

        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Configure the narrow Linux OverlayFS mount helper.
    Overlayfs {
        #[command(subcommand)]
        command: OverlayFsCommand,
    },

    /// Configure shell-scoped interception for normal Git commands.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },

    /// Print a shell completion script for riftri commands on stdout.
    Completions {
        /// Shell whose completion script should be emitted.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Write one troff man page per riftri command into a directory.
    Man {
        /// Existing or new directory that receives the man pages.
        directory: PathBuf,
    },

    /// Inspect Git and show the planned storage path without changing anything.
    Doctor {
        /// Repository path to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Repository to inspect (alternative to the positional path).
        #[arg(long, value_name = "PATH", conflicts_with = "path")]
        repository: Option<PathBuf>,

        /// Proposed worktree destination whose volume should be probed.
        #[arg(long)]
        destination: Option<PathBuf>,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Probe storage backends for a concrete destination volume.
    Backends {
        /// Existing path or proposed destination to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Report retained bases, active views, reference counts, and disk use.
    Status {
        /// Repository whose default Riftri state should be inspected.
        #[arg(default_value = ".")]
        repository: PathBuf,

        /// Repository to inspect (alternative to the positional repository).
        #[arg(
            long = "repository",
            value_name = "PATH",
            conflicts_with = "repository"
        )]
        repository_option: Option<PathBuf>,

        /// Explicit Riftri state directory instead of <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Safely resume or roll back interrupted journaled operations.
    Repair {
        /// Repository whose default Riftri state should be repaired.
        #[arg(default_value = ".")]
        repository: PathBuf,

        /// Repository to repair (alternative to the positional repository).
        #[arg(
            long = "repository",
            value_name = "PATH",
            conflicts_with = "repository"
        )]
        repository_option: Option<PathBuf>,

        /// Explicit Riftri state directory instead of <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Plan or apply collection of immutable bases with no journaled references.
    Gc {
        /// Repository whose default Riftri state should be collected.
        #[arg(default_value = ".")]
        repository: PathBuf,

        /// Repository to collect (alternative to the positional repository).
        #[arg(
            long = "repository",
            value_name = "PATH",
            conflicts_with = "repository"
        )]
        repository_option: Option<PathBuf>,

        /// Explicit Riftri state directory instead of <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Apply the collection plan. Without this flag, nothing is deleted.
        #[arg(long)]
        apply: bool,

        /// Skip the interactive confirmation before applying the plan.
        #[arg(long, requires = "apply")]
        yes: bool,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Manage repository-local Riftri state registrations.
    State {
        #[command(subcommand)]
        command: StateCommand,
    },

    /// Create or recover Riftri-backed real Git worktrees.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
}

impl Command {
    fn machine_output(&self) -> bool {
        match self {
            Self::Doctor { json, .. }
            | Self::Backends { json, .. }
            | Self::Status { json, .. }
            | Self::Repair { json, .. }
            | Self::Gc { json, .. } => *json,
            Self::Worktree { command } => match command {
                WorktreeCommand::List { json, .. }
                | WorktreeCommand::Add { json, .. }
                | WorktreeCommand::Remove { json, .. }
                | WorktreeCommand::Move { json, .. }
                | WorktreeCommand::Compact { json, .. }
                | WorktreeCommand::Prune { json, .. } => *json,
            },
            Self::Exec { .. } | Self::Completions { .. } => true,
            Self::Shell { command } => !matches!(command, ShellCommand::Status { .. }),
            _ => false,
        }
    }

    fn operation_name(&self) -> &'static str {
        match self {
            Self::Setup { .. } => "setup",
            Self::Enable { .. } => "enable",
            Self::Disable { .. } => "disable",
            Self::Exec { .. } => "exec",
            Self::Overlayfs { .. } => "overlayfs-helper-install",
            Self::Shell { .. } => "shell",
            Self::Completions { .. } => "completions",
            Self::Man { .. } => "man",
            Self::Doctor { .. } => "doctor",
            Self::Backends { .. } => "backends",
            Self::Status { .. } => "status",
            Self::Repair { .. } => "repair",
            Self::Gc { .. } => "garbage-collection",
            Self::State { .. } => "state",
            Self::Worktree { command } => match command {
                WorktreeCommand::List { .. } => "worktree-list",
                WorktreeCommand::Add { .. } => "worktree-add",
                WorktreeCommand::Remove { .. } => "worktree-remove",
                WorktreeCommand::Move { .. } => "worktree-move",
                WorktreeCommand::Compact { .. } => "worktree-compact",
                WorktreeCommand::Prune { .. } => "worktree-prune",
            },
        }
    }

    /// The repository and state directory this invocation selected.
    ///
    /// `riftri repair` and `riftri status` resolve their state directory from
    /// the current directory unless told otherwise, so a receipt suggesting a
    /// bare command can send the caller to unrelated state. Receipts repeat
    /// the selection made here instead.
    fn invocation_context(&self) -> InvocationContext {
        let repository_and_state =
            |repository: &PathBuf, state_dir: &Option<PathBuf>| InvocationContext {
                repository: Some(repository.clone()),
                state_directory: state_dir.clone(),
            };
        match self {
            Self::Setup { repository, .. } => InvocationContext::for_repository(repository),
            Self::Enable { path, repository } | Self::Disable { path, repository } => {
                InvocationContext::for_repository(repository.as_deref().unwrap_or(path))
            }
            Self::Doctor {
                path, repository, ..
            } => InvocationContext::for_repository(repository.as_deref().unwrap_or(path)),
            Self::Status {
                repository,
                repository_option,
                state_dir,
                ..
            }
            | Self::Repair {
                repository,
                repository_option,
                state_dir,
                ..
            }
            | Self::Gc {
                repository,
                repository_option,
                state_dir,
                ..
            } => repository_and_state(repository_option.as_ref().unwrap_or(repository), state_dir),
            Self::State { command } => match command {
                StateCommand::Unregister { repository, .. } => {
                    InvocationContext::for_repository(repository)
                }
            },
            Self::Worktree { command } => match command {
                WorktreeCommand::List {
                    repository,
                    state_dir,
                    ..
                }
                | WorktreeCommand::Add {
                    repository,
                    state_dir,
                    ..
                }
                | WorktreeCommand::Remove {
                    repository,
                    state_dir,
                    ..
                }
                | WorktreeCommand::Move {
                    repository,
                    state_dir,
                    ..
                }
                | WorktreeCommand::Compact {
                    repository,
                    state_dir,
                    ..
                }
                | WorktreeCommand::Prune {
                    repository,
                    state_dir,
                    ..
                } => repository_and_state(repository, state_dir),
            },
            // These commands address no repository and no state directory, so
            // there is nothing to carry into a receipt.
            Self::Exec { .. }
            | Self::Overlayfs { .. }
            | Self::Shell { .. }
            | Self::Completions { .. }
            | Self::Man { .. }
            | Self::Backends { .. } => InvocationContext::default(),
        }
    }
}

/// The repository and state directory a failing invocation actually used.
///
/// Paths are kept exactly as the caller wrote them and made absolute only when
/// rendered into a suggested command, so that command targets the same
/// directory wherever it is run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct InvocationContext {
    repository: Option<PathBuf>,
    state_directory: Option<PathBuf>,
}

/// What a recovery or inspection command should be pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContextTarget {
    /// An explicit state directory fully determines the target.
    StateDirectory(PathBuf),
    /// Without one, the repository still resolves the same default state
    /// directory the failing command used.
    Repository(PathBuf),
}

impl InvocationContext {
    fn for_repository(repository: &Path) -> Self {
        Self {
            repository: Some(repository.to_path_buf()),
            state_directory: None,
        }
    }

    /// The most specific selection this invocation made, made absolute.
    fn target(&self) -> Option<ContextTarget> {
        if let Some(state_directory) = &self.state_directory {
            return Some(ContextTarget::StateDirectory(riftri_core::command_path(
                state_directory,
            )));
        }
        self.repository
            .as_deref()
            .map(|repository| ContextTarget::Repository(riftri_core::command_path(repository)))
    }
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// List active Riftri-managed worktrees and their storage use.
    List {
        /// Repository whose default Riftri state should be inspected.
        #[arg(long, default_value = ".")]
        repository: PathBuf,

        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Inspect every state directory the repository registers, including
        /// the default location, and report unusable registrations.
        #[arg(long, conflicts_with = "state_dir")]
        all_states: bool,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Create a real linked worktree using the platform's native COW backend.
    Add {
        /// New worktree directory.
        path: PathBuf,

        /// Create and check out a new branch.
        #[arg(short = 'b', long, value_name = "BRANCH", conflicts_with = "detach")]
        branch: Option<OsString>,

        /// Create a detached worktree instead of a branch.
        #[arg(long, conflicts_with = "branch")]
        detach: bool,

        /// Commit-ish to use for the new worktree.
        #[arg(required_unless_present_any = ["branch", "detach"])]
        revision: Option<OsString>,

        /// Materialize only this directory (plus all repository-root files)
        /// using Git cone-mode sparse checkout. Repeatable; directories are
        /// repository-relative with `/` separators. Each distinct selection
        /// keys its own immutable base. Unsupported sparse forms are refused
        /// before any state is created.
        #[arg(long = "sparse-dir", value_name = "DIR")]
        sparse_dir: Vec<String>,

        /// Repository in which Git should create linked-worktree metadata.
        #[arg(long, default_value = ".")]
        repository: PathBuf,

        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Safely remove a Riftri-managed linked worktree.
    Remove {
        /// Existing Riftri-managed worktree directory.
        path: PathBuf,

        /// Repository owning the linked worktree.
        #[arg(long, default_value = ".")]
        repository: PathBuf,

        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Discard current changes after recording an exact recovery snapshot.
        #[arg(long, short = 'f')]
        force: bool,

        /// Skip the interactive confirmation before a forced removal.
        #[arg(long, requires = "force")]
        yes: bool,

        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Move a Riftri-managed linked worktree with recoverable metadata updates.
    Move {
        /// Existing Riftri-managed worktree directory.
        source: PathBuf,
        /// New worktree directory on the same filesystem volume.
        destination: PathBuf,
        /// Repository owning the linked worktree.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Replace a pristine managed worktree with a fresh native COW view.
    Compact {
        /// Existing clean Riftri-managed worktree directory.
        path: PathBuf,
        /// Repository owning the linked worktree.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Prune stale unmanaged Git metadata without risking managed worktrees.
    Prune {
        /// Repository owning the linked worktrees.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum StateCommand {
    /// Remove a registration for a missing state directory. No files are deleted.
    #[command(alias = "forget-missing")]
    Unregister {
        /// Missing state directory whose registration should be removed.
        path: PathBuf,

        /// Repository containing the local registration.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ShellCommand {
    /// Print initialization code to evaluate in a shell.
    Hook {
        /// Shell whose initialization code should be emitted.
        #[arg(value_enum)]
        shell: ShellKind,
    },

    /// Print code to evaluate to deactivate Riftri in the current shell.
    Deactivate {
        /// Shell whose deactivation code should be emitted.
        #[arg(value_enum)]
        shell: ShellKind,
    },

    /// Show shell interception and repository opt-in status.
    Status {
        /// Repository to inspect for local Riftri enablement.
        #[arg(default_value = ".")]
        repository: PathBuf,

        /// Repository to inspect (alternative to the positional repository).
        #[arg(
            long = "repository",
            value_name = "PATH",
            conflicts_with = "repository"
        )]
        repository_option: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum OverlayFsCommand {
    /// Install the root-owned mount helper for unprivileged shells.
    InstallHelper {
        /// Atomically replace an existing safe helper during an upgrade.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ShellKind {
    Sh,
    Bash,
    Zsh,
    Powershell,
}

fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    if invoked_as_overlayfs_helper() {
        return run_overlayfs_helper();
    }

    if invoked_as_git_shim() {
        std::process::exit(run_git_shim()?);
    }

    // Clap handles --help before normal parsing returns. Honor --plain there
    // too, and force plain rendering whenever `--json-errors` is requested so a
    // usage-error receipt never carries terminal color escapes.
    let flag_before_double_dash = |flag: &str| {
        env::args_os()
            .take_while(|arg| arg != "--")
            .any(|arg| arg == flag)
    };
    let plain_help = flag_before_double_dash("--plain");
    let json_errors_flag = flag_before_double_dash("--json-errors");
    let matches = match Cli::command()
        .color(if plain_help || json_errors_flag {
            clap::ColorChoice::Never
        } else {
            clap::ColorChoice::Auto
        })
        .try_get_matches()
    {
        Ok(matches) => matches,
        // `try_get_matches` returns the same conditions `get_matches` would have
        // exited on: genuine usage errors (exit 2) as well as the `--help` and
        // `--version` display paths (exit 0). Only the former are failures, so
        // only they are turned into a JSON receipt when `--json-errors` is set.
        // Parsing failed, so the flag is detected from the raw arguments rather
        // than the parsed struct.
        Err(error) if json_errors_flag && error.use_stderr() => {
            eprintln!("{}", serde_json::to_string(&usage_receipt(&error))?);
            // Match clap's usage-error exit code so automation can still tell a
            // usage error apart from an operational command failure (exit 1).
            std::process::exit(2);
        }
        // Help, version, and (without `--json-errors`) every usage error keep
        // clap's exact human-readable behavior and exit code.
        Err(error) => error.exit(),
    };
    let cli = Cli::from_arg_matches(&matches)?;
    let json_errors = cli.json_errors;
    let operation = cli.command.operation_name();
    let _presentation = ui::initialize(
        cli.plain || json_errors || cli.command.machine_output(),
        cli.no_animation || cli.no_progress,
    );
    // Captured before `run` consumes the command, so a failure receipt can
    // name the repository and state directory this invocation selected.
    let context = cli.command.invocation_context();

    // Progress lines share stderr with the `--json-errors` receipt, so that
    // flag suppresses them automatically: callers expecting one machine-
    // readable failure receipt must never receive interleaved progress text.
    if !cli.no_progress && !json_errors {
        riftri_core::progress::set_progress_observer(Box::new(report_progress));
    }

    match run(cli) {
        Ok(()) => Ok(()),
        Err(error) => {
            ui::pause_progress();
            if json_errors {
                eprintln!(
                    "{}",
                    serde_json::to_string(&failure_receipt(operation, &error, &context))?
                );
            } else {
                ui::print_error(&format!("Error: {error:?}"));
            }
            std::process::exit(failure_exit_code(&error));
        }
    }
}

/// Exit codes: 0 success, 1 operational failure, 2 command-line usage error
/// (clap), 3 policy refusal. Mirrors the receipt `category` field.
fn failure_exit_code(error: &anyhow::Error) -> i32 {
    if let Some(interrupted) = error.downcast_ref::<ui::Interrupted>() {
        return interrupted.0;
    }
    if absent_repository(error) {
        return 3;
    }
    match error
        .downcast_ref::<riftri_core::WorktreeError>()
        .map(worktree_failure_fields)
    {
        Some((_, "policy", ..)) => 3,
        _ => 1,
    }
}

/// Whether the failure is simply that the caller is not inside a Git
/// repository.
///
/// The exit code and the receipt must agree — `custom-harness.md` tells a
/// runner to branch on the exit code *before* parsing the receipt, so a
/// `policy` receipt delivered with exit 1 would be read as operational and
/// retried, which is the defect this answers. One predicate serves both so
/// they cannot drift.
fn absent_repository(error: &anyhow::Error) -> bool {
    error.chain().any(riftri_core::is_absent_repository)
}

/// Ask before a destructive action when running interactively. Non-interactive
/// callers (agents, CI) are never prompted so existing automation is
/// unaffected; `--yes` skips the prompt for interactive scripts.
fn confirm_destructive_action(warning: &str, yes: bool) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};

    if yes || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    if ui::interactive() {
        let answer = ui::prompt(warning, "Continue?", ui::PromptKind::Confirm)?;
        if answer.as_deref() == Some("y") {
            return Ok(());
        }
        anyhow::bail!("aborted without confirmation; pass --yes to skip the prompt");
    }
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{warning} Continue? [y/N] ").context("write confirmation prompt")?;
    stderr.flush().context("flush confirmation prompt")?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("read confirmation answer")?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
        Ok(())
    } else {
        anyhow::bail!("aborted without confirmation; pass --yes to skip the prompt")
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Setup {
            repository,
            destination,
            branch,
        } => {
            let status = setup::run(repository, destination, branch, cli.json_errors)?;
            if status != 0 {
                std::process::exit(status);
            }
        }
        Command::Enable { path, repository } => {
            let path = repository.unwrap_or(path);
            let activation = riftri_core::enable_repository(&path)?;
            outputln!("Enabled Riftri for {}", activation.repository.display());
            outputln!("Git config: riftri.enabled=true");
            if env::var_os(riftri_core::SHIM_ACTIVE_ENV).is_some() {
                outputln!("Normal Git interception is active in this shell");
            } else {
                #[cfg(unix)]
                outputln!(
                    "Activate this shell with: eval \"$(riftri shell hook {})\"",
                    detected_posix_shell()
                );
                #[cfg(target_os = "windows")]
                outputln!(
                    "Activate this PowerShell session with: Invoke-Expression (& riftri shell hook powershell | Out-String)"
                );
                outputln!("Or activate one process with: riftri exec -- <command>");
            }
        }
        Command::Disable { path, repository } => {
            let path = repository.unwrap_or(path);
            let activation = riftri_core::disable_repository(&path)?;
            outputln!("Disabled Riftri for {}", activation.repository.display());
        }
        Command::Exec { worktree, command } => {
            let status = match worktree {
                Some(worktree) => {
                    riftri_core::execute_scoped_command_in_worktree(&worktree, &command)?
                }
                None => riftri_core::execute_scoped_command(&command)?,
            };
            std::process::exit(status);
        }
        Command::Overlayfs { command } => match command {
            OverlayFsCommand::InstallHelper { replace } => {
                let destination = riftri_core::install_overlayfs_helper(replace)?;
                outputln!(
                    "Installed Riftri OverlayFS helper at {}",
                    destination.display()
                );
                outputln!(
                    "The helper is available system-wide; `riftri enable` still opts in one repository at a time."
                );
            }
        },
        Command::Shell { command } => match command {
            ShellCommand::Hook { shell } => match shell {
                ShellKind::Sh | ShellKind::Bash | ShellKind::Zsh => {
                    ui::print_machine_raw(riftri_core::prepare_posix_shell_hook()?.as_bytes());
                }
                ShellKind::Powershell => {
                    ui::print_machine_raw(riftri_core::prepare_powershell_hook()?.as_bytes());
                }
            },
            ShellCommand::Deactivate { shell } => match shell {
                ShellKind::Sh | ShellKind::Bash | ShellKind::Zsh => {
                    ui::print_machine_raw(
                        riftri_core::prepare_posix_shell_deactivation()?.as_bytes(),
                    );
                }
                ShellKind::Powershell => {
                    ui::print_machine_raw(
                        riftri_core::prepare_powershell_deactivation()?.as_bytes(),
                    );
                }
            },
            ShellCommand::Status {
                repository,
                repository_option,
            } => {
                print_shell_status(&repository_option.unwrap_or(repository))?;
            }
        },
        Command::Completions { shell } => {
            use clap::CommandFactory;

            // clap_complete `.expect()`s its writer, so render into memory and
            // emit through the broken-pipe-safe path (a closed reader exits 0).
            let mut buffer = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "riftri", &mut buffer);
            ui::print_machine_raw(&buffer);
        }
        Command::Man { directory } => {
            use clap::CommandFactory;

            std::fs::create_dir_all(&directory)
                .with_context(|| format!("create man page directory {}", directory.display()))?;
            clap_mangen::generate_to(Cli::command(), &directory).context("write man pages")?;
            outputln!("Man pages written to {}", directory.display());
        }
        Command::Doctor {
            path,
            repository,
            destination,
            json,
        } => {
            let path = repository.unwrap_or(path);
            let destination = destination.as_deref().unwrap_or(&path);
            let report = riftri_core::doctor_for_destination(&path, destination);

            if json {
                machineln!(
                    "{}",
                    serde_json::to_string_pretty(&doctor_json(&report))
                        .context("serialize doctor report")?
                );
            } else {
                print_doctor(&report);
            }
        }
        Command::Backends { path, json } => {
            let backends = riftri_core::backends(&path);

            if json {
                machineln!(
                    "{}",
                    serde_json::to_string_pretty(&backends_json(&path, &backends))
                        .context("serialize backend report")?
                );
            } else {
                outputln!("Storage capabilities for {}:", path.display());
                for backend in backends {
                    outputln!(
                        "- {} ({}): {}",
                        backend.kind.display_name(),
                        backend.status.display_name(),
                        backend.explanation
                    );
                }
                outputln!("\nCapability support does not mean a backend is active yet.");
            }
        }
        Command::Status {
            repository,
            repository_option,
            state_dir,
            json,
        } => {
            let repository = repository_option.unwrap_or(repository);
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::storage_accounting(&state_directory)?;
            print_storage_accounting(&state_directory, &report, json)?;
        }
        Command::Repair {
            repository,
            repository_option,
            state_dir,
            json,
        } => {
            let repository = repository_option.unwrap_or(repository);
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::recover_incomplete_operations(&state_directory)?;
            print_recovery_report(&state_directory, &report, json)?;
        }
        Command::Gc {
            repository,
            repository_option,
            state_dir,
            apply,
            yes,
            json,
        } => {
            let repository = repository_option.unwrap_or(repository);
            if apply {
                confirm_destructive_action(
                    "riftri gc --apply permanently deletes every base in the plan.",
                    yes,
                )?;
            }
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::garbage_collect(&state_directory, apply)?;
            print_garbage_collection_report(&state_directory, &report, json)?;
        }
        Command::State { command } => match command {
            StateCommand::Unregister { path, repository } => {
                let unregistered = riftri_core::forget_missing_state_directory(&repository, &path)?;
                outputln!("Unregistered missing Riftri state directory");
                outputln!("State: {}", unregistered.display());
            }
        },
        Command::Worktree { command } => match command {
            WorktreeCommand::List {
                repository,
                state_dir,
                all_states,
                json,
            } => {
                if all_states {
                    let inventory = riftri_core::worktree_inventory_across_states(&repository)?;
                    print_all_states_worktree_inventory(&inventory, json)?;
                } else {
                    let state_directory = resolve_state_directory(&repository, state_dir)?;
                    let report = riftri_core::storage_accounting(&state_directory)?;
                    print_worktree_inventory(&state_directory, &report, json)?;
                }
            }
            WorktreeCommand::Add {
                path,
                branch,
                detach,
                revision,
                sparse_dir,
                repository,
                state_dir,
                json,
            } => {
                let (mode, revision) = match (branch, detach, revision) {
                    (Some(branch), false, revision) => (
                        riftri_core::WorktreeMode::NewBranch(branch),
                        revision.unwrap_or_else(|| OsString::from("HEAD")),
                    ),
                    (None, true, revision) => (
                        riftri_core::WorktreeMode::Detached,
                        revision.unwrap_or_else(|| OsString::from("HEAD")),
                    ),
                    (None, false, Some(branch)) => (
                        riftri_core::WorktreeMode::ExistingBranch(branch.clone()),
                        branch,
                    ),
                    _ => unreachable!("Clap enforces exactly one worktree head mode"),
                };
                let result = riftri_core::add_worktree(riftri_core::AddWorktreeRequest {
                    repository,
                    destination: path,
                    revision,
                    mode,
                    state_dir,
                    sparse_directories: sparse_dir,
                })?;
                print_add_result(&result, json)?;
                // Git reports a failing post-checkout through its own exit
                // code and leaves the worktree in place. Match that: the
                // worktree is created and registered either way, so this is a
                // status, not a rollback.
                if let Some(hook) = result
                    .post_checkout
                    .as_ref()
                    .filter(|hook| !hook.succeeded())
                {
                    std::process::exit(hook.exit_code.unwrap_or(1));
                }
            }
            WorktreeCommand::Remove {
                path,
                repository,
                state_dir,
                force,
                yes,
                json,
            } => {
                if force {
                    confirm_destructive_action(
                        "riftri worktree remove --force discards uncommitted changes after recording a recovery snapshot.",
                        yes,
                    )?;
                }
                let request = riftri_core::RemoveWorktreeRequest {
                    repository,
                    destination: path,
                    state_dir,
                };
                let result = if force {
                    riftri_core::force_remove_worktree(request)?
                } else {
                    riftri_core::remove_worktree(request)?
                };
                print_remove_result(&result, force, json)?;
            }
            WorktreeCommand::Move {
                source,
                destination,
                repository,
                state_dir,
                json,
            } => {
                let result = riftri_core::move_worktree(riftri_core::MoveWorktreeRequest {
                    repository,
                    source,
                    destination,
                    state_dir,
                })?;
                print_move_result(&result, json)?;
            }
            WorktreeCommand::Compact {
                path,
                repository,
                state_dir,
                json,
            } => {
                let result = riftri_core::compact_worktree(riftri_core::CompactWorktreeRequest {
                    repository,
                    destination: path,
                    state_dir,
                })?;
                print_compact_result(&result, json)?;
            }
            WorktreeCommand::Prune {
                repository,
                state_dir,
                json,
            } => {
                let result = riftri_core::prune_worktrees(riftri_core::PruneWorktreesRequest {
                    repository,
                    state_dir,
                })?;
                print_prune_result(&result, json)?;
            }
        },
    }

    Ok(())
}

/// Actual lifecycle events drive terminal progress. Pipes and plain mode retain
/// bounded phase lines; no UI state is an authority for operation completion.
fn report_progress(event: &riftri_core::progress::ProgressEvent) {
    use riftri_core::progress::ProgressEvent;

    let line = match event {
        ProgressEvent::LockContended { operation } => {
            format!("waiting: {operation} (held by another process)")
        }
        ProgressEvent::LockAcquired { operation } => format!("resumed: {operation}"),
        ProgressEvent::AddPhase { phase } => {
            format!("worktree-add: {}", add_phase_name(*phase))
        }
        ProgressEvent::BaseReused => "worktree-add: immutable base cached; reusing it".to_owned(),
        ProgressEvent::BaseMaterializing => {
            "worktree-add: materializing new immutable base".to_owned()
        }
        ProgressEvent::RepairScanned { operations } => {
            format!("repair: scanned {operations} journaled operation(s)")
        }
        ProgressEvent::RepairRecovering { kind, operation_id } => {
            format!("repair: recovering {kind} operation {operation_id}")
        }
        ProgressEvent::GcPlanned { candidates } => {
            format!("garbage-collection: {candidates} candidate base(s)")
        }
        ProgressEvent::GcPhase { phase } => {
            format!("garbage-collection: {}", collection_phase_name(*phase))
        }
        _ => return,
    };
    ui::progress(line);
}

/// A machine-readable receipt for a command-line usage error, produced when
/// clap rejects the arguments before a subcommand is ever selected and the
/// caller asked for `--json-errors`. It mirrors [`failure_receipt`]'s shape so
/// the same parser handles it, but carries the `usage` category matching exit
/// code 2. Parsing failed, so no operation, repository, or state directory is
/// known: those fields are explicit `null`.
fn usage_receipt(error: &clap::Error) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "outcome": "failed",
        "operation": serde_json::Value::Null,
        "code": "usage-error",
        "category": "usage",
        "message": error.to_string().trim_end(),
        "phase": serde_json::Value::Null,
        "cleanup": "not-needed",
        "recovery": "not-required",
        "nextCommand": serde_json::Value::Null,
        "repository": serde_json::Value::Null,
        "repositoryNativeHex": serde_json::Value::Null,
        "stateDirectory": serde_json::Value::Null,
        "stateDirectoryNativeHex": serde_json::Value::Null,
        "nativePathEncoding": native_path_encoding(),
    })
}

fn failure_receipt(
    operation: &'static str,
    error: &anyhow::Error,
    context: &InvocationContext,
) -> serde_json::Value {
    let worktree_error = error.downcast_ref::<riftri_core::WorktreeError>();
    // Standing outside a repository is a caller mistake, not an operational
    // failure: retrying never helps, nothing was attempted, and the fix is the
    // caller's. It reaches this function by two routes — wrapped in a
    // `WorktreeError` from the worktree commands, and in an `ActivationError`
    // from `status`, `gc`, and `repair`, which does not downcast here and so
    // used to land on the operational fallback below. Matching on the chain
    // classifies both identically, so the commands cannot drift apart again.
    let absent_repository = absent_repository(error);
    let (code, category, phase, cleanup, recovery) = if absent_repository {
        (
            "not-a-repository",
            "policy",
            None,
            "not-needed",
            "not-required",
        )
    } else {
        worktree_error.map(worktree_failure_fields).unwrap_or((
            "command-failed",
            "operational",
            None,
            "unknown",
            recovery_for_operation(operation),
        ))
    };
    // Never answer a failure with the command that just produced it. `repair`
    // is the standard remedy for `recovery: "required"`, but when `repair`
    // itself is what failed that advice is a retry loop for any caller that
    // follows `nextCommand`; point at `status`, which isolates and names the
    // state that needs attention.
    let recovery = if operation == "repair" && recovery == "required" {
        "inspect"
    } else {
        recovery
    };
    let next_command = recovery_next_command(recovery, worktree_error, context);
    // An error that names its own state directory outranks anything the
    // command line selected: that is where the state needing attention
    // actually lives.
    let state_directory = match worktree_error {
        Some(
            riftri_core::WorktreeError::RecoveryPending {
                state_directory, ..
            }
            | riftri_core::WorktreeError::SymlinkedBaseParent {
                state_directory, ..
            }
            | riftri_core::WorktreeError::StaleStateRegistration {
                state_directory, ..
            },
        ) => Some(state_directory.clone()),
        _ => context
            .state_directory
            .as_deref()
            .map(riftri_core::command_path),
    };
    let repository = match worktree_error {
        Some(riftri_core::WorktreeError::StaleStateRegistration { repository, .. }) => {
            Some(repository.clone())
        }
        _ => context.repository.as_deref().map(riftri_core::command_path),
    };

    serde_json::json!({
        "schemaVersion": 1,
        "outcome": "failed",
        "operation": operation,
        "code": code,
        "category": category,
        "message": error.to_string(),
        "phase": phase,
        "cleanup": cleanup,
        "recovery": recovery,
        "nextCommand": next_command,
        // Exact context for automation. `nextCommand` is a shell string and is
        // omitted entirely when a path cannot be written as a shell argument;
        // these fields stay faithful in every case.
        "repository": repository.as_ref().map(|path| path.display().to_string()),
        "repositoryNativeHex": repository.as_deref().map(native_path_hex),
        "stateDirectory": state_directory.as_ref().map(|path| path.display().to_string()),
        "stateDirectoryNativeHex": state_directory.as_deref().map(native_path_hex),
        "nativePathEncoding": native_path_encoding(),
    })
}

/// The recovery or inspection command a caller should run next, targeted at
/// the state the failing command actually used.
///
/// Returns `None` when no command applies, and also when the path involved
/// cannot be written as a shell argument: a command that silently addresses
/// the wrong directory is worse than no command, because `riftri repair`
/// reports an all-clear for any state directory it does not find.
fn recovery_next_command(
    recovery: &str,
    worktree_error: Option<&riftri_core::WorktreeError>,
    context: &InvocationContext,
) -> Option<String> {
    if let Some(riftri_core::WorktreeError::RecoveryPending {
        state_directory, ..
    }) = worktree_error
    {
        // Core's human-readable message names exactly this command, so the
        // two can never disagree.
        return riftri_core::repair_command(state_directory);
    }
    if let Some(riftri_core::WorktreeError::SymlinkedBaseParent {
        state_directory, ..
    }) = worktree_error
    {
        // Same guarantee for the symlinked-base safety stop: its message
        // names exactly this status command.
        return riftri_core::status_command(state_directory);
    }
    if let Some(riftri_core::WorktreeError::StaleStateRegistration {
        repository,
        state_directory,
        ..
    }) = worktree_error
    {
        return riftri_core::unregister_state_command(repository, state_directory);
    }
    let subcommand = match recovery {
        "required" => "repair",
        "inspect" => "status",
        _ => return None,
    };
    match context.target() {
        Some(ContextTarget::StateDirectory(path)) => riftri_core::shell_quoted_path(&path)
            .map(|quoted| format!("riftri {subcommand} --state-dir {quoted}")),
        Some(ContextTarget::Repository(path)) => riftri_core::shell_quoted_path(&path)
            .map(|quoted| format!("riftri {subcommand} --repository {quoted}")),
        None => Some(format!("riftri {subcommand}")),
    }
}

fn worktree_failure_fields(
    error: &riftri_core::WorktreeError,
) -> (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static str,
) {
    use riftri_core::WorktreeError;

    match error {
        WorktreeError::Git(_) => ("git-failed", "operational", None, "unknown", "inspect"),
        WorktreeError::Storage(_) => ("storage-failed", "operational", None, "unknown", "inspect"),
        WorktreeError::Journal(_) => ("journal-failed", "operational", None, "unknown", "required"),
        WorktreeError::JournalTransition(_)
        | WorktreeError::RemoveJournalTransition(_)
        | WorktreeError::MoveJournalTransition(_)
        | WorktreeError::CompactJournalTransition(_)
        | WorktreeError::PruneJournalTransition(_) => (
            "journal-transition-failed",
            "operational",
            None,
            "unknown",
            "required",
        ),
        WorktreeError::Unsupported(_) => (
            "unsupported-checkout",
            "policy",
            None,
            "not-needed",
            "not-required",
        ),
        WorktreeError::InvalidRequest(_) => (
            "invalid-request",
            "policy",
            None,
            "not-needed",
            "not-required",
        ),
        // The safety stop that refuses to follow a symbolic link inside
        // Riftri's own base storage. Nothing was attempted, so it stays a
        // policy refusal with no cleanup — but unlike other invalid requests
        // its message tells the caller to inspect the affected state, so the
        // receipt must carry the same instruction and the same command.
        WorktreeError::SymlinkedBaseParent { .. } => {
            ("invalid-request", "policy", None, "not-needed", "inspect")
        }
        WorktreeError::StaleStateRegistration { .. } => (
            "stale-state-registration",
            "policy",
            None,
            "not-needed",
            "required",
        ),
        // An interrupted lifecycle operation left a durable journal behind:
        // nothing was changed by this command, but the caller must run the
        // repair command echoed in `nextCommand` before retrying.
        WorktreeError::RecoveryPending { .. } => (
            "recovery-pending",
            "operational",
            None,
            "not-needed",
            "required",
        ),
        // Another live process holds the operation lock. No repair is needed;
        // the caller should wait for the concurrent operation and retry.
        WorktreeError::Busy { .. } => ("worktree-busy", "operational", None, "not-needed", "retry"),
        WorktreeError::Io { .. } => (
            "filesystem-io-failed",
            "operational",
            None,
            "unknown",
            "inspect",
        ),
        WorktreeError::OperationAndRollback { .. } => (
            "rollback-failed",
            "operational",
            Some("rollback"),
            "unknown",
            "required",
        ),
        WorktreeError::InjectedFailure(phase) => (
            "injected-failure",
            "operational",
            Some(add_phase_name(*phase)),
            "rolled-back",
            "not-required",
        ),
        WorktreeError::InjectedRemovalFailure(phase) => (
            "injected-failure",
            "operational",
            Some(remove_phase_name(*phase)),
            "unknown",
            "inspect",
        ),
        WorktreeError::InjectedMoveFailure(phase) => (
            "injected-failure",
            "operational",
            Some(move_phase_name(*phase)),
            "unknown",
            "inspect",
        ),
        WorktreeError::InjectedCompactionFailure(phase) => (
            "injected-failure",
            "operational",
            Some(compact_phase_name(*phase)),
            "unknown",
            "inspect",
        ),
        WorktreeError::InjectedPruneFailure(phase) => (
            "injected-failure",
            "operational",
            Some(prune_phase_name(*phase)),
            "unknown",
            "inspect",
        ),
        WorktreeError::InjectedCollectionFailure(phase) => (
            "injected-failure",
            "operational",
            Some(collection_phase_name(*phase)),
            "unknown",
            "inspect",
        ),
    }
}

fn recovery_for_operation(operation: &str) -> &'static str {
    match operation {
        "worktree-add" | "worktree-remove" | "worktree-move" | "worktree-compact"
        | "worktree-prune" | "garbage-collection" | "repair" => "inspect",
        _ => "unknown",
    }
}

fn add_phase_name(phase: riftri_core::AddWorktreePhase) -> &'static str {
    use riftri_core::AddWorktreePhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        GitMetadataCreated => "git-metadata-created",
        BaseReady => "base-ready",
        ViewCreated => "view-created",
        GitPointerRestored => "git-pointer-restored",
        IndexSynchronized => "index-synchronized",
        CleanVerified => "clean-verified",
        Active => "active",
        RollbackPending => "rollback-pending",
        RolledBack => "rolled-back",
    }
}

fn remove_phase_name(phase: riftri_core::RemoveWorktreePhase) -> &'static str {
    use riftri_core::RemoveWorktreePhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        CleanVerified => "clean-verified",
        WorktreeRemoved => "worktree-removed",
        BaseReleased => "base-released",
        Complete => "complete",
    }
}

fn move_phase_name(phase: riftri_core::MoveWorktreePhase) -> &'static str {
    use riftri_core::MoveWorktreePhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        WorktreeMoved => "worktree-moved",
        AddJournalUpdated => "add-journal-updated",
        Complete => "complete",
    }
}

fn compact_phase_name(phase: riftri_core::CompactWorktreePhase) -> &'static str {
    use riftri_core::CompactWorktreePhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        ReplacementReady => "replacement-ready",
        ReplacementActivated => "replacement-activated",
        AddJournalUpdated => "add-journal-updated",
        Complete => "complete",
        Cancelled => "cancelled",
    }
}

fn prune_phase_name(phase: riftri_core::PruneWorktreesPhase) -> &'static str {
    use riftri_core::PruneWorktreesPhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        GitMetadataPruned => "git-metadata-pruned",
        Complete => "complete",
    }
}

fn collection_phase_name(phase: riftri_core::GarbageCollectionPhase) -> &'static str {
    use riftri_core::GarbageCollectionPhase::*;
    match phase {
        IntentRecorded => "intent-recorded",
        MarkerRemoved => "marker-removed",
        BaseQuarantined => "base-quarantined",
        Complete => "complete",
        Cancelled => "cancelled",
    }
}

#[cfg(target_os = "linux")]
fn invoked_as_overlayfs_helper() -> bool {
    let elevated = unsafe { libc::geteuid() } != unsafe { libc::getuid() };
    let installed_name = env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .is_some_and(|name| name == OsStr::new("riftri-overlayfs-helper"));
    elevated || installed_name
}

#[cfg(target_os = "linux")]
fn run_overlayfs_helper() -> Result<()> {
    anyhow::ensure!(
        unsafe { libc::geteuid() } == 0,
        "the OverlayFS helper is not elevated; reinstall it with `sudo riftri overlayfs install-helper --replace`"
    );
    let requester_uid = unsafe { libc::getuid() };
    let requester_gid = unsafe { libc::getgid() };
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let Some(operation) = arguments.first().and_then(|value| value.to_str()) else {
        anyhow::bail!("invalid internal OverlayFS helper request");
    };
    match (operation, &arguments[1..]) {
        ("mount", [layout_root, lower, merged]) => {
            let identity = riftri_storage::OverlayFsMounter::helper_mount(
                Path::new(layout_root),
                Path::new(lower),
                Path::new(merged),
                requester_uid,
            )?;
            outputln!("{}", serde_json::to_string(&identity)?);
        }
        ("unmount", [layout_root, lower, merged, identity]) => {
            let identity = identity
                .to_str()
                .context("internal mount identity is not UTF-8")?;
            let identity = serde_json::from_str(identity)
                .context("decode internal OverlayFS mount identity")?;
            let unmounted = riftri_storage::OverlayFsMounter::helper_unmount(
                Path::new(layout_root),
                Path::new(lower),
                Path::new(merged),
                &identity,
                requester_uid,
                requester_gid,
            )?;
            outputln!("{unmounted}");
        }
        ("reset-work", [layout_root, lower, merged, context]) => {
            let context = context
                .to_str()
                .context("internal mount context is not UTF-8")?;
            let context =
                serde_json::from_str(context).context("decode internal OverlayFS mount context")?;
            riftri_storage::OverlayFsMounter::helper_reset_work(
                Path::new(layout_root),
                Path::new(lower),
                Path::new(merged),
                requester_uid,
                requester_gid,
                &context,
            )?;
        }
        _ => anyhow::bail!("invalid internal OverlayFS helper request"),
    }
    Ok(())
}

/// Resolve the state directory an inspection or recovery command should read.
///
/// The two cases are deliberately different. A repository that has never
/// created Riftri state is a normal, healthy situation: the default directory
/// is simply absent, every count is zero, and an all-clear is correct. An
/// explicitly named `--state-dir` that does not exist is not: the caller
/// asserted that state lives there. Treating it as empty — which is how the
/// journal scanner treats any missing directory — would answer "nothing needs
/// attention" about a directory Riftri never looked in, and a mistyped or
/// shell-split path is exactly how callers arrive here.
fn resolve_state_directory(repository: &Path, state_directory: Option<PathBuf>) -> Result<PathBuf> {
    match state_directory {
        Some(state_directory) => {
            match std::fs::symlink_metadata(&state_directory) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(anyhow::Error::new(
                        riftri_core::WorktreeError::InvalidRequest(format!(
                            "state directory {} does not exist.\n\
                             Nothing was inspected, so this is not an all-clear. Check the path \
                             — a path containing spaces must be quoted in your shell — or omit \
                             --state-dir to use the repository default.",
                            state_directory.display()
                        )),
                    ));
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error)).with_context(|| {
                        format!("inspect state directory {}", state_directory.display())
                    });
                }
            }
            Ok(state_directory)
        }
        None => {
            let activation = riftri_core::repository_activation(repository)?;
            Ok(activation.common_git_dir.join("riftri"))
        }
    }
}

fn print_shell_status(repository: &Path) -> Result<()> {
    let shell = riftri_core::shell_activation_status()?;
    outputln!(
        "Shell interception: {}",
        if shell.active {
            "active"
        } else if shell.marker_set || shell.shim_first_on_path || shell.real_git.is_some() {
            "incomplete"
        } else {
            "inactive"
        }
    );
    outputln!("Shim directory: {}", shell.shim_directory.display());
    if let Some(real_git) = &shell.real_git {
        outputln!("Real Git: {}", real_git.display());
    }
    outputln!(
        "Global shell scope: {}",
        if shell.active {
            "this shell and its children; every new shell too only if you added the hook to your profile"
        } else {
            "not active in this shell"
        }
    );
    match riftri_core::repository_activation(repository) {
        Ok(activation) => {
            outputln!("Repository: {}", activation.repository.display());
            outputln!(
                "Repository optimization: {}",
                if activation.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            outputln!(
                "Effective optimized interception: {}",
                if shell.bypass {
                    "inactive (RIFTRI_BYPASS)"
                } else if shell.active && activation.enabled {
                    "active"
                } else {
                    "inactive"
                }
            );
        }
        Err(_) => {
            outputln!("Repository: none at {}", repository.display());
            outputln!("Repository optimization: not applicable");
            outputln!("Effective optimized interception: inactive");
        }
    }
    Ok(())
}

fn relocated_worktrees_json(report: &riftri_core::RecoveryReport) -> Vec<serde_json::Value> {
    report
        .relocations
        .iter()
        .map(|relocation| {
            serde_json::json!({
                "operation_id": relocation.operation_id,
                "journal_destination": relocation.journal_destination.display().to_string(),
                "journal_destination_native_hex": native_path_hex(&relocation.journal_destination),
                "registered_path": relocation.registered_path.display().to_string(),
                "registered_path_native_hex": native_path_hex(&relocation.registered_path),
            })
        })
        .collect()
}

fn print_recovery_report(
    state_directory: &Path,
    report: &riftri_core::RecoveryReport,
    json: bool,
) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "state_directory": state_directory.display().to_string(),
            "state_directory_native_hex": native_path_hex(state_directory),
            "native_path_encoding": native_path_encoding(),
            "scanned": report.scanned,
            "busy_adds": report.busy_adds,
            "active": report.active,
            "recovered_mounts": report.recovered_mounts,
            "recovered_adds": report.recovered,
            "completed_removals": report.completed_removals,
            "recovered_removals": report.recovered_removals,
            "completed_moves": report.completed_moves,
            "recovered_moves": report.recovered_moves,
            "completed_compactions": report.completed_compactions,
            "recovered_compactions": report.recovered_compactions,
            "completed_prunes": report.completed_prunes,
            "recovered_prunes": report.recovered_prunes,
            "completed_collections": report.completed_collections,
            "recovered_collections": report.recovered_collections,
            "retired_adds": report.retired_adds,
            "relocated_worktrees": relocated_worktrees_json(report),
            "unresolvable_worktrees": report
                .unresolvable_worktrees
                .iter()
                .map(|path| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "path_native_hex": native_path_hex(path),
                    })
                })
                .collect::<Vec<_>>(),
            "reaped_artifacts": report
                .reaped_artifacts
                .iter()
                .map(|path| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "path_native_hex": native_path_hex(path),
                    })
                })
                .collect::<Vec<_>>(),
            "reaped_probe_roots": report
                .reaped_probe_roots
                .iter()
                .map(|path| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "path_native_hex": native_path_hex(path),
                    })
                })
                .collect::<Vec<_>>(),
            "preserved_probe_mounts": report
                .preserved_probe_mounts
                .iter()
                .map(|path| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "path_native_hex": native_path_hex(path),
                    })
                })
                .collect::<Vec<_>>(),
            "errors": report.errors,
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize recovery report")?
        );
        if !report.errors.is_empty() {
            anyhow::bail!(
                "{} operation(s) need manual attention; no changed worktree was deleted",
                report.errors.len()
            );
        }
        return Ok(());
    }

    outputln!("Riftri repair");
    outputln!("State: {}", state_directory.display());
    outputln!("Scanned operations: {}", report.scanned);
    outputln!("Busy add operations skipped: {}", report.busy_adds);
    outputln!("Active worktrees: {}", report.active);
    outputln!("Recovered mounts: {}", report.recovered_mounts);
    outputln!("Recovered add operations: {}", report.recovered);
    outputln!("Completed removals: {}", report.completed_removals);
    outputln!("Recovered removals: {}", report.recovered_removals);
    outputln!("Completed moves: {}", report.completed_moves);
    outputln!("Recovered moves: {}", report.recovered_moves);
    outputln!("Completed compactions: {}", report.completed_compactions);
    outputln!("Recovered compactions: {}", report.recovered_compactions);
    outputln!("Completed prunes: {}", report.completed_prunes);
    outputln!("Recovered prunes: {}", report.recovered_prunes);
    outputln!("Completed collections: {}", report.completed_collections);
    outputln!("Recovered collections: {}", report.recovered_collections);
    outputln!("Retired add operations: {}", report.retired_adds);
    outputln!(
        "Reaped interrupted journal writes: {}",
        report.reaped_artifacts.len()
    );
    for path in &report.reaped_artifacts {
        outputln!("- {}", path.display());
    }
    outputln!(
        "Reaped abandoned OverlayFS probes: {}",
        report.reaped_probe_roots.len()
    );
    for path in &report.reaped_probe_roots {
        outputln!("- {}", path.display());
    }
    if !report.preserved_probe_mounts.is_empty() {
        outputln!("Abandoned OverlayFS probes still covered by a mount (preserved):");
        for path in &report.preserved_probe_mounts {
            outputln!(
                "- {}: unmount it, then rerun `riftri repair`",
                path.display()
            );
        }
    }
    if !report.relocations.is_empty() {
        outputln!("Relocated worktrees Riftri no longer tracks:");
        for relocation in &report.relocations {
            outputln!(
                "- operation {}: journaled {} is now registered by Git at {}; Riftri did not adopt the new path",
                relocation.operation_id,
                relocation.journal_destination.display(),
                relocation.registered_path.display()
            );
        }
    }
    if !report.unresolvable_worktrees.is_empty() {
        outputln!("Worktrees Git lists without a resolvable HEAD:");
        for path in &report.unresolvable_worktrees {
            outputln!(
                "- {}: run `git worktree repair` or remove the worktree",
                path.display()
            );
        }
    }
    if !report.errors.is_empty() {
        outputln!("Operations needing attention:");
        for error in &report.errors {
            outputln!("- {error}");
        }
        anyhow::bail!(
            "{} operation(s) need manual attention; no changed worktree was deleted",
            report.errors.len()
        );
    }
    outputln!("No journaled operation needs manual attention");
    Ok(())
}

fn invoked_as_git_shim() -> bool {
    let named_git = env::args_os()
        .next()
        .as_deref()
        .and_then(|argument| Path::new(argument).file_name())
        .is_some_and(|name| name == OsStr::new("git") || name == OsStr::new("git.exe"));
    // A `git`-named invocation without the activation marker is still a shim
    // when a Riftri shim scope is otherwise recognizable: answering as the
    // Riftri CLI would shadow normal Git behavior, so fail toward delegation.
    named_git
        && (env::var_os(riftri_core::SHIM_ACTIVE_ENV).is_some()
            || riftri_core::stripped_shim_scope_detected())
}

#[cfg(unix)]
fn detected_posix_shell() -> &'static str {
    let shell = env::var_os("SHELL");
    match shell
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
    {
        Some(name) if name == OsStr::new("bash") => "bash",
        Some(name) if name == OsStr::new("zsh") => "zsh",
        _ => "sh",
    }
}

fn run_git_shim() -> Result<i32> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if !riftri_core::shim_environment_complete() {
        // Fail safe: a shim whose environment was stripped — for example by
        // shell deactivation evaluated inside a `riftri exec` session — must
        // behave exactly like the real Git instead of answering as Riftri.
        return Ok(riftri_core::delegate_stripped_shim_invocation(&arguments)?);
    }
    let current_directory = env::current_dir().context("resolve Git working directory")?;

    match riftri_core::proxy_git_command(&current_directory, &arguments)? {
        riftri_core::GitProxyOutcome::Passthrough(status) => Ok(status),
        riftri_core::GitProxyOutcome::OptimizedAdd { result, quiet } => {
            if !quiet {
                eprintln!(
                    "Riftri created an optimized {} worktree at {} ({})",
                    result.backend.display_name(),
                    result.destination.display(),
                    if result.reused_base {
                        "reused base"
                    } else {
                        "new base"
                    }
                );
            }
            Ok(0)
        }
        riftri_core::GitProxyOutcome::OptimizedRemove(result) => {
            eprintln!(
                "Riftri safely removed worktree at {} (base retained)",
                result.destination.display()
            );
            Ok(0)
        }
        riftri_core::GitProxyOutcome::OptimizedMove(result) => {
            eprintln!(
                "Riftri moved managed Riftri worktree to {}",
                result.destination.display()
            );
            Ok(0)
        }
        riftri_core::GitProxyOutcome::OptimizedPrune(_) => {
            eprintln!("Riftri pruned stale Git worktree metadata");
            Ok(0)
        }
    }
}

fn print_add_result(result: &riftri_core::AddWorktreeResult, json: bool) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "native_path_encoding": native_path_encoding(),
            "destination": result.destination.display().to_string(),
            "destination_native_hex": native_path_hex(&result.destination),
            "commit": result.commit.as_str(),
            "tree": result.tree.as_str(),
            "backend": result.backend,
            "base_path": result.base_path.display().to_string(),
            "base_path_native_hex": native_path_hex(&result.base_path),
            "reused_base": result.reused_base,
            "post_checkout": result.post_checkout.as_ref().map(|hook| serde_json::json!({
                "hook": hook.hook.display().to_string(),
                "started": hook.started,
                "exit_code": hook.exit_code,
            })),
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize add result")?
        );
        return Ok(());
    }

    outputln!(
        "Created {}-backed Git worktree",
        result.backend.display_name()
    );
    outputln!("Destination: {}", result.destination.display());
    outputln!("Commit: {}", result.commit.as_str());
    outputln!("Tree: {}", result.tree.as_str());
    outputln!("Immutable base: {}", result.base_path.display());
    outputln!(
        "Base: {}",
        if result.reused_base {
            "reused"
        } else {
            "created"
        }
    );
    outputln!("Journal: {}", result.journal_path.display());
    if let Some(hook) = &result.post_checkout {
        outputln!(
            "post-checkout hook: {} ({})",
            hook.hook.display(),
            describe_hook_outcome(hook)
        );
    }
    Ok(())
}

/// How the repository's post-checkout hook finished, for humans.
fn describe_hook_outcome(hook: &riftri_core::PostCheckoutOutcome) -> String {
    if !hook.started {
        return "could not be started".to_owned();
    }
    match hook.exit_code {
        Some(0) => "ran".to_owned(),
        Some(code) => format!("exited {code}"),
        None => "terminated by a signal".to_owned(),
    }
}

fn print_remove_result(
    result: &riftri_core::RemoveWorktreeResult,
    forced: bool,
    json: bool,
) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "native_path_encoding": native_path_encoding(),
            "destination": result.destination.display().to_string(),
            "destination_native_hex": native_path_hex(&result.destination),
            "forced": forced,
            "base_path": result.base_path.display().to_string(),
            "base_path_native_hex": native_path_hex(&result.base_path),
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize remove result")?
        );
        return Ok(());
    }

    if forced {
        outputln!("Force-removed Riftri-backed Git worktree after snapshot verification");
    } else {
        outputln!("Removed Riftri-backed Git worktree");
    }
    outputln!("Destination: {}", result.destination.display());
    outputln!("Retained immutable base: {}", result.base_path.display());
    outputln!("Journal: {}", result.journal_path.display());
    Ok(())
}

fn print_move_result(result: &riftri_core::MoveWorktreeResult, json: bool) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "native_path_encoding": native_path_encoding(),
            "source": result.source.display().to_string(),
            "source_native_hex": native_path_hex(&result.source),
            "destination": result.destination.display().to_string(),
            "destination_native_hex": native_path_hex(&result.destination),
            "base_path": result.base_path.display().to_string(),
            "base_path_native_hex": native_path_hex(&result.base_path),
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize move result")?
        );
        return Ok(());
    }

    outputln!("Moved Riftri-backed Git worktree");
    outputln!("Source: {}", result.source.display());
    outputln!("Destination: {}", result.destination.display());
    outputln!("Retained immutable base: {}", result.base_path.display());
    outputln!("Journal: {}", result.journal_path.display());
    Ok(())
}

fn print_compact_result(result: &riftri_core::CompactWorktreeResult, json: bool) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "native_path_encoding": native_path_encoding(),
            "destination": result.destination.display().to_string(),
            "destination_native_hex": native_path_hex(&result.destination),
            "commit": result.commit.as_str(),
            "tree": result.tree.as_str(),
            "old_base_path": result.old_base_path.display().to_string(),
            "old_base_path_native_hex": native_path_hex(&result.old_base_path),
            "base_path": result.base_path.display().to_string(),
            "base_path_native_hex": native_path_hex(&result.base_path),
            "reused_base": result.reused_base,
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize compact result")?
        );
        return Ok(());
    }

    outputln!("Compacted Riftri-backed Git worktree");
    outputln!("Destination: {}", result.destination.display());
    outputln!("Commit: {}", result.commit.as_str());
    outputln!("Immutable base: {}", result.base_path.display());
    outputln!("Reused immutable base: {}", result.reused_base);
    outputln!("Journal: {}", result.journal_path.display());
    Ok(())
}

fn print_prune_result(result: &riftri_core::PruneWorktreesResult, json: bool) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "native_path_encoding": native_path_encoding(),
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize prune result")?
        );
        return Ok(());
    }

    outputln!("Pruned stale Git worktree metadata");
    outputln!("Journal: {}", result.journal_path.display());
    Ok(())
}

fn worktree_view_json(view: &riftri_core::ViewStorageAccounting) -> serde_json::Value {
    serde_json::json!({
        "repository": view.repository.display().to_string(),
        "repository_native_hex": native_path_hex(&view.repository),
        "path": view.destination.display().to_string(),
        "path_native_hex": native_path_hex(&view.destination),
        "head": view.head.as_str(),
        "branch": view.branch.as_deref().map(display_git_bytes),
        "branch_hex": view.branch.as_deref().map(encode_hex),
        "detached": view.detached,
        "locked_reason": view.locked_reason.as_deref().map(display_git_bytes),
        "locked_reason_hex": view.locked_reason.as_deref().map(encode_hex),
        "prunable_reason": view.prunable_reason.as_deref().map(display_git_bytes),
        "prunable_reason_hex": view.prunable_reason.as_deref().map(encode_hex),
        "backend": view.backend,
        "base_path": view.base_path.display().to_string(),
        "base_path_native_hex": native_path_hex(&view.base_path),
        "logical_bytes": view.logical_bytes,
        "allocated_bytes": view.allocated_bytes,
    })
}

fn diagnostic_issue_json(issue: &riftri_core::StateDiagnosticIssue) -> serde_json::Value {
    serde_json::json!({
        "path": issue.path.display().to_string(),
        "path_native_hex": native_path_hex(&issue.path),
        "reason": issue.reason,
    })
}

fn print_worktree_inventory(
    state_directory: &Path,
    report: &riftri_core::StorageAccountingReport,
    json: bool,
) -> Result<()> {
    if json {
        let worktrees = report
            .views
            .iter()
            .map(worktree_view_json)
            .collect::<Vec<_>>();
        let diagnostic_issues = report
            .diagnostic_issues
            .iter()
            .map(diagnostic_issue_json)
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "schema_version": 1,
            "state_directory": state_directory.display().to_string(),
            "state_directory_native_hex": native_path_hex(state_directory),
            "native_path_encoding": native_path_encoding(),
            "worktrees": worktrees,
            "diagnostic_issues": diagnostic_issues,
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize worktree inventory")?
        );
        return Ok(());
    }

    outputln!("Riftri managed worktrees");
    outputln!("State: {}", state_directory.display());
    outputln!("Managed worktrees: {}", report.views.len());
    for view in &report.views {
        outputln!("- {}", view.destination.display());
        print_worktree_view_details(view, None);
    }
    if !report.diagnostic_issues.is_empty() {
        outputln!("Diagnostic issues: {}", report.diagnostic_issues.len());
        for issue in &report.diagnostic_issues {
            outputln!("- {}: {}", issue.path.display(), issue.reason);
        }
    }
    Ok(())
}

fn print_worktree_view_details(view: &riftri_core::ViewStorageAccounting, state: Option<&Path>) {
    if let Some(state) = state {
        outputln!("  State: {}", state.display());
    }
    outputln!("  Repository: {}", view.repository.display());
    outputln!("  Head: {}", view.head.as_str());
    if let Some(branch) = &view.branch {
        outputln!("  Branch: {}", display_git_bytes(branch));
    } else if view.detached {
        outputln!("  Branch: detached");
    }
    if let Some(reason) = &view.locked_reason {
        outputln!("  Locked: {}", display_git_bytes(reason));
    }
    if let Some(reason) = &view.prunable_reason {
        outputln!("  Prunable: {}", display_git_bytes(reason));
    }
    outputln!("  Backend: {}", view.backend.display_name());
    outputln!("  Immutable base: {}", view.base_path.display());
    outputln!("  Logical: {}", display_byte_count(view.logical_bytes));
    outputln!(
        "  Filesystem-accounted allocated: {}",
        display_byte_count(view.allocated_bytes)
    );
}

fn print_all_states_worktree_inventory(
    inventory: &riftri_core::AllStatesWorktreeInventory,
    json: bool,
) -> Result<()> {
    if json {
        let state_directories = inventory
            .states
            .iter()
            .map(|state| {
                serde_json::json!({
                    "path": state.state_directory.display().to_string(),
                    "path_native_hex": native_path_hex(&state.state_directory),
                    "source": state.source.as_str(),
                })
            })
            .collect::<Vec<_>>();
        let worktrees = inventory
            .states
            .iter()
            .flat_map(|state| {
                state.views.iter().map(|view| {
                    let mut value = worktree_view_json(view);
                    value["state_directory"] =
                        serde_json::Value::from(state.state_directory.display().to_string());
                    value["state_directory_native_hex"] =
                        serde_json::Value::from(native_path_hex(&state.state_directory));
                    value
                })
            })
            .collect::<Vec<_>>();
        let diagnostic_issues = inventory
            .registration_issues
            .iter()
            .map(|issue| {
                // A registration-level issue belongs to no usable state
                // directory, so the display string and its native-hex twin
                // are both explicitly null rather than absent or unpaired.
                let mut value = diagnostic_issue_json(issue);
                value["state_directory"] = serde_json::Value::Null;
                value["state_directory_native_hex"] = serde_json::Value::Null;
                value
            })
            .chain(inventory.states.iter().flat_map(|state| {
                state.diagnostic_issues.iter().map(|issue| {
                    let mut value = diagnostic_issue_json(issue);
                    value["state_directory"] =
                        serde_json::Value::from(state.state_directory.display().to_string());
                    value["state_directory_native_hex"] =
                        serde_json::Value::from(native_path_hex(&state.state_directory));
                    value
                })
            }))
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "schema_version": 2,
            "scope": "all-registered-states",
            "native_path_encoding": native_path_encoding(),
            "state_directories": state_directories,
            "worktrees": worktrees,
            "diagnostic_issues": diagnostic_issues,
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize worktree inventory")?
        );
        return Ok(());
    }

    outputln!("Riftri managed worktrees (all registered states)");
    outputln!("State directories: {}", inventory.states.len());
    for state in &inventory.states {
        outputln!(
            "- {} ({})",
            state.state_directory.display(),
            state.source.as_str()
        );
    }
    let total_views = inventory
        .states
        .iter()
        .map(|state| state.views.len())
        .sum::<usize>();
    outputln!("Managed worktrees: {total_views}");
    for state in &inventory.states {
        for view in &state.views {
            outputln!("- {}", view.destination.display());
            print_worktree_view_details(view, Some(&state.state_directory));
        }
    }
    let total_issues = inventory.registration_issues.len()
        + inventory
            .states
            .iter()
            .map(|state| state.diagnostic_issues.len())
            .sum::<usize>();
    if total_issues > 0 {
        outputln!("Diagnostic issues: {total_issues}");
        for issue in inventory.registration_issues.iter().chain(
            inventory
                .states
                .iter()
                .flat_map(|state| state.diagnostic_issues.iter()),
        ) {
            outputln!("- {}: {}", issue.path.display(), issue.reason);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn native_path_hex(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    encode_hex(path.as_os_str().as_bytes())
}

#[cfg(target_os = "windows")]
fn native_path_hex(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;

    let bytes = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    encode_hex(&bytes)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn native_path_hex(path: &Path) -> String {
    encode_hex(path.to_string_lossy().as_bytes())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn display_git_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Exact byte count first, with a binary-unit rendering for readability once
/// the count reaches one KiB. JSON reports keep raw integers.
fn display_byte_count(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let rendered = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    format!("{bytes} bytes ({rendered} {})", UNITS[unit])
}

const fn native_path_encoding() -> &'static str {
    #[cfg(unix)]
    return "unix-bytes-hex";
    #[cfg(target_os = "windows")]
    return "windows-utf16le-hex";
    #[cfg(not(any(unix, target_os = "windows")))]
    return "utf8-lossy-bytes-hex";
}

fn print_storage_accounting(
    state_directory: &Path,
    report: &riftri_core::StorageAccountingReport,
    json: bool,
) -> Result<()> {
    // Every count below is derived only from journals that parsed. A journal
    // that could not be read still holds a claim, so when any are unreadable
    // the counts are a lower bound rather than the truth.
    let counts_are_complete = report.diagnostic_issues.is_empty();
    if json {
        let bases = report
            .bases
            .iter()
            .map(|base| {
                serde_json::json!({
                    "path": base.path.display().to_string(),
                    "path_native_hex": native_path_hex(&base.path),
                    "reference_count": base.reference_count,
                    // Never claim a base is unused on an incomplete inventory.
                    "in_use": base.reference_count > 0 || !counts_are_complete,
                    "reference_count_complete": counts_are_complete,
                    "logical_bytes": base.logical_bytes,
                    "allocated_bytes": base.allocated_bytes,
                })
            })
            .collect::<Vec<_>>();
        let worktrees = report
            .views
            .iter()
            .map(worktree_view_json)
            .collect::<Vec<_>>();
        let diagnostic_issues = report
            .diagnostic_issues
            .iter()
            .map(diagnostic_issue_json)
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "schema_version": 1,
            "state_directory": state_directory.display().to_string(),
            "state_directory_native_hex": native_path_hex(state_directory),
            "native_path_encoding": native_path_encoding(),
            // False when any journal could not be read: every count in this
            // document is then a lower bound, not the truth.
            "counts_complete": counts_are_complete,
            "operations": {
                "active_views": report.active_views,
                "pending_adds": report.pending_adds,
                "completed_removals": report.completed_removals,
                "pending_removals": report.pending_removals,
                "completed_moves": report.completed_moves,
                "pending_moves": report.pending_moves,
                "completed_compactions": report.completed_compactions,
                "cancelled_compactions": report.cancelled_compactions,
                "pending_compactions": report.pending_compactions,
                "completed_prunes": report.completed_prunes,
                "pending_prunes": report.pending_prunes,
                "completed_collections": report.completed_collections,
                "cancelled_collections": report.cancelled_collections,
                "pending_collections": report.pending_collections,
                "coordination_locks": report.coordination_locks,
            },
            "bases": bases,
            "worktrees": worktrees,
            "diagnostic_issues": diagnostic_issues,
            "total_logical_bytes": report.total_logical_bytes,
            "total_allocated_bytes": report.total_allocated_bytes,
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize storage status")?
        );
        return Ok(());
    }

    // `riftri repair` defaults to the state directory of the current
    // repository, which is not necessarily the one reported here, so name
    // this one in every hint.
    let repair = repair_hint(state_directory);
    outputln!("Riftri storage status");
    outputln!("State: {}", state_directory.display());
    outputln!("Active views: {}", report.active_views);
    outputln!("Pending adds: {}", report.pending_adds);
    if report.pending_adds > 0 {
        outputln!("Attention: {repair} to roll back pending adds");
    }
    outputln!("Completed removals: {}", report.completed_removals);
    outputln!("Pending removals: {}", report.pending_removals);
    if report.pending_removals > 0 {
        outputln!("Attention: {repair} to resume pending removals");
    }
    outputln!("Completed moves: {}", report.completed_moves);
    outputln!("Pending moves: {}", report.pending_moves);
    if report.pending_moves > 0 {
        outputln!("Attention: {repair} to resume pending moves");
    }
    outputln!("Completed compactions: {}", report.completed_compactions);
    outputln!("Cancelled compactions: {}", report.cancelled_compactions);
    outputln!("Pending compactions: {}", report.pending_compactions);
    if report.pending_compactions > 0 {
        outputln!("Attention: {repair} to resume pending compactions");
    }
    outputln!("Completed prunes: {}", report.completed_prunes);
    outputln!("Pending prunes: {}", report.pending_prunes);
    if report.pending_prunes > 0 {
        outputln!("Attention: {repair} to resume pending prunes");
    }
    outputln!("Completed collections: {}", report.completed_collections);
    outputln!("Cancelled collections: {}", report.cancelled_collections);
    outputln!("Pending collections: {}", report.pending_collections);
    if report.pending_collections > 0 {
        outputln!("Attention: {repair} to resume pending collections");
    }
    outputln!(
        "Coordination locks: {} (safe persistent metadata)",
        report.coordination_locks
    );
    outputln!("Retained bases: {}", report.bases.len());
    for base in &report.bases {
        // Reference counts are derived only from journals that parsed. When
        // some did not, a zero is "none that could be counted", not "none" —
        // and a base a live worktree still uses must never be described as an
        // unreferenced cache.
        let state = if base.reference_count > 0 {
            "in use"
        } else if counts_are_complete {
            "retained cache; no active views"
        } else {
            "reference count unknown; unreadable journals may still claim it"
        };
        outputln!(
            "- {}: refs={}, logical={}, filesystem-accounted allocated={}, state={}",
            base.path.display(),
            base.reference_count,
            display_byte_count(base.logical_bytes),
            display_byte_count(base.allocated_bytes),
            state
        );
    }
    outputln!("Active view storage:");
    for view in &report.views {
        outputln!(
            "- {}: backend={}, logical={}, filesystem-accounted allocated={}, base={}",
            view.destination.display(),
            view.backend.display_name(),
            display_byte_count(view.logical_bytes),
            display_byte_count(view.allocated_bytes),
            view.base_path.display()
        );
    }
    outputln!("State issues: {}", report.diagnostic_issues.len());
    for issue in &report.diagnostic_issues {
        outputln!("- {}: {}", issue.path.display(), issue.reason);
    }
    if !report.diagnostic_issues.is_empty() {
        outputln!(
            "Attention: Riftri preserves unexplained state; inspect it before manual cleanup"
        );
    }
    outputln!(
        "Total logical: {}",
        display_byte_count(report.total_logical_bytes)
    );
    outputln!(
        "Total filesystem-accounted allocated: {}",
        display_byte_count(report.total_allocated_bytes)
    );
    print_allocation_note();
    Ok(())
}

fn print_garbage_collection_report(
    state_directory: &Path,
    report: &riftri_core::GarbageCollectionReport,
    json: bool,
) -> Result<()> {
    if json {
        let candidates = report
            .candidates
            .iter()
            .map(|candidate| {
                serde_json::json!({
                    "base_path": candidate.base_path.display().to_string(),
                    "base_path_native_hex": native_path_hex(&candidate.base_path),
                    "logical_bytes": candidate.logical_bytes,
                    "allocated_bytes": candidate.allocated_bytes,
                })
            })
            .collect::<Vec<_>>();
        let collected = report
            .collected
            .iter()
            .map(|path| {
                serde_json::json!({
                    "path": path.display().to_string(),
                    "path_native_hex": native_path_hex(path),
                })
            })
            .collect::<Vec<_>>();
        let skipped_in_use = report
            .skipped_in_use
            .iter()
            .map(|path| {
                serde_json::json!({
                    "path": path.display().to_string(),
                    "path_native_hex": native_path_hex(path),
                })
            })
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "schema_version": 1,
            "state_directory": state_directory.display().to_string(),
            "state_directory_native_hex": native_path_hex(state_directory),
            "native_path_encoding": native_path_encoding(),
            "applied": report.applied,
            "candidates": candidates,
            "collected": collected,
            "skipped_in_use": skipped_in_use,
            "skipped_protected": report
                .skipped_protected
                .iter()
                .map(|protection| {
                    serde_json::json!({
                        "base_path": protection.base_path.display().to_string(),
                        "base_path_native_hex": native_path_hex(&protection.base_path),
                        "operation_id": protection.operation_id,
                        "reason": protection.reason,
                    })
                })
                .collect::<Vec<_>>(),
            "resumed_collections": report.resumed_collections,
            "removed_logical_bytes": report.removed_logical_bytes,
            "removed_allocated_bytes": report.removed_allocated_bytes,
        });
        machineln!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize collection report")?
        );
        return Ok(());
    }

    outputln!("Riftri garbage collection");
    outputln!("State: {}", state_directory.display());
    outputln!(
        "Mode: {}",
        if report.applied {
            "applied"
        } else {
            "plan only"
        }
    );
    outputln!("Eligible bases: {}", report.candidates.len());
    for candidate in &report.candidates {
        outputln!(
            "- {}: logical={}, filesystem-accounted allocated={}",
            candidate.base_path.display(),
            display_byte_count(candidate.logical_bytes),
            display_byte_count(candidate.allocated_bytes)
        );
    }
    outputln!("Collected bases: {}", report.collected.len());
    outputln!("Resumed prior collections: {}", report.resumed_collections);
    outputln!(
        "Skipped because now in use: {}",
        report.skipped_in_use.len()
    );
    outputln!(
        "Skipped because a journaled operation still claims them: {}",
        report.skipped_protected.len()
    );
    for protection in &report.skipped_protected {
        outputln!(
            "- {}: {} (operation {})",
            protection.base_path.display(),
            protection.reason,
            protection.operation_id
        );
    }
    outputln!(
        "Removed logical: {}",
        display_byte_count(report.removed_logical_bytes)
    );
    outputln!(
        "Removed filesystem-accounted allocated: {}",
        display_byte_count(report.removed_allocated_bytes)
    );
    print_allocation_note();
    if !report.applied && !report.candidates.is_empty() {
        let apply = match riftri_core::shell_quoted_path(state_directory) {
            Some(quoted) => format!("`riftri gc --apply --state-dir {quoted}`"),
            None => "riftri gc --apply against the state directory shown above".to_owned(),
        };
        outputln!("Nothing was deleted; rerun with {apply} to collect this plan");
    }
    Ok(())
}

/// Phrase naming the `riftri repair` invocation for one state directory, for
/// use inside a human-readable sentence.
fn repair_hint(state_directory: &Path) -> String {
    match riftri_core::repair_command(state_directory) {
        Some(command) => format!("run `{command}`"),
        None => "run riftri repair against the state directory shown above".to_owned(),
    }
}

fn print_allocation_note() {
    outputln!(
        "Allocation note: filesystem-accounted allocation may count shared COW blocks more than once; it is not exclusive physical disk use"
    );
    outputln!("Physical-sharing proof: use the platform volume-delta benchmark on a quiet volume");
}

fn doctor_json(report: &riftri_core::DoctorReport) -> serde_json::Value {
    let git = match &report.git.value {
        Some(git) => serde_json::json!({
            "available": report.git.available,
            "value": {
                "command": git.command.display().to_string(),
                "command_native_hex": native_path_hex(&git.command),
                "version": git.version,
            },
        }),
        None => serde_json::json!(report.git),
    };
    let repository = match &report.repository.value {
        Some(repository) => serde_json::json!({
            "available": report.repository.available,
            "value": {
                "root": repository.root.as_deref().map(|path| path.display().to_string()),
                "root_native_hex": repository.root.as_deref().map(native_path_hex),
                "identity": {
                    "common_git_dir": repository.identity.common_git_dir.display().to_string(),
                    "common_git_dir_native_hex": native_path_hex(&repository.identity.common_git_dir),
                },
                "is_bare": repository.is_bare,
                "head_commit": repository.head_commit,
                "head_tree": repository.head_tree,
                "clean": repository.clean,
            },
        }),
        None => serde_json::json!(report.repository),
    };
    let readiness = &report.destination_readiness;
    // `backend` and `next_command` are always present, explicitly null when
    // no backend is selected or no command applies, matching every sibling
    // report; strictly-typed consumers must never lose a key on the blocked
    // path that the ready path carries.
    let destination_readiness = serde_json::json!({
        "destination": readiness.destination.display().to_string(),
        "destination_native_hex": native_path_hex(&readiness.destination),
        "status": readiness.status,
        "backend": readiness.backend,
        "copy_on_write": readiness.copy_on_write,
        "overlayfs_helper": readiness.overlayfs_helper,
        "blockers": readiness.blockers,
        "next_command": readiness.next_command,
    });
    let storage_capabilities = report
        .storage_capabilities
        .iter()
        .map(backend_capability_json)
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": 1,
        "native_path_encoding": native_path_encoding(),
        "project_stage": report.project_stage,
        "operating_system": report.operating_system,
        "architecture": report.architecture,
        "cow_backend_active": report.cow_backend_active,
        "repository_enabled": report.repository_enabled,
        "git_shim_active": report.git_shim_active,
        "git": git,
        "repository": repository,
        "repository_compatibility": report.repository_compatibility,
        "destination_readiness": destination_readiness,
        "storage_capabilities": storage_capabilities,
    })
}

fn backends_json(
    requested_path: &Path,
    capabilities: &[riftri_storage::BackendCapability],
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "native_path_encoding": native_path_encoding(),
        "requested_path": requested_path.display().to_string(),
        "requested_path_native_hex": native_path_hex(requested_path),
        "storage_capabilities": capabilities
            .iter()
            .map(backend_capability_json)
            .collect::<Vec<_>>(),
    })
}

/// Shared native-path-aware JSON shape for one storage capability, so the
/// `backends` and `doctor` reports describe capabilities identically.
fn backend_capability_json(capability: &riftri_storage::BackendCapability) -> serde_json::Value {
    let mut output = serde_json::json!({
        "kind": capability.kind,
        "status": capability.status,
        "explanation": capability.explanation,
        "requires_explicit_fallback": capability.requires_explicit_fallback,
    });
    if let Some(volume) = &capability.volume {
        output["volume"] = serde_json::json!({
            "requested_path": volume.requested_path.display().to_string(),
            "requested_path_native_hex": native_path_hex(&volume.requested_path),
            "probe_path": volume.probe_path.display().to_string(),
            "probe_path_native_hex": native_path_hex(&volume.probe_path),
            "identity": volume.identity,
            "read_only": volume.read_only,
        });
    }
    output
}

fn print_doctor(report: &riftri_core::DoctorReport) {
    outputln!("Riftri doctor");
    outputln!(
        "Destination readiness: {}",
        match report.destination_readiness.status {
            riftri_core::DestinationReadinessStatus::Ready => "ready",
            riftri_core::DestinationReadinessStatus::NeedsActivation => "needs activation",
            riftri_core::DestinationReadinessStatus::Blocked => "blocked",
        }
    );
    outputln!(
        "Destination: {}",
        report.destination_readiness.destination.display()
    );
    outputln!(
        "Selected backend: {}",
        report
            .destination_readiness
            .backend
            .map_or("none", |backend| backend.display_name())
    );
    outputln!(
        "Copy-on-write: {}",
        if report.destination_readiness.copy_on_write {
            "verified"
        } else {
            "unavailable"
        }
    );
    outputln!(
        "OverlayFS helper: {}",
        match report.destination_readiness.overlayfs_helper {
            riftri_core::OverlayFsHelperReadiness::NotApplicable => "not applicable",
            riftri_core::OverlayFsHelperReadiness::NotRequired => "not required",
            riftri_core::OverlayFsHelperReadiness::Ready => "required and verified",
            riftri_core::OverlayFsHelperReadiness::Unavailable => "required but unavailable",
        }
    );
    if report.destination_readiness.blockers.is_empty() {
        outputln!("Readiness blockers: none");
    } else {
        outputln!("Readiness blockers:");
        for blocker in &report.destination_readiness.blockers {
            outputln!("- {}: {}", blocker.kind, blocker.explanation);
            outputln!("  Fix: {}", blocker.remedy);
        }
    }
    if let Some(command) = &report.destination_readiness.next_command {
        outputln!("Next command: {command}");
    }
    outputln!();
    outputln!("Project stage: {}", report.project_stage);
    outputln!(
        "Platform: {} / {}",
        report.operating_system,
        report.architecture
    );
    outputln!(
        "Repository enabled: {}",
        report
            .repository_enabled
            .map_or("unknown", |enabled| if enabled { "yes" } else { "no" })
    );
    outputln!(
        "Process-scoped Git interception: {}",
        if report.git_shim_active {
            "active"
        } else {
            "inactive"
        }
    );

    match &report.git.value {
        Some(git) => outputln!("Git: {} ({})", git.version, git.command.display()),
        None => outputln!(
            "Git: unavailable ({})",
            report.git.error.as_deref().unwrap_or("unknown error")
        ),
    }

    match &report.repository.value {
        Some(repository) => {
            match &repository.root {
                Some(root) => outputln!("Repository: {}", root.display()),
                None => outputln!("Repository: bare"),
            }
            outputln!(
                "Common Git directory: {}",
                repository.identity.common_git_dir.display()
            );
            outputln!(
                "HEAD: {}",
                repository
                    .head_commit
                    .as_ref()
                    .map_or("unborn", |object_id| object_id.as_str())
            );
            match repository.clean {
                Some(clean) => outputln!("Working tree clean: {clean}"),
                None => outputln!("Working tree clean: not applicable (bare repository)"),
            }
        }
        None => outputln!(
            "Repository: unavailable ({})",
            report
                .repository
                .error
                .as_deref()
                .unwrap_or("unknown error")
        ),
    }

    match &report.repository_compatibility.value {
        Some(compatibility) => {
            outputln!(
                "Repository checkout compatibility (HEAD): {}",
                if compatibility.compatible {
                    "supported"
                } else {
                    "blocked"
                }
            );
            for blocker in &compatibility.blockers {
                outputln!("- {}: {}", blocker.kind.as_str(), blocker.explanation);
            }
        }
        None => outputln!(
            "Repository checkout compatibility (HEAD): unavailable ({})",
            report
                .repository_compatibility
                .error
                .as_deref()
                .unwrap_or("unknown error")
        ),
    }

    outputln!("Destination storage capabilities:");
    for backend in &report.storage_capabilities {
        outputln!(
            "- {} ({}): {}",
            backend.kind.display_name(),
            backend.status.display_name(),
            backend.explanation
        );
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::{
        Cli, Command, InvocationContext, OverlayFsCommand, ShellCommand, ShellKind,
        WorktreeCommand, failure_receipt,
    };
    use super::{PathBuf, native_path_encoding, native_path_hex};

    /// Receipt for an invocation that selected no repository and no state
    /// directory, matching the historical two-argument helper.
    fn receipt(operation: &'static str, error: &anyhow::Error) -> serde_json::Value {
        failure_receipt(operation, error, &InvocationContext::default())
    }

    /// The context clap would produce for the given command line.
    fn context_for(arguments: &[&str]) -> InvocationContext {
        Cli::try_parse_from(arguments)
            .expect("parse command line")
            .command
            .invocation_context()
    }

    #[test]
    fn help_uses_the_public_product_description() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("Lightweight Git workspaces for parallel development"));
        assert!(!help.contains("Copy-on-write acceleration"));
    }

    #[test]
    fn parses_machine_readable_failure_output_globally() {
        let cli = Cli::try_parse_from([
            "riftri",
            "worktree",
            "add",
            "../app-auth",
            "-b",
            "feature/auth",
            "--json-errors",
        ])
        .expect("parse global JSON failure flag after subcommands");

        assert!(cli.json_errors);
        assert_eq!(cli.command.operation_name(), "worktree-add");
    }

    #[test]
    fn parses_stable_json_flags_on_lifecycle_commands() {
        let status = Cli::try_parse_from(["riftri", "status", "--json"])
            .expect("parse status with JSON output");
        let Command::Status { json, .. } = status.command else {
            panic!("unexpected status command");
        };
        assert!(json);

        let repair = Cli::try_parse_from(["riftri", "repair", "--json"])
            .expect("parse repair with JSON output");
        let Command::Repair { json, .. } = repair.command else {
            panic!("unexpected repair command");
        };
        assert!(json);

        let gc = Cli::try_parse_from(["riftri", "gc", "--apply", "--json"])
            .expect("parse gc with JSON output");
        let Command::Gc { apply, json, .. } = gc.command else {
            panic!("unexpected gc command");
        };
        assert!(apply);
        assert!(json);

        for arguments in [
            &[
                "riftri", "worktree", "add", "../view", "-b", "topic", "--json",
            ][..],
            &["riftri", "worktree", "remove", "../view", "--json"][..],
            &[
                "riftri", "worktree", "move", "../view", "../moved", "--json",
            ][..],
            &["riftri", "worktree", "compact", "../view", "--json"][..],
            &["riftri", "worktree", "prune", "--json"][..],
        ] {
            let cli = Cli::try_parse_from(arguments).expect("parse worktree command with JSON");
            let Command::Worktree { command } = cli.command else {
                panic!("unexpected worktree command");
            };
            let json = match command {
                WorktreeCommand::Add { json, .. }
                | WorktreeCommand::Remove { json, .. }
                | WorktreeCommand::Move { json, .. }
                | WorktreeCommand::Compact { json, .. }
                | WorktreeCommand::Prune { json, .. }
                | WorktreeCommand::List { json, .. } => json,
            };
            assert!(json, "JSON flag not parsed for {arguments:?}");
        }
    }

    #[test]
    fn parses_the_branch_long_flag_as_an_alias_for_b() {
        let cli = Cli::try_parse_from([
            "riftri",
            "worktree",
            "add",
            "../view",
            "--branch",
            "feature/topic",
        ])
        .expect("parse worktree add with --branch");
        let Command::Worktree {
            command: WorktreeCommand::Add { branch, .. },
        } = cli.command
        else {
            panic!("unexpected worktree command");
        };
        assert_eq!(branch.as_deref(), Some(OsStr::new("feature/topic")));
    }

    #[test]
    fn byte_counts_render_exact_values_with_binary_units() {
        use super::display_byte_count;

        assert_eq!(display_byte_count(0), "0 bytes");
        assert_eq!(display_byte_count(1023), "1023 bytes");
        assert_eq!(display_byte_count(1024), "1024 bytes (1.0 KiB)");
        assert_eq!(display_byte_count(1_572_864), "1572864 bytes (1.5 MiB)");
        assert_eq!(
            display_byte_count(214_748_364_800),
            "214748364800 bytes (200 GiB)"
        );
    }

    #[test]
    fn parses_shell_completion_generation() {
        let cli = Cli::try_parse_from(["riftri", "completions", "zsh"])
            .expect("parse completions command");
        assert_eq!(cli.command.operation_name(), "completions");

        Cli::try_parse_from(["riftri", "completions"])
            .expect_err("completions requires an explicit shell");
    }

    #[test]
    fn parses_man_page_generation() {
        let cli = Cli::try_parse_from(["riftri", "man", "../man"]).expect("parse man command");
        assert_eq!(cli.command.operation_name(), "man");
        let Command::Man { directory } = cli.command else {
            panic!("unexpected man command");
        };
        assert_eq!(directory, std::path::PathBuf::from("../man"));

        Cli::try_parse_from(["riftri", "man"])
            .expect_err("man requires an explicit output directory");
    }

    #[test]
    fn writes_one_man_page_per_command() {
        use clap::CommandFactory;

        let directory = tempfile::tempdir().expect("create temporary directory");
        clap_mangen::generate_to(Cli::command(), directory.path()).expect("generate man pages");

        for page in [
            "riftri.1",
            "riftri-completions.1",
            "riftri-worktree-add.1",
            "riftri-state-unregister.1",
        ] {
            assert!(
                directory.path().join(page).is_file(),
                "missing man page {page}"
            );
        }
        assert!(
            !directory
                .path()
                .join("riftri-state-forget-missing.1")
                .exists()
        );
    }

    #[test]
    fn failure_receipt_has_a_stable_policy_shape() {
        let error = anyhow::Error::new(riftri_core::WorktreeError::InvalidRequest(
            "destination already exists".to_owned(),
        ));
        let receipt = receipt("worktree-add", &error);

        assert_eq!(receipt["schemaVersion"], 1);
        assert_eq!(receipt["outcome"], "failed");
        assert_eq!(receipt["operation"], "worktree-add");
        assert_eq!(receipt["code"], "invalid-request");
        assert_eq!(receipt["category"], "policy");
        assert_eq!(receipt["cleanup"], "not-needed");
        assert_eq!(receipt["recovery"], "not-required");
        assert!(receipt["phase"].is_null());
        assert!(receipt["nextCommand"].is_null());
        assert_eq!(receipt["nativePathEncoding"], native_path_encoding());
    }

    /// The repair command a receipt advertises is a shell string. It must be
    /// quoted, or a state directory whose path contains a space becomes two
    /// arguments and repair silently inspects a different directory.
    #[test]
    fn pending_recovery_receipts_require_repair_with_the_quoted_state_directory() {
        for relative in [
            "riftri-state",
            "My Projects/app/.git/riftri",
            "it's a state/riftri",
            "quotes 'and' spaces/riftri",
        ] {
            let state_directory = riftri_core::command_path(Path::new(relative));
            let error = anyhow::Error::new(riftri_core::recovery_pending_error(
                "a move of /tmp/view is already pending".to_owned(),
                &state_directory,
            ));
            let receipt = receipt("worktree-move", &error);

            assert_eq!(receipt["code"], "recovery-pending");
            assert_eq!(receipt["category"], "operational");
            assert_eq!(receipt["cleanup"], "not-needed");
            assert_eq!(receipt["recovery"], "required");
            let next_command = receipt["nextCommand"].as_str().expect("next command");
            assert!(
                receipt["message"]
                    .as_str()
                    .expect("message")
                    .contains(next_command),
                "human guidance and nextCommand must agree"
            );
            // The exact directory stays available to automation that should
            // not have to parse a shell string at all.
            assert_eq!(
                receipt["stateDirectory"],
                state_directory.display().to_string()
            );
            assert_eq!(
                receipt["stateDirectoryNativeHex"],
                native_path_hex(&state_directory)
            );

            // The command carries the whole path as one shell word.
            let quoted = next_command
                .strip_prefix("riftri repair --state-dir ")
                .expect("next command names the state directory");
            assert_eq!(
                shell_split(quoted),
                vec![state_directory.clone()],
                "{next_command}"
            );
        }
    }

    /// The symlinked-base safety stop is a policy refusal whose own message
    /// directs the caller to `riftri status`, so its receipt must say
    /// `recovery: inspect` with that exact command — not `not-required` with
    /// a null `nextCommand` contradicting the message.
    #[test]
    fn symlinked_base_receipts_inspect_the_state_directory_the_error_names() {
        let state_directory = riftri_core::command_path(Path::new("My Projects/app/state"));
        let error = anyhow::Error::new(riftri_core::WorktreeError::SymlinkedBaseParent {
            message: "cleanup stopped: immutable-base path is a symbolic link".to_owned(),
            state_directory: state_directory.clone(),
        });
        // The caller selected a different state directory; the error's own
        // directory is where the redirected layout actually lives.
        let context = context_for(&["riftri", "gc", "--state-dir", "elsewhere/state"]);
        let receipt = failure_receipt("garbage-collection", &error, &context);

        assert_eq!(receipt["code"], "invalid-request");
        assert_eq!(receipt["category"], "policy");
        assert_eq!(receipt["cleanup"], "not-needed");
        assert_eq!(receipt["recovery"], "inspect");
        let next_command = receipt["nextCommand"].as_str().expect("next command");
        assert_eq!(
            next_command,
            riftri_core::status_command(&state_directory)
                .expect("the fixture path is representable")
        );
        let quoted = next_command
            .strip_prefix("riftri status --state-dir ")
            .expect("next command names the state directory");
        assert_eq!(
            shell_split(quoted),
            vec![state_directory.clone()],
            "{next_command}"
        );
        assert_eq!(
            receipt["stateDirectory"],
            state_directory.display().to_string()
        );
        assert_eq!(
            receipt["stateDirectoryNativeHex"],
            native_path_hex(&state_directory)
        );
        assert_eq!(super::failure_exit_code(&error), 3);
    }

    /// A path Riftri cannot write as a shell argument must produce no command
    /// at all: a lossy rendering would name a directory that does not exist,
    /// and repair reports an all-clear for any directory it cannot find.
    #[cfg(unix)]
    #[test]
    fn non_utf8_state_directories_produce_no_command_but_keep_exact_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let state_directory =
            std::path::PathBuf::from(OsString::from_vec(b"/tmp/riftri-state-\xff".to_vec()));
        let error = anyhow::Error::new(riftri_core::recovery_pending_error(
            "a move of /tmp/view is already pending".to_owned(),
            &state_directory,
        ));
        let receipt = receipt("worktree-move", &error);

        assert_eq!(receipt["recovery"], "required");
        assert!(
            receipt["nextCommand"].is_null(),
            "a non-UTF-8 path must never produce a runnable-looking command"
        );
        assert_eq!(
            receipt["stateDirectoryNativeHex"],
            native_path_hex(&state_directory)
        );
        assert_eq!(receipt["nativePathEncoding"], native_path_encoding());
        let message = receipt["message"].as_str().expect("message");
        assert!(message.contains("riftri repair"), "{message}");
        assert!(!message.contains("--state-dir '"), "{message}");
    }

    /// Without a pending journal to name a directory, the receipt still has to
    /// repeat whatever the failing invocation selected, or the suggestion
    /// inspects the caller's default state instead.
    #[test]
    fn generic_receipts_repeat_the_selected_state_directory() {
        let selected = "My Projects/app/state";
        let expected = riftri_core::command_path(Path::new(selected));
        // Driven through `gc` rather than `repair`: a failing `repair` is the
        // one operation that must not answer with `riftri repair`, which
        // `repair_failures_do_not_advise_running_repair_again` covers.
        let context = context_for(&["riftri", "gc", "--state-dir", selected]);
        let error = anyhow::Error::new(riftri_core::WorktreeError::JournalTransition(
            riftri_core::JournalTransitionError {
                current: riftri_core::AddWorktreePhase::IntentRecorded,
                requested: riftri_core::AddWorktreePhase::Active,
            },
        ));
        let receipt = failure_receipt("garbage-collection", &error, &context);

        assert_eq!(receipt["recovery"], "required");
        let next_command = receipt["nextCommand"].as_str().expect("next command");
        let quoted = next_command
            .strip_prefix("riftri repair --state-dir ")
            .expect("next command names the selected state directory");
        assert_eq!(
            shell_split(quoted),
            vec![expected.clone()],
            "{next_command}"
        );
        assert_eq!(receipt["stateDirectory"], expected.display().to_string());
    }

    /// `repair` is the standard answer to `recovery: "required"`, but when
    /// `repair` is what failed that advice is a retry loop for any caller that
    /// follows `nextCommand`. It must point at `status`, which isolates and
    /// names the state needing attention instead of failing the same way.
    #[test]
    fn repair_failures_do_not_advise_running_repair_again() {
        let selected = "My Projects/app/state";
        let expected = riftri_core::command_path(Path::new(selected));
        let context = context_for(&["riftri", "repair", "--state-dir", selected]);
        let error = anyhow::Error::new(riftri_core::WorktreeError::JournalTransition(
            riftri_core::JournalTransitionError {
                current: riftri_core::AddWorktreePhase::IntentRecorded,
                requested: riftri_core::AddWorktreePhase::Active,
            },
        ));

        let receipt = failure_receipt("repair", &error, &context);
        assert_eq!(receipt["recovery"], "inspect");
        let next_command = receipt["nextCommand"].as_str().expect("next command");
        assert!(
            !next_command.starts_with("riftri repair"),
            "a failed repair must not advise repair: {next_command}"
        );
        let quoted = next_command
            .strip_prefix("riftri status --state-dir ")
            .expect("next command inspects the selected state directory");
        assert_eq!(shell_split(quoted), vec![expected], "{next_command}");

        // Every other operation still routes to repair.
        let receipt = failure_receipt("garbage-collection", &error, &context);
        assert_eq!(receipt["recovery"], "required");
        assert!(
            receipt["nextCommand"]
                .as_str()
                .expect("next command")
                .starts_with("riftri repair"),
            "{receipt:?}"
        );
    }

    /// A repository chosen with `--repository` resolves a different default
    /// state directory than the caller's working directory does, so the
    /// receipt has to name it.
    #[test]
    fn generic_receipts_repeat_an_explicit_repository() {
        let selected = "repos/other app";
        let expected = riftri_core::command_path(Path::new(selected));
        let context = context_for(&[
            "riftri",
            "worktree",
            "add",
            "../view",
            "--detach",
            "HEAD",
            "--repository",
            selected,
        ]);
        let error = anyhow::anyhow!("git failed");
        let receipt = failure_receipt("worktree-add", &error, &context);

        assert_eq!(receipt["recovery"], "inspect");
        let next_command = receipt["nextCommand"].as_str().expect("next command");
        let quoted = next_command
            .strip_prefix("riftri status --repository ")
            .expect("next command names the selected repository");
        assert_eq!(
            shell_split(quoted),
            vec![expected.clone()],
            "{next_command}"
        );
        assert_eq!(receipt["repository"], expected.display().to_string());
        assert!(receipt["stateDirectory"].is_null());
    }

    /// Relative selections are made absolute so the suggested command targets
    /// the same directory wherever the caller runs it.
    #[test]
    fn suggested_commands_name_absolute_paths() {
        let context = context_for(&["riftri", "repair", "--state-dir", "relative-state"]);
        let error = anyhow::anyhow!("something operational");
        let receipt = failure_receipt("repair", &error, &context);

        let next_command = receipt["nextCommand"].as_str().expect("next command");
        let quoted = next_command
            .strip_prefix("riftri status --state-dir ")
            .expect("next command names the state directory");
        assert_eq!(
            shell_split(quoted),
            vec![riftri_core::command_path(Path::new("relative-state"))]
        );
        assert!(
            Path::new(receipt["stateDirectory"].as_str().expect("state directory")).is_absolute()
        );
    }

    /// An invocation that named neither a repository nor a state directory
    /// keeps the historical bare suggestion.
    #[test]
    fn receipts_without_context_keep_the_bare_command() {
        let error = anyhow::anyhow!("something operational");
        let receipt = receipt("worktree-add", &error);

        assert_eq!(receipt["nextCommand"], "riftri status");
        assert!(receipt["repository"].is_null());
        assert!(receipt["stateDirectory"].is_null());
    }

    /// A `--state-dir` the caller named explicitly must exist. Reporting an
    /// all-clear for a directory Riftri never found is how a mistyped or
    /// shell-split path turns into a lost pending journal.
    #[test]
    fn an_explicitly_named_missing_state_directory_is_refused() {
        use super::resolve_state_directory;

        let fixture = tempfile::tempdir().expect("fixture directory");
        let missing = fixture.path().join("absent-state");
        let error = resolve_state_directory(fixture.path(), Some(missing.clone()))
            .expect_err("a missing explicit state directory is refused");

        assert_eq!(super::failure_exit_code(&error), 3);
        let message = error.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
        assert!(message.contains("does not exist"), "{message}");
        assert!(message.contains("not an all-clear"), "{message}");

        // An existing directory still resolves to itself, untouched.
        let present = fixture.path().join("present-state");
        std::fs::create_dir(&present).expect("create state directory");
        assert_eq!(
            resolve_state_directory(fixture.path(), Some(present.clone())).expect("resolve"),
            present
        );
    }

    /// A blocked destination must keep the exact keys a ready destination
    /// carries: `backend` and `next_command` are explicitly null when absent,
    /// never dropped, so strictly-typed consumers that parsed a ready report
    /// do not fail on a blocked one.
    #[test]
    fn doctor_json_emits_explicit_nulls_for_a_blocked_destination() {
        let report = riftri_core::DoctorReport {
            project_stage: "native-cow-with-repository-activation",
            operating_system: "test-os",
            architecture: "test-arch",
            cow_backend_active: false,
            repository_enabled: None,
            git_shim_active: false,
            git: riftri_core::Diagnostic {
                available: false,
                value: None,
                error: Some("git executable not found".to_owned()),
            },
            repository: riftri_core::Diagnostic {
                available: false,
                value: None,
                error: Some("not a Git repository".to_owned()),
            },
            repository_compatibility: riftri_core::Diagnostic {
                available: false,
                value: None,
                error: Some("repository inspection did not succeed".to_owned()),
            },
            destination_readiness: riftri_core::DestinationReadiness {
                destination: PathBuf::from("blocked-destination"),
                status: riftri_core::DestinationReadinessStatus::Blocked,
                backend: None,
                copy_on_write: false,
                overlayfs_helper: riftri_core::OverlayFsHelperReadiness::NotApplicable,
                blockers: Vec::new(),
                next_command: None,
            },
            storage_capabilities: Vec::new(),
        };

        let output = super::doctor_json(&report);

        let readiness = output["destination_readiness"]
            .as_object()
            .expect("destination_readiness object");
        assert!(
            readiness.contains_key("backend"),
            "backend key must be present even when no backend is selected"
        );
        assert!(readiness["backend"].is_null());
        assert!(
            readiness.contains_key("next_command"),
            "next_command key must be present even when no command applies"
        );
        assert!(readiness["next_command"].is_null());
    }

    #[test]
    fn busy_receipts_ask_the_caller_to_retry_without_repair() {
        let error = anyhow::Error::new(riftri_core::WorktreeError::Busy {
            message: "worktree /tmp/view is busy with another Riftri operation; wait for it to finish and retry".to_owned(),
        });
        let receipt = receipt("worktree-compact", &error);

        assert_eq!(receipt["code"], "worktree-busy");
        assert_eq!(receipt["category"], "operational");
        assert_eq!(receipt["cleanup"], "not-needed");
        assert_eq!(receipt["recovery"], "retry");
        assert!(receipt["nextCommand"].is_null());
    }

    /// Minimal quote-aware argument reader for the host platform's shell,
    /// enough to prove that one quoted path survives as exactly one argument.
    ///
    /// POSIX shells splice a literal quote in as `'"'"'`; PowerShell doubles
    /// the quote inside the string. Both forms are read here.
    fn shell_split(command: &str) -> Vec<PathBuf> {
        let mut arguments = Vec::new();
        let mut current = String::new();
        let mut started = false;
        let mut single = false;
        let mut double = false;
        let mut characters = command.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\'' if double => current.push('\''),
                '\'' if single && cfg!(windows) && characters.peek() == Some(&'\'') => {
                    characters.next();
                    current.push('\'');
                }
                '\'' => {
                    single = !single;
                    started = true;
                }
                '"' if single => current.push('"'),
                '"' => {
                    double = !double;
                    started = true;
                }
                ' ' if !single && !double => {
                    if started {
                        arguments.push(PathBuf::from(std::mem::take(&mut current)));
                        started = false;
                    }
                }
                other => {
                    current.push(other);
                    started = true;
                }
            }
        }
        assert!(!single && !double, "unterminated quote in {command}");
        if started {
            arguments.push(PathBuf::from(current));
        }
        arguments
    }

    #[test]
    fn policy_failures_exit_with_a_distinct_code() {
        use super::failure_exit_code;

        let policy = anyhow::Error::new(riftri_core::WorktreeError::InvalidRequest(
            "destination already exists".to_owned(),
        ));
        assert_eq!(failure_exit_code(&policy), 3);

        let unsupported = anyhow::Error::new(riftri_core::WorktreeError::Unsupported(
            "sparse checkout".to_owned(),
        ));
        assert_eq!(failure_exit_code(&unsupported), 3);

        let operational = anyhow::anyhow!("disk on fire");
        assert_eq!(failure_exit_code(&operational), 1);

        // Pending recovery and busy states are operational: the caller can
        // proceed after repair or retry, unlike a policy refusal.
        let pending = anyhow::Error::new(riftri_core::WorktreeError::RecoveryPending {
            message: "a move is already pending".to_owned(),
            state_directory: std::path::PathBuf::from("/tmp/riftri-state"),
        });
        assert_eq!(failure_exit_code(&pending), 1);
        let busy = anyhow::Error::new(riftri_core::WorktreeError::Busy {
            message: "worktree is busy".to_owned(),
        });
        assert_eq!(failure_exit_code(&busy), 1);
    }

    #[test]
    fn confirmation_never_prompts_without_a_terminal() {
        // Test harnesses run without a TTY on stdin, so both the `--yes` and
        // the plain path must return without blocking on input.
        super::confirm_destructive_action("would delete things.", true).expect("--yes path");
        super::confirm_destructive_action("would delete things.", false).expect("non-tty path");
    }

    #[test]
    fn machine_commands_never_opt_into_terminal_presentation() {
        for arguments in [
            vec!["riftri", "doctor", "--json"],
            vec!["riftri", "backends", "--json"],
            vec!["riftri", "status", "--json"],
            vec!["riftri", "repair", "--json"],
            vec!["riftri", "gc", "--json"],
            vec!["riftri", "worktree", "list", "--json"],
            vec!["riftri", "worktree", "add", "view", "--detach", "--json"],
            vec!["riftri", "worktree", "remove", "view", "--json"],
            vec!["riftri", "worktree", "move", "old", "new", "--json"],
            vec!["riftri", "worktree", "compact", "view", "--json"],
            vec!["riftri", "worktree", "prune", "--json"],
            vec!["riftri", "shell", "hook", "zsh"],
            vec!["riftri", "shell", "deactivate", "powershell"],
            vec!["riftri", "completions", "bash"],
            vec!["riftri", "exec", "--", "agent"],
        ] {
            let cli = Cli::try_parse_from(&arguments).unwrap();
            assert!(cli.command.machine_output(), "{arguments:?}");
        }
        for command in ["setup", "doctor", "status"] {
            assert!(
                !Cli::try_parse_from(["riftri", command])
                    .unwrap()
                    .command
                    .machine_output()
            );
        }
    }

    #[test]
    fn skipping_confirmation_requires_the_destructive_flag() {
        assert!(Cli::try_parse_from(["riftri", "gc", "--yes"]).is_err());
        assert!(Cli::try_parse_from(["riftri", "gc", "--apply", "--yes"]).is_ok());
        assert!(Cli::try_parse_from(["riftri", "worktree", "remove", "w", "--yes"]).is_err());
        assert!(
            Cli::try_parse_from(["riftri", "worktree", "remove", "w", "--force", "--yes"]).is_ok()
        );
    }

    #[test]
    fn long_help_documents_examples_and_environment() {
        use clap::CommandFactory;

        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("Examples:"), "{help}");
        assert!(help.contains("RIFTRI_BYPASS"), "{help}");
    }

    #[test]
    fn parses_repository_activation_commands() {
        let enable = Cli::try_parse_from(["riftri", "enable", "../repository"])
            .expect("parse repository enable command");
        let Command::Enable { path, .. } = enable.command else {
            panic!("unexpected enable command");
        };
        assert_eq!(path, Path::new("../repository"));

        let disable =
            Cli::try_parse_from(["riftri", "disable"]).expect("parse repository disable command");
        let Command::Disable { path, .. } = disable.command else {
            panic!("unexpected disable command");
        };
        assert_eq!(path, Path::new("."));
    }

    #[cfg(unix)]
    #[test]
    fn repository_flag_parsing_preserves_native_path_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let path = OsStr::from_bytes(b"/repository-\xff");
        let cli = Cli::try_parse_from([
            OsStr::new("riftri"),
            OsStr::new("enable"),
            OsStr::new("--repository"),
            path,
        ])
        .expect("parse native path");
        let Command::Enable { repository, .. } = cli.command else {
            panic!("unexpected command")
        };
        assert_eq!(repository.unwrap().as_os_str().as_bytes(), path.as_bytes());
    }

    #[test]
    fn parses_process_scoped_activation_command() {
        let cli = Cli::try_parse_from(["riftri", "exec", "--", "agent", "--flag"])
            .expect("parse exec command");
        let Command::Exec { worktree, command } = cli.command else {
            panic!("unexpected exec command");
        };
        assert!(worktree.is_none());
        assert_eq!(command, [OsStr::new("agent"), OsStr::new("--flag")]);

        let bound = Cli::try_parse_from([
            "riftri",
            "exec",
            "--worktree",
            "../app-auth",
            "--",
            "agent",
            "--flag",
        ])
        .expect("parse bound exec command");
        let Command::Exec { worktree, command } = bound.command else {
            panic!("unexpected bound exec command");
        };
        assert_eq!(worktree.as_deref(), Some(Path::new("../app-auth")));
        assert_eq!(command, [OsStr::new("agent"), OsStr::new("--flag")]);
    }

    #[test]
    fn parses_shell_hook_activation_command() {
        let cli = Cli::try_parse_from(["riftri", "shell", "hook", "zsh"])
            .expect("parse shell hook command");
        let Command::Shell {
            command: ShellCommand::Hook { shell },
        } = cli.command
        else {
            panic!("unexpected shell command");
        };
        assert_eq!(shell, ShellKind::Zsh);

        let deactivate = Cli::try_parse_from(["riftri", "shell", "deactivate", "bash"])
            .expect("parse shell deactivation command");
        let Command::Shell {
            command: ShellCommand::Deactivate { shell },
        } = deactivate.command
        else {
            panic!("unexpected shell deactivation command");
        };
        assert_eq!(shell, ShellKind::Bash);

        let powershell = Cli::try_parse_from(["riftri", "shell", "hook", "powershell"])
            .expect("parse PowerShell hook command");
        let Command::Shell {
            command: ShellCommand::Hook { shell },
        } = powershell.command
        else {
            panic!("unexpected PowerShell command");
        };
        assert_eq!(shell, ShellKind::Powershell);

        let status = Cli::try_parse_from(["riftri", "shell", "status", "../app"])
            .expect("parse shell status command");
        let Command::Shell {
            command: ShellCommand::Status { repository, .. },
        } = status.command
        else {
            panic!("unexpected shell status command");
        };
        assert_eq!(repository, Path::new("../app"));
    }

    #[test]
    fn parses_overlayfs_helper_installation() {
        let cli = Cli::try_parse_from(["riftri", "overlayfs", "install-helper", "--replace"])
            .expect("parse OverlayFS helper installation");
        let Command::Overlayfs {
            command: OverlayFsCommand::InstallHelper { replace },
        } = cli.command
        else {
            panic!("unexpected OverlayFS command");
        };
        assert!(replace);
    }

    #[test]
    fn parses_worktree_removal_and_storage_status() {
        let list = Cli::try_parse_from([
            "riftri",
            "worktree",
            "list",
            "--repository",
            "../app",
            "--state-dir",
            "../state",
            "--json",
        ])
        .expect("parse managed worktree inventory");
        let Command::Worktree {
            command:
                WorktreeCommand::List {
                    repository,
                    state_dir,
                    all_states,
                    json,
                },
        } = list.command
        else {
            panic!("unexpected worktree list command");
        };
        assert_eq!(repository, Path::new("../app"));
        assert_eq!(state_dir.as_deref(), Some(Path::new("../state")));
        assert!(!all_states);
        assert!(json);

        let all_states = Cli::try_parse_from(["riftri", "worktree", "list", "--all-states"])
            .expect("parse all-states inventory");
        let Command::Worktree {
            command:
                WorktreeCommand::List {
                    all_states,
                    state_dir,
                    ..
                },
        } = all_states.command
        else {
            panic!("unexpected all-states list command");
        };
        assert!(all_states);
        assert!(state_dir.is_none());

        Cli::try_parse_from([
            "riftri",
            "worktree",
            "list",
            "--all-states",
            "--state-dir",
            "../state",
        ])
        .expect_err("--all-states conflicts with an explicit --state-dir");

        let remove = Cli::try_parse_from([
            "riftri",
            "worktree",
            "remove",
            "../app-auth",
            "--repository",
            "../app",
        ])
        .expect("parse worktree removal");
        let Command::Worktree {
            command:
                WorktreeCommand::Remove {
                    path,
                    repository,
                    state_dir,
                    force,
                    yes: _,
                    json,
                },
        } = remove.command
        else {
            panic!("unexpected removal command");
        };
        assert_eq!(path, Path::new("../app-auth"));
        assert_eq!(repository, Path::new("../app"));
        assert!(state_dir.is_none());
        assert!(!force);
        assert!(!json);

        let forced =
            Cli::try_parse_from(["riftri", "worktree", "remove", "--force", "../app-auth"])
                .expect("parse forced worktree removal");
        let Command::Worktree {
            command: WorktreeCommand::Remove { force, .. },
        } = forced.command
        else {
            panic!("unexpected forced removal command");
        };
        assert!(force);

        let compact = Cli::try_parse_from([
            "riftri",
            "worktree",
            "compact",
            "../app-auth",
            "--repository",
            "../app",
        ])
        .expect("parse worktree compaction");
        let Command::Worktree {
            command:
                WorktreeCommand::Compact {
                    path,
                    repository,
                    state_dir,
                    json,
                },
        } = compact.command
        else {
            panic!("unexpected compaction command");
        };
        assert_eq!(path, Path::new("../app-auth"));
        assert_eq!(repository, Path::new("../app"));
        assert!(state_dir.is_none());
        assert!(!json);

        let status =
            Cli::try_parse_from(["riftri", "status", "../app"]).expect("parse storage status");
        let Command::Status {
            repository,
            state_dir,
            json,
            ..
        } = status.command
        else {
            panic!("unexpected status command");
        };
        assert_eq!(repository, Path::new("../app"));
        assert!(state_dir.is_none());
        assert!(!json);

        let repair = Cli::try_parse_from(["riftri", "repair", "../app", "--state-dir", "../state"])
            .expect("parse lifecycle repair");
        let Command::Repair {
            repository,
            state_dir,
            json,
            ..
        } = repair.command
        else {
            panic!("unexpected repair command");
        };
        assert_eq!(repository, Path::new("../app"));
        assert_eq!(state_dir.as_deref(), Some(Path::new("../state")));
        assert!(!json);

        let gc = Cli::try_parse_from(["riftri", "gc", "../app", "--apply"])
            .expect("parse garbage collection");
        let Command::Gc {
            repository,
            state_dir,
            apply,
            yes: _,
            json,
            ..
        } = gc.command
        else {
            panic!("unexpected garbage-collection command");
        };
        assert_eq!(repository, Path::new("../app"));
        assert!(state_dir.is_none());
        assert!(apply);
        assert!(!json);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_path_encoding_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/worktree-\xff"));

        assert_eq!(native_path_encoding(), "unix-bytes-hex");
        assert_eq!(native_path_hex(path), "2f776f726b747265652dff");
    }

    #[test]
    fn parses_the_documented_explicit_worktree_command() {
        let cli = Cli::try_parse_from([
            "riftri",
            "worktree",
            "add",
            "../app-auth",
            "-b",
            "feature/auth",
            "main",
        ])
        .expect("parse explicit worktree command");

        let Command::Worktree {
            command:
                WorktreeCommand::Add {
                    path,
                    branch,
                    revision,
                    ..
                },
        } = cli.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(path, Path::new("../app-auth"));
        assert_eq!(branch.as_deref(), Some(OsStr::new("feature/auth")));
        assert_eq!(revision.as_deref(), Some(OsStr::new("main")));
    }

    #[test]
    fn parses_an_existing_branch_worktree_command() {
        let cli = Cli::try_parse_from(["riftri", "worktree", "add", "../app-auth", "feature/auth"])
            .expect("parse existing-branch worktree command");

        let Command::Worktree {
            command:
                WorktreeCommand::Add {
                    path,
                    branch,
                    detach,
                    revision,
                    ..
                },
        } = cli.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(path, Path::new("../app-auth"));
        assert!(branch.is_none());
        assert!(!detach);
        assert_eq!(revision.as_deref(), Some(OsStr::new("feature/auth")));
    }

    #[test]
    fn requires_an_explicit_head_mode() {
        let error = Cli::try_parse_from(["riftri", "worktree", "add", "../app-auth"])
            .expect_err("head mode must be explicit");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
