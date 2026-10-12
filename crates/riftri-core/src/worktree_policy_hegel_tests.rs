//! Checkout policy that needs no repository: the attribute allowlist, strict
//! Git LFS pointers, cone-mode sparse selections, and path alias detection.
//! Each is compared with an independent model and must fail closed outside it.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use riftri_git::{GitAttribute, ObjectId, TreeEntry};

use super::super::tests::inventory_entry;
use super::super::{
    WorktreeError, WorktreeInventory, canonicalize_sparse_directories, classify_in_tree_attributes,
    has_ascii_case_alias, interrupted_journal_temporary_owner, journal_identifier_like,
    needs_destination_path_probe, next_operation_id, parse_git_lfs_pointer, paths_match,
    repository_cache_id, select_lfs_tree_entries, validate_destination_path_semantics,
    validate_sparse_directories_in_tree,
};
use super::snapshot;

fn canonical_pointer(oid: &str, size: u64) -> Vec<u8> {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
        .into_bytes()
}

fn lowercase_sha256(tc: &TestCase) -> String {
    tc.draw(
        gs::text()
            .alphabet("0123456789abcdef")
            .min_size(64)
            .max_size(64),
    )
}

#[hegel::test]
fn canonical_lfs_pointers_round_trip(tc: TestCase) {
    let oid = lowercase_sha256(&tc);
    let size = tc.draw(gs::integers::<u64>());
    let pointer = parse_git_lfs_pointer(&canonical_pointer(&oid, size)).expect("canonical pointer");
    assert_eq!((pointer.oid.as_str(), pointer.size), (oid.as_str(), size));
}

/// Anything the parser accepts is byte-for-byte the canonical encoding of what
/// it returned, so no extension line, alternate hash, or padding gets through.
#[hegel::test(test_cases = 500)]
fn lfs_pointer_parser_accepts_only_the_canonical_encoding(tc: TestCase) {
    let bytes = if tc.draw(gs::booleans()) {
        tc.draw(gs::binary().max_size(160))
    } else {
        let mut bytes = canonical_pointer(&lowercase_sha256(&tc), tc.draw(gs::integers::<u64>()));
        for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(3)) {
            let index = tc.draw(gs::integers::<usize>().max_value(bytes.len()));
            let byte = tc.draw(gs::sampled_from(b"\n\r 0189afAFgxz:-+\0".to_vec()));
            match tc.draw(gs::integers::<u8>().max_value(2)) {
                0 => bytes.insert(index, byte),
                1 if index < bytes.len() => {
                    bytes.remove(index);
                }
                _ if index < bytes.len() => bytes[index] = byte,
                _ => bytes.push(byte),
            }
        }
        bytes
    };
    if let Ok(pointer) = parse_git_lfs_pointer(&bytes) {
        assert_eq!(canonical_pointer(&pointer.oid, pointer.size), bytes);
        assert_eq!(pointer.oid.len(), 64);
        assert!(
            pointer
                .oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "accepted a non-canonical object ID {}",
            pointer.oid
        );
    }
}

fn attribute(tc: &TestCase, path: &Path) -> GitAttribute {
    const NAMES: &[&str] = &[
        "text",
        "eol",
        "binary",
        "diff",
        "merge",
        "filter",
        "ident",
        "working-tree-encoding",
        "crlf",
        "export-subst",
        "linguist-generated",
        "linguist-vendored",
        "linguist-documentation",
        "linguist-detectable",
        "linguist-language",
        "linguist-generate",
        "Text",
        "",
    ];
    const VALUES: &[&str] = &[
        "set",
        "unset",
        "auto",
        "lf",
        "crlf",
        "lfs",
        "true",
        "false",
        "Rust",
        "",
        "unspecified",
        "SET",
        "utf-16",
    ];
    // Mostly records that are fine on their own, so a single stray name or
    // value is what decides the verdict.
    const USUAL: &[(&str, &str)] = &[
        ("text", "set"),
        ("text", "auto"),
        ("eol", "lf"),
        ("eol", "crlf"),
        ("binary", "set"),
        ("diff", "unset"),
        ("merge", "unset"),
        ("linguist-vendored", "true"),
        ("linguist-language", "Rust"),
    ];
    let (name, value) = if tc.draw(gs::integers::<u8>().max_value(3)) > 0 {
        tc.draw(gs::sampled_from(USUAL))
    } else {
        (
            tc.draw(gs::sampled_from(NAMES)),
            tc.draw(gs::sampled_from(VALUES)),
        )
    };
    GitAttribute {
        path: path.to_path_buf(),
        name: name.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
    }
}

