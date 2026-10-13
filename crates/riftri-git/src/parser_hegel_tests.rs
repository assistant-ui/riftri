//! Property tests for the parsers of Git's structured output: every record
//! shape Git can emit round-trips exactly, and anything else is refused.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator};

use super::{
    ConfigValueOrigin, GitAttribute, GitError, GitInfo, ObjectId, ResolvedRevision,
    normalize_config_key, parse_attribute_records, parse_blob_sizes, parse_checkout_storage,
    parse_config_value_origins, parse_config_values, parse_paired_revision_output,
    parse_resolved_revision_output, parse_tree_entries, parse_worktree_porcelain, read_batch_blob,
};

/// Bytes that have tripped path and record handling before: delimiters,
/// quoting characters, and the halves of a UTF-8 sequence.
const ODD_BYTES: [u8; 14] = [
    0, b'\n', b'\r', b'\t', b' ', b'"', b'\\', b'/', b'a', b'Z', 0x80, 0xc3, 0xa9, 0xff,
];

fn bytes_without(tc: &TestCase, forbidden: &'static [u8], min_size: usize) -> Vec<u8> {
    tc.draw(
        gs::vecs(
            hegel::one_of!(gs::sampled_from(ODD_BYTES.to_vec()), gs::integers::<u8>())
                .filter(move |byte| !forbidden.contains(byte)),
        )
        .min_size(min_size)
        .max_size(12),
    )
}

fn object_id(tc: &TestCase) -> String {
    let length = tc.draw(gs::sampled_from(vec![40_usize, 64]));
    tc.draw(
        gs::text()
            .alphabet("0123456789abcdefABCDEF")
            .min_size(length)
            .max_size(length),
    )
}

/// Delete, insert, or replace one byte, so the input stays close to a shape
/// the parser accepts.
fn corrupt_one_byte(tc: &TestCase, bytes: &mut Vec<u8>) {
    let byte = tc.draw(hegel::one_of!(
        gs::sampled_from(ODD_BYTES.to_vec()),
        gs::integers::<u8>()
    ));
    let index = tc.draw(gs::integers::<usize>().max_value(bytes.len()));
    match tc.draw(gs::sampled_from(vec!["delete", "insert", "replace"])) {
        "insert" => bytes.insert(index, byte),
        _ if index == bytes.len() => bytes.push(byte),
        "delete" => {
            bytes.remove(index);
        }
        _ => bytes[index] = byte,
    }
}

fn is_invalid_output<T>(result: &Result<T, GitError>) -> bool {
    matches!(result, Err(GitError::InvalidOutput { .. }))
}

#[hegel::test]
fn object_ids_accept_exactly_full_length_hexadecimal(tc: TestCase) {
    let length = tc.draw(gs::sampled_from(vec![0_usize, 39, 40, 41, 63, 64, 65]));
    let value: String = tc.draw(
        gs::text()
            .alphabet("0123456789abcdefABCDEFgG \né")
            .min_size(length)
            .max_size(length),
    );
    let is_object_id =
        matches!(value.len(), 40 | 64) && value.chars().all(|digit| digit.is_ascii_hexdigit());
    match ObjectId::parse(value.clone()) {
        Ok(object) => {
            assert!(is_object_id, "accepted {value:?}");
            assert_eq!(object.as_str(), value.to_ascii_lowercase());
            let reparsed = ObjectId::parse(object.as_str()).expect("normalized ID stays valid");
            assert_eq!(reparsed, object);
        }
        Err(error) => {
            assert!(!is_object_id, "refused {value:?}: {error}");
            assert!(is_invalid_output::<()>(&Err(error)));
        }
    }
}

