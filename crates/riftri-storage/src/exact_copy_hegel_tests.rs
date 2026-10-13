use std::io::{Cursor, ErrorKind};

use hegel::TestCase;
use hegel::generators as gs;

use super::copy_exact;

#[hegel::test]
fn copies_exactly_the_requested_prefix_or_reports_the_shortfall(tc: TestCase) {
    // Repeating a small chunk reaches sizes past the copy buffer cheaply.
    let chunk = tc.draw(gs::binary().max_size(64));
    let contents = chunk.repeat(tc.draw(gs::integers::<usize>().max_value(400)));
    let available = contents.len() as u64;
    let expected = if tc.draw(gs::booleans()) {
        tc.draw(gs::integers::<u64>().max_value(available + 16))
    } else {
        tc.draw(gs::integers::<u64>())
    };
    let mut reader = Cursor::new(&contents);
    let mut written = Vec::new();

    let result = copy_exact(&mut reader, &mut written, expected);

    let copied = expected.min(available);
    assert_eq!(written, contents[..copied as usize]);
    assert_eq!(
        reader.position(),
        copied,
        "the copy must not read past the expected length"
    );
    match result {
        Ok(()) => assert!(expected <= available),
        Err(error) => {
            assert!(expected > available);
            assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
            assert_eq!(
                error.to_string(),
                format!("source ended after {available} of {expected} expected bytes")
            );
        }
    }
}
