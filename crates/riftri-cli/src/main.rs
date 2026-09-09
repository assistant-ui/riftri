use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "riftri",
    version,
    about = "Copy-on-write acceleration for real Git worktrees"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enable optimized worktree creation for one repository.
    Enable {
        /// Repository to enable.
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Disable optimized worktree creation for one repository.
    Disable {
        /// Repository to disable.
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Run a command with process-scoped Git worktree interception.
    Exec {
        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Configure shell-scoped interception for normal Git commands.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },

    /// Inspect Git and show the planned storage path without changing anything.
    Doctor {
        /// Repository path to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,

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

    /// Create or recover Riftri-backed real Git worktrees.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },

    /// Roll back incomplete add operations recorded in a Riftri state directory.
    Recover {
        /// Riftri state directory containing operation journals.
        #[arg(long)]
        state_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// Create a real linked worktree using a native APFS COW clone.
    Add {
        /// New worktree directory.
        path: PathBuf,

        /// Create and check out a new branch.
        #[arg(
            short = 'b',
            value_name = "BRANCH",
            conflicts_with = "detach",
            required_unless_present = "detach"
        )]
        branch: Option<OsString>,

        /// Create a detached worktree instead of a branch.
        #[arg(long, conflicts_with = "branch")]
        detach: bool,

        /// Commit-ish to use for the new worktree.
        #[arg(default_value = "HEAD")]
        revision: OsString,

        /// Repository in which Git should create linked-worktree metadata.
        #[arg(long, default_value = ".")]
        repository: PathBuf,

        /// Riftri state directory; defaults to <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum ShellCommand {
    /// Print initialization code to evaluate in a shell.
    Hook {
        /// Bourne-compatible shell whose initialization code should be emitted.
        #[arg(value_enum)]
        shell: PosixShell,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PosixShell {
    Sh,
    Bash,
    Zsh,
}

fn main() -> Result<()> {
    if invoked_as_git_shim() {
        std::process::exit(run_git_shim()?);
    }

    let cli = Cli::parse();

    match cli.command {
        Command::Enable { path } => {
            let activation = riftri_core::enable_repository(&path)?;
            println!("Enabled Riftri for {}", activation.repository.display());
            println!("Git config: riftri.enabled=true");
            if env::var_os(riftri_core::SHIM_ACTIVE_ENV).is_some() {
                println!("Normal Git interception is active in this shell");
            } else {
                #[cfg(unix)]
                println!(
                    "Activate this shell with: eval \"$(riftri shell hook {})\"",
                    detected_posix_shell()
                );
                println!("Or activate one process with: riftri exec -- <command>");
            }
        }
        Command::Disable { path } => {
            let activation = riftri_core::disable_repository(&path)?;
            println!("Disabled Riftri for {}", activation.repository.display());
        }
        Command::Exec { command } => {
            std::process::exit(riftri_core::execute_scoped_command(&command)?);
        }
        Command::Shell {
            command: ShellCommand::Hook { shell: _ },
        } => {
            print!("{}", riftri_core::prepare_posix_shell_hook()?);
        }
        Command::Doctor {
            path,
            destination,
            json,
        } => {
            let destination = destination.as_deref().unwrap_or(&path);
            let report = riftri_core::doctor_for_destination(&path, destination);

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).context("serialize doctor report")?
                );
            } else {
                print_doctor(&report);
            }
        }
        Command::Backends { path, json } => {
            let backends = riftri_core::backends(&path);

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&backends).context("serialize backend report")?
                );
            } else {
                println!("Storage capabilities for {}:", path.display());
                for backend in backends {
                    println!(
                        "- {:?} ({:?}): {}",
                        backend.kind, backend.status, backend.explanation
                    );
                }
                println!("\nCapability support does not mean a backend is active yet.");
            }
        }
        Command::Worktree { command } => match command {
            WorktreeCommand::Add {
                path,
                branch,
                detach,
                revision,
                repository,
                state_dir,
            } => {
                let mode = match (branch, detach) {
                    (Some(branch), false) => riftri_core::WorktreeMode::NewBranch(branch),
                    (None, true) => riftri_core::WorktreeMode::Detached,
                    _ => unreachable!("Clap enforces exactly one worktree head mode"),
                };
                let result = riftri_core::add_worktree(riftri_core::AddWorktreeRequest {
                    repository,
                    destination: path,
                    revision,
                    mode,
                    state_dir,
                })?;
                print_add_result(&result);
            }
        },
        Command::Recover { state_dir } => {
            let report = riftri_core::recover_incomplete_operations(&state_dir)?;
            println!("Scanned operations: {}", report.scanned);
            println!("Active worktrees: {}", report.active);
            println!("Recovered operations: {}", report.recovered);
            if !report.errors.is_empty() {
                println!("Operations needing attention:");
                for error in &report.errors {
                    println!("- {error}");
                }
                anyhow::bail!(
                    "{} operation(s) need manual attention; no changed worktree was deleted",
                    report.errors.len()
                );
            }
        }
    }

    Ok(())
}