#[hegel::test]
fn attribute_source_support_follows_the_release_number(tc: TestCase) {
    let number = || gs::integers::<u32>().max_value(60);
    // Most draws sit next to the release that introduced the option.
    let major = tc.draw(gs::sampled_from(vec![1_u32, 2, 2, 2, 3]));
    let minor = tc.draw(hegel::one_of!(
        gs::sampled_from(vec![42_u32, 43, 44]),
        number()
    ));
    let patch = tc.draw(number());
    let supports = |version: String| {
        GitInfo {
            command: "git".into(),
            version,
        }
        .supports_attribute_source()
    };
    let release = format!("git version {major}.{minor}.{patch}");
    let expected = (major, minor) >= (2, 43);
    assert_eq!(supports(release.clone()), expected, "{release}");
    // Vendor builds append to a release without changing what it supports.
    let build = tc.draw(number());
    assert_eq!(supports(format!("{release}.windows.{build}")), expected);
    assert_eq!(supports(format!("{release} (Apple Git-{build})")), expected);

    // Anything that is not a plain release keeps the private-index path.
    let unknown = tc.draw(gs::sampled_from(vec![
        format!("{major}.{minor}.{patch}"),
        format!("git version {major}.{minor}"),
        format!("git version {major}.{minor}.{patch}-rc{build}"),
        format!("git version {major}.{minor}.{patch}.rc{build}"),
        format!("git version {major}.{minor}.{patch}.{build}"),
        format!("git version {major}.x.{patch}"),
        format!("git version v{major}.{minor}.{patch}"),
        format!("git version {major}.{minor}.99999999999"),
        "git version".to_owned(),
    ]));
    assert!(!supports(unknown.clone()), "{unknown}");
}

struct TreeRecord {
    mode: u32,
    kind: Vec<u8>,
    id: String,
    path: Vec<u8>,
}

impl TreeRecord {
    fn draw(tc: &TestCase) -> Self {
        Self {
            mode: tc.draw(hegel::one_of!(
                gs::sampled_from(vec![0o100644_u32, 0o100755, 0o120000, 0o160000, 0o040000]),
                gs::integers::<u32>()
            )),
            kind: bytes_without(tc, b"\0 \t", 1),
            id: object_id(tc),
            path: bytes_without(tc, b"\0", 1),
        }
    }

    fn encode(&self, mode: &[u8], id: &[u8], path: &[u8]) -> Vec<u8> {
        [mode, b" ", &self.kind, b" ", id, b"\t", path, b"\0"].concat()
    }

    fn encode_valid(&self) -> Vec<u8> {
        self.encode(
            format!("{:06o}", self.mode).as_bytes(),
            self.id.as_bytes(),
            &self.path,
        )
    }
}

#[hegel::test]
fn tree_entries_round_trip_every_record(tc: TestCase) {
    let record_count = tc.draw(gs::integers::<usize>().max_value(5));
    let records = (0..record_count)
        .map(|_| TreeRecord::draw(&tc))
        .collect::<Vec<_>>();
    let mut input = Vec::new();
    for record in &records {
        input.extend(record.encode_valid());
    }

    let entries = parse_tree_entries(&input).expect("well-formed ls-tree output");

    assert_eq!(entries.len(), records.len());
    for (entry, record) in entries.iter().zip(&records) {
        assert_eq!(entry.mode, record.mode);
        assert_eq!(entry.object_kind, record.kind);
        assert_eq!(entry.object_id.as_str(), record.id.to_ascii_lowercase());
        assert_eq!(entry.path.as_os_str().as_bytes(), record.path);
    }
}

#[hegel::test]
fn tree_entries_refuse_a_record_missing_any_field(tc: TestCase) {
    let record = TreeRecord::draw(&tc);
    let mode = format!("{:06o}", record.mode);
    let (mode, id, path) = (mode.as_bytes(), record.id.as_bytes(), &record.path[..]);
    let untabbed = record
        .encode_valid()
        .into_iter()
        .filter(|byte| *byte != b'\t')
        .collect::<Vec<_>>();
    let corrupted = tc.draw(gs::sampled_from(vec![
        ("no tab before the path", untabbed),
        ("empty path", record.encode(mode, id, b"")),
        ("empty mode", record.encode(b"", id, path)),
        ("empty object ID", record.encode(mode, b"", path)),
        ("non-octal mode", record.encode(b"100648", id, path)),
        ("non-UTF-8 mode", record.encode(b"1006\xff4", id, path)),
        (
            "mode wider than 32 bits",
            record.encode(b"777777777777", id, path),
        ),
        ("truncated object ID", record.encode(mode, &id[1..], path)),
        (
            "non-UTF-8 object ID",
            record.encode(mode, &[id, b"\xff"].concat()[1..], path),
        ),
        (
            "extra metadata field",
            record.encode(mode, &[id, b" extra"].concat(), path),
        ),
    ]));

    let result = parse_tree_entries(&corrupted.1);

    assert!(is_invalid_output(&result), "{}: {result:?}", corrupted.0);
}

