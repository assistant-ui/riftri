//! Property tests for the setup prompts and the text around them.

use std::ffi::OsStr;
use std::io::{Cursor, Write};
use std::path::Path;

use hegel::TestCase;
use hegel::generators as gs;

use super::{
    SetupOutput, choose_agent, confirm, default_destination, display, print_next_steps, question,
    resolve_program,
};

const REJECTION: &str = "Please enter a value without control characters.\n";

/// The prompt loop as specified: lines are read one at a time, a line with a
/// control character is refused and asked again, and end of input is no answer.
struct Script<'a> {
    remaining: &'a str,
    refused: usize,
}

impl<'a> Script<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            remaining: input,
            refused: 0,
        }
    }

    fn answer(&mut self) -> Option<&'a str> {
        loop {
            if self.remaining.is_empty() {
                return None;
            }
            let end = self
                .remaining
                .find('\n')
                .map_or(self.remaining.len(), |index| index + 1);
            let (raw, rest) = self.remaining.split_at(end);
            self.remaining = rest;
            let line = raw.strip_suffix('\n').unwrap_or(raw);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.chars().any(char::is_control) {
                self.refused += 1;
                continue;
            }
            return Some(line);
        }
    }
}

/// Typed input: mostly short answers a person would give, sometimes anything.
fn typed(tc: &TestCase) -> String {
    const ANSWERS: [&str; 16] = [
        "",
        "0",
        "1",
        "2",
        "3",
        "9",
        "y",
        "Y",
        "yes",
        " YES ",
        "n",
        "no",
        "yes!",
        "\ty",
        "agent",
        "\u{1b}[2J",
    ];
    let mut input = String::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(8)) {
        if tc.draw(gs::booleans()) {
            input.push_str(tc.draw(gs::sampled_from(ANSWERS.to_vec())));
        } else {
            input.push_str(&tc.draw(gs::text().max_size(12)));
        }
        input.push_str(tc.draw(gs::sampled_from(vec!["\n", "\r\n", ""])));
    }
    input
}

#[hegel::test]
fn questions_return_the_first_line_free_of_control_characters(tc: TestCase) {
    let input = typed(&tc);
    let prompt = tc.draw(gs::text().max_size(12));
    let mut script = Script::new(&input);
    let expected = script.answer();

    let mut reader = Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    let answer = question(&mut reader, &mut output, &prompt).expect("read from memory");

    assert_eq!(answer.as_deref(), expected);
    assert!(!answer.is_some_and(|answer| answer.chars().any(char::is_control)));
    // Nothing past the accepted line is consumed: it answers the next prompt.
    assert_eq!(
        reader.position() as usize,
        input.len() - script.remaining.len()
    );
    assert_eq!(
        String::from_utf8(output).expect("prompt transcript"),
        format!("{prompt}{REJECTION}").repeat(script.refused) + &prompt
    );
}

#[hegel::test]
fn confirmation_needs_an_explicit_yes(tc: TestCase) {
    let input = typed(&tc);
    let expected = Script::new(&input).answer().is_some_and(|answer| {
        let answer = answer.trim();
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    });

    let confirmed = confirm(
        &mut Cursor::new(input.as_bytes()),
        &mut Vec::new(),
        "Create this worktree? [y/N] ",
    )
    .expect("read from memory");

    assert_eq!(confirmed, expected);
}

#[hegel::test]
fn agent_choice_resolves_one_literal_executable_or_nothing(tc: TestCase) {
    let input = typed(&tc);
    let installed = tc.draw(gs::vecs(gs::sampled_from(vec!["claude", "codex", "agent"])));
    let resolve = |program: &OsStr| {
        installed
            .iter()
            .any(|name| program == OsStr::new(name))
            .then(|| Path::new("/resolved").join(program))
    };

    // The specified dialogue, replayed against the same typed input.
    let mut script = Script::new(&input);
    let (mut menus, mut invalid, mut missing) = (0, 0, 0);
    let expected = loop {
        menus += 1;
        let Some(choice) = script.answer() else {
            break None;
        };
        let program = match choice.trim() {
            "" | "0" => break None,
            "1" => "claude",
            "2" => "codex",
            "3" => match script.answer() {
                None => break None,
                Some("") => continue,
                Some(name) => name,
            },
            _ => {
                invalid += 1;
                continue;
            }
        };
        if let Some(path) = resolve(OsStr::new(program)) {
            break Some(path);
        }
        missing += 1;
    };

    let mut reader = Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    let chosen = choose_agent(&mut reader, &mut output, resolve).expect("read from memory");

    assert_eq!(chosen, expected);
    assert_eq!(
        reader.position() as usize,
        input.len() - script.remaining.len()
    );
    let output = String::from_utf8(output).expect("prompt transcript");
    let shown = |text: &str| output.matches(text).count();
    assert_eq!(shown("Which coding agent would you like to open?"), menus);
    assert_eq!(shown("Choose 0, 1, 2, or 3.\n"), invalid);
    assert_eq!(shown("No agent was installed."), missing);
    assert_eq!(shown(REJECTION), script.refused);
}