/// The allowlist as a flat table, written from the documented policy rather
/// than from the implementation's match arms.
fn allowed(name: &[u8], value: &[u8]) -> bool {
    let table: &[(&[u8], &[&[u8]])] = &[
        (b"text", &[b"set", b"unset", b"auto"]),
        (b"eol", &[b"lf", b"crlf"]),
        (b"binary", &[b"set"]),
        (b"diff", &[b"unset"]),
        (b"merge", &[b"unset"]),
        (
            b"linguist-generated",
            &[b"set", b"unset", b"true", b"false"],
        ),
        (b"linguist-vendored", &[b"set", b"unset", b"true", b"false"]),
        (
            b"linguist-documentation",
            &[b"set", b"unset", b"true", b"false"],
        ),
        (
            b"linguist-detectable",
            &[b"set", b"unset", b"true", b"false"],
        ),
    ];
    if name == b"linguist-language" {
        return !value.is_empty();
    }
    table
        .iter()
        .any(|(allowed_name, values)| *allowed_name == name && values.contains(&value))
}

#[hegel::test(test_cases = 500)]
fn attribute_policy_matches_the_allowlist_and_fails_closed(tc: TestCase) {
    let paths = ["a.txt", "dir/b.bin", "c"].map(PathBuf::from);
    let mut attributes = Vec::new();
    for path in &paths {
        if tc.draw(gs::integers::<u8>().max_value(3)) == 0 {
            // Canonical LFS, sometimes with one record changed or added.
            for (name, value) in [("filter", "lfs"), ("diff", "lfs"), ("merge", "lfs")] {
                attributes.push(GitAttribute {
                    path: path.clone(),
                    name: name.as_bytes().to_vec(),
                    value: value.as_bytes().to_vec(),
                });
            }
            attributes.push(GitAttribute {
                path: path.clone(),
                name: b"text".to_vec(),
                value: b"unset".to_vec(),
            });
            match tc.draw(gs::integers::<u8>().max_value(3)) {
                0 => attributes.push(attribute(&tc, path)),
                1 => {
                    let last = attributes.len() - 1;
                    let index = last - tc.draw(gs::integers::<usize>().max_value(3));
                    attributes[index] = attribute(&tc, path);
                }
                _ => {}
            }
        } else {
            for _ in 0..tc.draw(gs::integers::<usize>().max_value(4)) {
                attributes.push(attribute(&tc, path));
            }
        }
    }
    let order = tc.draw(gs::permutations(attributes.clone()));

    let mut by_path = BTreeMap::<&Path, Vec<(&[u8], &[u8])>>::new();
    for attribute in &attributes {
        by_path
            .entry(attribute.path.as_path())
            .or_default()
            .push((&attribute.name, &attribute.value));
    }
    let mut expected_lfs = Vec::new();
    let mut supported = true;
    for (path, records) in &by_path {
        let lfs = |name: &[u8]| records.contains(&(name, b"lfs".as_slice()));
        if lfs(b"filter") || lfs(b"diff") || lfs(b"merge") {
            let exact = records.len() == 4
                && lfs(b"filter")
                && lfs(b"diff")
                && lfs(b"merge")
                && records.contains(&(b"text".as_slice(), b"unset".as_slice()));
            supported &= exact;
            expected_lfs.push(path.to_path_buf());
        } else {
            supported &= records.iter().all(|(name, value)| allowed(name, value));
        }
    }

    match classify_in_tree_attributes(&order) {
        Ok(lfs_paths) => {
            assert!(supported, "accepted attributes outside the allowlist");
            assert_eq!(lfs_paths, expected_lfs);
        }
        Err(_) => assert!(!supported, "refused attributes the allowlist permits"),
    }
}