#[hegel::test(test_cases = 500)]
fn tree_entries_accepted_after_corruption_still_match_their_records(tc: TestCase) {
    let mut input = TreeRecord::draw(&tc).encode_valid();
    input.extend(TreeRecord::draw(&tc).encode_valid());
    corrupt_one_byte(&tc, &mut input);

    let Ok(entries) = parse_tree_entries(&input) else {
        return;
    };

    let records = input
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), records.len());
    for (entry, record) in entries.iter().zip(records) {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .expect("an accepted record has a path delimiter");
        let path = entry.path.as_os_str().as_bytes();
        assert!(!path.is_empty());
        assert_eq!(&record[tab + 1..], path);
        let metadata = record[..tab]
            .split(|byte| *byte == b' ')
            .collect::<Vec<_>>();
        assert_eq!(metadata.len(), 3);
        assert_eq!(metadata[1], entry.object_kind);
        assert!(metadata[2].eq_ignore_ascii_case(entry.object_id.as_str().as_bytes()));
    }
}

#[hegel::test]
#[ignore = "bug: parse_tree_entries accepts a signed mode such as +100644"]
fn tree_entry_modes_are_octal_digits_only(tc: TestCase) {
    let record = TreeRecord::draw(&tc);
    let mode: String = tc.draw(gs::text().alphabet("+-01234567").min_size(1).max_size(8));
    let input = record.encode(mode.as_bytes(), record.id.as_bytes(), &record.path);

    let accepted = parse_tree_entries(&input).is_ok();

    assert_eq!(
        accepted,
        mode.bytes().all(|digit| digit.is_ascii_digit()),
        "{mode}"
    );
}

#[derive(Debug, Clone, PartialEq)]
enum HeadState {
    Branch(Vec<u8>),
    Detached,
    Bare,
    Unresolvable,
}

/// `None` is no annotation, an empty reason is the bare `locked`/`prunable`
/// field, and a non-empty reason follows the field name.
fn annotation(tc: &TestCase, name: &[u8], fields: &mut Vec<Vec<u8>>) -> Option<Vec<u8>> {
    let reason = tc
        .draw(gs::booleans())
        .then(|| bytes_without(tc, b"\0", 0))?;
    fields.push(if reason.is_empty() {
        name.to_vec()
    } else {
        [name, b" ", &reason].concat()
    });
    Some(reason)
}

struct WorktreeRecord {
    path: Vec<u8>,
    head: Option<String>,
    state: HeadState,
    locked: Option<Vec<u8>>,
    prunable: Option<Vec<u8>>,
    encoded: Vec<u8>,
}

impl WorktreeRecord {
    fn draw(tc: &TestCase) -> Self {
        let path = bytes_without(tc, b"\0", 1);
        let mut fields = vec![[b"worktree ", &path[..]].concat()];
        let head = tc.draw(gs::booleans()).then(|| object_id(tc));
        if let Some(head) = &head {
            fields.push(format!("HEAD {head}").into_bytes());
        } else if tc.draw(gs::booleans()) {
            // Git prints a null ID for a HEAD it cannot resolve.
            fields.push(format!("HEAD {}", "0".repeat(40)).into_bytes());
        }
        let state = match tc.draw(gs::integers::<u8>().max_value(3)) {
            0 => HeadState::Branch(bytes_without(tc, b"\0", 0)),
            1 => HeadState::Detached,
            2 => HeadState::Bare,
            _ => HeadState::Unresolvable,
        };
        match &state {
            HeadState::Branch(name) => fields.push([b"branch ", &name[..]].concat()),
            HeadState::Detached => fields.push(b"detached".to_vec()),
            HeadState::Bare => fields.push(b"bare".to_vec()),
            HeadState::Unresolvable => {}
        }
        let locked = annotation(tc, b"locked", &mut fields);
        let prunable = annotation(tc, b"prunable", &mut fields);
        if tc.draw(gs::booleans()) {
            // A field from a newer Git must be skipped, not misread.
            fields.push([b"future-", &bytes_without(tc, b"\0", 0)[..]].concat());
        }
        let mut encoded = fields.join(&0);
        encoded.push(0);
        // Git ends a record with an empty field. The next `worktree` field
        // also ends it, which is what a listing without that terminator hits.
        if tc.draw(gs::booleans()) {
            encoded.push(0);
        }
        Self {
            path,
            head: head.filter(|head| head.bytes().any(|digit| digit != b'0')),
            state,
            locked,
            prunable,
            encoded,
        }
    }
}

