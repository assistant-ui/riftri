//! The machine-readable contract that property tests check, shared by the
//! unit tests inside the binary and the tests that drive it from outside.

use std::ffi::OsString;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;
use serde_json::Value;

/// Every key a receipt carries, whatever failed. Harnesses parse receipts with
/// strict schemas, so a key that comes and goes is a contract break.
pub(crate) const RECEIPT_KEYS: [&str; 15] = [
    "category",
    "cleanup",
    "code",
    "message",
    "nativePathEncoding",
    "nextCommand",
    "operation",
    "outcome",
    "phase",
    "recovery",
    "repository",
    "repositoryNativeHex",
    "schemaVersion",
    "stateDirectory",
    "stateDirectoryNativeHex",
];

/// Settings for a property whose cases start processes or touch the
/// filesystem. The case count is what bounds its runtime; Hegel's wall-clock
/// check on generation speed would only turn a busy machine into a failure,
/// which is why its own CI profile suppresses it too. The fixed seed keeps
/// the few cases such a property can afford the same from run to run.
pub(crate) fn process_cases(count: u64) -> hegel::Settings {
    hegel::Settings::new()
        .test_cases(count)
        .derandomize(true)
        .suppress_health_check([hegel::HealthCheck::TooSlow])
}

/// The exit code that mirrors a receipt's `category`.
pub(crate) fn exit_code_for(category: &str) -> i32 {
    match category {
        "operational" => 1,
        "usage" => 2,
        "policy" => 3,
        other => panic!("unknown receipt category {other}"),
    }
}

pub(crate) fn assert_receipt_keys(receipt: &Value) {
    let mut keys = receipt
        .as_object()
        .expect("receipt object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, RECEIPT_KEYS);
}

/// A path of arbitrary native bytes: quotes, spaces, control characters, and
/// (on Unix) sequences that are not UTF-8.
pub(crate) fn native_path(tc: &TestCase) -> PathBuf {
    #[cfg(unix)]
    if tc.draw(gs::booleans()) {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = tc.draw(gs::binary().min_size(1).max_size(24));
        for byte in &mut bytes {
            if *byte == 0 {
                *byte = b'/';
            }
        }
        return PathBuf::from(OsString::from_vec(bytes));
    }
    let text = tc.draw(gs::text().min_size(1).max_size(24));
    PathBuf::from(text.replace('\0', "/"))
}

pub(crate) fn decode_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "odd-length hex: {hex}");
    assert!(
        hex.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "hex is lowercase digits only: {hex}"
    );
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex digit pair"))
        .collect()
}

#[cfg(unix)]
pub(crate) fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(windows)]
pub(crate) fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    PathBuf::from(OsString::from_wide(&wide))
}

/// Check what every `--json` report promises: snake_case keys, and beside
/// each lossy display string the exact bytes it stands for.
pub(crate) fn check_report_value(value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                assert!(
                    key.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'),
                    "report key {key} is not snake_case"
                );
                let twin = key
                    .strip_suffix("_native_hex")
                    .map(|display| (display, true))
                    .or_else(|| key.strip_suffix("_hex").map(|display| (display, false)));
                if let Some((display, is_path)) = twin {
                    let display = map
                        .get(display)
                        .unwrap_or_else(|| panic!("{key} has no display twin"));
                    assert_eq!(display.is_null(), nested.is_null(), "{key}");
                    if let Some(hex) = nested.as_str() {
                        let bytes = decode_hex(hex);
                        let expected = if is_path {
                            path_from_bytes(bytes).display().to_string()
                        } else {
                            String::from_utf8_lossy(&bytes).into_owned()
                        };
                        assert_eq!(display.as_str().expect("display string"), expected, "{key}");
                    }
                }
                check_report_value(nested);
            }
        }
        Value::Array(items) => items.iter().for_each(check_report_value),
        _ => {}
    }
}
