//! Terminal presentation only. Git, storage policy, and operation results stay
//! in core. Machine output never enters a terminal renderer.
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::style::{ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthChar;

const ACCENT: Color = Color::Indexed(208);
const MUTED: Color = Color::Indexed(245);
const TICKS: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
static UI: OnceLock<Arc<Presentation>> = OnceLock::new();
static SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::Ansi256Color;
    clap::builder::Styles::styled()
        .header(Ansi256Color(208).on_default())
        .usage(Ansi256Color(208).on_default())
        .literal(Ansi256Color(208).on_default())
}

#[derive(Clone, Copy, Debug)]
struct Policy {
    rich: bool,
    color: bool,
    animate: bool,
}

impl Policy {
    fn detect(
        plain: bool,
        no_animation: bool,
        terminals: bool,
        dumb: bool,
        ci: bool,
        no_color: bool,
    ) -> Self {
        let rich = terminals && !plain && !dumb && !ci;
        Self {
            rich,
            color: rich && !no_color,
            animate: rich && !no_animation,
        }
    }
}

struct Presentation {
    policy: Policy,
    progress: Mutex<Progress>,
    wake: Condvar,
}

#[derive(Default)]
struct Progress {
    message: Option<String>,
    painted: bool,
    stopped: bool,
    tick: usize,
}

pub(super) struct Guard(Option<JoinHandle<()>>);

pub(super) fn initialize(plain: bool, no_animation: bool) -> Guard {
    let nonempty = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    let policy = Policy::detect(
        plain,
        no_animation || nonempty("RIFTRI_NO_ANIMATION"),
        io::stdout().is_terminal() && io::stderr().is_terminal(),
        std::env::var("TERM").map_or(!cfg!(windows), |term| term.is_empty() || term == "dumb"),
        nonempty("CI"),
        nonempty("NO_COLOR"),
    );
    let state = Arc::new(Presentation {
        policy,
        progress: Mutex::new(Progress::default()),
        wake: Condvar::new(),
    });
    let _ = UI.set(state.clone());
    if policy.rich {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_screen();
            previous(info);
        }));
    }
    // No timer or terminal ownership at all in pipes, JSON, or reduced-motion mode.
    let worker = policy.animate.then(|| {
        std::thread::spawn(move || {
            let mut progress = state
                .progress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while !progress.stopped {
                if progress.message.is_none() {
                    progress = state
                        .wake
                        .wait(progress)
                        .unwrap_or_else(|error| error.into_inner());
                    continue;
                }
                if let Some(message) = &progress.message {
                    let width = terminal::size()
                        .map_or(80, |(width, _)| width)
                        .saturating_sub(1);
                    let line = progress_line(message, progress.tick, width, state.policy.color);
                    let mut stderr = io::stderr().lock();
                    let _ = queue!(stderr, MoveToColumn(0), Clear(ClearType::CurrentLine));
                    let _ = write_styled(&mut stderr, &line, state.policy.color);
                    let _ = stderr.flush();
                    progress.painted = true;
                    progress.tick = progress.tick.wrapping_add(1);
                }
                progress = state
                    .wake
                    .wait_timeout(progress, Duration::from_millis(100))
                    .unwrap_or_else(|error| error.into_inner())
                    .0;
            }
        })
    });
    Guard(worker)
}