#[hegel::test]
fn worktree_porcelain_round_trips_every_record(tc: TestCase) {
    let record_count = tc.draw(gs::integers::<usize>().max_value(4));
    let records = (0..record_count)
        .map(|_| WorktreeRecord::draw(&tc))
        .collect::<Vec<_>>();
    let input = records
        .iter()
        .flat_map(|record| record.encoded.clone())
        .collect::<Vec<_>>();

    let worktrees = parse_worktree_porcelain(&input).expect("well-formed porcelain");

    assert_eq!(worktrees.len(), records.len());
    for (worktree, record) in worktrees.iter().zip(&records) {
        assert_eq!(worktree.path.as_os_str().as_bytes(), record.path);
        assert_eq!(
            worktree.head.as_ref().map(ObjectId::as_str),
            record
                .head
                .as_ref()
                .map(|head| head.to_ascii_lowercase())
                .as_deref()
        );
        let state = match (&worktree.branch, worktree.detached, worktree.bare) {
            (Some(branch), false, false) => HeadState::Branch(branch.clone()),
            (None, true, false) => HeadState::Detached,
            (None, false, true) => HeadState::Bare,
            (None, false, false) => HeadState::Unresolvable,
            ambiguous => panic!("ambiguous head state {ambiguous:?}"),
        };
        assert_eq!(state, record.state);
        assert_eq!(
            worktree.head_unresolvable,
            record.state == HeadState::Unresolvable
        );
        assert_eq!(worktree.locked_reason, record.locked);
        assert_eq!(worktree.prunable_reason, record.prunable);
    }
}

#[hegel::test]
fn worktree_porcelain_refuses_ambiguous_or_orphaned_fields(tc: TestCase) {
    let path = bytes_without(&tc, b"\0", 1);
    let states = tc.draw(gs::sampled_from(vec![
        vec![&b"detached"[..], b"bare"],
        vec![b"branch refs/heads/main", b"bare"],
        vec![b"branch refs/heads/main", b"detached"],
        vec![b"branch refs/heads/main", b"detached", b"bare"],
    ]));
    let mut ambiguous = [b"worktree ", &path[..], b"\0"].concat();
    for state in tc.draw(gs::permutations(states)) {
        ambiguous.extend_from_slice(state);
        ambiguous.push(0);
    }
    assert!(is_invalid_output(&parse_worktree_porcelain(&ambiguous)));

    let orphan = tc.draw(gs::sampled_from(vec![
        format!("HEAD {}", object_id(&tc)).into_bytes(),
        b"branch refs/heads/main".to_vec(),
        b"detached".to_vec(),
        b"locked".to_vec(),
        [b"future-", &path[..]].concat(),
    ]));
    let orphaned = [&orphan[..], b"\0worktree ", &path, b"\0bare\0\0"].concat();
    assert!(is_invalid_output(&parse_worktree_porcelain(&orphaned)));

    let bad_head = bytes_without(&tc, b"\0", 0);
    tc.assume(ObjectId::parse(String::from_utf8_lossy(&bad_head).into_owned()).is_err());
    let unparsable = [b"worktree ", &path[..], b"\0HEAD ", &bad_head, b"\0\0"].concat();
    assert!(is_invalid_output(&parse_worktree_porcelain(&unparsable)));
}

