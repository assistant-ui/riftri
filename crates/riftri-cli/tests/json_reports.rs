//! Cross-platform JSON report coverage for read-only lifecycle commands.
//!
//! These tests run against an explicit empty state directory so they exercise
//! the stable JSON shapes on every supported platform without requiring a
//! copy-on-write capable volume.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn riftri(current_directory: &Path, arguments: &[&str], state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(arguments)
        .args(["--state-dir"])
        .arg(state)
        .current_dir(current_directory)
        .output()
        .expect("run Riftri CLI")
}

#[test]
fn status_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["status", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse status JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["operations"]["active_views"], 0);
    assert_eq!(report["operations"]["pending_adds"], 0);
    assert_eq!(report["operations"]["coordination_locks"], 0);
    assert_eq!(report["bases"], serde_json::json!([]));
    assert_eq!(report["worktrees"], serde_json::json!([]));
    assert_eq!(report["diagnostic_issues"], serde_json::json!([]));
    assert_eq!(report["total_logical_bytes"], 0);
    assert_eq!(report["total_allocated_bytes"], 0);
}

#[test]
fn repair_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["repair", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri repair failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse repair JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["scanned"], 0);
    assert_eq!(report["busy_adds"], 0);
    assert_eq!(report["recovered_adds"], 0);
    assert_eq!(report["retired_adds"], 0);
    assert_eq!(report["relocated_worktrees"], serde_json::json!([]));
    assert_eq!(report["reaped_artifacts"], serde_json::json!([]));
    assert_eq!(report["reaped_probe_roots"], serde_json::json!([]));
    assert_eq!(report["preserved_probe_mounts"], serde_json::json!([]));
    assert_eq!(report["errors"], serde_json::json!([]));
}

#[test]
fn gc_plan_emits_a_stable_json_report_for_an_empty_state_directory() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let output = riftri(fixture.path(), &["gc", ".", "--json"], &state);
    assert!(
        output.status.success(),
        "riftri gc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse gc JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], state.to_string_lossy().as_ref());
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["applied"], false);
    assert_eq!(report["candidates"], serde_json::json!([]));
    assert_eq!(report["collected"], serde_json::json!([]));
    assert_eq!(report["skipped_in_use"], serde_json::json!([]));
    assert_eq!(report["skipped_protected"], serde_json::json!([]));
    assert_eq!(report["removed_logical_bytes"], 0);
    assert_eq!(report["removed_allocated_bytes"], 0);
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
        .collect()
}

#[test]
fn backends_emits_a_stable_json_report_with_native_path_fields() {
    let fixture = tempdir().expect("fixture directory");
    let destination = fixture.path().join("proposed-worktree");

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("backends")
        .arg(&destination)
        .arg("--json")
        .current_dir(fixture.path())
        .output()
        .expect("run Riftri CLI");
    assert!(
        output.status.success(),
        "riftri backends failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse backends JSON");
    assert_eq!(report["schema_version"], 1);
    assert!(report["native_path_encoding"].is_string());
    assert_eq!(report["requested_path"], destination.display().to_string());
    assert!(report["requested_path_native_hex"].is_string());
    let capabilities = report["storage_capabilities"]
        .as_array()
        .expect("capabilities array");
    assert!(!capabilities.is_empty());
    for capability in capabilities {
        assert!(capability["kind"].is_string());
        assert!(capability["status"].is_string());
        assert!(capability["explanation"].is_string());
        assert!(capability["requires_explicit_fallback"].is_boolean());
        if let Some(volume) = capability.get("volume") {
            assert_eq!(volume["requested_path"], destination.display().to_string());
            assert!(volume["requested_path_native_hex"].is_string());
            assert!(volume["probe_path"].is_string());
            assert!(volume["probe_path_native_hex"].is_string());
            assert!(volume["identity"]["filesystem"].is_string());
            assert!(volume["read_only"].is_boolean());
        }
    }
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn backends_json_preserves_non_utf8_destination_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fixture = tempdir().expect("fixture directory");
    let destination = fixture
        .path()
        .join(OsStr::from_bytes(b"riftri-invalid-\xff"));

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("backends")
        .arg(&destination)
        .arg("--json")
        .current_dir(fixture.path())
        .output()
        .expect("run backends with a native path");
    assert!(
        output.status.success(),
        "riftri backends failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse backends JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["native_path_encoding"], "unix-bytes-hex");
    let display = report["requested_path"].as_str().expect("requested path");
    assert!(display.ends_with("riftri-invalid-\u{fffd}"));
    let requested_hex = report["requested_path_native_hex"]
        .as_str()
        .expect("requested path hex");
    assert_eq!(
        decode_hex(requested_hex),
        destination.as_os_str().as_bytes()
    );
    let probe_bytes = fs::canonicalize(fixture.path())
        .expect("canonical fixture path")
        .as_os_str()
        .as_bytes()
        .to_vec();
    for capability in report["storage_capabilities"]
        .as_array()
        .expect("capabilities array")
    {
        if let Some(volume) = capability.get("volume") {
            let volume_hex = volume["requested_path_native_hex"]
                .as_str()
                .expect("volume requested path hex");
            assert_eq!(decode_hex(volume_hex), destination.as_os_str().as_bytes());
            let probe_hex = volume["probe_path_native_hex"]
                .as_str()
                .expect("volume probe path hex");
            assert_eq!(decode_hex(probe_hex), probe_bytes);
        }
    }
    assert!(!destination.exists());
}

#[cfg(windows)]
#[test]
fn backends_json_preserves_unpaired_surrogate_destination_paths() {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let fixture = tempdir().expect("fixture directory");
    // 0xD800 is an unpaired surrogate: valid in Windows paths, invalid UTF-8.
    let mut name: Vec<u16> = "riftri-invalid-".encode_utf16().collect();
    name.push(0xD800);
    let destination = fixture.path().join(OsString::from_wide(&name));

    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .arg("backends")
        .arg(&destination)
        .arg("--json")
        .current_dir(fixture.path())
        .output()
        .expect("run backends with a native path");
    assert!(
        output.status.success(),
        "riftri backends failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse backends JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["native_path_encoding"], "windows-utf16le-hex");
    let display = report["requested_path"].as_str().expect("requested path");
    assert!(display.ends_with("riftri-invalid-\u{fffd}"));
    let expected_bytes: Vec<u8> = destination
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    let requested_hex = report["requested_path_native_hex"]
        .as_str()
        .expect("requested path hex");
    assert_eq!(decode_hex(requested_hex), expected_bytes);
    assert!(!destination.exists());
}