impl Drop for Guard {
    fn drop(&mut self) {
        pause_progress();
        if let Some(state) = UI.get() {
            state
                .progress
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .stopped = true;
            state.wake.notify_all();
        }
        if let Some(worker) = self.0.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn interactive() -> bool {
    UI.get().is_some_and(|state| state.policy.rich) && io::stdin().is_terminal()
}

pub(super) fn pause_progress() {
    if let Some(state) = UI.get() {
        let mut progress = state
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.message = None;
        if progress.painted {
            let _ = execute!(io::stderr(), MoveToColumn(0), Clear(ClearType::CurrentLine));
            progress.painted = false;
        }
    }
}

pub(super) fn progress(message: String) {
    if let Some(state) = UI.get().filter(|state| state.policy.animate) {
        state
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .message = Some(safe(&message));
        state.wake.notify_all();
    } else {
        eprintln!("riftri: {message}");
    }
}

pub(super) fn print_line(args: fmt::Arguments<'_>) {
    pause_progress();
    let mut stdout = io::stdout().lock();
    let result = if let Some(state) = UI.get().filter(|state| state.policy.rich) {
        args.to_string().split('\n').try_for_each(|line| {
            write_styled(&mut stdout, &report_line(&safe(line)), state.policy.color)?;
            writeln!(stdout)
        })
    } else {
        writeln!(stdout, "{args}")
    };
    commit_stdout(result);
}

// Machine output (for example `--json`) bypasses the terminal renderer but
// shares the same broken-pipe handling so `riftri … | head` never panics.
pub(super) fn print_machine(args: fmt::Arguments<'_>) {
    pause_progress();
    let mut stdout = io::stdout().lock();
    commit_stdout(writeln!(stdout, "{args}"));
}

// A reader that closes the pipe early (`| head`, `| less` then `q`) is a clean
// stop, not a failure: leave without a panic or backtrace. Rust ignores SIGPIPE
// by default, so this is what keeps a closed stdout from aborting the process.
// Handling the error kind (rather than resetting SIGPIPE) also works on Windows
// and still lets terminal-restore cleanup run before any real write error
// surfaces loudly. Any other error kind is a genuine fault worth reporting.
fn commit_stdout(result: io::Result<()>) {
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        panic!("write command output: {error}");
    }
}

pub(super) fn print_error(message: &str) {
    pause_progress();
    let color = UI.get().is_some_and(|state| state.policy.color);
    let line = Line::styled(
        if UI.get().is_some_and(|state| state.policy.rich) {
            safe(message)
        } else {
            message.to_owned()
        },
        Style::default().fg(ACCENT),
    );
    let mut stderr = io::stderr().lock();
    let _ = write_styled(&mut stderr, &line, color);
    let _ = writeln!(stderr);
}

// Setup keeps a plain transcript in scrollback as well as its temporary screens.
pub(super) fn write_transcript(output: &mut impl Write, text: &str) -> io::Result<()> {
    pause_progress();
    let color = UI.get().is_some_and(|state| state.policy.color);
    for part in text.split_inclusive('\n') {
        write_styled(
            output,
            &report_line(&safe(part.trim_end_matches('\n'))),
            color,
        )?;
        if part.ends_with('\n') {
            writeln!(output)?;
        }
    }
    output.flush()
}

fn report_line(text: &str) -> Line<'_> {
    if let Some((key, value)) = text.split_once(": ") {
        Line::from(vec![
            Span::styled(format!("{key}: "), Style::default().fg(ACCENT)),
            Span::raw(value),
        ])
    } else if text.ends_with(':') || text == "Riftri setup" {
        Line::styled(text, Style::default().fg(ACCENT))
    } else {
        Line::raw(text)
    }
}

// Render Ratatui spans without cursor positioning: reports remain in scrollback,
// can be selected/copied, and are never truncated to a dashboard viewport.
fn write_styled(output: &mut impl Write, line: &Line<'_>, color: bool) -> io::Result<()> {
    for span in &line.spans {
        if color {
            let foreground = span.style.fg.or(line.style.fg).unwrap_or(Color::Reset);
            let foreground = match foreground {
                Color::Indexed(index) => crossterm::style::Color::AnsiValue(index),
                _ => crossterm::style::Color::Reset,
            };
            queue!(output, SetForegroundColor(foreground))?;
        }
        write!(output, "{}", span.content)?;
    }
    if color {
        queue!(output, ResetColor)?;
    }
    Ok(())
}

fn progress_line(message: &str, tick: usize, width: u16, color: bool) -> Line<'static> {
    let label = format!("{} {message}", TICKS[tick % TICKS.len()]);
    let mut clipped = String::new();
    let mut columns = 0;
    for character in label.chars() {
        columns += character.width().unwrap_or(0);
        if columns > usize::from(width) {
            break;
        }
        clipped.push(character);
    }
    Line::styled(
        clipped,
        if color {
            Style::default().fg(ACCENT)
        } else {
            Style::default()
        },
    )
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptKind {
    Text,
    Confirm,
    Agent,
}