fn invoked_as_git_shim() -> bool {
    env::var_os(riftri_core::SHIM_ACTIVE_ENV).is_some()
        && env::args_os()
            .next()
            .as_deref()
            .and_then(|argument| Path::new(argument).file_name())
            .is_some_and(|name| name == OsStr::new("git") || name == OsStr::new("git.exe"))
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
    env::var_os(riftri_core::REAL_GIT_ENV)
        .filter(|path| !path.is_empty())
        .context("Riftri Git shim is missing the real Git executable path")?;
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let current_directory = env::current_dir().context("resolve Git working directory")?;

    match riftri_core::proxy_git_command(&current_directory, &arguments)? {
        riftri_core::GitProxyOutcome::Passthrough(status) => Ok(status),
        riftri_core::GitProxyOutcome::OptimizedAdd(result) => {
            eprintln!(
                "Riftri created an optimized APFS worktree at {} ({})",
                result.destination.display(),
                if result.reused_base {
                    "reused base"
                } else {
                    "new base"
                }
            );
            Ok(0)
        }
    }
}

fn print_add_result(result: &riftri_core::AddWorktreeResult) {
    println!("Created APFS-backed Git worktree");
    println!("Destination: {}", result.destination.display());
    println!("Commit: {}", result.commit.as_str());
    println!("Tree: {}", result.tree.as_str());
    println!("Immutable base: {}", result.base_path.display());
    println!(
        "Base: {}",
        if result.reused_base {
            "reused"
        } else {
            "created"
        }
    );
    println!("Journal: {}", result.journal_path.display());
}

fn print_doctor(report: &riftri_core::DoctorReport) {
    println!("Riftri doctor");
    println!("Project stage: {}", report.project_stage);
    println!(
        "Platform: {} / {}",
        report.operating_system, report.architecture
    );
    println!(
        "Repository enabled: {}",
        report
            .repository_enabled
            .map_or("unknown", |enabled| if enabled { "yes" } else { "no" })
    );
    println!(
        "Process-scoped Git interception: {}",
        if report.git_shim_active {
            "active"
        } else {
            "inactive"
        }
    );

    match &report.git.value {
        Some(git) => println!("Git: {} ({})", git.version, git.command.display()),
        None => println!(
            "Git: unavailable ({})",
            report.git.error.as_deref().unwrap_or("unknown error")
        ),
    }

    match &report.repository.value {
        Some(repository) => {
            match &repository.root {
                Some(root) => println!("Repository: {}", root.display()),
                None => println!("Repository: bare"),
            }
            println!(
                "Common Git directory: {}",
                repository.identity.common_git_dir.display()
            );
            println!(
                "HEAD: {}",
                repository
                    .head_commit
                    .as_ref()
                    .map_or("unborn", |object_id| object_id.as_str())
            );
            match repository.clean {
                Some(clean) => println!("Working tree clean: {clean}"),
                None => println!("Working tree clean: not applicable (bare repository)"),
            }
        }
        None => println!(
            "Repository: unavailable ({})",
            report
                .repository
                .error
                .as_deref()
                .unwrap_or("unknown error")
        ),
    }

    println!("Destination storage capabilities:");
    for backend in &report.storage_capabilities {
        println!(
            "- {:?} ({:?}): {}",
            backend.kind, backend.status, backend.explanation
        );
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use clap::Parser;
    use clap::error::ErrorKind;

    use super::{Cli, Command, PosixShell, ShellCommand, WorktreeCommand};

    #[test]
    fn parses_repository_activation_commands() {
        let enable = Cli::try_parse_from(["riftri", "enable", "../repository"])
            .expect("parse repository enable command");
        let Command::Enable { path } = enable.command else {
            panic!("unexpected enable command");
        };
        assert_eq!(path, Path::new("../repository"));

        let disable =
            Cli::try_parse_from(["riftri", "disable"]).expect("parse repository disable command");
        let Command::Disable { path } = disable.command else {
            panic!("unexpected disable command");
        };
        assert_eq!(path, Path::new("."));
    }

    #[test]
    fn parses_process_scoped_activation_command() {
        let cli = Cli::try_parse_from(["riftri", "exec", "--", "agent", "--flag"])
            .expect("parse exec command");
        let Command::Exec { command } = cli.command else {
            panic!("unexpected exec command");
        };
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
        assert_eq!(shell, PosixShell::Zsh);
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
        assert_eq!(revision, OsStr::new("main"));
    }

    #[test]
    fn requires_a_branch_or_detached_mode() {
        let error = Cli::try_parse_from(["riftri", "worktree", "add", "../app-auth"])
            .expect_err("head mode must be explicit");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