fn sparse_directory(tc: &TestCase) -> String {
    let parts = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            "a",
            "b",
            "ab",
            "a b",
            "A",
            "c-d",
            "\u{e9}",
            ".git",
            ".GIT",
            ".",
            "..",
            "",
            "*",
            "x?",
            "[a]",
            "back\\slash",
            "!a",
            "tab\t",
            ".gitignore",
        ]))
        .min_size(1)
        .max_size(3),
    );
    let mut directory = parts.join("/");
    match tc.draw(gs::integers::<u8>().max_value(9)) {
        0 => directory.push('/'),
        1 => directory.insert(0, '/'),
        _ => {}
    }
    directory
}

/// The documented literal-directory subset, stated per component.
fn literal_cone_directory(raw: &str) -> Option<String> {
    let directory = raw.strip_suffix('/').unwrap_or(raw);
    let literal = !raw.starts_with('/')
        && !directory.starts_with('!')
        && directory.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.eq_ignore_ascii_case(".git")
                && !component
                    .chars()
                    .any(|c| "*?[]\\".contains(c) || c.is_control())
        });
    literal.then(|| directory.to_owned())
}

fn covers(ancestor: &str, directory: &str) -> bool {
    directory == ancestor
        || directory
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[hegel::test(test_cases = 500)]
fn sparse_cones_canonicalize_to_a_minimal_cover_or_are_refused(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().max_value(6));
    let requested = (0..count)
        .map(|_| sparse_directory(&tc))
        .collect::<Vec<_>>();
    tc.note(&format!("requested {requested:?}"));
    let literal = requested
        .iter()
        .map(|raw| literal_cone_directory(raw))
        .collect::<Option<Vec<_>>>();
    let Some(literal) = literal else {
        assert!(matches!(
            canonicalize_sparse_directories(&requested),
            Err(WorktreeError::InvalidRequest(_))
        ));
        return;
    };
    let canonical = canonicalize_sparse_directories(&requested).expect("literal cone list");

    assert!(canonical.windows(2).all(|pair| pair[0] < pair[1]));
    for directory in &canonical {
        assert!(literal.contains(directory), "invented cone {directory}");
        assert!(
            !canonical
                .iter()
                .any(|other| other != directory && covers(other, directory)),
            "{directory} is nested under another retained cone"
        );
    }
    for directory in &literal {
        assert!(
            canonical.iter().any(|cone| covers(cone, directory)),
            "{directory} is no longer selected"
        );
    }
    assert_eq!(
        canonicalize_sparse_directories(&canonical).expect("canonical list"),
        canonical
    );
    let reordered = tc.draw(gs::permutations(requested));
    assert_eq!(
        canonicalize_sparse_directories(&reordered).expect("reordered list"),
        canonical
    );
}

#[hegel::test(test_cases = 300)]
fn sparse_directories_must_be_directories_of_the_tree(tc: TestCase) {
    let paths = |maximum: usize| {
        let count = tc.draw(gs::integers::<usize>().max_value(maximum));
        (0..count)
            .map(|_| {
                tc.draw(
                    gs::vecs(gs::sampled_from(vec!["a", "b", "c", "ab", "a.txt"]))
                        .min_size(1)
                        .max_size(3),
                )
                .join("/")
            })
            .collect::<Vec<_>>()
    };
    let checkout_paths = paths(8).into_iter().map(PathBuf::from).collect::<Vec<_>>();
    let directories = paths(6);
    let tree = ObjectId::parse("0".repeat(40)).expect("object id");

    let is_directory = |directory: &str| {
        checkout_paths
            .iter()
            .any(|path| path.as_path() != Path::new(directory) && path.starts_with(directory))
    };
    let result = validate_sparse_directories_in_tree(&directories, &checkout_paths, &tree);
    match directories
        .iter()
        .find(|directory| !is_directory(directory))
    {
        None => result.expect("every selection is a directory of the tree"),
        Some(missing) => {
            let message = result
                .expect_err("a selection is not a directory")
                .to_string();
            assert!(message.contains(missing.as_str()), "{message}");
        }
    }
}

