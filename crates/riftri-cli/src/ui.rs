//! Terminal presentation. The rich interactive layer (ratatui/crossterm) is
//! behind the `tui` feature; without it, output stays plain and `setup` uses
//! its built-in text prompts.

#[cfg(feature = "tui")]
mod rich;
#[cfg(feature = "tui")]
pub(crate) use rich::*;

#[cfg(not(feature = "tui"))]
mod plain;
#[cfg(not(feature = "tui"))]
pub(crate) use plain::*;
