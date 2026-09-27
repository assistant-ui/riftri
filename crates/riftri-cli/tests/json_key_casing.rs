//! The two machine-readable surfaces use different key casing, and each must
//! stay internally consistent.
//!
//! Success reports (`--json`) spell keys in `snake_case`; failure receipts
//! (`--json-errors`) spell them in `camelCase`. Six receipt keys have exact
//! `snake_case` twins in the report surface — `schemaVersion`,
//! `nativePathEncoding`, `stateDirectory`, `stateDirectoryNativeHex`,
//! `repositoryNativeHex` and `nextCommand` — so the split is easy to cross
//! without noticing, in either direction.
//!
//! These tests do not endorse the split. They pin it, so that a new key cannot
//! quietly land in the wrong style while the question of unifying the two is
//! still open, and so that unifying them later is a deliberate, visible change
//! rather than a drift nobody reviewed.

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

/// Every object key in the document, at every depth, including inside arrays.
fn collect_keys(value: &serde_json::Value, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                found.push(key.clone());
                collect_keys(nested, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_keys(item, found);
            }
        }
        _ => {}
    }
}

fn keys_of(document: &serde_json::Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_keys(document, &mut found);
    found.sort();
    found.dedup();
    assert!(!found.is_empty(), "the document carried no keys at all");
    found
}

#[test]
fn success_reports_spell_every_key_in_snake_case() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    // One case per read-only reporting command, so a new key in any of them is
    // covered without waiting for that command to grow its own test. `backends`
    // and `doctor` inspect a destination rather than a state directory and do
    // not accept `--state-dir`.
    let state_scoped = [
        vec!["status", ".", "--json"],
        vec!["repair", ".", "--json"],
        vec!["gc", ".", "--json"],
    ];
    let destination_scoped = [vec!["backends", ".", "--json"], vec!["doctor", "--json"]];

    for arguments in state_scoped.iter().chain(destination_scoped.iter()) {
        let output = if destination_scoped.contains(arguments) {
            Command::new(env!("CARGO_BIN_EXE_riftri"))
                .args(arguments)
                .current_dir(fixture.path())
                .output()
                .expect("run Riftri CLI")
        } else {
            riftri(fixture.path(), arguments, &state)
        };
        assert!(
            output.status.success(),
            "riftri {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("parse {arguments:?} JSON: {error}"));

        for key in keys_of(&report) {
            assert!(
                !key.chars().any(|character| character.is_ascii_uppercase()),
                "riftri {arguments:?} emits `{key}`; success reports are snake_case",
            );
        }
    }
}

#[test]
fn failure_receipts_spell_every_key_in_camel_case() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    // The fixture is not a repository, so this refuses and emits one receipt.
    let output = riftri(
        fixture.path(),
        &[
            "worktree",
            "add",
            "./created",
            "-b",
            "branch",
            "main",
            "--json-errors",
        ],
        &state,
    );
    assert!(
        !output.status.success(),
        "the refusal should not have succeeded"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("parse receipt JSON from stderr");

    for key in keys_of(&receipt) {
        assert!(
            !key.contains('_'),
            "the failure receipt emits `{key}`; receipts are camelCase",
        );
    }
}

#[test]
fn the_two_surfaces_still_disagree_in_exactly_the_known_places() {
    let fixture = tempdir().expect("fixture directory");
    let state = fixture.path().join("state");
    fs::create_dir(&state).expect("create state directory");

    let report = riftri(fixture.path(), &["status", ".", "--json"], &state);
    let report: serde_json::Value =
        serde_json::from_slice(&report.stdout).expect("parse status JSON");

    let receipt = riftri(
        fixture.path(),
        &[
            "worktree",
            "add",
            "./created",
            "-b",
            "branch",
            "main",
            "--json-errors",
        ],
        &state,
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&receipt.stderr).expect("parse receipt JSON");

    // Spelled out rather than derived: if unifying the surfaces lands, this
    // list is what has to change, and the failure names each survivor.
    let expected_twins = [
        ("schemaVersion", "schema_version"),
        ("nativePathEncoding", "native_path_encoding"),
        ("stateDirectory", "state_directory"),
        ("stateDirectoryNativeHex", "state_directory_native_hex"),
    ];

    let report_keys = keys_of(&report);
    let receipt_keys = keys_of(&receipt);

    for (camel, snake) in expected_twins {
        assert!(
            receipt_keys.iter().any(|key| key == camel),
            "the receipt no longer carries `{camel}`; update this list if the \
             surfaces were unified",
        );
        assert!(
            report_keys.iter().any(|key| key == snake),
            "the status report no longer carries `{snake}`; update this list if \
             the surfaces were unified",
        );
    }
}
