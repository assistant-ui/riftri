use hegel::TestCase;
use hegel::generators as gs;

use super::{parse_umask, writable_bits_for_umask};

#[hegel::test]
fn every_mask_parses_back_from_shell_output(tc: TestCase) {
    let mask = tc.draw(gs::integers::<u32>().max_value(0o777));
    let width = tc.draw(gs::integers::<usize>().max_value(40));
    let padding = || tc.draw(gs::sampled_from(vec!["", " ", "\t", "\n", "\r\n"]));
    // Shells print `0022`, `022`, or `22` depending on the implementation.
    let output = format!("{}{mask:0width$o}{}", padding(), padding());

    assert_eq!(parse_umask(output.as_bytes()).expect("valid mask"), mask);
}

#[hegel::test]
fn only_octal_permission_masks_are_accepted(tc: TestCase) {
    let bytes = if tc.draw(gs::booleans()) {
        tc.draw(gs::binary().max_size(8))
    } else {
        tc.draw(gs::text().alphabet("01234567 89\n+-").max_size(8))
            .into_bytes()
    };

    let digits = std::str::from_utf8(&bytes).ok().map(str::trim);
    let significant = digits.map(|digits| digits.trim_start_matches('0'));
    let is_mask = digits.is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| (b'0'..=b'7').contains(&byte))
    }) && significant.is_some_and(|digits| digits.len() <= 3);

    match parse_umask(&bytes) {
        Ok(mask) => {
            assert!(is_mask, "{bytes:?} parsed as {mask:o}");
            let significant = significant.unwrap();
            let canonical = if significant.is_empty() {
                "0"
            } else {
                significant
            };
            assert_eq!(format!("{mask:o}"), canonical);
        }
        Err(_) => assert!(!is_mask, "{bytes:?} was rejected"),
    }
}

#[hegel::test]
fn restored_write_bits_reproduce_a_normal_checkout(tc: TestCase) {
    let umask = tc.draw(gs::integers::<u32>().max_value(0o777));
    // Git creates files as 0666 or 0777 and directories as 0777.
    let requested = tc.draw(gs::sampled_from(vec![0o666_u32, 0o777]));
    let checked_out = requested & !umask;

    let protected = checked_out & !0o222;
    assert_eq!(protected | writable_bits_for_umask(umask), checked_out);
}