impl PromptKind {
    fn choices(self) -> &'static [&'static str] {
        match self {
            Self::Text => &[],
            Self::Confirm => &["No — leave things unchanged", "Yes — continue"],
            Self::Agent => &[
                "Not now",
                "Claude Code  ·  claude",
                "Codex  ·  codex",
                "Another executable",
            ],
        }
    }
}

#[derive(Debug)]
pub(super) struct Interrupted(pub i32);
impl fmt::Display for Interrupted {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            output,
            "cancelled; any already-created worktree is retained"
        )
    }
}
impl std::error::Error for Interrupted {}

#[derive(Default)]
struct Input {
    text: Vec<char>,
    cursor: usize,
    selected: usize,
    scroll: u16,
    rejected_paste: bool,
}

#[derive(Debug)]
enum Action {
    Redraw,
    Finish(Option<String>),
}

impl Input {
    fn handle(&mut self, event: Event, kind: PromptKind) -> Result<Action> {
        let Event::Key(key) = event else {
            if let Event::Paste(value) = event {
                if kind == PromptKind::Text
                    && !value.chars().any(char::is_control)
                    && self.text.len() + value.chars().count() <= 4096
                {
                    for ch in value.chars() {
                        self.text.insert(self.cursor, ch);
                        self.cursor += 1;
                    }
                    self.rejected_paste = false;
                } else {
                    self.rejected_paste = true;
                }
            }
            return Ok(Action::Redraw);
        };
        if key.kind == KeyEventKind::Release {
            return Ok(Action::Redraw);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Err(Interrupted(130).into()),
                KeyCode::Char('d') => return Ok(Action::Finish(None)),
                _ => return Ok(Action::Redraw),
            }
        }
        match key.code {
            KeyCode::Esc => return Ok(Action::Finish(None)),
            KeyCode::Enter => {
                return Ok(Action::Finish(Some(match kind {
                    PromptKind::Text => self.text.iter().collect(),
                    PromptKind::Confirm => if self.selected == 1 { "y" } else { "n" }.into(),
                    PromptKind::Agent => self.selected.to_string(),
                })));
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(5),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(5),
            code if kind != PromptKind::Text => {
                let count = kind.choices().len();
                match code {
                    KeyCode::Up | KeyCode::Left => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Right => {
                        self.selected = (self.selected + 1).min(count - 1)
                    }
                    KeyCode::Char('y' | 'Y') if kind == PromptKind::Confirm => self.selected = 1,
                    KeyCode::Char('n' | 'N') if kind == PromptKind::Confirm => self.selected = 0,
                    KeyCode::Char(number) if kind == PromptKind::Agent => {
                        if let Some(index) = number
                            .to_digit(10)
                            .filter(|index| (*index as usize) < count)
                        {
                            self.selected = index as usize;
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Char(ch)
                if !ch.is_control()
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && self.text.len() < 4096 =>
            {
                self.text.insert(self.cursor, ch);
                self.cursor += 1;
                self.rejected_paste = false;
            }
            _ => {}
        }
        Ok(Action::Redraw)
    }
}

// Raw mode exists only while asking a question, never during a transaction or
// agent launch. Drop also runs after draw/read errors and unwinding panics.
struct Screen {
    #[cfg(unix)]
    signals: prompt_signals::Guard,
}
impl Screen {
    fn enter() -> io::Result<Self> {
        let guard = Self {
            #[cfg(unix)]
            signals: prompt_signals::Guard::install()?,
        };
        terminal::enable_raw_mode()?;
        SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        execute!(
            io::stderr(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide
        )?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        restore_screen();
        // The signal guard then restores the exact inherited dispositions.
    }
}

#[cfg(unix)]
mod prompt_signals {
    use std::io;
    use std::sync::atomic::{AtomicI32, Ordering};

    static RECEIVED: AtomicI32 = AtomicI32::new(0);

    extern "C" fn record(signal: libc::c_int) {
        RECEIVED.store(signal, Ordering::SeqCst);
    }

    // Unlike unregistering a signal-hook callback, this restores SIG_DFL,
    // SIG_IGN, or an inherited handler before a transaction or child launch.
    pub(super) struct Guard(Vec<(libc::c_int, libc::sigaction)>);

    impl Guard {
        pub(super) fn install() -> io::Result<Self> {
            RECEIVED.store(0, Ordering::SeqCst);
            let mut guard = Self(Vec::with_capacity(3));
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                // SAFETY: initialized action structs; record only performs an
                // atomic store and is async-signal-safe. Failed installation
                // drops the guard and restores any already-installed actions.
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = record as *const () as usize;
                    action.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&mut action.sa_mask);
                    let mut previous = std::mem::zeroed();
                    if libc::sigaction(signal, &action, &mut previous) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    guard.0.push((signal, previous));
                }
            }
            Ok(guard)
        }

        pub(super) fn received(&self) -> i32 {
            RECEIVED.load(Ordering::SeqCst)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            for (signal, previous) in self.0.iter().rev() {
                // SAFETY: previous is the disposition returned for this signal.
                unsafe {
                    libc::sigaction(*signal, previous, std::ptr::null_mut());
                }
            }
        }
    }
}

fn restore_screen() {
    if SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = execute!(
            io::stderr(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show,
            ResetColor
        );
        let _ = terminal::disable_raw_mode();
    }
}

pub(super) fn prompt(context: &str, label: &str, kind: PromptKind) -> Result<Option<String>> {
    pause_progress();
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    let context = safe(context);
    let label = safe(label);
    let mut input = Input::default();
    let color = UI.get().is_some_and(|state| state.policy.color);
    loop {
        terminal.draw(|frame| draw_prompt(frame, &context, &label, kind, &input, color))?;
        loop {
            #[cfg(unix)]
            {
                let signal = _screen.signals.received();
                if signal != 0 {
                    return Err(Interrupted(128 + signal).into());
                }
            }
            if event::poll(Duration::from_millis(100))? {
                break;
            }
        }
        let event = event::read()?;
        let size = terminal.size()?;
        // Do not accept an unseen confirmation when the terminal is too small.
        if (size.width < 30 || size.height < 16)
            && !matches!(event, Event::Key(key) if key.code == KeyCode::Esc || (key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'd'))))
        {
            continue;
        }
        match input.handle(event, kind)? {
            Action::Finish(answer) => return Ok(answer),
            Action::Redraw => {}
        }
    }
}