#[hegel::test(test_cases = 200)]
fn lfs_tree_selection_matches_a_linear_lookup(tc: TestCase) {
    // Sizes on both sides of the 1% cutoff between the two lookup strategies.
    let count = tc.draw(gs::sampled_from(vec![0_usize, 3, 99, 100, 250]));
    let entries = (0..count)
        .map(|index| TreeEntry {
            mode: 0o100644,
            object_kind: b"blob".to_vec(),
            object_id: ObjectId::parse(format!("{index:040x}")).expect("object id"),
            path: PathBuf::from(format!("file-{index}")),
        })
        .collect::<Vec<_>>();
    let paths = tc
        .draw(gs::vecs(gs::integers::<usize>().max_value(count + 1)).max_size(4))
        .into_iter()
        .map(|index| PathBuf::from(format!("file-{index}")))
        .collect::<Vec<_>>();

    let expected = paths
        .iter()
        .map(|path| entries.iter().find(|entry| &entry.path == path))
        .collect::<Option<Vec<_>>>();
    match select_lfs_tree_entries(&entries, &paths) {
        Ok(selected) => assert_eq!(Some(selected), expected),
        Err(message) => {
            assert!(expected.is_none(), "refused a present path: {message}");
            assert!(message.contains("absent from the exact tree"));
        }
    }
}

fn byte_path(tc: &TestCase) -> PathBuf {
    byte_path_from(tc, b"aAbB.-_ \xc3\xa9\xff\x80")
}

fn byte_path_from(tc: &TestCase, alphabet: &[u8]) -> PathBuf {
    let depth = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    let mut path = PathBuf::new();
    for _ in 0..depth {
        let mut component = tc.draw(
            gs::vecs(gs::sampled_from(alphabet.to_vec()))
                .min_size(1)
                .max_size(3),
        );
        if component == b"." || component == b".." {
            component.push(b'x');
        }
        path.push(OsString::from_vec(component));
    }
    path
}

#[hegel::test(test_cases = 500)]
fn ascii_case_aliases_match_a_pairwise_comparison(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().max_value(6));
    let paths = (0..count).map(|_| byte_path(&tc)).collect::<Vec<_>>();
    tc.note(&format!("paths {paths:?}"));

    let prefixes = paths
        .iter()
        .flat_map(|path| path.ancestors())
        .filter(|prefix| !prefix.as_os_str().is_empty())
        .map(|prefix| prefix.as_os_str().as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    let aliased = prefixes.iter().any(|left| {
        prefixes
            .iter()
            .any(|right| left != right && left.eq_ignore_ascii_case(right))
    });
    assert_eq!(has_ascii_case_alias(&paths), aliased);

    let non_ascii = paths
        .iter()
        .any(|path| !path.as_os_str().as_bytes().is_ascii());
    assert_eq!(needs_destination_path_probe(&paths), aliased || non_ascii);
}

/// The preflight agrees with what the destination volume really does: it
/// accepts a path set exactly when every path can be created side by side.
#[hegel::test(test_cases = 150)]
fn path_preflight_agrees_with_the_destination_filesystem(tc: TestCase) {
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
    // A Git tree names each path once, and never as both a file and a
    // directory.
    // Half the trees are plain ASCII, where only a case alias can collide and
    // the preflight has to notice it without probing every tree.
    let alphabet: &[u8] = if tc.draw(gs::booleans()) {
        b"aAbB.-_ "
    } else {
        b"aAbB.-_ \xc3\xa9\xff\x80"
    };
    let mut drawn = (0..count)
        .map(|_| byte_path_from(&tc, alphabet))
        .collect::<BTreeSet<_>>();
    if tc.draw(gs::booleans()) {
        let source = drawn.iter().next().expect("at least one path").clone();
        let flipped = source
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| {
                if byte.is_ascii_lowercase() {
                    byte.to_ascii_uppercase()
                } else {
                    byte.to_ascii_lowercase()
                }
            })
            .collect::<Vec<_>>();
        drawn.insert(PathBuf::from(OsString::from_vec(flipped)));
    }
    let paths = drawn
        .iter()
        .filter(|path| {
            !drawn
                .iter()
                .any(|other| other != *path && other.starts_with(path))
        })
        .cloned()
        .collect::<Vec<_>>();
    tc.note(&format!("paths {paths:?}"));
    let parent = tempfile::tempdir().expect("destination parent");
    let destination = parent.path().join("worktree");

    // Write the tree for real, then count what the volume kept: an aliased
    // name either fails to create or silently lands on its twin.
    let oracle = tempfile::tempdir().expect("oracle directory");
    let created = paths.iter().all(|path| {
        let target = oracle.path().join(path);
        fs::create_dir_all(target.parent().expect("parent")).is_ok()
            && fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .is_ok()
    });
    let distinct = paths
        .iter()
        .flat_map(|path| path.ancestors())
        .filter(|prefix| !prefix.as_os_str().is_empty())
        .collect::<BTreeSet<_>>();
    let coexist = created && snapshot(oracle.path()).len() == distinct.len();

    let result = validate_destination_path_semantics(&paths, &destination);
    match result {
        Ok(()) => assert!(coexist, "accepted paths that collide on this volume"),
        Err(error) => {
            assert!(
                matches!(error, WorktreeError::Unsupported(_)),
                "collisions are a policy refusal: {error}"
            );
            // An all-ASCII set without case aliases is never probed, so a
            // refusal there could only be a false positive.
            assert!(!coexist || needs_destination_path_probe(&paths));
            assert!(!coexist, "refused paths this volume keeps apart: {error}");
        }
    }
    assert_eq!(
        fs::read_dir(parent.path()).expect("read parent").count(),
        0,
        "the probe left files beside the destination"
    );
}