/// A terminal that stops accepting output after `budget` bytes, or at the
/// first write that would show `refuses`.
pub(super) struct ClosingTerminal {
    pub(super) shown: Vec<u8>,
    pub(super) budget: usize,
    pub(super) refuses: Option<&'static str>,
}

impl Write for ClosingTerminal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let refused = self
            .refuses
            .is_some_and(|text| String::from_utf8_lossy(bytes).contains(text));
        if refused || self.shown.len() + bytes.len() > self.budget {
            // Once closed, a terminal takes nothing further.
            self.budget = 0;
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        self.shown.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl super::PromptOutput for ClosingTerminal {}

#[hegel::test(test_cases = 300)]
fn a_prompt_that_cannot_be_shown_is_an_error_not_an_answer(tc: TestCase) {
    let input = typed(&tc);
    let dialogue = tc.draw(gs::sampled_from(vec![
        "question", "confirm", "agent", "steps",
    ]));
    // The same dialogue, on a terminal that takes everything and on one that
    // closes part way through.
    let ask = |output: &mut dyn DialogueOutput| {
        let reader = &mut Cursor::new(input.as_bytes());
        match dialogue {
            "question" => output.question(reader),
            "confirm" => output.confirm(reader),
            "agent" => output.agent(reader),
            _ => output.steps(Path::new(&input)),
        }
    };
    let mut complete = Vec::new();
    let answer = ask(&mut complete).expect("an open terminal");
    // Anywhere in the dialogue, and often within its last few writes.
    let budget = tc.draw(gs::integers::<usize>().max_value(complete.len() + 1));
    let budget = if tc.draw(gs::booleans()) {
        budget
    } else {
        complete.len().saturating_sub(budget % 160)
    };
    let mut closing = ClosingTerminal {
        shown: Vec::new(),
        budget,
        refuses: None,
    };

    let interrupted = ask(&mut closing);

    // It never shows anything the full dialogue would not have shown.
    assert!(complete.starts_with(&closing.shown));
    if closing.budget >= complete.len() {
        assert_eq!(interrupted.expect("the whole dialogue fits"), answer);
    } else {
        assert!(interrupted.is_err(), "answered {interrupted:?} unseen");
    }
}

/// The three setup dialogues over any prompt output, with their answers
/// rendered comparably.
trait DialogueOutput {
    fn question(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String>;
    fn confirm(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String>;
    fn agent(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String>;
    fn steps(&mut self, destination: &Path) -> anyhow::Result<String>;
}

impl<T: super::PromptOutput> DialogueOutput for T {
    fn question(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String> {
        question(reader, self, "Worktree directory: ").map(|answer| format!("{answer:?}"))
    }

    fn confirm(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String> {
        confirm(reader, self, "Create this worktree? [y/N] ").map(|answer| format!("{answer:?}"))
    }

    fn agent(&mut self, reader: &mut Cursor<&[u8]>) -> anyhow::Result<String> {
        choose_agent(reader, self, |program| {
            Some(Path::new("/resolved").join(program))
        })
        .map(|answer| format!("{answer:?}"))
    }

    fn steps(&mut self, destination: &Path) -> anyhow::Result<String> {
        print_next_steps(self, destination).map(|()| String::new())
    }
}

#[hegel::test]
fn displayed_values_carry_no_control_characters(tc: TestCase) {
    let value = super::super::hegel_tests::native_path(&tc).into_os_string();

    let shown = display(&value);

    assert!(!shown.chars().any(char::is_control), "{shown:?}");
    let lossy = value.to_string_lossy();
    if !lossy.chars().any(char::is_control) {
        assert_eq!(shown, lossy);
    }
    // Escaping only ever replaces a control character, in place.
    let kept = lossy.chars().filter(|character| !character.is_control());
    let mut shown = shown.chars();
    for character in kept {
        assert!(shown.any(|candidate| candidate == character));
    }
}

#[hegel::test]
fn the_default_destination_is_a_sibling_named_after_the_repository(tc: TestCase) {
    let repository = super::super::hegel_tests::native_path(&tc);

    let destination = default_destination(&repository);

    let (Some(name), Some(parent)) = (repository.file_name(), repository.parent()) else {
        assert!(destination.is_err(), "{repository:?} has no sibling");
        return;
    };
    let destination = destination.expect("a named repository has a sibling");
    assert_eq!(destination.parent(), Some(parent));
    let mut expected = name.to_os_string();
    expected.push(".task");
    assert_eq!(destination.file_name(), Some(expected.as_os_str()));
}

#[cfg(unix)]
#[hegel::test(crate::hegel_tests::process_cases(100))]
fn next_steps_print_a_command_only_when_a_shell_reads_the_path_back(tc: TestCase) {
    use std::os::unix::ffi::OsStrExt;
    let destination = super::super::hegel_tests::native_path(&tc);

    let mut output = Vec::new();
    print_next_steps(&mut output, &destination).expect("write to memory");
    let output = String::from_utf8(output).expect("next steps");

    let Some(command) = output
        .strip_prefix("Start working:\n  ")
        .and_then(|rest| rest.strip_suffix("\n  git status\n"))
    else {
        // No command at all, rather than one that names a different path.
        assert!(!output.contains("cd "), "{output}");
        assert!(riftri_core::shell_quoted_path(&destination).is_none());
        return;
    };
    let shown =
        crate::hegel_tests::shell_output(&format!("cd() {{ printf '%s' \"$@\"; }}\n{command}\n"));
    assert_eq!(shown, destination.as_os_str().as_bytes());
}

#[hegel::test]
fn plain_setup_output_passes_bytes_through_unchanged(tc: TestCase) {
    let chunks = tc.draw(gs::vecs(gs::binary().max_size(16)).max_size(6));
    let mut output = SetupOutput {
        output: Vec::new(),
        transcript: String::new(),
        pending: Vec::new(),
    };

    for chunk in &chunks {
        output.write_all(chunk).expect("write to memory");
        output.flush().expect("flush to memory");
    }

    assert_eq!(output.output, chunks.concat());
    // Without a terminal UI there is no screen to give context to.
    assert_eq!(super::PromptOutput::context(&output), None);
}

#[cfg(unix)]
#[hegel::test(crate::hegel_tests::process_cases(40))]
fn only_executable_files_resolve_and_resolution_is_absolute(tc: TestCase) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().expect("fixture directory");
    let name = tc.draw(gs::sampled_from(vec![
        "agent",
        "agent with spaces",
        "agent; echo nope",
        "$(agent)",
    ]));
    let program = fixture.path().join("bin").join(name);
    std::fs::create_dir(fixture.path().join("bin")).expect("create bin");
    let mode = tc.draw(gs::integers::<u32>().max_value(0o777));
    let kind = tc.draw(gs::sampled_from(vec!["file", "directory", "missing"]));
    match kind {
        "file" => {
            std::fs::write(&program, "").expect("create program");
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(mode))
                .expect("set mode");
        }
        "directory" => std::fs::create_dir(&program).expect("create directory"),
        _ => {}
    }

    let resolved = resolve_program(program.as_os_str());

    let runnable = kind == "file" && mode & 0o111 != 0;
    assert_eq!(resolved, runnable.then(|| program.clone()));

    // A bare name is looked up on PATH and never in the current directory.
    let bare = tc.draw(gs::sampled_from(vec!["sh", "ls", "riftri-absent-agent"]));
    match resolve_program(OsStr::new(bare)) {
        Some(found) => {
            assert!(found.is_absolute());
            assert_eq!(found.file_name(), Some(OsStr::new(bare)));
            assert!(
                found
                    .metadata()
                    .expect("resolved program")
                    .permissions()
                    .mode()
                    & 0o111
                    != 0
            );
        }
        None => assert_eq!(bare, "riftri-absent-agent"),
    }
}
