//! Interactive presentation around the existing doctor/add/enable/exec APIs.
//! No separate checkout policy, storage implementation, or agent integration.
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

struct Launch {
    worktree: PathBuf,
    program: PathBuf,
}

pub(super) fn run(
    repository: PathBuf,
    destination: Option<PathBuf>,
    branch: Option<OsString>,
    json_errors: bool,
) -> Result<i32> {
    if json_errors {
        // A refusal of the flag combination itself: nothing was attempted, so
        // the receipt must report the policy shape (exit 3, no cleanup, no
        // recovery) rather than an operational failure telling harnesses to
        // retry and inspect state that was never touched.
        return Err(riftri_core::WorktreeError::InvalidRequest(
            "setup is interactive and cannot use --json-errors; use riftri worktree add for automation"
                .to_owned(),
        )
        .into());
    }
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        bail!(
            "setup requires an interactive terminal; use riftri doctor and riftri worktree add for automation"
        );
    }

    // Drop terminal locks before handing the terminal to the chosen child.
    let launch = {
        let mut input = std::io::stdin().lock();
        let mut output = std::io::stdout().lock();
        prepare(&mut input, &mut output, &repository, destination, branch)?
    };
    let Some(launch) = launch else { return Ok(0) };
    riftri_core::enable_repository(&launch.worktree)
        .context("worktree was created and is retained, but repository enablement failed")?;
    riftri_core::execute_scoped_command_in_worktree(
        &launch.worktree,
        &[launch.program.into_os_string()],
    )
    .context("worktree is retained and repository remains enabled; could not launch the agent (no agent was installed)")
}

fn prepare(
    input: &mut impl BufRead,
    output: &mut impl Write,
    repository: &Path,
    destination: Option<PathBuf>,
    branch: Option<OsString>,
) -> Result<Option<Launch>> {
    let activation = riftri_core::repository_activation(repository)
        .context("run setup from an existing non-bare Git repository")?;
    let repository = activation.repository;
    writeln!(
        output,
        "Riftri setup\nRepository: {}",
        display(repository.as_os_str())
    )?;
    writeln!(
        output,
        "Creates a new branch at HEAD (your current commit, not uncommitted files)."
    )?;
    let destination = match destination {
        Some(path) => path,
        None => {
            let default = default_destination(&repository)?;
            let Some(answer) = question(
                input,
                output,
                &format!("Worktree directory [{}]: ", display(default.as_os_str())),
            )?
            else {
                return cancelled(output);
            };
            if answer.is_empty() {
                default
            } else {
                PathBuf::from(answer)
            }
        }
    };
    let branch = match branch {
        Some(branch) => branch,
        None => {
            let Some(answer) = question(input, output, "New branch [task/first]: ")? else {
                return cancelled(output);
            };
            if answer.is_empty() {
                OsString::from("task/first")
            } else {
                OsString::from(answer)
            }
        }
    };
    // Make the displayed plan and the eventual request refer to the same paths,
    // even when --repository names a different checkout than the caller's cwd.
    let destination = std::path::absolute(destination).context("resolve destination")?;
    let report = riftri_core::doctor_for_destination(&repository, &destination);
    let readiness = &report.destination_readiness;
    if readiness.status == riftri_core::DestinationReadinessStatus::Blocked {
        for blocker in &readiness.blockers {
            if blocker.kind != "repository-activation" {
                writeln!(
                    output,
                    "Blocked: {}\nNext step: {}",
                    blocker.explanation, blocker.remedy
                )?;
            }
        }
        bail!("setup stopped before creation; resolve the doctor diagnostics and try again");
    }
    let backend = readiness
        .backend
        .context("doctor did not select a COW backend")?;
    writeln!(
        output,
        "\nDestination: {}\nNew branch: {}\nStart at: HEAD\nBackend: {}",
        display(destination.as_os_str()),
        display(&branch),
        backend.display_name()
    )?;
    writeln!(
        output,
        "No agent, shell profile, or repository activation will be changed by creation."
    )?;
    if !confirm(input, output, "Create this worktree? [y/N] ")? {
        return cancelled(output);
    }
    let result = riftri_core::add_worktree(riftri_core::AddWorktreeRequest {
        repository,
        destination,
        revision: OsString::from("HEAD"),
        mode: riftri_core::WorktreeMode::NewBranch(branch),
        state_dir: None,
        sparse_directories: Vec::new(),
    })?;
    writeln!(
        output,
        "\nCreated {}-backed worktree: {}",
        result.backend.display_name(),
        display(result.destination.as_os_str())
    )?;
    writeln!(
        output,
        "Copy-on-write is already active for these files; edits stay private."
    )?;
    print_next_steps(output, &result.destination)?;

    let Some(program) = choose_agent(input, output, resolve_program)? else {
        writeln!(
            output,
            "Worktree kept. No agent was launched or installed; repository activation was not changed."
        )?;
        return Ok(None);
    };
    writeln!(output, "\nExecutable: {}", display(program.as_os_str()))?;
    writeln!(
        output,
        "This sets repository-local riftri.enabled=true (shared by its worktrees)."
    )?;
    writeln!(
        output,
        "The agent starts here through riftri exec. Supported Git worktree commands use COW; other Git commands pass through."
    )?;
    writeln!(
        output,
        "Your shell profile and the agent's permission settings are unchanged. Undo repository opt-in with riftri disable."
    )?;
    if env::var_os(riftri_core::BYPASS_ENV).is_some() {
        writeln!(
            output,
            "RIFTRI_BYPASS is set; if active, it disables interception. Setup does not unset it."
        )?;
    }
    if !confirm(
        input,
        output,
        "Enable this repository and launch the agent now? [y/N] ",
    )? {
        writeln!(
            output,
            "Worktree kept. No agent launched; repository activation was not changed."
        )?;
        return Ok(None);
    }
    writeln!(
        output,
        "Ask the agent to use a separate Git worktree when you want one; Riftri does not make that decision for it."
    )?;
    writeln!(
        output,
        "Launching in {}. The worktree is kept when the agent exits.",
        display(result.destination.as_os_str())
    )?;
    Ok(Some(Launch {
        worktree: result.destination,
        program,
    }))
}

