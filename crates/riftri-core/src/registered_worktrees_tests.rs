use std::path::{Path, PathBuf};

use super::{RegisteredWorktrees, paths_match};

fn entry(path: PathBuf) -> riftri_git::WorktreeInfo {
    riftri_git::WorktreeInfo {
        path,
        head: None,
        branch: None,
        detached: true,
        bare: false,
        head_unresolvable: false,
        locked_reason: None,
        prunable_reason: None,
    }
}

#[test]
fn full_reconciliation_indexes_repeated_lookups_but_single_destination_stays_linear() {
    let entries = (0..1000)
        .map(|n| entry(PathBuf::from(format!("/work/view-{n:04}"))))
        .collect::<Vec<_>>();
    let inventory = RegisteredWorktrees::new(entries.clone(), false);
    assert!(matches!(inventory, RegisteredWorktrees::Indexed(_)));
    for worktree in &entries {
        assert!(inventory.contains(&worktree.path));
    }
    assert!(!inventory.contains(Path::new("/not-registered")));
    assert!(matches!(
        RegisteredWorktrees::new(entries, true),
        RegisteredWorktrees::Linear(_)
    ));
}

#[test]
fn registration_cache_matches_the_original_predicate_for_native_paths_and_prunable_duplicates() {
    let mut paths = vec![
        PathBuf::from("/work/plain"),
        PathBuf::from("/work/caf\u{e9}"),
        PathBuf::from("/work/cafe\u{301}"),
        PathBuf::from("/work//cafe\u{301}/"),
        PathBuf::from("/work/./caf\u{e9}"),
        PathBuf::from("/WORK/PLAIN"),
        PathBuf::from(r"C:\work\plain"),
        PathBuf::from(r"\\?\C:\work\plain"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        paths.push(PathBuf::from(std::ffi::OsString::from_vec(
            b"/work/\xff".to_vec(),
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        paths.push(PathBuf::from(std::ffi::OsString::from_wide(&[0xd800])));
    }
    for live_duplicates in [false, true] {
        let mut entries = paths.iter().cloned().map(entry).collect::<Vec<_>>();
        for worktree in &mut entries {
            worktree.prunable_reason = Some(b"missing".to_vec());
        }
        if live_duplicates {
            // A prunable first entry must not hide a later live alias.
            entries.extend(paths.iter().rev().cloned().map(entry));
        }
        for single_destination in [false, true] {
            let inventory = RegisteredWorktrees::new(entries.clone(), single_destination);
            for path in paths
                .iter()
                .chain(std::iter::once(&PathBuf::from("/absent")))
            {
                let expected = entries.iter().any(|candidate| {
                    paths_match(&candidate.path, path) && candidate.prunable_reason.is_none()
                });
                assert_eq!(inventory.contains(path), expected, "{path:?}");
            }
        }
    }
}

#[test]
fn empty_and_one_entry_inventories_stay_linear() {
    for entries in [vec![], vec![entry(PathBuf::from("/work/view"))]] {
        assert!(matches!(
            RegisteredWorktrees::new(entries, false),
            RegisteredWorktrees::Linear(_)
        ));
    }
}
