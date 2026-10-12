//! Property tests that read trees and blobs written by the real Git and
//! compare every byte with what was written.
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use riftri_git::{Git, GitError, ObjectId};

#[allow(dead_code)]
mod support;

use support::Fixture;

fn fixture(tc: &TestCase) -> Fixture {
    Fixture::new(tc.draw(gs::sampled_from(vec!["sha1", "sha256"])))
}

/// One path component. Git stores any bytes but NUL and the separator, so
/// the listing must return quotes, newlines, and invalid UTF-8 untouched.
fn component() -> impl PrintableGenerator<Vec<u8>> {
    gs::vecs(hegel::one_of!(
        gs::sampled_from(b"\n\t \"\\'-:*?aZ.\x80\xc3\xa9\xff".to_vec()),
        gs::integers::<u8>().min_value(1)
    ))
    .min_size(1)
    .max_size(8)
    .filter(|name: &Vec<u8>| !name.contains(&b'/'))
}

fn missing_object(tc: &TestCase, like: &ObjectId) -> ObjectId {
    let length = like.as_str().len();
    let id: String = tc.draw(
        gs::text()
            .alphabet("0123456789abcdef")
            .min_size(length)
            .max_size(length),
    );
    ObjectId::parse(id).unwrap()
}

#[hegel::test(test_cases = 20)]
fn tree_listing_reports_exactly_the_written_entries(tc: TestCase) {
    let fixture = fixture(&tc);
    let blob = fixture.blob(&tc.draw(gs::binary().max_size(16)));
    let mut names = tc.draw(gs::vecs(component()).min_size(1).max_size(6).unique(true));
    let directory_count = tc.draw(gs::integers::<usize>().max_value(names.len().min(2)));
    let directories = names.split_off(names.len() - directory_count);
    let leaf = |tc: &TestCase, name: Vec<u8>| {
        let (mode, kind) = tc.draw(gs::sampled_from(vec![
            (0o100644_u32, "blob"),
            (0o100755, "blob"),
            (0o120000, "blob"),
            (0o160000, "commit"),
        ]));
        let object = if kind == "commit" {
            // A gitlink names a commit of another repository.
            missing_object(tc, &blob)
        } else {
            blob.clone()
        };
        (mode, kind, object, name)
    };
    let mut expected = Vec::new();
    let mut root = names
        .into_iter()
        .map(|name| leaf(&tc, name))
        .collect::<Vec<_>>();
    expected.extend(root.iter().cloned());
    for directory in directories {
        let children = tc
            .draw(gs::vecs(component()).min_size(1).max_size(3).unique(true))
            .into_iter()
            .map(|name| leaf(&tc, name))
            .collect::<Vec<_>>();
        for (mode, kind, object, name) in &children {
            let path = [&directory[..], b"/", name].concat();
            expected.push((*mode, *kind, object.clone(), path));
        }
        let children = children
            .iter()
            .map(|(mode, kind, object, name)| (*mode, *kind, object, &name[..]))
            .collect::<Vec<_>>();
        root.push((0o040000, "tree", fixture.tree(&children), directory));
    }
    let root = root
        .iter()
        .map(|(mode, kind, object, name)| (*mode, *kind, object, &name[..]))
        .collect::<Vec<_>>();
    let tree = fixture.tree(&root);

    let mut listed = Git::default()
        .list_tree(&fixture.repository(), &tree)
        .expect("list the written tree")
        .into_iter()
        .map(|entry| {
            (
                entry.mode,
                String::from_utf8(entry.object_kind).expect("object kind"),
                entry.object_id,
                entry.path.as_os_str().as_bytes().to_vec(),
            )
        })
        .collect::<Vec<_>>();

    let mut expected = expected
        .into_iter()
        .map(|(mode, kind, object, path)| (mode, kind.to_owned(), object, path))
        .collect::<Vec<_>>();
    listed.sort_by(|left, right| left.3.cmp(&right.3));
    expected.sort_by(|left, right| left.3.cmp(&right.3));
    assert_eq!(listed, expected);
}

struct Blobs {
    fixture: Fixture,
    contents: Vec<Vec<u8>>,
    ids: Vec<ObjectId>,
}

impl Blobs {
    fn draw(tc: &TestCase) -> Self {
        let fixture = fixture(tc);
        let contents = tc.draw(gs::vecs(gs::binary().max_size(48)).min_size(1).max_size(3));
        let ids = contents
            .iter()
            .map(|content| fixture.blob(content))
            .collect();
        Self {
            fixture,
            contents,
            ids,
        }
    }