#[hegel::test]
fn non_relative_checkout_paths_are_refused(tc: TestCase) {
    let mut paths = vec![byte_path(&tc)];
    let hostile = tc.draw(gs::sampled_from(vec![
        "/etc/passwd",
        "../escape",
        "a/../../b",
        "",
        "./a",
        "a/..",
    ]));
    let index = tc.draw(gs::integers::<usize>().max_value(1));
    paths.insert(index, PathBuf::from(hostile));
    let parent = tempfile::tempdir().expect("destination parent");
    let error = validate_destination_path_semantics(&paths, &parent.path().join("worktree"))
        .expect_err("a non-relative tree path");
    assert!(error.to_string().contains("not a relative checkout path"));
    assert_eq!(fs::read_dir(parent.path()).expect("read parent").count(), 0);
}

/// An absolute path with no empty, `.`, or `..` components, as every path
/// Riftri compares has been resolved to.
#[cfg(target_os = "macos")]
fn normalized_path_text(tc: &TestCase, alphabet: &str) -> String {
    let depth = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    (0..depth)
        .map(|_| {
            let name = tc.draw(gs::text().alphabet(alphabet).min_size(1).max_size(3));
            format!("/{name}")
        })
        .collect()
}

/// `Path` equality ignores a repeated or trailing separator, so the
/// normalization-insensitive comparison should too.
#[cfg(target_os = "macos")]
#[hegel::test]
#[ignore = "bug: paths_match stops ignoring redundant separators once the two spellings differ in Unicode normalization"]
fn path_matching_ignores_redundant_separators_in_any_normalization(tc: TestCase) {
    use unicode_normalization::UnicodeNormalization;

    let text = normalized_path_text(&tc, "ab\u{e9}e\u{301}");
    let composed = text.nfc().collect::<String>();
    let decomposed = text.nfd().collect::<String>();
    let respelled = match tc.draw(gs::integers::<u8>().max_value(1)) {
        0 => format!("{decomposed}/"),
        _ => format!("/{decomposed}"),
    };
    assert!(paths_match(Path::new(&decomposed), Path::new(&respelled)));
    assert!(
        paths_match(Path::new(&composed), Path::new(&respelled)),
        "{composed:?} and {respelled:?} name the same file"
    );
}

#[cfg(target_os = "macos")]
#[hegel::test]
fn path_matching_is_an_equivalence_over_normalization(tc: TestCase) {
    use unicode_normalization::UnicodeNormalization;

    let text = normalized_path_text(&tc, "ab\u{e9}\u{f1}\u{c5}\u{1ee5}\u{4e2d}e\u{301}");
    let composed = PathBuf::from(text.nfc().collect::<String>());
    let decomposed = PathBuf::from(text.nfd().collect::<String>());
    let other = PathBuf::from(normalized_path_text(&tc, "abe\u{e9}\u{301}"));

    assert!(paths_match(&composed, &composed));
    assert_eq!(
        paths_match(&composed, &other),
        paths_match(&other, &composed)
    );
    assert!(paths_match(&composed, &decomposed));
    assert_eq!(
        paths_match(&composed, &other),
        paths_match(&decomposed, &other)
    );

    let mut raw = tc.draw(gs::binary().min_size(1).max_size(6));
    raw.push(0xff);
    let invalid = PathBuf::from(OsString::from_vec(raw));
    assert!(paths_match(&invalid, &invalid));
    assert!(!paths_match(&invalid, &composed));
}

