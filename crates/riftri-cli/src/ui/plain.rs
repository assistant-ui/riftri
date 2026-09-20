//! Plain presentation used when the `tui` feature is off. Output is
//! unstyled; `interactive()` is always false, so `setup` drives its own
//! text prompts and never reaches the TUI picker.
use std::fmt;
use std::io::{self, Write};

use anyhow::Result;

pub(crate) fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::Ansi256Color;
    clap::builder::Styles::styled()
        .header(Ansi256Color(208).on_default())
        .usage(Ansi256Color(208).on_default())
        .literal(Ansi256Color(208).on_default())
}

pub(crate) struct Guard;

pub(crate) fn initialize(_plain: bool, _no_animation: bool) -> Guard {
    Guard
}

/// No rich terminal without the TUI feature, so callers fall back to plain
/// line-based prompts and unstyled output.
pub(crate) fn interactive() -> bool {
    false
}

pub(crate) fn pause_progress() {}

pub(crate) fn progress(message: String) {
    eprintln!("riftri: {message}");
}

pub(crate) fn print_line(args: fmt::Arguments<'_>) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{args}").expect("write command output");
}

pub(crate) fn print_error(message: &str) {
    eprintln!("{message}");
}

pub(crate) fn write_transcript(output: &mut impl Write, text: &str) -> io::Result<()> {
    write!(output, "{text}")
}

#[allow(dead_code)]
pub(crate) enum PromptKind {
    Text,
    Confirm,
    Agent,
}

/// Unreachable in practice: `setup` only calls this when `interactive()` is
/// true, which it never is here. Kept as a working line reader for safety.
pub(crate) fn prompt(_context: &str, label: &str, _kind: PromptKind) -> Result<Option<String>> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{label}")?;
    stderr.flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_owned()))
}

#[derive(Debug)]
pub(crate) struct Interrupted(pub i32);
impl fmt::Display for Interrupted {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            output,
            "cancelled; any already-created worktree is retained"
        )
    }
}
impl std::error::Error for Interrupted {}