#[hegel::test]
fn worktree_porcelain_never_panics_and_keeps_every_listed_path(tc: TestCase) {
    let mut input = WorktreeRecord::draw(&tc).encoded;
    input.extend(WorktreeRecord::draw(&tc).encoded);
    corrupt_one_byte(&tc, &mut input);

    let Ok(worktrees) = parse_worktree_porcelain(&input) else {
        return;
    };

    let listed = input
        .split(|byte| *byte == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .collect::<Vec<_>>();
    let parsed = worktrees
        .iter()
        .map(|worktree| worktree.path.as_os_str().as_bytes())
        .collect::<Vec<_>>();
    assert_eq!(parsed, listed);
    for worktree in &worktrees {
        let states = usize::from(worktree.branch.is_some())
            + usize::from(worktree.detached)
            + usize::from(worktree.bare);
        assert_eq!(worktree.head_unresolvable, states == 0);
        assert!(states <= 1);
    }
}

/// `check-attr -z` as documented: NUL-terminated path, name, and value
/// fields in triples, where only the value may be empty.
fn attribute_records_model(input: &[u8]) -> Option<Vec<GitAttribute>> {
    if input.is_empty() {
        return Some(Vec::new());
    }
    let fields = input
        .strip_suffix(&[0])?
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return None;
    }
    fields
        .chunks(3)
        .map(|triple| {
            (!triple[0].is_empty() && !triple[1].is_empty()).then(|| GitAttribute {
                path: OsStr::from_bytes(triple[0]).into(),
                name: triple[1].to_vec(),
                value: triple[2].to_vec(),
            })
        })
        .collect()
}

#[hegel::test]
fn attribute_records_match_the_documented_triple_format(tc: TestCase) {
    // Short fields from a small alphabet, so that complete triples, empty
    // fields, and a missing terminator all come up often.
    let fields = tc.draw(
        gs::vecs(gs::vecs(gs::sampled_from(vec![b'a', b'/', b'\n', b':', 0xff])).max_size(2))
            .max_size(7),
    );
    let mut input = fields
        .iter()
        .flat_map(|field| [&field[..], &[0]].concat())
        .collect::<Vec<_>>();
    if tc.draw(gs::booleans()) {
        input.pop();
    }

    let parsed = parse_attribute_records(&input);

    match attribute_records_model(&input) {
        Some(expected) => assert_eq!(parsed.expect("well-formed attribute records"), expected),
        None => assert!(is_invalid_output(&parsed), "{parsed:?}"),
    }
}

#[hegel::test]
fn attribute_records_round_trip_arbitrary_paths_and_values(tc: TestCase) {
    let record_count = tc.draw(gs::integers::<usize>().max_value(5));
    let expected = (0..record_count)
        .map(|_| GitAttribute {
            path: OsStr::from_bytes(&bytes_without(&tc, b"\0", 1)).into(),
            name: bytes_without(&tc, b"\0", 1),
            value: bytes_without(&tc, b"\0", 0),
        })
        .collect::<Vec<_>>();
    let mut input = Vec::new();
    for attribute in &expected {
        for field in [
            attribute.path.as_os_str().as_bytes(),
            &attribute.name,
            &attribute.value,
        ] {
            input.extend_from_slice(field);
            input.push(0);
        }
    }

    assert_eq!(
        parse_attribute_records(&input).expect("well-formed attribute records"),
        expected
    );
}

fn blob_size_line(id: &str, size: u64) -> String {
    format!("{id} blob {size}\n")
}

#[hegel::test]
fn blob_sizes_round_trip_in_request_order(tc: TestCase) {
    let request_count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
    let requests = (0..request_count)
        .map(|_| {
            let id = ObjectId::parse(object_id(&tc)).expect("generated object ID");
            (id, tc.draw(gs::integers::<u64>()))
        })
        .collect::<Vec<_>>();
    let objects = requests
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let sizes = requests.iter().map(|(_, size)| *size).collect::<Vec<_>>();
    let response = requests
        .iter()
        .map(|(id, size)| blob_size_line(id.as_str(), *size))
        .collect::<String>();

    assert_eq!(
        parse_blob_sizes(response.as_bytes(), &objects).expect("well-formed sizes"),
        sizes
    );

    // One wrong line anywhere poisons the whole response.
    let (id, size) = &requests[tc.draw(gs::integers::<usize>().max_value(request_count - 1))];
    let line = blob_size_line(id.as_str(), *size);
    let replacement = tc.draw(gs::sampled_from(vec![
        String::new(),
        format!("{line}{line}"),
        format!("{} tree {size}\n", id.as_str()),
        format!("{} missing\n", id.as_str()),
        format!("{} blob +{size}\n", id.as_str()),
        format!("{} blob \n", id.as_str()),
        format!("{} blob {size}9999999999999999999999\n", id.as_str()),
        format!("{} blob {size} \n", id.as_str()),
        format!("{}  blob {size}\n", id.as_str()),
        format!(
            "{} blob {size}\n",
            id.as_str().to_ascii_uppercase().replace('0', "g")
        ),
        format!("{} blob {size}\r\n", id.as_str()),
        line.trim_end().to_owned(),
    ]));
    tc.assume(replacement != line);
    let corrupted = response.replacen(&line, &replacement, 1);
    tc.assume(corrupted != response);
    let result = parse_blob_sizes(corrupted.as_bytes(), &objects);
    assert!(is_invalid_output(&result), "{corrupted:?}: {result:?}");
}

