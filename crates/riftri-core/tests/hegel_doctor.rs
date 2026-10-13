//! Property tests for `doctor`, the read-only preflight in front of
//! `worktree add`.
//!
//! Whatever shape the repository and the proposed destination are in, the
//! report has to be internally consistent, its suggested next command has to
//! name exactly the paths it was asked about, and producing it must not
//! change anything.

#![cfg(unix)]

#[path = "support/shell_words.rs"]
mod shell_words;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{Hegel, Settings, TestCase};
use riftri_core::{
    DestinationReadinessStatus, OverlayFsHelperReadiness, RepositoryCompatibilityBlockerKind,
    doctor, doctor_for_destination, enable_repository,
};
use shell_words::shell_words;

fn git(repository: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .expect("start Git");
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    output.stdout
}

fn repository(parent: &Path, name: &str, attributes: Option<&str>, commit: bool) -> PathBuf {
    let repository = parent.join(name);
    fs::create_dir(&repository).expect("repository directory");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    if commit {
        fs::write(repository.join("tracked.txt"), "tracked\n").expect("tracked file");
        fs::write(repository.join("asset.bin"), "not an LFS pointer\n").expect("binary file");
        if let Some(attributes) = attributes {
            fs::write(repository.join(".gitattributes"), attributes).expect("attributes");
        }
        git(&repository, &["add", "--all"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
    }
    repository
}

/// Everything `doctor` could disturb: Git's view of the repository, its local
/// configuration, and the entries next to the proposed destination.
fn observable_state(repository: &Path, destination: &Path) -> (Vec<u8>, Vec<u8>, Vec<OsString>) {
    let parent = destination.parent().expect("destination parent");
    let mut siblings = fs::read_dir(parent)
        .map(|entries| {
            entries
                .map(|entry| entry.expect("directory entry").file_name())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    siblings.sort_unstable();
    (
        git(
            repository,
            &["status", "--porcelain=v2", "--branch", "--ignored"],
        ),
        git(repository, &["config", "--local", "--list"]),
        siblings,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    New,
    EmptyDirectory,
    OccupiedDirectory,
    BelowAFile,
    MissingParents,
}

#[test]
fn doctor_reports_are_consistent_runnable_and_read_only() {
    let fixture = tempfile::tempdir().expect("fixture");
    let root = fs::canonicalize(fixture.path()).expect("canonical fixture");
    // One repository per checkout shape, built once: `doctor` must leave
    // each of them exactly as it found it, which every case verifies.
    let repositories = [
        ("plain", None, true),
        ("unborn", None, false),
        ("ident", Some("*.txt ident\n"), true),
        ("supported", Some("*.txt text eol=lf\n"), true),
        (
            "lfs",
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            true,
        ),
    ]
    .map(|(name, attributes, commit)| (name, repository(&root, name, attributes, commit)));
    fs::write(root.join("file"), b"not a directory").expect("regular file");

    let mut cases = 0_u32;
    Hegel::new(|tc: TestCase| {
        cases += 1;
        let (shape, repository) = tc.draw(gs::sampled_from(repositories.to_vec()));
        let enabled = tc.draw(gs::booleans());
        if enabled {
            enable_repository(&repository).expect("enable repository");
        } else {
            riftri_core::disable_repository(&repository).expect("disable repository");
        }
        let name = tc.draw(
            gs::text()
                .alphabet("ab '\"$*\u{e9}")
                .min_size(1)
                .max_size(6),
        );
        let parent = root.join(format!("destinations-{cases}"));
        fs::create_dir(&parent).expect("destination parent");
        let kind = tc.draw(gs::sampled_from(vec![
            Destination::New,
            Destination::EmptyDirectory,
            Destination::OccupiedDirectory,
            Destination::BelowAFile,
            Destination::MissingParents,
        ]));
        let destination = match kind {
            Destination::New => parent.join(&name),
            Destination::EmptyDirectory => {
                fs::create_dir(parent.join(&name)).expect("empty destination");
                parent.join(&name)
            }
            Destination::OccupiedDirectory => {
                fs::create_dir(parent.join(&name)).expect("occupied destination");
                fs::write(parent.join(&name).join("kept"), b"kept").expect("occupant");
                parent.join(&name)
            }
            Destination::BelowAFile => root.join("file").join(&name),
            Destination::MissingParents => parent.join("not/yet").join(&name),
        };
        let observed_parent = if kind == Destination::BelowAFile {
            parent.join("unused")
        } else {
            destination.clone()
        };

        let before = observable_state(&repository, &observed_parent);
        let report = doctor_for_destination(&repository, &destination);
        assert_eq!(observable_state(&repository, &observed_parent), before);
        assert_eq!(
            destination.join("kept").exists(),
            kind == Destination::OccupiedDirectory
        );

        assert!(report.git.available && report.repository.available);
        assert_eq!(report.repository_enabled, Some(enabled));
        let readiness = &report.destination_readiness;
        assert_eq!(readiness.destination, destination);
        assert_eq!(readiness.copy_on_write, readiness.backend.is_some());
        assert_eq!(report.cow_backend_active, readiness.copy_on_write);
        let has = |kind: &str| {
            readiness
                .blockers
                .iter()
                .any(|blocker| blocker.kind == kind)
        };
        for blocker in &readiness.blockers {
            assert!(!blocker.explanation.is_empty() && !blocker.remedy.is_empty());
        }

        // Each compatibility finding is carried into the readiness verdict
        // under the kind's machine name.
        match &report.repository_compatibility.value {
            Some(compatibility) => {
                assert_ne!(shape, "unborn");
                assert_eq!(compatibility.compatible, compatibility.blockers.is_empty());
                assert_eq!(compatibility.compatible, !matches!(shape, "ident" | "lfs"));
                for blocker in &compatibility.blockers {
                    assert!(has(blocker.kind.as_str()), "{blocker:?}");
                }
                let expected = match shape {
                    "ident" => Some(RepositoryCompatibilityBlockerKind::InTreeAttributes),
                    "lfs" => Some(RepositoryCompatibilityBlockerKind::GitLfs),
                    _ => None,
                };
                assert_eq!(
                    compatibility.blockers.first().map(|blocker| blocker.kind),
                    expected
                );
            }
            None => {
                assert_eq!(shape, "unborn");
                assert!(has("checkout-compatibility"));
            }
        }
        assert_eq!(has("repository-activation"), !enabled);
        assert_eq!(has("destination"), kind == Destination::OccupiedDirectory);
        assert_eq!(has("destination-parent"), kind == Destination::BelowAFile);

        let only_activation = readiness
            .blockers
            .iter()
            .all(|blocker| blocker.kind == "repository-activation");
        let expected_status = if readiness.blockers.is_empty() {
            DestinationReadinessStatus::Ready
        } else if only_activation {
            DestinationReadinessStatus::NeedsActivation
        } else {
            DestinationReadinessStatus::Blocked
        };
        assert_eq!(
            readiness.status, expected_status,
            "{:?}",
            readiness.blockers
        );
        let command = readiness.next_command.as_deref().map(shell_words);
        let word = |text: &str| OsString::from(text);
        let expected_command = match readiness.status {
            DestinationReadinessStatus::Ready => Some(vec![
                word("riftri"),
                word("worktree"),
                word("add"),
                destination.clone().into_os_string(),
                word("--detach"),
                word("HEAD"),
                word("--repository"),
                repository.clone().into_os_string(),
            ]),
            DestinationReadinessStatus::NeedsActivation => Some(vec![
                word("riftri"),
                word("enable"),
                repository.clone().into_os_string(),
            ]),
            // A blocked verdict names a command only when the missing
            // OverlayFS mount helper is what Riftri can point at.
            DestinationReadinessStatus::Blocked => {
                (readiness.overlayfs_helper == OverlayFsHelperReadiness::Unavailable).then(|| {
                    vec![
                        word("sudo"),
                        word("riftri"),
                        word("overlayfs"),
                        word("install-helper"),
                    ]
                })
            }
        };
        assert_eq!(command, expected_command);

        // Without a proposed destination the path is only a volume probe, so
        // an occupied directory is not a blocker.
        if kind == Destination::OccupiedDirectory {
            let probe = doctor(&repository);
            assert!(
                probe
                    .destination_readiness
                    .blockers
                    .iter()
                    .all(|blocker| blocker.kind != "destination")
            );
        }
    })
    .settings(Settings::new().test_cases(6))
    .run();
}
