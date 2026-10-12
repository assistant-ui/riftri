//! Property tests for failure receipts, exit codes, and command-line parsing.

use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_core::WorktreeError;

#[path = "../tests/support/contract.rs"]
mod contract;
pub(super) use contract::assert_receipt_keys;
use contract::exit_code_for;
pub(crate) use contract::process_cases;
pub(super) use contract::{check_report_value, decode_hex, native_path, path_from_bytes};

use super::{
    InvocationContext, display_byte_count, encode_hex, failure_exit_code, failure_receipt,
    native_path_hex, resolve_state_directory, ui,
};

const OPERATIONS: [&str; 12] = [
    "setup",
    "enable",
    "exec",
    "status",
    "repair",
    "garbage-collection",
    "worktree-add",
    "worktree-remove",
    "worktree-move",
    "worktree-compact",
    "worktree-prune",
    "worktree-list",
];

pub(super) fn is_kebab_case(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
}

fn add_phase(tc: &TestCase) -> riftri_core::AddWorktreePhase {
    use riftri_core::AddWorktreePhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        GitMetadataCreated,
        BaseReady,
        ViewCreated,
        GitPointerRestored,
        IndexSynchronized,
        CleanVerified,
        Active,
        RollbackPending,
        RolledBack,
    ]))
}

fn remove_phase(tc: &TestCase) -> riftri_core::RemoveWorktreePhase {
    use riftri_core::RemoveWorktreePhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        CleanVerified,
        WorktreeRemoved,
        BaseReleased,
        Complete,
        Cancelled,
    ]))
}

fn move_phase(tc: &TestCase) -> riftri_core::MoveWorktreePhase {
    use riftri_core::MoveWorktreePhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        WorktreeMoved,
        AddJournalUpdated,
        Complete,
        Cancelled,
    ]))
}

fn compact_phase(tc: &TestCase) -> riftri_core::CompactWorktreePhase {
    use riftri_core::CompactWorktreePhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        ReplacementReady,
        ReplacementActivated,
        AddJournalUpdated,
        Complete,
        Cancelled,
        RestoringOriginal,
    ]))
}

fn prune_phase(tc: &TestCase) -> riftri_core::PruneWorktreesPhase {
    use riftri_core::PruneWorktreesPhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        GitMetadataPruned,
        Complete,
    ]))
}

fn collection_phase(tc: &TestCase) -> riftri_core::GarbageCollectionPhase {
    use riftri_core::GarbageCollectionPhase::*;
    tc.draw(gs::sampled_from(vec![
        IntentRecorded,
        MarkerRemoved,
        BaseQuarantined,
        Complete,
        Cancelled,
    ]))
}

fn io_error(tc: &TestCase) -> std::io::Error {
    use std::io::ErrorKind::*;
    std::io::Error::from(tc.draw(gs::sampled_from(vec![
        StorageFull,
        PermissionDenied,
        NotFound,
        Other,
    ])))
}

