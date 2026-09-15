use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "riftri",
    version,
    about = "Lightweight Git workspaces for parallel development"
)]
struct Cli {
    /// Emit command failures as one machine-readable JSON receipt on stderr.
    #[arg(long, global = true)]
    json_errors: bool,

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

    /// Report retained bases, active views, reference counts, and disk use.
    Status {
        /// Repository whose default Riftri state should be inspected.
        #[arg(default_value = ".")]
        repository: PathBuf,

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

        /// Explicit Riftri state directory instead of <common-git-dir>/riftri.
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Apply the collection plan. Without this flag, nothing is deleted.
        #[arg(long)]
        apply: bool,

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
    fn operation_name(&self) -> &'static str {
        match self {
            Self::Enable { .. } => "enable",
            Self::Disable { .. } => "disable",
            Self::Exec { .. } => "exec",
            Self::Overlayfs { .. } => "overlayfs-helper-install",
            Self::Shell { .. } => "shell",
            Self::Completions { .. } => "completions",
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
    /// Forget an explicitly selected registration whose directory is missing.
    ForgetMissing {
        /// Missing state directory to forget.
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

    let cli = Cli::parse();
    let json_errors = cli.json_errors;
    let operation = cli.command.operation_name();

    match run(cli) {
        Ok(()) => Ok(()),
        Err(error) if json_errors => {
            eprintln!(
                "{}",
                serde_json::to_string(&failure_receipt(operation, &error))?
            );
            std::process::exit(1);
        }
        Err(error) => Err(error),
    }
}

fn run(cli: Cli) -> Result<()> {
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
                #[cfg(target_os = "windows")]
                println!(
                    "Activate this PowerShell session with: Invoke-Expression (& riftri shell hook powershell | Out-String)"
                );
                println!("Or activate one process with: riftri exec -- <command>");
            }
        }
        Command::Disable { path } => {
            let activation = riftri_core::disable_repository(&path)?;
            println!("Disabled Riftri for {}", activation.repository.display());
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
                println!(
                    "Installed Riftri OverlayFS helper at {}",
                    destination.display()
                );
                println!(
                    "The helper is available system-wide; `riftri enable` still opts in one repository at a time."
                );
            }
        },
        Command::Shell { command } => match command {
            ShellCommand::Hook { shell } => match shell {
                ShellKind::Sh | ShellKind::Bash | ShellKind::Zsh => {
                    print!("{}", riftri_core::prepare_posix_shell_hook()?);
                }
                ShellKind::Powershell => {
                    print!("{}", riftri_core::prepare_powershell_hook()?);
                }
            },
            ShellCommand::Deactivate { shell } => match shell {
                ShellKind::Sh | ShellKind::Bash | ShellKind::Zsh => {
                    print!("{}", riftri_core::prepare_posix_shell_deactivation()?);
                }
                ShellKind::Powershell => {
                    print!("{}", riftri_core::prepare_powershell_deactivation()?);
                }
            },
            ShellCommand::Status { repository } => print_shell_status(&repository)?,
        },
        Command::Completions { shell } => {
            use clap::CommandFactory;

            clap_complete::generate(shell, &mut Cli::command(), "riftri", &mut std::io::stdout());
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
                        "- {} ({}): {}",
                        backend.kind.display_name(),
                        backend.status.display_name(),
                        backend.explanation
                    );
                }
                println!("\nCapability support does not mean a backend is active yet.");
            }
        }
        Command::Status {
            repository,
            state_dir,
            json,
        } => {
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::storage_accounting(&state_directory)?;
            print_storage_accounting(&state_directory, &report, json)?;
        }
        Command::Repair {
            repository,
            state_dir,
            json,
        } => {
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::recover_incomplete_operations(&state_directory)?;
            print_recovery_report(&state_directory, &report, json)?;
        }
        Command::Gc {
            repository,
            state_dir,
            apply,
            json,
        } => {
            let state_directory = resolve_state_directory(&repository, state_dir)?;
            let report = riftri_core::garbage_collect(&state_directory, apply)?;
            print_garbage_collection_report(&state_directory, &report, json)?;
        }
        Command::State { command } => match command {
            StateCommand::ForgetMissing { path, repository } => {
                let forgotten = riftri_core::forget_missing_state_directory(&repository, &path)?;
                println!("Forgot missing Riftri state registration");
                println!("State: {}", forgotten.display());
            }
        },
        Command::Worktree { command } => match command {
            WorktreeCommand::List {
                repository,
                state_dir,
                json,
            } => {
                let state_directory = resolve_state_directory(&repository, state_dir)?;
                let report = riftri_core::storage_accounting(&state_directory)?;
                print_worktree_inventory(&state_directory, &report, json)?;
            }
            WorktreeCommand::Add {
                path,
                branch,
                detach,
                revision,
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
                })?;
                print_add_result(&result, json)?;
            }
            WorktreeCommand::Remove {
                path,
                repository,
                state_dir,
                force,
                json,
            } => {
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

fn failure_receipt(operation: &'static str, error: &anyhow::Error) -> serde_json::Value {
    let (code, category, phase, cleanup, recovery) = error
        .downcast_ref::<riftri_core::WorktreeError>()
        .map(worktree_failure_fields)
        .unwrap_or((
            "command-failed",
            "operational",
            None,
            "unknown",
            recovery_for_operation(operation),
        ));
    let next_command = match recovery {
        "required" => Some("riftri repair"),
        "inspect" => Some("riftri status"),
        _ => None,
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
    })
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
            println!("{}", serde_json::to_string(&identity)?);
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
            println!("{unmounted}");
        }
        ("reset-work", [layout_root, lower, merged]) => {
            riftri_storage::OverlayFsMounter::helper_reset_work(
                Path::new(layout_root),
                Path::new(lower),
                Path::new(merged),
                requester_uid,
                requester_gid,
            )?;
        }
        _ => anyhow::bail!("invalid internal OverlayFS helper request"),
    }
    Ok(())
}