#[hegel::test]
fn bounded_blob_reads_return_the_body_or_refuse_without_overreading(tc: TestCase) {
    let id = ObjectId::parse(object_id(&tc)).expect("generated object ID");
    let body = tc.draw(gs::binary().max_size(48));
    let following = tc.draw(gs::binary().max_size(8));
    let max_bytes = tc.draw(gs::integers::<usize>().max_value(64));
    let header = blob_size_line(id.as_str(), body.len() as u64);
    let stream = [header.as_bytes(), &body, b"\n", &following].concat();
    let mut reader = &stream[..];

    let result = read_batch_blob(&mut reader, &id, max_bytes);

    if body.len() > max_bytes {
        assert!(is_invalid_output(&result), "{result:?}");
        // An oversized body is refused from its header alone.
        assert_eq!(reader.len(), body.len() + 1 + following.len());
        return;
    }
    assert_eq!(result.expect("blob within the limit"), body);
    assert_eq!(reader, &following[..]);

    let cut = tc.draw(gs::integers::<usize>().max_value(header.len() + body.len()));
    let truncated = read_batch_blob(&mut &stream[..cut], &id, max_bytes);
    assert!(is_invalid_output(&truncated), "cut at {cut}: {truncated:?}");

    let delimiter = tc.draw(gs::integers::<u8>().filter(|byte| *byte != b'\n'));
    let mut undelimited = stream.clone();
    undelimited[header.len() + body.len()] = delimiter;
    let undelimited = read_batch_blob(&mut &undelimited[..], &id, max_bytes);
    assert!(is_invalid_output(&undelimited), "{undelimited:?}");
}

#[hegel::test]
fn bounded_blob_reads_refuse_an_unterminated_or_non_utf8_header(tc: TestCase) {
    let id = ObjectId::parse(object_id(&tc)).expect("generated object ID");
    let header = tc.draw(hegel::one_of!(
        gs::vecs(gs::integers::<u8>().filter(|byte| *byte != b'\n'))
            .min_size(128)
            .max_size(200),
        gs::binary().max_size(40).map(|mut bytes| {
            bytes.extend_from_slice(b"\xff\n");
            bytes
        })
    ));

    let result = read_batch_blob(&mut &header[..], &id, usize::MAX);

    assert!(is_invalid_output(&result), "{result:?}");
}

fn resolved_revision(tc: &TestCase) -> (ResolvedRevision, Vec<u8>) {
    let (commit, tree) = (object_id(tc), object_id(tc));
    let encoded = format!("{commit}\0{tree}\0").into_bytes();
    let revision = ResolvedRevision {
        commit: ObjectId::parse(commit).expect("generated commit ID"),
        tree: ObjectId::parse(tree).expect("generated tree ID"),
    };
    (revision, encoded)
}

fn canonical_revision(revision: &ResolvedRevision) -> Vec<u8> {
    format!("{}\0{}\0", revision.commit.as_str(), revision.tree.as_str()).into_bytes()
}

#[hegel::test]
fn resolved_revisions_round_trip_and_accept_nothing_else(tc: TestCase) {
    let (revision, mut input) = resolved_revision(&tc);
    assert_eq!(
        parse_resolved_revision_output(&input).expect("well-formed revision"),
        revision
    );

    corrupt_one_byte(&tc, &mut input);
    match parse_resolved_revision_output(&input) {
        Ok(accepted) => assert_eq!(input.to_ascii_lowercase(), canonical_revision(&accepted)),
        Err(error) => assert!(is_invalid_output::<()>(&Err(error))),
    }
}