fn worktree_error(tc: &TestCase, depth: u8) -> WorktreeError {
    let message = tc.draw(gs::text().max_size(40));
    match tc.draw(gs::integers::<u8>().max_value(25)) {
        0 => WorktreeError::Git(riftri_core::GitError::CommandFailed {
            arguments: "checkout-index --all".to_owned(),
            stderr: message,
            disposition: "exit code 128".to_owned(),
        }),
        1 => WorktreeError::Git(riftri_core::GitError::RepositoryAbsent {
            path: native_path(tc),
        }),
        2 => WorktreeError::Storage(riftri_storage::StorageError::Io {
            operation: "clone",
            path: native_path(tc),
            source: io_error(tc),
        }),
        3 => WorktreeError::JournalTransition(riftri_core::JournalTransitionError {
            current: add_phase(tc),
            requested: add_phase(tc),
        }),
        4 => WorktreeError::RemoveJournalTransition(riftri_core::RemoveJournalTransitionError {
            current: remove_phase(tc),
            requested: remove_phase(tc),
        }),
        5 => WorktreeError::MoveJournalTransition(riftri_core::MoveJournalTransitionError {
            current: move_phase(tc),
            requested: move_phase(tc),
        }),
        6 => WorktreeError::CompactJournalTransition(riftri_core::CompactJournalTransitionError {
            current: compact_phase(tc),
            requested: compact_phase(tc),
        }),
        7 => WorktreeError::PruneJournalTransition(riftri_core::PruneJournalTransitionError {
            current: prune_phase(tc),
            requested: prune_phase(tc),
        }),
        8 => WorktreeError::Unsupported(message),
        9 => WorktreeError::InvalidRequest(message),
        10 => WorktreeError::UnbornHead(message),
        11 => WorktreeError::BranchAlreadyExists(message),
        12 => WorktreeError::BranchCheckedOut(message),
        13 => WorktreeError::DestinationExists(message),
        14 => WorktreeError::SymlinkedBaseParent {
            message,
            state_directory: native_path(tc),
        },
        15 => WorktreeError::StaleStateRegistration {
            message,
            repository: native_path(tc),
            state_directory: native_path(tc),
        },
        16 => WorktreeError::RecoveryPending {
            message,
            state_directory: native_path(tc),
        },
        17 => WorktreeError::Busy { message },
        18 => WorktreeError::Io {
            operation: "flush operation journal",
            path: native_path(tc),
            source: io_error(tc),
        },
        19 if depth < 2 => WorktreeError::OperationAndRollback {
            operation: Box::new(worktree_error(tc, depth + 1)),
            rollback: Box::new(worktree_error(tc, depth + 1)),
        },
        19 | 20 => WorktreeError::InjectedFailure(add_phase(tc)),
        21 => WorktreeError::InjectedRemovalFailure(remove_phase(tc)),
        22 => WorktreeError::InjectedMoveFailure(move_phase(tc)),
        23 => WorktreeError::InjectedCompactionFailure(compact_phase(tc)),
        24 => WorktreeError::InjectedPruneFailure(prune_phase(tc)),
        _ => WorktreeError::InjectedCollectionFailure(collection_phase(tc)),
    }
}

/// A failure as `run` can return it: a lifecycle error, an activation error,
/// an interrupted prompt, or free text, each optionally under added context.
fn failure(tc: &TestCase) -> anyhow::Error {
    use riftri_core::ActivationError;
    let error = match tc.draw(gs::integers::<u8>().max_value(9)) {
        0 => anyhow::anyhow!(tc.draw(gs::text().max_size(40))),
        1 => anyhow::Error::new(ActivationError::BareRepository),
        2 => anyhow::Error::new(ActivationError::Git(
            riftri_core::GitError::RepositoryAbsent {
                path: native_path(tc),
            },
        )),
        3 => anyhow::Error::new(ActivationError::WorktreeBinding(
            tc.draw(gs::text().max_size(40)),
        )),
        4 => anyhow::Error::new(ActivationError::Process(tc.draw(gs::text().max_size(40)))),
        5 => anyhow::Error::new(io_error(tc)),
        _ => anyhow::Error::new(worktree_error(tc, 0)),
    };
    if tc.draw(gs::booleans()) {
        error.context(tc.draw(gs::text().max_size(40)))
    } else {
        error
    }
}

fn invocation_context(tc: &TestCase) -> InvocationContext {
    InvocationContext {
        repository: tc.draw(gs::booleans()).then(|| native_path(tc)),
        state_directory: tc.draw(gs::booleans()).then(|| native_path(tc)),
    }
}

/// The path a receipt names in `field`, decoded from its native-hex twin.
fn receipt_path(receipt: &serde_json::Value, field: &str) -> Option<PathBuf> {
    let display = &receipt[field];
    let hex = &receipt[format!("{field}NativeHex")];
    assert_eq!(
        display.is_null(),
        hex.is_null(),
        "{field} and its hex twin are present together"
    );
    let bytes = decode_hex(hex.as_str()?);
    let path = path_from_bytes(bytes);
    assert_eq!(
        display.as_str().expect("display path"),
        path.display().to_string(),
        "{field} is the lossy rendering of its exact bytes"
    );
    Some(path)
}

/// What `/bin/sh` prints for `script`, run where generated text can do no
/// harm: an empty directory, no environment, and so no program to find.
#[cfg(unix)]
pub(crate) fn shell_output(script: &str) -> Vec<u8> {
    let sandbox = tempfile::tempdir().expect("shell sandbox");
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .current_dir(sandbox.path())
        .env_clear()
        .output()
        .expect("run sh");
    assert!(output.status.success(), "sh rejected {script:?}");
    output.stdout
}