    /// Requests may repeat a blob; answers must keep request order.
    fn requests(&self, tc: &TestCase, min_size: usize) -> Vec<usize> {
        tc.draw(
            gs::vecs(gs::integers::<usize>().max_value(self.ids.len() - 1))
                .min_size(min_size)
                .max_size(5),
        )
    }

    fn ids(&self, requests: &[usize]) -> Vec<ObjectId> {
        requests
            .iter()
            .map(|index| self.ids[*index].clone())
            .collect()
    }

    fn contents(&self, requests: &[usize]) -> Vec<Vec<u8>> {
        requests
            .iter()
            .map(|index| self.contents[*index].clone())
            .collect()
    }

    fn fits(&self, requests: &[usize], max_bytes: usize) -> bool {
        requests
            .iter()
            .all(|index| self.contents[*index].len() <= max_bytes)
    }
}

fn is_invalid_output<T>(result: &Result<T, GitError>) -> bool {
    matches!(result, Err(GitError::InvalidOutput { .. }))
}

#[hegel::test(test_cases = 20)]
fn blob_reads_return_exactly_the_written_bytes(tc: TestCase) {
    let blobs = Blobs::draw(&tc);
    let repository = blobs.fixture.repository();
    let git = Git::default();
    let requests = blobs.requests(&tc, 0);
    let objects = blobs.ids(&requests);
    let contents = blobs.contents(&requests);

    for (object, content) in objects.iter().zip(&contents) {
        assert_eq!(&git.read_blob(&repository, object).unwrap(), content);
        assert_eq!(
            git.blob_size(&repository, object).unwrap(),
            content.len() as u64
        );
    }
    assert_eq!(
        git.blob_sizes(&repository, &objects).unwrap(),
        contents
            .iter()
            .map(|content| content.len() as u64)
            .collect::<Vec<_>>()
    );

    let max_bytes = tc.draw(gs::integers::<usize>().max_value(64));
    let bounded = git.read_small_blobs(&repository, &objects, max_bytes);
    if blobs.fits(&requests, max_bytes) {
        assert_eq!(bounded.unwrap(), contents);
    } else {
        assert!(is_invalid_output(&bounded), "{bounded:?}");
    }
}

#[hegel::test(test_cases = 20)]
fn blob_batches_refuse_any_request_that_is_not_a_present_blob(tc: TestCase) {
    let blobs = Blobs::draw(&tc);
    let repository = blobs.fixture.repository();
    let git = Git::default();
    let tree = blobs
        .fixture
        .tree(&[(0o100644, "blob", &blobs.ids[0], b"file")]);
    let intruder = if tc.draw(gs::booleans()) {
        tree
    } else {
        let missing = missing_object(&tc, &blobs.ids[0]);
        tc.assume(!blobs.ids.contains(&missing));
        missing
    };
    let mut objects = blobs.ids(&blobs.requests(&tc, 0));
    let position = tc.draw(gs::integers::<usize>().max_value(objects.len()));
    objects.insert(position, intruder.clone());

    let sizes = git.blob_sizes(&repository, &objects);
    assert!(is_invalid_output(&sizes), "{sizes:?}");
    let bodies = git.read_small_blobs(&repository, &objects, usize::MAX);
    assert!(is_invalid_output(&bodies), "{bodies:?}");
    assert!(git.read_blob(&repository, &intruder).is_err());
}

#[hegel::test(test_cases = 20)]
fn a_blob_session_serves_batches_until_its_first_refusal(tc: TestCase) {
    let blobs = Blobs::draw(&tc);
    let git = Git::default();
    let mut session = git
        .small_blob_reader(&blobs.fixture.repository())
        .expect("start a blob session");
    let mut refused = false;

    for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(4)) {
        let requests = blobs.requests(&tc, 0);
        let max_bytes = tc.draw(gs::integers::<usize>().max_value(64));

        let batch = session.read(&blobs.ids(&requests), max_bytes);

        // A refused batch leaves an unread body in the pipe, so nothing
        // after it can be trusted, not even an empty batch.
        if refused || !blobs.fits(&requests, max_bytes) {
            assert!(is_invalid_output(&batch), "{batch:?}");
            refused = true;
        } else {
            assert_eq!(batch.unwrap(), blobs.contents(&requests));
        }
    }

    let finished = session.finish();
    assert_eq!(finished.is_ok(), !refused, "{finished:?}");
}