fn resolve_state_directory(repository: &Path, state_directory: Option<PathBuf>) -> Result<PathBuf> {
    match state_directory {
        Some(state_directory) => Ok(state_directory),
        None => {
            let activation = riftri_core::repository_activation(repository)?;
            Ok(activation.common_git_dir.join("riftri"))
        }
    }
}

fn print_shell_status(repository: &Path) -> Result<()> {
    let shell = riftri_core::shell_activation_status()?;
    println!(
        "Shell interception: {}",
        if shell.active {
            "active"
        } else if shell.marker_set || shell.shim_first_on_path || shell.real_git.is_some() {
            "incomplete"
        } else {
            "inactive"
        }
    );
    println!("Shim directory: {}", shell.shim_directory.display());
    if let Some(real_git) = &shell.real_git {
        println!("Real Git: {}", real_git.display());
    }
    println!(
        "Global shell scope: {}",
        if shell.active {
            "this shell and its children; every new shell too only if you added the hook to your profile"
        } else {
            "not active in this shell"
        }
    );
    match riftri_core::repository_activation(repository) {
        Ok(activation) => {
            println!("Repository: {}", activation.repository.display());
            println!(
                "Repository optimization: {}",
                if activation.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!(
                "Effective optimized interception: {}",
                if shell.active && activation.enabled {
                    "active"
                } else {
                    "inactive"
                }
            );
        }
        Err(_) => {
            println!("Repository: none at {}", repository.display());
            println!("Repository optimization: not applicable");
            println!("Effective optimized interception: inactive");
        }
    }
    Ok(())
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
            "errors": report.errors,
        });
        println!(
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

    println!("Riftri repair");
    println!("State: {}", state_directory.display());
    println!("Scanned operations: {}", report.scanned);
    println!("Busy add operations skipped: {}", report.busy_adds);
    println!("Active worktrees: {}", report.active);
    println!("Recovered mounts: {}", report.recovered_mounts);
    println!("Recovered add operations: {}", report.recovered);
    println!("Completed removals: {}", report.completed_removals);
    println!("Recovered removals: {}", report.recovered_removals);
    println!("Completed moves: {}", report.completed_moves);
    println!("Recovered moves: {}", report.recovered_moves);
    println!("Completed compactions: {}", report.completed_compactions);
    println!("Recovered compactions: {}", report.recovered_compactions);
    println!("Completed prunes: {}", report.completed_prunes);
    println!("Recovered prunes: {}", report.recovered_prunes);
    println!("Completed collections: {}", report.completed_collections);
    println!("Recovered collections: {}", report.recovered_collections);
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
    println!("No journaled operation needs manual attention");
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
                "Riftri created an optimized {} worktree at {} ({})",
                result.backend.display_name(),
                result.destination.display(),
                if result.reused_base {
                    "reused base"
                } else {
                    "new base"
                }
            );
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
            "journal_path": result.journal_path.display().to_string(),
            "journal_path_native_hex": native_path_hex(&result.journal_path),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize add result")?
        );
        return Ok(());
    }

    println!(
        "Created {}-backed Git worktree",
        result.backend.display_name()
    );
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
    Ok(())
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize remove result")?
        );
        return Ok(());
    }

    if forced {
        println!("Force-removed Riftri-backed Git worktree after snapshot verification");
    } else {
        println!("Removed Riftri-backed Git worktree");
    }
    println!("Destination: {}", result.destination.display());
    println!("Retained immutable base: {}", result.base_path.display());
    println!("Journal: {}", result.journal_path.display());
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize move result")?
        );
        return Ok(());
    }

    println!("Moved Riftri-backed Git worktree");
    println!("Source: {}", result.source.display());
    println!("Destination: {}", result.destination.display());
    println!("Retained immutable base: {}", result.base_path.display());
    println!("Journal: {}", result.journal_path.display());
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize compact result")?
        );
        return Ok(());
    }

    println!("Compacted Riftri-backed Git worktree");
    println!("Destination: {}", result.destination.display());
    println!("Commit: {}", result.commit.as_str());
    println!("Immutable base: {}", result.base_path.display());
    println!("Reused immutable base: {}", result.reused_base);
    println!("Journal: {}", result.journal_path.display());
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize prune result")?
        );
        return Ok(());
    }

    println!("Pruned stale Git worktree metadata");
    println!("Journal: {}", result.journal_path.display());
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize worktree inventory")?
        );
        return Ok(());
    }

    println!("Riftri managed worktrees");
    println!("State: {}", state_directory.display());
    println!("Managed worktrees: {}", report.views.len());
    for view in &report.views {
        println!("- {}", view.destination.display());
        println!("  Repository: {}", view.repository.display());
        println!("  Head: {}", view.head.as_str());
        if let Some(branch) = &view.branch {
            println!("  Branch: {}", display_git_bytes(branch));
        } else if view.detached {
            println!("  Branch: detached");
        }
        if let Some(reason) = &view.locked_reason {
            println!("  Locked: {}", display_git_bytes(reason));
        }
        if let Some(reason) = &view.prunable_reason {
            println!("  Prunable: {}", display_git_bytes(reason));
        }
        println!("  Backend: {}", view.backend.display_name());
        println!("  Immutable base: {}", view.base_path.display());
        println!("  Logical: {}", display_byte_count(view.logical_bytes));
        println!(
            "  Filesystem-accounted allocated: {}",
            display_byte_count(view.allocated_bytes)
        );
    }
    if !report.diagnostic_issues.is_empty() {
        println!("Diagnostic issues: {}", report.diagnostic_issues.len());
        for issue in &report.diagnostic_issues {
            println!("- {}: {}", issue.path.display(), issue.reason);
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
    if json {
        let bases = report
            .bases
            .iter()
            .map(|base| {
                serde_json::json!({
                    "path": base.path.display().to_string(),
                    "path_native_hex": native_path_hex(&base.path),
                    "reference_count": base.reference_count,
                    "in_use": base.reference_count > 0,
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
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize storage status")?
        );
        return Ok(());
    }

    println!("Riftri storage status");
    println!("State: {}", state_directory.display());
    println!("Active views: {}", report.active_views);
    println!("Pending adds: {}", report.pending_adds);
    if report.pending_adds > 0 {
        println!("Attention: run `riftri repair` to roll back pending adds");
    }
    println!("Completed removals: {}", report.completed_removals);
    println!("Pending removals: {}", report.pending_removals);
    if report.pending_removals > 0 {
        println!("Attention: run `riftri repair` to resume pending removals");
    }
    println!("Completed moves: {}", report.completed_moves);
    println!("Pending moves: {}", report.pending_moves);
    if report.pending_moves > 0 {
        println!("Attention: run `riftri repair` to resume pending moves");
    }
    println!("Completed compactions: {}", report.completed_compactions);
    println!("Cancelled compactions: {}", report.cancelled_compactions);
    println!("Pending compactions: {}", report.pending_compactions);
    if report.pending_compactions > 0 {
        println!("Attention: run `riftri repair` to resume pending compactions");
    }
    println!("Completed prunes: {}", report.completed_prunes);
    println!("Pending prunes: {}", report.pending_prunes);
    if report.pending_prunes > 0 {
        println!("Attention: run `riftri repair` to resume pending prunes");
    }
    println!("Completed collections: {}", report.completed_collections);
    println!("Cancelled collections: {}", report.cancelled_collections);
    println!("Pending collections: {}", report.pending_collections);
    if report.pending_collections > 0 {
        println!("Attention: run `riftri repair` to resume pending collections");
    }
    println!(
        "Coordination locks: {} (safe persistent metadata)",
        report.coordination_locks
    );
    println!("Retained bases: {}", report.bases.len());
    for base in &report.bases {
        let state = if base.reference_count == 0 {
            "retained cache; no active views"
        } else {
            "in use"
        };
        println!(
            "- {}: refs={}, logical={}, filesystem-accounted allocated={}, state={}",
            base.path.display(),
            base.reference_count,
            display_byte_count(base.logical_bytes),
            display_byte_count(base.allocated_bytes),
            state
        );
    }
    println!("Active view storage:");
    for view in &report.views {
        println!(
            "- {}: backend={}, logical={}, filesystem-accounted allocated={}, base={}",
            view.destination.display(),
            view.backend.display_name(),
            display_byte_count(view.logical_bytes),
            display_byte_count(view.allocated_bytes),
            view.base_path.display()
        );
    }
    println!("State issues: {}", report.diagnostic_issues.len());
    for issue in &report.diagnostic_issues {
        println!("- {}: {}", issue.path.display(), issue.reason);
    }
    if !report.diagnostic_issues.is_empty() {
        println!("Attention: Riftri preserves unexplained state; inspect it before manual cleanup");
    }
    println!(
        "Total logical: {}",
        display_byte_count(report.total_logical_bytes)
    );
    println!(
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
            "resumed_collections": report.resumed_collections,
            "removed_logical_bytes": report.removed_logical_bytes,
            "removed_allocated_bytes": report.removed_allocated_bytes,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize collection report")?
        );
        return Ok(());
    }

    println!("Riftri garbage collection");
    println!("State: {}", state_directory.display());
    println!(
        "Mode: {}",
        if report.applied {
            "applied"
        } else {
            "plan only"
        }
    );
    println!("Eligible bases: {}", report.candidates.len());
    for candidate in &report.candidates {
        println!(
            "- {}: logical={}, filesystem-accounted allocated={}",
            candidate.base_path.display(),
            display_byte_count(candidate.logical_bytes),
            display_byte_count(candidate.allocated_bytes)
        );
    }
    println!("Collected bases: {}", report.collected.len());
    println!("Resumed prior collections: {}", report.resumed_collections);
    println!(
        "Skipped because now in use: {}",
        report.skipped_in_use.len()
    );
    println!(
        "Removed logical: {}",
        display_byte_count(report.removed_logical_bytes)
    );
    println!(
        "Removed filesystem-accounted allocated: {}",
        display_byte_count(report.removed_allocated_bytes)
    );
    print_allocation_note();
    if !report.applied && !report.candidates.is_empty() {
        println!("Nothing was deleted; rerun with `riftri gc --apply` to collect this plan");
    }
    Ok(())
}

fn print_allocation_note() {
    println!(
        "Allocation note: filesystem-accounted allocation may count shared COW blocks more than once; it is not exclusive physical disk use"
    );
    println!("Physical-sharing proof: use the platform volume-delta benchmark on a quiet volume");
}

fn print_doctor(report: &riftri_core::DoctorReport) {
    println!("Riftri doctor");
    println!(
        "Destination readiness: {}",
        match report.destination_readiness.status {
            riftri_core::DestinationReadinessStatus::Ready => "ready",
            riftri_core::DestinationReadinessStatus::NeedsActivation => "needs activation",
            riftri_core::DestinationReadinessStatus::Blocked => "blocked",
        }
    );
    println!(
        "Destination: {}",
        report.destination_readiness.destination.display()
    );
    println!(
        "Selected backend: {}",
        report
            .destination_readiness
            .backend
            .map_or("none", |backend| backend.display_name())
    );
    println!(
        "Copy-on-write: {}",
        if report.destination_readiness.copy_on_write {
            "verified"
        } else {
            "unavailable"
        }
    );
    println!(
        "OverlayFS helper: {}",
        match report.destination_readiness.overlayfs_helper {
            riftri_core::OverlayFsHelperReadiness::NotApplicable => "not applicable",
            riftri_core::OverlayFsHelperReadiness::NotRequired => "not required",
            riftri_core::OverlayFsHelperReadiness::Ready => "required and verified",
            riftri_core::OverlayFsHelperReadiness::Unavailable => "required but unavailable",
        }
    );
    if report.destination_readiness.blockers.is_empty() {
        println!("Readiness blockers: none");
    } else {
        println!("Readiness blockers:");
        for blocker in &report.destination_readiness.blockers {
            println!("- {}: {}", blocker.kind, blocker.explanation);
            println!("  Fix: {}", blocker.remedy);
        }
    }
    if let Some(command) = &report.destination_readiness.next_command {
        println!("Next command: {command}");
    }
    println!();
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

    match &report.repository_compatibility.value {
        Some(compatibility) => {
            println!(
                "Repository checkout compatibility (HEAD): {}",
                if compatibility.compatible {
                    "supported"
                } else {
                    "blocked"
                }
            );
            for blocker in &compatibility.blockers {
                println!("- {}: {}", blocker.kind.as_str(), blocker.explanation);
            }
        }
        None => println!(
            "Repository checkout compatibility (HEAD): unavailable ({})",
            report
                .repository_compatibility
                .error
                .as_deref()
                .unwrap_or("unknown error")
        ),
    }

    println!("Destination storage capabilities:");
    for backend in &report.storage_capabilities {
        println!(
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
        Cli, Command, OverlayFsCommand, ShellCommand, ShellKind, WorktreeCommand, failure_receipt,
    };
    #[cfg(unix)]
    use super::{native_path_encoding, native_path_hex};

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
    fn failure_receipt_has_a_stable_policy_shape() {
        let error = anyhow::Error::new(riftri_core::WorktreeError::InvalidRequest(
            "destination already exists".to_owned(),
        ));
        let receipt = failure_receipt("worktree-add", &error);

        assert_eq!(receipt["schemaVersion"], 1);
        assert_eq!(receipt["outcome"], "failed");
        assert_eq!(receipt["operation"], "worktree-add");
        assert_eq!(receipt["code"], "invalid-request");
        assert_eq!(receipt["category"], "policy");
        assert_eq!(receipt["cleanup"], "not-needed");
        assert_eq!(receipt["recovery"], "not-required");
        assert!(receipt["phase"].is_null());
        assert!(receipt["nextCommand"].is_null());
    }

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
            command: ShellCommand::Status { repository },
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
                    json,
                },
        } = list.command
        else {
            panic!("unexpected worktree list command");
        };
        assert_eq!(repository, Path::new("../app"));
        assert_eq!(state_dir.as_deref(), Some(Path::new("../state")));
        assert!(json);

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
            json,
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
