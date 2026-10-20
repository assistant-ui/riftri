//! Destination validation against `git worktree add` itself: a destination
//! Riftri accepts is one Git creates, at exactly the path Riftri predicted,
//! and a refusal creates nothing.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};

use super::super::{
    DestinationRules, WorktreeError, normalize_new_destination, planned_destination_parent,
    resolve_destination_parent, validate_new_add_destination, validate_new_worktree_destination,
};
use super::{Fixture, TreeFile, registered, snapshot};

#[hegel::test(
    test_cases = 60,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn accepted_destinations_are_the_ones_git_creates(tc: TestCase) {
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("a.txt", b"text\n")]);
    let area = fixture.path("area");
    fs::create_dir_all(area.join("dir/sub")).unwrap();
    fs::create_dir(area.join("empty")).unwrap();
    fs::create_dir(area.join("full")).unwrap();
    fs::write(area.join("full/kept"), b"kept\n").unwrap();
    fs::write(area.join("file"), b"file\n").unwrap();
    symlink("dir", area.join("link-dir")).unwrap();
    symlink("empty", area.join("link-empty")).unwrap();
    symlink("nowhere", area.join("dangling")).unwrap();
    symlink("file", area.join("link-file")).unwrap();

    let components = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "dir",
            "sub",
            "empty",
            "full",
            "file",
            "link-dir",
            "link-empty",
            "dangling",
            "link-file",
            "new",
            "new",
            "other",
            "..",
            ".",
        ]))
        .max_size(3),
    );
    let destination = if components.is_empty() {
        PathBuf::new()
    } else {
        components
            .iter()
            .fold(area.clone(), |path, part| path.join(part))
    };
    let before = snapshot(&area);

    let normalized = normalize_new_destination(&destination, DestinationRules::WorktreeAdd);
    let for_move = normalize_new_destination(&destination, DestinationRules::WorktreeMove);
    let preflight =
        validate_new_worktree_destination(&fixture.repository, &destination, OsStr::new(&commit));
    tc.note(&format!(
        "{destination:?}: {normalized:?}, move {for_move:?}"
    ));
    assert_eq!(
        snapshot(&area),
        before,
        "validation created or removed a path"
    );
    assert_eq!(
        validate_new_add_destination(&destination).map_err(|error| error.to_string()),
        normalized.as_ref().map(|_| ()).map_err(ToString::to_string)
    );
    assert_eq!(
        preflight.map_err(|error| error.to_string()),
        normalized.as_ref().map(|_| ()).map_err(ToString::to_string),
        "the public preflight and the add's own check disagree"
    );

    // A move never creates leading directories or fills an existing one, so
    // it accepts a subset of what an add accepts, at the same path.
    if let Ok(moved) = &for_move {
        assert_eq!(normalized.as_ref().ok(), Some(moved));
        assert!(!destination.exists());
        assert_eq!(
            resolve_destination_parent(&destination).ok().as_deref(),
            moved.parent()
        );
    }
    if let Ok(parent) = resolve_destination_parent(&destination) {
        assert_eq!(
            planned_destination_parent(&destination).ok(),
            Some(parent),
            "an existing parent resolves differently when planned"
        );
    }

    // Git has no answer to compare for an empty path: Apple Git 2.50.1 aborts
    // with `BUG: builtin/worktree.c:464`, and the aborted child can stall the
    // suite on macOS. Riftri's refusal is checked here instead.
    if destination.as_os_str().is_empty() {
        assert!(
            matches!(normalized, Err(WorktreeError::InvalidRequest(_))),
            "an empty destination is a request error: {normalized:?}"
        );
        return;
    }

    let by_git = Command::new("git")
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(&destination)
        .arg(&commit)
        .current_dir(&fixture.repository)
        .output()
        .expect("run git worktree add");
    match normalized {
        Ok(normalized) => {
            assert!(
                by_git.status.success(),
                "Git refuses a destination Riftri accepts: {}",
                String::from_utf8_lossy(&by_git.stderr)
            );
            assert_eq!(
                fs::canonicalize(&destination).expect("resolve the created worktree"),
                normalized,
                "Git created the worktree somewhere else"
            );
            assert!(registered(&fixture.repository, &normalized));
        }
        Err(error) => {
            // Stricter than Git by design: a path that ends in `..` or needs
            // a directory created before a `..` can resolve, and a symbolic
            // link to an empty directory.
            let message = error.to_string();
            let stricter = message.contains("climbs out of a directory")
                || message.contains("must name a new directory")
                // A trailing `.` resolves through the link, so check the
                // path without it, as Riftri does.
                || fs::symlink_metadata(destination.components().collect::<PathBuf>())
                    .is_ok_and(|entry| entry.is_symlink());
            assert!(
                !by_git.status.success() || stricter,
                "Git creates a destination Riftri refuses: {error}"
            );
            assert!(
                matches!(
                    error,
                    WorktreeError::InvalidRequest(_) | WorktreeError::DestinationExists(_)
                ),
                "a bad destination is a request error, not an operational one: {error}"
            );
        }
    }
}
