//! Property tests for the rich build's prompt input, rendering, and policy.

use crossterm::event::{KeyEvent, KeyEventKind};
use hegel::TestCase;
use hegel::generators as gs;
use ratatui::backend::TestBackend;
use unicode_width::UnicodeWidthStr;

use super::*;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn modified(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn prompt_kind(tc: &TestCase, names: &[&'static str]) -> PromptKind {
    match tc.draw(gs::sampled_from(names.to_vec())) {
        "text" => PromptKind::Text,
        "confirm" => PromptKind::Confirm,
        _ => PromptKind::Agent,
    }
}

/// Feed one event that must leave the prompt open.
fn feed(input: &mut Input, event: Event, kind: PromptKind) {
    let action = input.handle(event, kind).expect("the prompt stays open");
    assert!(matches!(action, Action::Redraw), "{action:?}");
}

/// Text mixing ordinary characters with the control characters a hostile ref
/// name or path would use to drive a terminal.
fn terminal_text(tc: &TestCase) -> String {
    let mut text = String::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(6)) {
        if tc.draw(gs::booleans()) {
            text.push_str(tc.draw(gs::sampled_from(vec![
                "\n",
                "\r",
                "\t",
                "\u{1b}[2J",
                "\u{7}",
                "\u{9b}",
                "\u{7f}",
                "\0",
                ": ",
                ":",
            ])));
        } else {
            text.push_str(&tc.draw(gs::text().max_size(8)));
        }
    }
    text
}

/// The text prompt against the obvious editor model: characters and a cursor.
struct TextPrompt {
    input: Input,
    text: Vec<char>,
    cursor: usize,
}

impl TextPrompt {
    fn feed(&mut self, event: Event) {
        feed(&mut self.input, event, PromptKind::Text);
    }

    fn insert(&mut self, value: &str) {
        for character in value.chars() {
            self.text.insert(self.cursor, character);
            self.cursor += 1;
        }
    }

    fn paste(&mut self, value: String) {
        let fits = self.text.len() + value.chars().count() <= 4096;
        let accepted = fits && !value.chars().any(char::is_control);
        self.feed(Event::Paste(value.clone()));
        if accepted {
            self.insert(&value);
        }
        // A refused paste must be visible, never silently truncated.
        assert_eq!(self.input.rejected_paste, !accepted);
    }
}

#[hegel::state_machine]
impl TextPrompt {
    #[rule]
    fn type_character(&mut self, tc: TestCase) {
        let character = tc.draw(gs::characters());
        self.feed(key(KeyCode::Char(character)));
        if !character.is_control() && self.text.len() < 4096 {
            self.insert(&character.to_string());
        }
    }

    #[rule]
    fn paste_text(&mut self, tc: TestCase) {
        self.paste(tc.draw(gs::text().max_size(12)));
    }

    #[rule]
    fn paste_up_to_the_length_limit(&mut self, tc: TestCase) {
        let length = tc.draw(gs::integers::<usize>().min_value(1000).max_value(4097));
        self.paste("x".repeat(length));
    }

    #[rule]
    fn move_cursor(&mut self, tc: TestCase) {
        let code = tc.draw(gs::sampled_from(vec![
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
        ]));
        self.feed(key(code));
        self.cursor = match code {
            KeyCode::Left => self.cursor.saturating_sub(1),
            KeyCode::Right => (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => 0,
            _ => self.text.len(),
        };
    }

    #[rule]
    fn erase(&mut self, tc: TestCase) {
        if tc.draw(gs::booleans()) {
            self.feed(key(KeyCode::Backspace));
            if self.cursor > 0 {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
        } else {
            self.feed(key(KeyCode::Delete));
            if self.cursor < self.text.len() {
                self.text.remove(self.cursor);
            }
        }
    }

    #[rule]
    fn events_that_edit_nothing(&mut self, tc: TestCase) {
        let character = tc.draw(gs::sampled_from(vec!['a', 'y', '3', 'x']));
        let event = match tc.draw(gs::integers::<u8>().max_value(6)) {
            0 => Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(character),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            1 => modified(KeyCode::Char(character), KeyModifiers::ALT),
            2 => modified(KeyCode::Char(character), KeyModifiers::CONTROL),
            3 => key(tc.draw(gs::sampled_from(vec![
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Tab,
                KeyCode::PageUp,
                KeyCode::PageDown,
            ]))),
            4 => Event::Resize(80, 24),
            5 => Event::FocusGained,
            _ => Event::FocusLost,
        };
        self.feed(event);
    }

    #[rule]
    fn submit_returns_exactly_what_is_shown(&mut self, _: TestCase) {
        let action = self
            .input
            .handle(key(KeyCode::Enter), PromptKind::Text)
            .expect("submit");
        let expected = self.text.iter().collect::<String>();
        assert!(matches!(action, Action::Finish(Some(answer)) if answer == expected));
    }

    #[rule]
    fn cancel_never_answers(&mut self, tc: TestCase) {
        let event = tc.draw(gs::sampled_from(vec![
            key(KeyCode::Esc),
            modified(KeyCode::Char('d'), KeyModifiers::CONTROL),
        ]));
        let action = self.input.handle(event, PromptKind::Text).expect("cancel");
        assert!(matches!(action, Action::Finish(None)));
        let interrupt = self
            .input
            .handle(
                modified(KeyCode::Char('c'), KeyModifiers::CONTROL),
                PromptKind::Text,
            )
            .expect_err("interrupt");
        assert_eq!(
            interrupt.downcast_ref::<Interrupted>().map(|i| i.0),
            Some(130)
        );
    }

    #[rule]
    fn the_cursor_stays_inside_the_input_box(&mut self, tc: TestCase) {
        let width = tc.draw(gs::integers::<u16>().min_value(30).max_value(140));
        let height = tc.draw(gs::integers::<u16>().min_value(16).max_value(40));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw_prompt(frame, "", "Path:", PromptKind::Text, &self.input, false))
            .expect("draw");
        let cursor = terminal.get_cursor_position().expect("cursor");
        let box_width = (width - 4).min(88);
        let left = (width - box_width) / 2;
        assert!(
            cursor.x > left && cursor.x < left + box_width - 1,
            "{cursor:?}"
        );
        assert!(cursor.y < height);
    }

    #[invariant(always_run)]
    fn the_editor_matches_the_model(&mut self, _: TestCase) {
        assert_eq!(self.input.text, self.text);
        assert_eq!(self.input.cursor, self.cursor);
        assert!(self.cursor <= self.text.len() && self.text.len() <= 4096);
        // What Enter returns is typed into a path or a ref name.
        assert!(!self.text.iter().any(|character| character.is_control()));
    }
}

#[hegel::test]
fn text_prompts_edit_like_a_line_editor(tc: TestCase) {
    let prompt = TextPrompt {
        input: Input::default(),
        text: Vec::new(),
        cursor: 0,
    };
    hegel::stateful::machine(prompt).run(tc);
}

#[hegel::test]
fn choice_prompts_answer_with_the_highlighted_choice(tc: TestCase) {
    let kind = prompt_kind(&tc, &["confirm", "agent"]);
    let count = kind.choices().len();
    let mut input = Input::default();
    let mut selected = 0usize;
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(12)) {
        let code = tc.draw(gs::sampled_from(vec![
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('y'),
            KeyCode::Char('N'),
            KeyCode::Char('0'),
            KeyCode::Char('3'),
            KeyCode::Char('7'),
            KeyCode::Char('x'),
            KeyCode::Tab,
            KeyCode::PageUp,
        ]));
        feed(&mut input, key(code), kind);
        selected = match (code, kind) {
            (KeyCode::Up | KeyCode::Left, _) => selected.saturating_sub(1),
            (KeyCode::Down | KeyCode::Right, _) => (selected + 1).min(count - 1),
            (KeyCode::Char('y'), PromptKind::Confirm) => 1,
            (KeyCode::Char('N'), PromptKind::Confirm) => 0,
            (KeyCode::Char(digit @ ('0' | '3')), PromptKind::Agent) => {
                digit.to_digit(10).expect("digit") as usize
            }
            _ => selected,
        };
        assert_eq!(input.selected, selected);
    }

    let action = input.handle(key(KeyCode::Enter), kind).expect("submit");

    let expected = match kind {
        // Nothing but an explicit move to "yes" confirms.
        PromptKind::Confirm => if selected == 1 { "y" } else { "n" }.to_owned(),
        _ => selected.to_string(),
    };
    assert!(matches!(action, Action::Finish(Some(answer)) if answer == expected));
    assert!(selected < count);
}

/// The rendered screen, one string per row.
fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

#[hegel::test]
fn prompts_render_at_any_size_and_keep_the_question_in_view(tc: TestCase) {
    let width = tc.draw(gs::integers::<u16>().min_value(1).max_value(140));
    let height = tc.draw(gs::integers::<u16>().min_value(1).max_value(40));
    let kind = prompt_kind(&tc, &["text", "confirm", "agent"]);
    // Often far taller than the screen, as a transcript of retries is.
    let context = (0..tc.draw(gs::integers::<usize>().max_value(60)))
        .map(|_| safe(&tc.draw(gs::text().max_size(40))))
        .collect::<Vec<_>>()
        .join("\n");
    let label = tc.draw(gs::sampled_from(vec![
        "Create this worktree? [y/N] ",
        "Choose [0]: ",
        "New branch [task/first]: ",
    ]));
    let typed = tc.draw(gs::vecs(gs::sampled_from(vec!['a', ' ', 'é', '界', '/'])).max_size(120));
    let input = Input {
        cursor: tc.draw(gs::integers::<usize>().max_value(typed.len())),
        text: typed,
        selected: tc.draw(gs::integers::<usize>().max_value(kind.choices().len().max(1) - 1)),
        scroll: 0,
        rejected_paste: tc.draw(gs::booleans()),
    };
    let color = tc.draw(gs::booleans());

    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| draw_prompt(frame, &context, label, kind, &input, color))
        .expect("draw");

    let rows = rows(&terminal);
    let screen = rows.concat();
    if width < 30 || height < 16 {
        // Too small to show the question, so it asks for room instead.
        assert!(!screen.contains("Riftri"));
        assert!(width < 10 || screen.contains("Enlarge to"), "{rows:#?}");
        return;
    }
    assert!(rows[1].contains("Riftri"), "{rows:#?}");
    // However long the context is, the question being asked stays visible,
    // wrapped between words where the screen is narrower than it.
    let words = rows
        .iter()
        .flat_map(|row| row.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(words.contains(label.trim_end()), "{rows:#?}");
    if input.rejected_paste {
        assert!(screen.contains("Paste rejected"), "{rows:#?}");
    }
    if kind == PromptKind::Text {
        if let Some(before) = input.cursor.checked_sub(1) {
            // The character just typed is on screen, directly left of the cursor.
            let character = input.text[before].to_string();
            let cursor = terminal.get_cursor_position().expect("cursor");
            let cell = cursor.x - character.width() as u16;
            let shown = terminal.backend().buffer()[(cell, cursor.y)].symbol();
            assert_eq!(shown, character, "{rows:#?}");
        }
        return;
    }
    let marked = rows
        .iter()
        .filter(|row| row.contains('›'))
        .collect::<Vec<_>>();
    assert_eq!(marked.len(), 1, "{rows:#?}");
    let choice = kind.choices()[input.selected];
    let start = choice.chars().take(8).collect::<String>();
    assert!(marked[0].contains(&format!("› {start}")), "{rows:#?}");
}

#[hegel::test]
fn progress_lines_fit_the_terminal_and_clip_no_more_than_needed(tc: TestCase) {
    let message = safe(&terminal_text(&tc)).replace('\n', " ");
    let tick = tc.draw(gs::integers::<usize>());
    let width = tc.draw(gs::integers::<u16>().max_value(120));
    let color = tc.draw(gs::booleans());

    let line = progress_line(&message, tick, width, color);

    let shown = line.to_string();
    assert!(shown.width() <= usize::from(width), "{shown:?}");
    let label = format!("{} {message}", TICKS[tick % TICKS.len()]);
    let clipped = label.strip_prefix(&shown).expect("a prefix of the label");
    if let Some(next) = clipped.chars().next() {
        let widths = shown.chars().chain([next]).map(|c| c.width().unwrap_or(0));
        assert!(widths.sum::<usize>() > usize::from(width), "{shown:?}");
    }
    assert_eq!(
        line,
        progress_line(&message, tick % TICKS.len(), width, color)
    );
}

/// `text` with every SGR color sequence removed.
fn without_colors(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\u{1b}[") {
        plain.push_str(&rest[..start]);
        let end = rest[start..].find('m').expect("a complete color sequence");
        rest = &rest[start + end + 1..];
    }
    plain + rest
}

#[hegel::test]
fn styled_lines_carry_the_text_unchanged(tc: TestCase) {
    let text = safe(&terminal_text(&tc)).replace('\n', " ");

    let line = report_line(&text);

    assert_eq!(line.to_string(), text);
    // Only a `key: value` line gets a highlighted key.
    assert_eq!(line.spans.len() == 2, text.contains(": "));
    let mut plain = Vec::new();
    write_styled(&mut plain, &line, false).expect("write to memory");
    assert_eq!(String::from_utf8(plain).expect("plain line"), text);
    let mut colored = Vec::new();
    write_styled(&mut colored, &line, true).expect("write to memory");
    let colored = String::from_utf8(colored).expect("colored line");
    assert_eq!(without_colors(&colored), text);
    assert!(colored.ends_with("\u{1b}[0m"), "{colored:?}");
}

#[hegel::test]
fn the_setup_transcript_is_escaped_like_every_other_line(tc: TestCase) {
    let text = terminal_text(&tc);

    let mut output = Vec::new();
    write_transcript(&mut output, &text).expect("write to memory");

    // Byte for byte what the plain build writes for the same text.
    assert_eq!(String::from_utf8(output).expect("transcript"), safe(&text));
    assert!(
        !safe(&text)
            .chars()
            .any(|character| character.is_control() && character != '\n')
    );
}

#[hegel::test]
fn presentation_is_rich_only_on_a_capable_interactive_terminal(tc: TestCase) {
    let [plain, no_animation, terminals, dumb, ci, no_color] =
        [(); 6].map(|()| tc.draw(gs::booleans()));

    let policy = Policy::detect(plain, no_animation, terminals, dumb, ci, no_color);

    assert_eq!(policy.rich, terminals && !plain && !dumb && !ci);
    // Color and animation are refinements of the rich renderer, never
    // something a pipe, CI log, or `--plain` run can receive.
    assert_eq!(policy.color, policy.rich && !no_color);
    assert_eq!(policy.animate, policy.rich && !no_animation);
}
