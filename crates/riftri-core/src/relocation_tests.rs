use std::collections::HashSet;
use std::path::{Path, PathBuf};

use riftri_git::{ObjectId, WorktreeInfo};

use super::{find_relocation, paths_match};

const COMMIT: &str = "1111111111111111111111111111111111111111";
const BRANCH: &[u8] = b"refs/heads/expected";

fn entry(path: PathBuf) -> WorktreeInfo {
    WorktreeInfo {
        path,
        head: Some(ObjectId::parse(COMMIT).unwrap()),
        branch: Some(BRANCH.to_vec()),
        detached: true,
        bare: false,
        head_unresolvable: false,
        locked_reason: None,
        prunable_reason: None,
    }
}

#[test]
fn unrelated_relocations_do_not_scan_claimed_paths() {
    let entries = (0..1000)
        .map(|n| {
            let mut candidate = entry(PathBuf::from(format!("/registered/{n}")));
            candidate.branch = Some(b"refs/heads/unrelated".to_vec());
            candidate.head = None;
            candidate
        })
        .collect::<Vec<_>>();
    let claims = (0..1000)
        .map(|n| PathBuf::from(format!("/claimed/{n}")))
        .collect::<HashSet<_>>();
    for branch in [Some(BRANCH), None] {
        let mut comparisons = 0;
        assert!(
            find_relocation(&entries, branch, COMMIT, |path| {
                claims.iter().any(|claim| {
                    comparisons += 1;
                    paths_match(claim, path)
                })
            })
            .is_none()
        );
        assert_eq!(
            comparisons, 0,
            "unrelated branch/HEAD needs no ownership scan"
        );
    }
}

#[test]
fn relocation_filter_preserves_first_eligible_candidate_and_native_claims() {
    let mut paths = vec![
        PathBuf::from("/work/plain"),
        PathBuf::from("/work/caf\u{e9}"),
        PathBuf::from("/work/cafe\u{301}"),
        PathBuf::from("/work//cafe\u{301}/"),
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
    for claim in &paths {
        for path in &paths {
            for branch in [Some(BRANCH), None] {
                for bare in [false, true] {
                    for prunable in [false, true] {
                        let mut first = entry(path.clone());
                        first.bare = bare;
                        first.prunable_reason = prunable.then(|| b"missing".to_vec());
                        let entries = vec![first, entry(PathBuf::from("/fallback"))];
                        // Original predicate ordering. Prunable entries remain
                        // conservative relocation candidates, as before.
                        let expected = entries.iter().find(|candidate| {
                            !candidate.bare
                                && !paths_match(claim, &candidate.path)
                                && match branch {
                                    Some(expected) => candidate.branch.as_deref() == Some(expected),
                                    None => {
                                        candidate.detached
                                            && candidate
                                                .head
                                                .as_ref()
                                                .is_some_and(|head| head.as_str() == COMMIT)
                                    }
                                }
                        });
                        let actual =
                            find_relocation(&entries, branch, COMMIT, |p| paths_match(claim, p));
                        assert_eq!(
                            actual.map(|entry| &entry.path),
                            expected.map(|entry| &entry.path)
                        );
                    }
                }
            }
        }
    }
    assert!(find_relocation(&[], Some(BRANCH), COMMIT, |_| panic!("empty registry")).is_none());
    let mut attached = entry(PathBuf::from("/attached"));
    attached.detached = false;
    assert!(find_relocation(&[attached], None, COMMIT, |_| false).is_none());
}

#[test]
fn relocation_filter_preserves_raw_branch_names() {
    let mut candidate = entry(PathBuf::from("/raw-branch"));
    let branch = b"refs/heads/\xff";
    candidate.branch = Some(branch.to_vec());
    assert!(
        find_relocation(&[candidate], Some(branch), COMMIT, |path| {
            assert_eq!(path, Path::new("/raw-branch"));
            false
        })
        .is_some()
    );
}
