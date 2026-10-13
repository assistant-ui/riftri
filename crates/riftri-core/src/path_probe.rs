//! Keep collision candidates on the real destination filesystem.
use std::path::{Path, PathBuf};

#[cfg(not(unix))]
pub(super) fn select<'a>(paths: &'a [PathBuf], _root: &Path) -> Vec<&'a PathBuf> {
    // Windows has additional namespace rules (device names, short names,
    // trailing dots/spaces). Keep its complete replay until separately proved.
    paths.iter().collect()
}

#[cfg(unix)]
pub(super) fn select<'a>(paths: &'a [PathBuf], root: &Path) -> Vec<&'a PathBuf> {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[derive(Default)]
    struct Siblings<'a> {
        witnesses: HashMap<&'a OsStr, usize>,
        folded: HashMap<Vec<u8>, &'a OsStr>,
        ambiguous: bool,
    }

    let full = || paths.iter().collect();
    let leaves = paths.iter().map(PathBuf::as_path).collect::<HashSet<_>>();
    if leaves.len() != paths.len() {
        return full();
    }
    let mut groups = HashMap::<&Path, Siblings<'_>>::new();
    for (index, path) in paths.iter().enumerate() {
        if root.as_os_str().len() + 1 + path.as_os_str().len() >= libc::PATH_MAX as usize {
            return full();
        }
        for child in path.ancestors().take_while(|p| !p.as_os_str().is_empty()) {
            if child != path && leaves.contains(child) {
                return full(); // A leaf also used as a directory is not a valid tree.
            }
            let Some(name) = child.file_name() else {
                return full();
            };
            let bytes = name.as_bytes();
            // Do not remove incidental refusal coverage for unusual names or
            // excessive component lengths. The omitted groups must contain
            // only ordinary ASCII spellings with no case-alias candidates.
            if bytes.len() > 255
                || (bytes.is_ascii()
                    && (bytes.ends_with(b".")
                        || !bytes
                            .iter()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))))
            {
                return full();
            }
            let group = groups
                .entry(child.parent().expect("relative child"))
                .or_default();
            if group.witnesses.contains_key(name) {
                continue;
            }
            group.witnesses.insert(name, index);
            if !bytes.is_ascii() {
                group.ambiguous = true;
            } else if let Some(previous) = group.folded.insert(bytes.to_ascii_lowercase(), name) {
                group.ambiguous |= previous != name;
            }
        }
    }
    let mut keep = vec![false; paths.len()];
    for group in groups.values().filter(|g| g.ambiguous) {
        // Any non-ASCII spelling could alias an ASCII sibling (for example
        // Kelvin sign/K). Replay ALL siblings, not just the suspicious name.
        // A witness below a directory recreates that directory and its parents
        // through the unchanged probe, including file/directory collisions.
        for &index in group.witnesses.values() {
            keep[index] = true;
        }
    }
    paths
        .iter()
        .zip(keep)
        .filter_map(|(path, keep)| keep.then_some(path))
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn localized_unicode_keeps_its_ascii_siblings_not_unrelated_subtrees() {
        let mut paths = (0..2048)
            .map(|i| PathBuf::from(format!("src/d{}/file-{i}.txt", i / 128)))
            .collect::<Vec<_>>();
        paths[0] = PathBuf::from("src/d0/café.txt");
        let selected = select(&paths, Path::new("/tmp/probe"));
        assert_eq!(selected.len(), 128);
        assert_eq!(selected, paths[..128].iter().collect::<Vec<_>>());
    }

    #[test]
    fn directory_alias_candidates_keep_a_witness_under_every_sibling() {
        let paths = [
            "Ü/a/first",
            "Ü/a/second",
            "ü/b/first",
            "ordinary/a",
            "ordinary/b",
        ]
        .map(PathBuf::from);
        let selected = select(&paths, Path::new("/tmp/probe"));
        assert_eq!(selected, vec![&paths[0], &paths[2], &paths[3]]);
    }

    #[test]
    fn unusual_ascii_and_overlong_paths_keep_the_complete_probe() {
        for unusual in [
            "trailing.",
            "has space",
            "back\\slash",
            "has:colon",
            &"a".repeat(256),
        ] {
            let paths = [
                PathBuf::from("café/file"),
                PathBuf::from(format!("other/{unusual}")),
                PathBuf::from("ordinary/file"),
                PathBuf::from("ordinary/another"),
            ];
            assert_eq!(select(&paths, Path::new("/tmp/probe")).len(), paths.len());
        }
        let paths = [PathBuf::from("café/file"), PathBuf::from("ordinary/file")];
        assert_eq!(
            select(&paths, &PathBuf::from("x".repeat(libc::PATH_MAX as usize))).len(),
            paths.len()
        );
    }

    #[test]
    fn selection_preserves_real_volume_alias_decisions_and_cleanup() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("reference");
        for pair in [
            ("Ä.txt", "ä.txt"),
            ("café.txt", "cafe\u{301}.txt"),
            ("K.txt", "K.txt"),
            ("Ü/one", "ü/two"),
            ("Docs/one", "docs/two"),
            ("café", "cafe\u{301}/child"),
            ("café/one", "unrelated/two"),
        ] {
            let paths = ["plain/a", pair.0, "plain/b", pair.1, "plain/c"].map(PathBuf::from);
            for reverse in [false, true] {
                let mut paths = paths.clone();
                if reverse {
                    paths.reverse();
                }
                // Independent full-tree reference, without the selection helper.
                std::fs::create_dir(&root).unwrap();
                let mut directories = std::collections::HashSet::new();
                let expected = (|| -> std::io::Result<()> {
                    for path in &paths {
                        let parents = path
                            .ancestors()
                            .skip(1)
                            .filter(|p| !p.as_os_str().is_empty())
                            .collect::<Vec<_>>();
                        for parent in parents.into_iter().rev() {
                            if directories.insert(parent) {
                                std::fs::create_dir(root.join(parent))?;
                            }
                        }
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(root.join(path))?;
                    }
                    Ok(())
                })()
                .is_ok();
                std::fs::remove_dir_all(&root).unwrap();
                assert_eq!(
                    super::super::validate_destination_path_semantics(
                        &paths,
                        &fixture.path().join("view")
                    )
                    .is_ok(),
                    expected,
                    "{pair:?}"
                );
                assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
            }
        }
    }

    #[test]
    fn native_non_utf8_siblings_are_never_decoded_lossily() {
        use std::os::unix::ffi::OsStringExt;
        let paths = [
            PathBuf::from(std::ffi::OsString::from_vec(b"directory/\xff".to_vec())),
            PathBuf::from("directory/ascii"),
            PathBuf::from("other/a"),
            PathBuf::from("other/b"),
        ];
        assert_eq!(
            select(&paths, Path::new("/tmp/probe")),
            vec![&paths[0], &paths[1]]
        );
    }
}