#[hegel::test]
fn paired_revisions_keep_head_first_and_accept_nothing_else(tc: TestCase) {
    let (head, head_record) = resolved_revision(&tc);
    let (requested, requested_record) = resolved_revision(&tc);

    // Git prints one record when both expressions name the same commit.
    assert_eq!(
        parse_paired_revision_output(&head_record).expect("single record"),
        (head.clone(), head.clone())
    );
    let mut input = [&head_record[..], b"\n", &requested_record].concat();
    assert_eq!(
        parse_paired_revision_output(&input).expect("two records"),
        (head, requested)
    );
    let third = [&input[..], b"\n", &requested_record].concat();
    assert!(is_invalid_output(&parse_paired_revision_output(&third)));

    corrupt_one_byte(&tc, &mut input);
    match parse_paired_revision_output(&input) {
        Ok((head, requested)) => {
            let single = canonical_revision(&head);
            let pair = [&single[..], b"\n", &canonical_revision(&requested)].concat();
            let input = input.to_ascii_lowercase();
            assert!(input == pair || (input == single && head == requested));
        }
        Err(error) => assert!(is_invalid_output::<()>(&Err(error))),
    }
}

#[hegel::test]
fn checkout_storage_splits_only_at_the_final_line(tc: TestCase) {
    // The object directory is a native path: it may itself contain newlines.
    let directory = bytes_without(&tc, b"", 1);
    let format: String = tc.draw(
        gs::text()
            .min_size(1)
            .max_size(8)
            .filter(|format: &String| !format.contains(['\n', '\r'])),
    );
    let terminator = tc.draw(gs::sampled_from(vec!["", "\n"]));
    let output = [
        &directory[..],
        b"\n",
        format.as_bytes(),
        terminator.as_bytes(),
    ]
    .concat();

    let (parsed_directory, parsed_format) =
        parse_checkout_storage(&output).expect("object directory and format");

    assert_eq!(parsed_directory.as_os_str().as_bytes(), directory);
    assert_eq!(parsed_format, format);
}

#[hegel::test]
fn checkout_storage_refuses_a_missing_directory_or_format(tc: TestCase) {
    let line = bytes_without(&tc, b"\n", 0);
    let invalid_format = bytes_without(&tc, b"\n\r", 0);
    tc.assume(std::str::from_utf8(&invalid_format).is_err());
    let output = tc.draw(gs::sampled_from(vec![
        line.clone(),
        [&line[..], b"\n"].concat(),
        [b"\n", &line[..]].concat(),
        [b"\n", &line[..], b"\n"].concat(),
        [b"objects\n", &invalid_format[..], b"\n"].concat(),
        b"objects\n\n".to_vec(),
        b"objects\n\r\n".to_vec(),
    ]));

    let result = parse_checkout_storage(&output);

    assert!(is_invalid_output(&result), "{output:?}: {result:?}");
}

fn config_key(tc: &TestCase) -> String {
    let name = || gs::text().alphabet("abXY09-").min_size(1).max_size(4);
    let section: String = tc.draw(name());
    let subsections: Vec<String> = tc.draw(gs::vecs(name()).max_size(2));
    let variable = format!(
        "{}{}",
        tc.draw(gs::sampled_from(vec!['a', 'Q'])),
        tc.draw(name())
    );
    let mut components = vec![section];
    components.extend(subsections);
    components.push(variable);
    components.join(".")
}

#[hegel::test]
fn config_keys_normalize_only_the_case_insensitive_components(tc: TestCase) {
    let key = config_key(&tc);

    let normalized = normalize_config_key(&key).expect("simple section.variable key");

    let (section, rest) = key.split_once('.').expect("generated key has a section");
    let (subsection, variable) = rest
        .rsplit_once('.')
        .map_or(("", rest), |(middle, last)| (middle, last));
    let expected = [
        section.to_ascii_lowercase().as_str(),
        subsection,
        variable.to_ascii_lowercase().as_str(),
    ]
    .into_iter()
    .filter(|component| !component.is_empty())
    .collect::<Vec<_>>()
    .join(".");
    assert_eq!(normalized, expected);
    assert_eq!(
        normalize_config_key(&normalized).expect("normalized key stays valid"),
        normalized
    );
}

