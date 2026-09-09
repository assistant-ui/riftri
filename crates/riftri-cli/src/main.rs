use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Enable { path } => {
            let activation = riftri_core::enable_repository(&path)?;
            println!("Enabled Riftri for {}", activation.repository.display());
            println!("Git config: riftri.enabled=true");
            println!("Create an optimized worktree with: riftri worktree add <path> [options]");
        }
        Command::Disable { path } => {
            let activation = riftri_core::disable_repository(&path)?;
            println!("Disabled Riftri for {}", activation.repository.display());
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

    use super::{Cli, Command, WorktreeCommand};

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