/// The indexed worktree inventory answers exactly as a linear scan with
/// `paths_match` would, first match included.
#[cfg(target_os = "macos")]
#[hegel::test(test_cases = 300)]
fn inventory_lookup_matches_a_linear_scan(tc: TestCase) {
    let path = || {
        let name = tc.draw(gs::sampled_from(vec![
            "/w/a",
            "/w/a/",
            "/w/A",
            "/w/\u{e9}",
            "/w/e\u{301}",
            "/w/\u{e9}/x",
            "/w/e\u{301}/x",
            "/w//a",
            "/w/b",
        ]));
        if tc.draw(gs::integers::<u8>().max_value(9)) == 0 {
            PathBuf::from(OsString::from_vec([name.as_bytes(), b"\xff"].concat()))
        } else {
            PathBuf::from(name)
        }
    };
    let count = tc.draw(gs::integers::<usize>().max_value(5));
    let entries = (0..count)
        .map(|_| inventory_entry(path()))
        .collect::<Vec<_>>();
    let query = path();
    let expected = entries
        .iter()
        .find(|entry| paths_match(&entry.path, &query))
        .map(|entry| entry.path.clone());
    let inventory = WorktreeInventory::new(entries);
    assert_eq!(
        inventory.find(&query).map(|entry| entry.path.clone()),
        expected,
        "query {query:?}"
    );
}

#[hegel::test]
fn operation_ids_are_unique_identifiers_that_temporaries_map_back_to(tc: TestCase) {
    let timestamp = tc.draw(gs::integers::<u128>());
    let first = next_operation_id(timestamp);
    let second = next_operation_id(timestamp);
    assert_ne!(first, second, "one timestamp must not repeat an ID");
    assert!(journal_identifier_like(&first));

    let suffix = tc.draw(gs::text().alphabet("abcXYZ019").min_size(1).max_size(8));
    let temporary = PathBuf::from(format!("/state/operations/.{first}.{suffix}.tmp"));
    assert_eq!(interrupted_journal_temporary_owner(&temporary), Some(first));

    let name = tc.draw(gs::text().alphabet("ab1-._/tmp ").max_size(16));
    if let Some(owner) = interrupted_journal_temporary_owner(Path::new(&name)) {
        assert!(journal_identifier_like(&owner));
        let file_name = Path::new(&name).file_name().expect("file name");
        let file_name = file_name.to_str().expect("UTF-8 name");
        assert!(file_name.starts_with(&format!(".{owner}.")) && file_name.ends_with(".tmp"));
    }
}

#[hegel::test]
fn base_bucket_ids_separate_repositories_and_profiles(tc: TestCase) {
    let directory = || gs::binary().min_size(1).max_size(12);
    let profile = || gs::binary().max_size(12);
    let (first_directory, first_profile) = (tc.draw(directory()), tc.draw(profile()));
    let (second_directory, second_profile) = (tc.draw(directory()), tc.draw(profile()));
    // A path never contains NUL, which is what separates it from the profile.
    tc.assume(!first_directory.contains(&0) && !second_directory.contains(&0));
    let id = |directory: &[u8], profile: &[u8]| {
        repository_cache_id(Path::new(std::ffi::OsStr::from_bytes(directory)), profile)
    };
    let first = id(&first_directory, &first_profile);
    let second = id(&second_directory, &second_profile);

    assert_eq!(first.len(), 66);
    assert!(first.starts_with("r-") && journal_identifier_like(&first));
    assert_eq!(first, id(&first_directory, &first_profile));
    assert_eq!(
        first == second,
        (first_directory, first_profile) == (second_directory, second_profile)
    );
}
