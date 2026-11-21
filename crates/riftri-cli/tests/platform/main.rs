#[cfg(unix)]
#[path = "../support/real_git.rs"]
mod real_git;
#[path = "../support/mod.rs"]
mod support;

mod apfs_cli;
mod fsmonitor;
mod gc_diagnostics;
mod progress_reporting;
mod recovery_concurrency;
mod repair_reconciliation_cli;
mod windows_cli;
mod xcrun_trampoline;
