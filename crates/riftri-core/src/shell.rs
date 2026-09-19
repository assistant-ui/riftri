//! Rendering filesystem paths into commands a user or harness can run.
//!
//! Suggested commands are only useful if they name the exact path Riftri
//! means. A path printed with `Path::display` is neither quoted nor faithful:
//! it splits on whitespace in a shell and replaces non-Unicode bytes with
//! `U+FFFD`. Either failure sends the caller to a different directory, where
//! Riftri then reports an all-clear for state it never inspected.
//!
//! Every suggested command therefore goes through this module, and a path that
//! cannot be represented safely produces no command at all. Callers must fall
//! back to the exact native path in their machine-readable fields instead of
//! emitting something that looks runnable but is not.

use std::path::Path;

/// Render `path` as one POSIX-shell argument, or `None` when it cannot be
/// represented safely.
///
/// Quoting is POSIX (`sh`/`bash`/`zsh`, and Git Bash / WSL / MSYS on Windows)
/// regardless of the build target, because the shell that runs a suggested
/// command is chosen at run time by its consumer, not at compile time. A path
/// is refused when it is not valid Unicode, or when it contains control
/// characters a terminal would interpret rather than display; both cases omit
/// the command, since there is no lossy rendering that stays correct.
pub fn shell_quoted_path(path: &Path) -> Option<String> {
    let value = path.to_str()?;
    if value.chars().any(char::is_control) {
        return None;
    }
    // POSIX single-quote quoting: single quotes suppress every other shell
    // metacharacter, and a literal quote is spliced in as `'"'"'`. The dialect
    // is deliberately fixed rather than chosen by build target — a suggested
    // command's consuming shell is decided at run time by whoever runs it, not
    // at compile time, and these recovery commands are run in a POSIX shell
    // (sh/bash/zsh, and on Windows the Git Bash, WSL, or MSYS environments Git
    // work happens in). A caller that needs the exact path in another shell
    // reads it from the accompanying native-path field instead.
    let escaped = value.replace('\'', "'\"'\"'");
    Some(format!("'{escaped}'"))
}

/// Render the `riftri repair` invocation that inspects `state_directory`, or
/// `None` when the path cannot be represented as a shell argument.
///
/// Human-readable messages and machine-readable receipts both call this, so
/// the command a harness executes is character-for-character the command the
/// message shows.
pub fn repair_command(state_directory: &Path) -> Option<String> {
    shell_quoted_path(state_directory).map(|quoted| format!("riftri repair --state-dir {quoted}"))
}

/// Render the `riftri status` invocation that inspects `state_directory`, or
/// `None` when the path cannot be represented as a shell argument.
///
/// Human-readable messages and machine-readable receipts both call this, so
/// the command a harness executes is character-for-character the command the
/// message shows.
pub fn status_command(state_directory: &Path) -> Option<String> {
    shell_quoted_path(state_directory).map(|quoted| format!("riftri status --state-dir {quoted}"))
}

/// Absolute form of a caller-supplied path, so a suggested command targets the
/// same directory regardless of where the caller runs it.
///
/// Falls back to the path as given when the current directory cannot be read;
/// a relative suggestion is still better than none.
pub fn command_path(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_stay_inside_one_shell_argument() {
        let quoted = shell_quoted_path(Path::new("/Users/me/My Projects/app/.git/riftri"))
            .expect("a path with a space is representable");
        assert_eq!(quoted, "'/Users/me/My Projects/app/.git/riftri'");
    }

    #[test]
    fn single_quotes_are_escaped_with_posix_quoting_on_every_platform() {
        // The rendering is POSIX regardless of build target, so a harness that
        // runs the command in a POSIX shell (including Git Bash / WSL on
        // Windows) gets the exact path back.
        let quoted =
            shell_quoted_path(Path::new("/tmp/it's here")).expect("a quoted path is representable");
        assert_eq!(quoted, "'/tmp/it'\"'\"'s here'");
    }

    #[cfg(unix)]
    #[test]
    fn a_posix_shell_parses_the_quoted_path_back_to_one_argument() {
        use std::process::Command;

        // Round-trip through a real POSIX shell: the awkward path with a space,
        // a single quote, and shell metacharacters must come back verbatim as a
        // single argument.
        let path = "/tmp/it's a $HOME; [test] dir";
        let quoted = shell_quoted_path(Path::new(path)).expect("representable");
        let script = format!("set -- {quoted}; printf '%s' \"$1\"; [ \"$#\" -eq 1 ]");
        let output = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run sh");
        assert!(
            output.status.success(),
            "path did not parse to one argument"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), path);
    }

    #[test]
    fn control_characters_produce_no_command() {
        assert!(shell_quoted_path(Path::new("/tmp/new\nline")).is_none());
        assert!(repair_command(Path::new("/tmp/new\nline")).is_none());
        assert!(status_command(Path::new("/tmp/new\nline")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_produce_no_command() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let path = PathBuf::from(OsString::from_vec(b"/tmp/state-\xff".to_vec()));
        assert!(shell_quoted_path(&path).is_none());
        assert!(repair_command(&path).is_none());
    }

    #[test]
    fn the_repair_command_names_the_state_directory() {
        assert_eq!(
            repair_command(Path::new("/tmp/riftri-state")).expect("representable"),
            "riftri repair --state-dir '/tmp/riftri-state'"
        );
    }

    #[test]
    fn the_status_command_names_the_state_directory() {
        assert_eq!(
            status_command(Path::new("/tmp/riftri-state")).expect("representable"),
            "riftri status --state-dir '/tmp/riftri-state'"
        );
    }

    #[test]
    fn command_paths_are_absolute() {
        assert!(command_path(Path::new("relative/state")).is_absolute());
        #[cfg(unix)]
        assert_eq!(
            command_path(Path::new("/already/absolute")),
            Path::new("/already/absolute")
        );
    }
}
