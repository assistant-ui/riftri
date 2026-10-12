//! Property tests for the destination-readiness verdict behind `doctor`.
//!
//! The verdict is assembled from independent diagnostics. Whatever each of
//! them reports, every finding becomes exactly one blocker, the status
//! follows from the blockers alone, and the suggested next command names the
//! exact paths involved or is withheld.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_git::{GitInfo, ObjectId, RepositoryIdentity, RepositoryInfo};

use super::{
    DestinationReadinessStatus, Diagnostic, RepositoryCompatibilityBlocker,
    RepositoryCompatibilityBlockerKind, RepositoryCompatibilityReport, destination_readiness,
    shell_quoted_path,
};

/// A path as raw bytes, most of them representable as a shell argument.
fn path(tc: &TestCase) -> PathBuf {
    let mut bytes = if tc.draw(gs::integers::<u8>().max_value(4)) > 0 {
        tc.draw(gs::text().alphabet("ab/ '$\u{e9}").min_size(1).max_size(8))
            .into_bytes()
    } else {
        tc.draw(gs::binary().min_size(1).max_size(8))
    };
    bytes.retain(|byte| *byte != 0);
    bytes.insert(0, b'/');
    PathBuf::from(OsString::from_vec(bytes))
}

fn object_id() -> ObjectId {
    ObjectId::parse("0".repeat(40)).expect("object ID")
}

fn repository(tc: &TestCase) -> RepositoryInfo {
    let is_bare = tc.draw(gs::booleans());
    RepositoryInfo {
        root: (!is_bare).then(|| path(tc)),
        identity: RepositoryIdentity {
            common_git_dir: path(tc),
        },
        is_bare,
        head_commit: Some(object_id()),
        head_tree: Some(object_id()),
        clean: (!is_bare).then_some(true),
    }
}

#[hegel::test]
fn readiness_is_decided_by_its_blockers_alone(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture");
    let name = tc.draw(gs::text().alphabet("ab '$\n").min_size(1).max_size(6));
    let destination = fixture.path().join(name);
    let occupied = tc.draw(gs::booleans());
    if occupied {
        fs::create_dir(&destination).expect("occupied destination");
        fs::write(destination.join("kept"), b"kept").expect("occupant");
    }
    let explicit = tc.draw(gs::booleans());

    let failure = |tc: &TestCase| tc.draw(gs::text().alphabet("xyz ").min_size(1).max_size(6));
    let git = if tc.draw(gs::integers::<u8>().max_value(4)) > 0 {
        Diagnostic::success(GitInfo {
            command: PathBuf::from("/usr/bin/git"),
            version: "git version 2.50.0".to_owned(),
        })
    } else {
        Diagnostic::failure(failure(&tc))
    };
    let repository = if tc.draw(gs::integers::<u8>().max_value(4)) > 0 {
        Diagnostic::success(repository(&tc))
    } else {
        Diagnostic::failure(failure(&tc))
    };
    let enabled = tc.draw(gs::sampled_from(vec![Some(true), Some(false), None]));
    let kinds = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            RepositoryCompatibilityBlockerKind::InTreeAttributes,
            RepositoryCompatibilityBlockerKind::EffectiveAttributes,
            RepositoryCompatibilityBlockerKind::GitLfs,
            RepositoryCompatibilityBlockerKind::Submodules,
            RepositoryCompatibilityBlockerKind::SparseCheckout,
            RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
        ]))
        .max_size(3),
    );
    let compatibility = match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => Diagnostic::failure(failure(&tc)),
        1 => Diagnostic {
            available: false,
            value: None,
            error: None,
        },
        _ => Diagnostic::success(RepositoryCompatibilityReport {
            commit: object_id(),
            tree: object_id(),
            compatible: kinds.is_empty(),
            blockers: kinds
                .iter()
                .map(|kind| RepositoryCompatibilityBlocker {
                    kind: *kind,
                    explanation: failure(&tc),
                })
                .collect(),
        }),
    };

    let readiness = destination_readiness(
        &destination,
        explicit,
        enabled,
        &git,
        &repository,
        &compatibility,
    );
    assert_eq!(readiness.destination, destination);
    assert_eq!(readiness.copy_on_write, readiness.backend.is_some());

    // One blocker per finding, in the order the diagnostics are consulted.
    let mut expected = Vec::new();
    expected.extend(git.error.as_ref().map(|error| ("git", error.clone())));
    expected.extend(
        repository
            .error
            .as_ref()
            .map(|error| ("repository", error.clone())),
    );
    let unknown_activation =
        enabled.is_none() && repository.value.as_ref().is_some_and(|info| !info.is_bare);
    if enabled == Some(false) || unknown_activation {
        expected.push(("repository-activation", String::new()));
    }
    match &compatibility.value {
        Some(report) => expected.extend(
            report
                .blockers
                .iter()
                .map(|blocker| (blocker.kind.as_str(), blocker.explanation.clone())),
        ),
        None => expected.push((
            "checkout-compatibility",
            compatibility.error.clone().unwrap_or_default(),
        )),
    }
    if readiness.backend.is_none() {
        expected.push(("storage-backend", String::new()));
    }
    if explicit && occupied {
        expected.push(("destination", String::new()));
    }
    assert_eq!(
        readiness
            .blockers
            .iter()
            .map(|blocker| blocker.kind)
            .collect::<Vec<_>>(),
        expected.iter().map(|(kind, _)| *kind).collect::<Vec<_>>()
    );
    for (blocker, (_, explanation)) in readiness.blockers.iter().zip(&expected) {
        assert!(!blocker.explanation.is_empty() && !blocker.remedy.is_empty());
        if !explanation.is_empty() {
            assert_eq!(&blocker.explanation, explanation);
        }
    }

    let status = if expected.is_empty() {
        DestinationReadinessStatus::Ready
    } else if expected
        .iter()
        .all(|(kind, _)| *kind == "repository-activation")
    {
        DestinationReadinessStatus::NeedsActivation
    } else {
        DestinationReadinessStatus::Blocked
    };
    assert_eq!(readiness.status, status);

    // A bare repository is addressed by its Git directory.
    let root = repository.value.as_ref().map(|info| {
        info.root
            .clone()
            .unwrap_or_else(|| info.identity.common_git_dir.clone())
    });
    let quoted_root = root.as_deref().and_then(shell_quoted_path);
    let next_command =
        match status {
            DestinationReadinessStatus::Ready => shell_quoted_path(&destination)
                .zip(quoted_root)
                .map(|(destination, root)| {
                    format!("riftri worktree add {destination} --detach HEAD --repository {root}")
                }),
            DestinationReadinessStatus::NeedsActivation => Some(match quoted_root {
                Some(root) => format!("riftri enable {root}"),
                None => "riftri enable".to_owned(),
            }),
            DestinationReadinessStatus::Blocked => None,
        };
    assert_eq!(readiness.next_command, next_command);
}