fn default_destination(repository: &Path) -> Result<PathBuf> {
    let mut name = repository
        .file_name()
        .context("repository has no directory name; pass --destination")?
        .to_os_string();
    name.push(".task");
    Ok(repository
        .parent()
        .context("repository has no parent; pass --destination")?
        .join(name))
}

fn display(value: &OsStr) -> String {
    value
        .to_string_lossy()
        .chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn print_next_steps(output: &mut impl Write, destination: &Path) -> Result<()> {
    // One shared quoting rule across doctor, setup, and failure receipts: a
    // path that cannot be written as a shell argument prints no command.
    if let Some(quoted) = riftri_core::shell_quoted_path(destination) {
        writeln!(output, "Start working:\n  cd {quoted}\n  git status")?;
    } else {
        writeln!(
            output,
            "Open this worktree using its exact native path from riftri worktree list --json."
        )?;
    }
    Ok(())
}

fn question(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
) -> Result<Option<String>> {
    loop {
        write!(output, "{prompt}")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        if answer.ends_with('\n') {
            answer.pop();
        }
        if answer.ends_with('\r') {
            answer.pop();
        }
        if answer.chars().any(char::is_control) {
            writeln!(output, "Please enter a value without control characters.")?;
            continue;
        }
        return Ok(Some(answer));
    }
}

fn confirm(input: &mut impl BufRead, output: &mut impl Write, prompt: &str) -> Result<bool> {
    Ok(question(input, output, prompt)?
        .is_some_and(|answer| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")))
}

fn cancelled(output: &mut impl Write) -> Result<Option<Launch>> {
    writeln!(
        output,
        "Setup cancelled. No worktree created or repository activation changed."
    )?;
    Ok(None)
}

fn choose_agent(
    input: &mut impl BufRead,
    output: &mut impl Write,
    resolve: impl Fn(&OsStr) -> Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    loop {
        writeln!(
            output,
            "\nWhich coding agent would you like to open?\n  0. Not now\n  1. Claude Code (claude)\n  2. Codex (codex)\n  3. Another executable"
        )?;
        let Some(answer) = question(input, output, "Choose [0]: ")? else {
            return Ok(None);
        };
        let program = match answer.trim() {
            "" | "0" => return Ok(None),
            "1" => OsString::from("claude"),
            "2" => OsString::from("codex"),
            "3" => {
                let Some(name) = question(
                    input,
                    output,
                    "Executable name or path (no arguments; blank to go back): ",
                )?
                else {
                    return Ok(None);
                };
                if name.is_empty() {
                    continue;
                }
                OsString::from(name)
            }
            _ => {
                writeln!(output, "Choose 0, 1, 2, or 3.")?;
                continue;
            }
        };
        if let Some(path) = resolve(&program) {
            return Ok(Some(path));
        }
        writeln!(
            output,
            "Executable not found or not runnable: {}. No agent was installed. Choose another option or 0 to finish.",
            display(&program)
        )?;
    }
}

// Resolve before changing the child working directory. A custom value is one
// executable, never a shell command; spaces and metacharacters stay literal.
fn resolve_program(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    let directories = if path.components().count() > 1 || path.is_absolute() {
        vec![PathBuf::new()]
    } else {
        env::split_paths(&env::var_os("PATH")?).collect()
    };
    for directory in directories {
        let candidate = directory.join(path);
        for candidate in executable_candidates(&candidate) {
            if is_executable(&candidate) {
                return std::path::absolute(candidate).ok();
            }
        }
    }
    None
}

fn executable_candidates(path: &Path) -> Vec<PathBuf> {
    #[cfg(not(windows))]
    {
        vec![path.to_path_buf()]
    }
    #[cfg(windows)]
    {
        if path.extension().is_some() {
            return vec![path.to_path_buf()];
        }
        let mut candidates = vec![path.to_path_buf()];
        for extension in ["exe", "com", "cmd", "bat"] {
            candidates.push(path.with_extension(extension));
        }
        candidates
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn confirmation_requires_an_explicit_yes_and_eof_is_no() {
        for (input, expected) in [
            ("", false),
            ("\n", false),
            ("n\n", false),
            ("y\n", true),
            (" YES \r\n", true),
        ] {
            assert_eq!(
                confirm(&mut Cursor::new(input), &mut Vec::new(), "Continue? ").unwrap(),
                expected
            );
        }
    }

    #[test]
    fn questions_preserve_spaces_but_reject_terminal_controls() {
        let mut output = Vec::new();
        let answer = question(
            &mut Cursor::new("bad\x1b[2J\n path with spaces \r\n"),
            &mut output,
            "Path: ",
        )
        .unwrap();
        assert_eq!(answer.as_deref(), Some(" path with spaces "));
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("control characters")
        );
        assert_eq!(display(OsStr::new("name\n\x1b")), "name\\n\\u{1b}");
    }

    #[test]
    fn agent_choices_are_optional_and_missing_programs_are_not_installed() {
        for input in ["", "\n", "0\n"] {
            assert!(
                choose_agent(&mut Cursor::new(input), &mut Vec::new(), |_| panic!(
                    "must not resolve an unchosen program"
                ))
                .unwrap()
                .is_none()
            );
        }
        let mut output = Vec::new();
        assert!(
            choose_agent(&mut Cursor::new("9\n1\n0\n"), &mut output, |_| None)
                .unwrap()
                .is_none()
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Choose 0, 1, 2, or 3."));
        assert!(output.contains("No agent was installed"));
    }

    #[test]
    fn agent_choices_resolve_a_single_literal_executable() {
        for (input, expected) in [
            ("1\n", "claude"),
            ("2\n", "codex"),
            ("3\n\n3\nmy agent; echo nope\n", "my agent; echo nope"),
        ] {
            let program = choose_agent(&mut Cursor::new(input), &mut Vec::new(), |value| {
                assert_eq!(value, OsStr::new(expected));
                Some(PathBuf::from(value))
            })
            .unwrap()
            .unwrap();
            assert_eq!(program, PathBuf::from(expected));
        }
        assert!(
            choose_agent(&mut Cursor::new("3\n"), &mut Vec::new(), |_| None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn command_resolution_rejects_directories_and_missing_paths() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_program(temp.path().as_os_str()).is_none());
        assert!(resolve_program(temp.path().join("missing").as_os_str()).is_none());
        let program = env::current_exe().unwrap();
        assert_eq!(resolve_program(program.as_os_str()), Some(program));
    }

    #[cfg(unix)]
    #[test]
    fn nonexecutable_files_are_not_offered_as_agents() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("agent with spaces");
        std::fs::write(&program, "stub").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolve_program(program.as_os_str()).is_none());
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(resolve_program(program.as_os_str()), Some(program));
    }

    #[test]
    fn next_steps_quote_paths_for_the_platform_shell() {
        let mut output = Vec::new();
        print_next_steps(&mut output, Path::new("folder with an'apostrophe")).unwrap();
        let output = String::from_utf8(output).unwrap();
        #[cfg(windows)]
        assert!(output.contains("cd 'folder with an''apostrophe'"));
        #[cfg(not(windows))]
        assert!(output.contains("cd 'folder with an'\"'\"'apostrophe'"));
    }

    #[cfg(unix)]
    #[test]
    fn defaults_keep_native_path_bytes_and_do_not_print_lossy_commands() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"/tmp/repo-\xff".to_vec()));
        assert_eq!(
            default_destination(&path)
                .unwrap()
                .into_os_string()
                .into_vec(),
            b"/tmp/repo-\xff.task"
        );
        let mut output = Vec::new();
        print_next_steps(&mut output, &path).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("cd '"));
        assert!(output.contains("exact native path"));
    }
}
