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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
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
    }

    Ok(())
}

fn print_doctor(report: &riftri_core::DoctorReport) {
    println!("Riftri doctor");
    println!("Project stage: {}", report.project_stage);
    println!(
        "Platform: {} / {}",
        report.operating_system, report.architecture
    );
    println!("Copy-on-write backend active: no");

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