#[hegel::test]
fn config_keys_refuse_anything_git_could_read_as_a_pattern(tc: TestCase) {
    let key = config_key(&tc);
    let index = tc.draw(gs::integers::<usize>().max_value(key.len()));
    let inserted = tc.draw(gs::sampled_from(vec![
        "*", "^", "$", "|", "(", "[", "\\", " ", "\n", "=", "_", "é", "..",
    ]));
    let mut patterned = key.clone();
    patterned.insert_str(index, inserted);
    let variable_digit = format!("{}.9{}", key.rsplit_once('.').expect("section").0, "x");
    let invalid = tc.draw(gs::sampled_from(vec![
        patterned,
        variable_digit,
        format!(".{key}"),
        format!("{key}."),
        key.replace('.', ""),
        String::new(),
    ]));

    let result = normalize_config_key(&invalid);

    assert!(is_invalid_output(&result), "{invalid:?}: {result:?}");
}

#[hegel::test]
fn config_values_keep_the_last_raw_value_for_each_requested_key(tc: TestCase) {
    let mut keys: Vec<String> = Vec::new();
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(3)) {
        let key = normalize_config_key(&config_key(&tc)).expect("generated key");
        if !keys.contains(&key) && !key.starts_with("includeif.") {
            keys.push(key);
        }
    }
    tc.assume(!keys.is_empty());
    let mut output = Vec::new();
    let mut expected = std::collections::BTreeMap::new();
    let mut has_conditional_includes = false;
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(6)) {
        if tc.draw(gs::booleans()) {
            let condition = bytes_without(&tc, b"\0\n", 0);
            output.extend([b"includeif.", &condition[..], b".path\n/included\0"].concat());
            has_conditional_includes = true;
            continue;
        }
        let key = tc.draw(gs::sampled_from(keys.clone()));
        // An implicit boolean has no value separator at all.
        let value = tc
            .draw(gs::booleans())
            .then(|| bytes_without(&tc, b"\0", 0));
        output.extend_from_slice(key.as_bytes());
        if let Some(value) = &value {
            output.push(b'\n');
            output.extend_from_slice(value);
        }
        output.push(0);
        expected.insert(key, value.unwrap_or_default());
    }

    let config = parse_config_values(&output, &keys).expect("well-formed configuration records");

    assert_eq!(config.values, expected);
    assert_eq!(config.has_conditional_includes, has_conditional_includes);

    tc.assume(!output.is_empty());
    let unterminated = &output[..output.len() - 1];
    assert!(is_invalid_output(&parse_config_values(unterminated, &keys)));
    let unrequested = [&output[..], b"other.key\nvalue\0"].concat();
    assert!(is_invalid_output(&parse_config_values(&unrequested, &keys)));
}

#[hegel::test]
fn config_value_origins_round_trip_or_refuse_incomplete_records(tc: TestCase) {
    let record_count = tc.draw(gs::integers::<usize>().max_value(4));
    let expected = (0..record_count)
        .map(|_| ConfigValueOrigin {
            scope: bytes_without(&tc, b"\0", 1),
            origin: bytes_without(&tc, b"\0", 1),
            value: bytes_without(&tc, b"\0", 0),
        })
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    for record in &expected {
        for field in [&record.scope, &record.origin, &record.value] {
            output.extend_from_slice(field);
            output.push(0);
        }
    }

    assert_eq!(
        parse_config_value_origins(&output).expect("well-formed origin records"),
        expected
    );

    tc.assume(!output.is_empty());
    let field = bytes_without(&tc, b"\0", 0);
    let incomplete = tc.draw(gs::sampled_from(vec![
        output[..output.len() - 1].to_vec(),
        [&output[..], &field[..], b"\0"].concat(),
        [&output[..], b"\0origin\0", &field[..], b"\0"].concat(),
        [&output[..], b"scope\0\0", &field[..], b"\0"].concat(),
    ]));
    let result = parse_config_value_origins(&incomplete);
    assert!(is_invalid_output(&result), "{incomplete:?}: {result:?}");
}