/// The arguments a POSIX shell hands to `riftri` when it runs `command`.
#[cfg(unix)]
fn shell_arguments(command: &str) -> Vec<PathBuf> {
    let printed = shell_output(&format!(
        "riftri() {{ printf '%s\\0' \"$@\"; }}\n{command}\n"
    ));
    let mut arguments = printed
        .split(|byte| *byte == 0)
        .map(|argument| path_from_bytes(argument.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(arguments.pop(), Some(PathBuf::new()));
    arguments
}

#[hegel::test(process_cases(1000))]
fn receipts_keep_one_shape_and_agree_with_the_exit_code(tc: TestCase) {
    let operation = tc.draw(gs::sampled_from(OPERATIONS.to_vec()));
    let context = invocation_context(&tc);
    let interrupted = (tc.draw(gs::integers::<u8>().max_value(9)) == 0)
        .then(|| tc.draw(gs::integers::<i32>().min_value(129).max_value(255)));
    let error = match interrupted {
        Some(status) => anyhow::Error::new(ui::Interrupted(status)),
        None => failure(&tc),
    };

    let receipt = failure_receipt(operation, &error, &context);
    let exit_code = failure_exit_code(&error);

    assert_receipt_keys(&receipt);
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(receipt["operation"], operation);
    assert_eq!(receipt["message"], error.to_string());

    let category = receipt["category"].as_str().expect("category");
    match interrupted {
        // A cancelled prompt exits with the shell's signal convention.
        Some(status) => assert_eq!(exit_code, status),
        None => {
            assert_ne!(category, "usage", "a parsed command is not a usage error");
            assert_eq!(exit_code, exit_code_for(category));
        }
    }

    let code = receipt["code"].as_str().expect("code");
    assert!(is_kebab_case(code), "{code}");
    if let Some(phase) = receipt["phase"].as_str() {
        assert!(is_kebab_case(phase), "{phase}");
    } else {
        assert!(receipt["phase"].is_null());
    }
    let cleanup = receipt["cleanup"].as_str().expect("cleanup");
    assert!(["not-needed", "unknown", "rolled-back"].contains(&cleanup));
    let recovery = receipt["recovery"].as_str().expect("recovery");
    assert!(["not-required", "required", "inspect", "retry", "unknown"].contains(&recovery));
    if category == "policy" {
        // Nothing was attempted, so there is nothing to clean up.
        assert_eq!(cleanup, "not-needed");
    }
    if code == "storage-full" {
        assert_eq!(category, "operational");
    }

    let repository = receipt_path(&receipt, "repository");
    let state_directory = receipt_path(&receipt, "stateDirectory");
    let next_command = receipt["nextCommand"].as_str();
    assert!(next_command.is_some() || receipt["nextCommand"].is_null());
    if operation == "repair" {
        // Answering a failed repair with repair is a retry loop.
        assert_ne!(recovery, "required");
        assert!(!next_command.is_some_and(|command| command.starts_with("riftri repair")));
    }

    let actionable = matches!(recovery, "required" | "inspect");
    if !actionable {
        assert_eq!(next_command, None);
        return;
    }
    // A command is withheld exactly when a path it must name cannot be
    // written as one shell argument.
    let quotable = |path: &Option<PathBuf>| {
        path.as_deref()
            .is_none_or(|path| riftri_core::shell_quoted_path(path).is_some())
    };
    let expressible = if code == "stale-state-registration" {
        quotable(&repository) && quotable(&state_directory)
    } else if state_directory.is_some() {
        quotable(&state_directory)
    } else {
        quotable(&repository)
    };
    assert_eq!(next_command.is_some(), expressible, "{receipt}");

    #[cfg(unix)]
    if let Some(command) = next_command {
        let arguments = shell_arguments(command);
        let words = arguments
            .iter()
            .map(|argument| argument.to_str())
            .collect::<Vec<_>>();
        match (words.as_slice(), arguments.as_slice()) {
            ([Some("repair" | "status")], _) => {
                assert_eq!((repository, state_directory), (None, None));
            }
            ([Some("repair" | "status"), Some("--state-dir"), _], [_, _, path]) => {
                assert_eq!(Some(path), state_directory.as_ref());
            }
            ([Some("repair" | "status"), Some("--repository"), _], [_, _, path]) => {
                assert_eq!(state_directory, None);
                assert_eq!(Some(path), repository.as_ref());
            }
            (
                [
                    Some("state"),
                    Some("unregister"),
                    _,
                    Some("--repository"),
                    _,
                ],
                [_, _, state, _, selected],
            ) => {
                assert_eq!(Some(state), state_directory.as_ref());
                assert_eq!(Some(selected), repository.as_ref());
            }
            _ => panic!("unexpected next command {command:?}"),
        }
    }
}

#[hegel::test]
fn native_hex_round_trips_exact_bytes(tc: TestCase) {
    let bytes = tc.draw(gs::binary().max_size(64));
    assert_eq!(decode_hex(&encode_hex(&bytes)), bytes);

    let path = native_path(&tc);
    assert_eq!(path_from_bytes(decode_hex(&native_path_hex(&path))), path);
}

/// The magnitude a rendered byte count stands for, in bytes.
fn rendered_magnitude(rendered: &str, bytes: u64) -> f64 {
    let Some(scaled) = rendered.strip_prefix(&format!("{bytes} bytes (")) else {
        assert_eq!(rendered, format!("{bytes} bytes"));
        return bytes as f64;
    };
    let (value, unit) = scaled
        .strip_suffix(')')
        .and_then(|scaled| scaled.split_once(' '))
        .expect("scaled value and unit");
    let power = ["KiB", "MiB", "GiB", "TiB", "PiB"]
        .iter()
        .position(|candidate| *candidate == unit)
        .expect("binary unit");
    let value = value.parse::<f64>().expect("scaled value");
    assert!(value >= 1.0, "{rendered}");
    assert!(value <= 1024.0 || unit == "PiB", "{rendered}");
    value * 1024f64.powi(power as i32 + 1)
}

#[hegel::test]
fn byte_counts_keep_the_exact_value_and_a_faithful_scale(tc: TestCase) {
    let smaller = tc.draw(gs::integers::<u64>());
    let larger = tc.draw(gs::integers::<u64>().min_value(smaller));

    let rendered = display_byte_count(smaller);
    assert_eq!(rendered.contains('('), smaller >= 1024);
    let magnitude = rendered_magnitude(&rendered, smaller);
    // One decimal place below 100 units bounds the rounding error at 5%.
    let error = (magnitude - smaller as f64).abs();
    assert!(error <= smaller as f64 * 0.05, "{rendered}");
    // Rounding never reorders two counts.
    assert!(magnitude <= rendered_magnitude(&display_byte_count(larger), larger));
}

#[hegel::test(process_cases(40))]
fn an_explicit_state_directory_is_never_assumed_empty(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    std::fs::create_dir(fixture.path().join("directory")).expect("create directory");
    std::fs::write(fixture.path().join("file"), "").expect("create file");
    let mut selected = fixture.path().to_path_buf();
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(3)) {
        selected.push(match tc.draw(gs::integers::<u8>().max_value(3)) {
            0 => "directory".to_owned(),
            1 => "file".to_owned(),
            2 => "missing".to_owned(),
            // Longer than any filesystem's name limit.
            _ => "n".repeat(tc.draw(gs::integers::<usize>().min_value(256).max_value(600))),
        });
    }

    let resolved = resolve_state_directory(fixture.path(), Some(selected.clone()));

    match std::fs::symlink_metadata(&selected) {
        Ok(_) => assert_eq!(resolved.expect("an existing path resolves"), selected),
        // Core refuses a path through a regular file with its own diagnostic.
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
            assert_eq!(resolved.expect("left to core"), selected);
        }
        Err(error) => {
            let refusal = resolved.expect_err("an uninspected directory is not an all-clear");
            let missing = error.kind() == std::io::ErrorKind::NotFound;
            assert_eq!(failure_exit_code(&refusal), if missing { 3 } else { 1 });
            assert!(
                format!("{refusal:#}").contains(&selected.display().to_string()),
                "{refusal:#}"
            );
        }
    }
}

/// The name a receipt gives `phase`, read back from an injected failure.
fn receipt_phase(error: WorktreeError) -> serde_json::Value {
    let receipt = failure_receipt(
        "worktree-add",
        &anyhow::Error::new(error),
        &InvocationContext::default(),
    );
    assert_eq!(receipt["code"], "injected-failure");
    receipt["phase"].clone()
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn receipts_name_a_phase_exactly_as_its_journal_does(tc: TestCase) {
    // Journals store each phase under its serialized name, and a harness
    // reads both, so the two spellings must be one.
    let phase = add_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
    let phase = remove_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedRemovalFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
    let phase = move_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedMoveFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
    let phase = compact_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedCompactionFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
    let phase = prune_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedPruneFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
    let phase = collection_phase(&tc);
    assert_eq!(
        receipt_phase(WorktreeError::InjectedCollectionFailure(phase)),
        serde_json::to_value(phase).expect("serialize phase")
    );
}