fn draw_prompt(
    frame: &mut Frame<'_>,
    context: &str,
    label: &str,
    kind: PromptKind,
    input: &Input,
    color: bool,
) {
    let accent = if color {
        Style::default().fg(ACCENT)
    } else {
        Style::default()
    };
    let muted = if color {
        Style::default().fg(MUTED)
    } else {
        Style::default()
    };
    let outer = frame.area();
    if outer.width < 30 || outer.height < 16 {
        frame.render_widget(
            Paragraph::new("Enlarge to 30×16 to continue.\nEsc cancels · or use --plain")
                .wrap(Wrap { trim: false }),
            outer,
        );
        return;
    }
    let width = outer.width.saturating_sub(4).min(88);
    let area = Rect::new(
        (outer.width - width) / 2,
        1,
        width,
        outer.height.saturating_sub(2),
    );
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(if kind == PromptKind::Text {
            3
        } else {
            kind.choices().len() as u16
        }),
        Constraint::Length(2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("R/  ", accent),
            Span::raw("Riftri"),
            Span::styled("  /  guided action", muted),
        ])),
        rows[0],
    );
    // Long paths and prompts remain reviewable, including at narrow widths.
    let paragraph = Paragraph::new(format!("{context}\n\n{label}")).wrap(Wrap { trim: false });
    let total = paragraph.line_count(rows[1].width).min(u16::MAX as usize) as u16;
    let scroll = total
        .saturating_sub(rows[1].height)
        .saturating_sub(input.scroll);
    frame.render_widget(paragraph.scroll((scroll, 0)), rows[1]);
    frame.render_widget(
        Paragraph::new(match kind {
            PromptKind::Text => "Enter a value below",
            PromptKind::Confirm => "Confirm this action",
            PromptKind::Agent => "Open a coding agent",
        })
        .style(accent)
        .wrap(Wrap { trim: false }),
        rows[2],
    );
    if kind == PromptKind::Text {
        let capacity = usize::from(rows[3].width.saturating_sub(3));
        let mut start = input.cursor;
        let mut columns = 0;
        while start > 0 {
            let width = input.text[start - 1].width().unwrap_or(0);
            if columns + width > capacity {
                break;
            }
            columns += width;
            start -= 1;
        }
        let value: String = input.text[start..].iter().collect();
        frame.render_widget(
            Paragraph::new(value).block(Block::default().borders(Borders::ALL).border_style(muted)),
            rows[3],
        );
        frame.set_cursor_position((rows[3].x + 1 + columns as u16, rows[3].y + 1));
    } else {
        let items: Vec<_> = kind
            .choices()
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                ListItem::new(format!(
                    "{} {choice}",
                    if index == input.selected { "›" } else { " " }
                ))
                .style(if index == input.selected {
                    accent
                } else {
                    Style::default()
                })
            })
            .collect();
        frame.render_widget(List::new(items), rows[3]);
    }
    let hint = if input.rejected_paste {
        "Paste rejected: enter one value without control characters."
    } else if kind == PromptKind::Text {
        "Enter continue · Esc cancel · PgUp/PgDn review"
    } else {
        "↑/↓ select · Enter confirm · Esc cancel · PgUp/PgDn review"
    };
    frame.render_widget(
        Paragraph::new(hint).style(muted).wrap(Wrap { trim: false }),
        rows[4],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn machine_plain_ci_and_dumb_terminals_never_animate() {
        for (plain, tty, dumb, ci) in [
            (true, true, false, false),
            (false, false, false, false),
            (false, true, true, false),
            (false, true, false, true),
        ] {
            let policy = Policy::detect(plain, false, tty, dumb, ci, false);
            assert!(!policy.rich && !policy.color && !policy.animate);
        }
        let reduced = Policy::detect(false, true, true, false, false, true);
        assert!(reduced.rich && !reduced.color && !reduced.animate);
    }

    #[test]
    fn confirmation_is_no_by_default_and_escape_never_confirms() {
        let mut input = Input::default();
        assert!(
            matches!(input.handle(key(KeyCode::Enter), PromptKind::Confirm).unwrap(), Action::Finish(Some(value)) if value == "n")
        );
        input
            .handle(key(KeyCode::Down), PromptKind::Confirm)
            .unwrap();
        assert!(matches!(
            input
                .handle(key(KeyCode::Esc), PromptKind::Confirm)
                .unwrap(),
            Action::Finish(None)
        ));
        assert!(
            matches!(input.handle(key(KeyCode::Enter), PromptKind::Confirm).unwrap(), Action::Finish(Some(value)) if value == "y")
        );
    }

    #[test]
    fn input_edits_unicode_and_rejects_multiline_or_control_paste() {
        let mut input = Input::default();
        input
            .handle(Event::Paste("a界é".into()), PromptKind::Text)
            .unwrap();
        input.handle(key(KeyCode::Left), PromptKind::Text).unwrap();
        input
            .handle(key(KeyCode::Backspace), PromptKind::Text)
            .unwrap();
        input
            .handle(Event::Paste("\nYES\x1b".into()), PromptKind::Text)
            .unwrap();
        assert!(input.rejected_paste);
        assert_eq!(input.text.iter().collect::<String>(), "aé");
        assert!(
            input
                .handle(
                    Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                    PromptKind::Text
                )
                .unwrap_err()
                .is::<Interrupted>()
        );
    }

    #[test]
    fn progress_is_width_bounded_and_has_no_invented_percentage() {
        assert_ne!(
            progress_line("waiting", 0, 80, false),
            progress_line("waiting", 1, 80, false)
        );
        for width in [0, 1, 20, 80] {
            for tick in 0..16 {
                let line = progress_line("worktree-add: materializing 界", tick, width, false);
                assert!(line.width() <= width as usize);
                assert!(!line.to_string().contains('%'));
            }
        }
    }

    #[test]
    fn screens_render_at_small_and_large_sizes_without_hiding_default_choice() {
        for (width, height) in [(20, 6), (40, 16), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_prompt(
                        frame,
                        "Repository: /project\nDestination: /task\nNo activation changes.",
                        "Create this worktree?",
                        PromptKind::Confirm,
                        &Input::default(),
                        true,
                    )
                })
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            if width < 30 {
                assert!(text.contains("Enlarge to"));
            } else {
                assert!(text.contains("› No"));
                assert!(text.contains("Create this worktree?"));
            }
        }
    }
}
