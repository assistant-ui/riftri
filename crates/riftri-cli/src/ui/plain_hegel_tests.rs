//! Property tests for the plain build's terminal-safety escaping.

use hegel::TestCase;
use hegel::generators as gs;

use super::{safe, write_transcript};

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
            ])));
        } else {
            text.push_str(&tc.draw(gs::text().max_size(8)));
        }
    }
    text
}

#[hegel::test]
fn escaping_leaves_no_control_character_but_the_newline(tc: TestCase) {
    let text = terminal_text(&tc);

    let escaped = safe(&text);

    assert!(
        !escaped
            .chars()
            .any(|character| character.is_control() && character != '\n'),
        "{escaped:?}"
    );
    // Line structure survives, so multi-line reports still wrap as authored.
    assert_eq!(escaped.matches('\n').count(), text.matches('\n').count());
    if !text
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        assert_eq!(escaped, text);
    }
    // Already-safe text is never escaped twice.
    assert_eq!(safe(&escaped), escaped);
}

#[hegel::test]
fn the_setup_transcript_is_escaped_like_every_other_line(tc: TestCase) {
    let text = terminal_text(&tc);

    let mut output = Vec::new();
    write_transcript(&mut output, &text).expect("write to memory");

    assert_eq!(String::from_utf8(output).expect("transcript"), safe(&text));
}
