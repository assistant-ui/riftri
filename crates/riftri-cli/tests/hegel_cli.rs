//! Property tests that drive the built `riftri` binary from outside.
#![cfg(unix)]

mod support;

#[path = "support/contract.rs"]
mod contract;

#[path = "hegel_cli/common.rs"]
mod common;
#[path = "hegel_cli/receipts.rs"]
mod receipts;
#[path = "hegel_cli/shim.rs"]
mod shim;
#[path = "hegel_cli/surface.rs"]
mod surface;
#[path = "hegel_cli/terminal.rs"]
mod terminal;
