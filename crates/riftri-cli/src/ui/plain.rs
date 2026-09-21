//! Plain presentation used when the `tui` feature is off. Output is
//! unstyled; `interactive()` is always false, so `setup` drives its own
//! text prompts and never reaches the TUI picker.
use std::fmt;
use std::io::{self, IsTerminal, Write};

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
    // Match the rich build: sanitize control characters only on an interactive
    // terminal, leaving piped/redirected stderr byte-for-byte faithful.
    if io::stderr().is_terminal() {
        eprintln!("riftri: {}", safe(&message));
    } else {
        eprintln!("riftri: {message}");
    }
}

pub(crate) fn print_line(args: fmt::Arguments<'_>) {
    let interactive = io::stdout().is_terminal();
    let mut stdout = io::stdout().lock();
    let result = if interactive {
        writeln!(stdout, "{}", safe(&args.to_string()))
    } else {
        writeln!(stdout, "{args}")
    };
    commit_stdout(result);
}

// Machine output (for example `--json`) shares the same broken-pipe handling so
// `riftri … | head` never panics; without the TUI feature it is a plain write.
pub(crate) fn print_machine(args: fmt::Arguments<'_>) {
    let mut stdout = io::stdout().lock();
    commit_stdout(writeln!(stdout, "{args}"));
}

// Byte-exact machine output (shell hooks, completions) that must not gain or
// lose a trailing newline: hooks are `eval`/`source`d, so the bytes are emitted
// verbatim while keeping the same broken-pipe handling as `print_machine`.
pub(crate) fn print_machine_raw(bytes: &[u8]) {
    let mut stdout = io::stdout().lock();
    commit_stdout(stdout.write_all(bytes));
}

// A reader that closes the pipe early (`| head`) is a clean stop, not a failure:
// leave without a panic. Any other error kind is a genuine fault worth reporting.
fn commit_stdout(result: io::Result<()>) {
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        panic!("write command output: {error}");
    }
}

pub(crate) fn print_error(message: &str) {
    if io::stderr().is_terminal() {
        eprintln!("{}", safe(message));
    } else {
        eprintln!("{message}");
    }
}

pub(crate) fn write_transcript(output: &mut impl Write, text: &str) -> io::Result<()> {
    // The rich build always escapes control characters in the setup transcript;
    // mirror that so the two builds render identical scrollback on a terminal.
    write!(output, "{}", safe(text))
}

// Escape control characters (except `\n`) so untrusted git stderr, ref names,
// and paths cannot inject terminal escape sequences into human output. Byte-for-
// byte identical to the rich build's `safe()`; machine output never uses it.
fn safe(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if ch.is_control() && ch != '\n' {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_escapes_control_characters_but_keeps_newlines_and_text() {
        assert_eq!(safe("plain text"), "plain text");
        assert_eq!(safe("with unicode 界é"), "with unicode 界é");
        // Newlines survive so multi-line reports still wrap as authored.
        assert_eq!(safe("line one\nline two"), "line one\nline two");
        // A branch name that tries to clear the screen is neutralized.
        assert_eq!(safe("clear\x1b[2Jscreen"), "clear\\u{1b}[2Jscreen");
        assert_eq!(safe("tab\there"), "tab\\there");
        assert_eq!(safe("bell\x07"), "bell\\u{7}");
    }

    #[test]
    fn write_transcript_sanitizes_control_characters() {
        let mut output = Vec::new();
        write_transcript(&mut output, "branch\x1b[2Jname\n").unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "branch\\u{1b}[2Jname\n");
    }
}
