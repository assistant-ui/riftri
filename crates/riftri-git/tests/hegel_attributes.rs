//! Property tests that compare attribute queries with a model of Git's
//! attribute precedence, over generated rules and arbitrary path bytes.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use riftri_git::{Git, GitAttribute, ObjectId};

#[allow(dead_code)]
mod support;

use support::Fixture;

/// One attributes line: a pattern and a single attribute state.
#[derive(Debug, Clone)]
struct Rule {
    /// `None` is the catch-all `*`; otherwise a basename suffix `*<suffix>`.
    suffix: Option<&'static str>,
    name: &'static str,
    /// `None` is `!name`, which returns the attribute to unspecified.
    state: Option<&'static str>,
}

impl Rule {
    fn line(&self) -> String {
        let pattern = format!("*{}", self.suffix.unwrap_or_default());
        match self.state {
            None => format!("{pattern} !{}\n", self.name),
            Some("set") => format!("{pattern} {}\n", self.name),
            Some("unset") => format!("{pattern} -{}\n", self.name),
            Some(value) => format!("{pattern} {}={value}\n", self.name),
        }
    }

    fn matches(&self, path: &[u8]) -> bool {
        let basename = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
        self.suffix
            .is_none_or(|suffix| basename.ends_with(suffix.as_bytes()))
    }
}

fn rules() -> impl PrintableGenerator<Vec<Rule>> {
    gs::vecs(hegel::compose!(|tc| {
        Rule {
            suffix: tc.draw(gs::sampled_from(vec![None, Some(".bin"), Some(".txt")])),
            name: tc.draw(gs::sampled_from(vec!["text", "eol", "custom"])),
            state: tc.draw(gs::sampled_from(vec![
                None,
                Some("set"),
                Some("unset"),
                Some("lf"),
                Some("crlf"),
            ])),
        }
    }))
    .max_size(4)
}

fn file(rules: &[Rule]) -> String {
    rules.iter().map(Rule::line).collect()
}

/// Git's precedence: sources are listed lowest first, and within a source a
/// later line overrides an earlier one.
fn model(sources: &[&[Rule]], paths: &[PathBuf]) -> Vec<GitAttribute> {
    let mut attributes = Vec::new();
    for path in paths {
        let mut states = BTreeMap::new();
        for rule in sources.iter().flat_map(|source| source.iter()) {
            if rule.matches(path.as_os_str().as_bytes()) {
                states.insert(rule.name, rule.state);
            }
        }
        for (name, state) in states {
            if let Some(state) = state {
                attributes.push(GitAttribute {
                    path: path.clone(),
                    name: name.as_bytes().to_vec(),
                    value: state.as_bytes().to_vec(),
                });
            }
        }
    }
    attributes
}

/// A relative path Git reports back unchanged: no empty, `.`, or `..`
/// component, which it would normalize or refuse.
fn path() -> impl PrintableGenerator<PathBuf> {
    let component = gs::vecs(hegel::one_of!(
        gs::sampled_from(b"\n\t \"\\'-:*?aZ\x80\xc3\xa9\xff".to_vec()),
        gs::integers::<u8>().min_value(1)
    ))
    .min_size(1)
    .max_size(6)
    .filter(|name: &Vec<u8>| !name.contains(&b'/') && name != b"." && name != b"..");
    hegel::tuples!(
        gs::vecs(component).min_size(1).max_size(3),
        gs::sampled_from(vec!["", ".bin", ".txt", ".BIN"])
    )
    .map(|(components, extension)| {
        let mut bytes = components.join(&b'/');
        bytes.extend_from_slice(extension.as_bytes());
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    })
}

/// Group by path: Git orders the attributes of one path as it pleases.
fn sorted(mut attributes: Vec<GitAttribute>) -> Vec<GitAttribute> {
    attributes.sort();
    attributes
}

struct Sources {
    fixture: Fixture,
    tree: ObjectId,
    global: Vec<Rule>,
    in_tree: Vec<Rule>,
    info: Vec<Rule>,
}

impl Sources {
    fn draw(tc: &TestCase, with_in_tree: bool) -> Self {
        let fixture = Fixture::new(tc.draw(gs::sampled_from(vec!["sha1", "sha256"])));
        let repository = fixture.repository();
        let global = tc.draw(rules());
        let in_tree = if with_in_tree {
            tc.draw(rules())
        } else {
            Vec::new()
        };
        let info = tc.draw(rules());
        // A repository-local attributes file outranks the user's own, so
        // the effective result does not depend on this machine.
        let global_file = fixture.root().join("global-attributes");
        std::fs::write(&global_file, file(&global)).unwrap();
        for (key, value) in [
            ("core.attributesFile", global_file.to_str().unwrap()),
            // Pattern matching follows the volume's case sensitivity
            // unless the repository says otherwise.
            ("core.ignorecase", "false"),
        ] {
            support::git(&repository, &["config", key, value]);
        }
        std::fs::write(repository.join(".git/info/attributes"), file(&info)).unwrap();
        let blob = fixture.blob(file(&in_tree).as_bytes());
        let name: &[u8] = if with_in_tree {
            b".gitattributes"
        } else {
            b"unrelated"
        };
        let tree = fixture.tree(&[(0o100644, "blob", &blob, name)]);
        Self {
            fixture,
            tree,
            global,
            in_tree,
            info,
        }
    }

    fn effective(&self, paths: &[PathBuf]) -> Vec<GitAttribute> {
        sorted(model(&[&self.global, &self.in_tree, &self.info], paths))
    }

    /// Isolation removes the configured file but not the repository's own
    /// `info/attributes`, which callers reject separately.
    fn isolated(&self, paths: &[PathBuf]) -> Vec<GitAttribute> {
        sorted(model(&[&self.in_tree, &self.info], paths))
    }
}

#[hegel::test(test_cases = 15)]
fn tree_queries_follow_git_precedence_and_isolate_configured_attributes(tc: TestCase) {
    let sources = Sources::draw(&tc, true);
    let repository = sources.fixture.repository();
    let git = Git::default();
    let paths = tc.draw(gs::vecs(path()).max_size(4));
    let (effective, isolated) = (sources.effective(&paths), sources.isolated(&paths));

    assert_eq!(
        sorted(
            git.effective_attributes_for_tree_paths(&repository, &sources.tree, &paths)
                .unwrap()
        ),
        effective
    );
    assert_eq!(
        sorted(
            git.in_tree_attributes_for_paths(&repository, &sources.tree, &paths)
                .unwrap()
        ),
        isolated
    );

    // One shared index answers the same questions as a private one each.
    let index = git
        .tree_attribute_index(&repository, &sources.tree)
        .unwrap();
    let (in_tree, shared) = git
        .attribute_pair_for_index(&repository, &index, &paths, true)
        .unwrap();
    assert_eq!(
        (sorted(in_tree), sorted(shared)),
        (isolated.clone(), effective.clone())
    );
    let info = git.detect().unwrap();
    let (in_tree, direct) = git
        .attribute_pair_for_tree(&repository, &sources.tree, &paths, true, &info)
        .unwrap();
    assert_eq!((sorted(in_tree), sorted(direct)), (isolated, effective));
}

#[hegel::test(test_cases = 15)]
fn a_tree_without_attributes_gets_the_same_answer_with_or_without_an_index(tc: TestCase) {
    let sources = Sources::draw(&tc, false);
    let repository = sources.fixture.repository();
    let git = Git::default();
    let paths = tc.draw(gs::vecs(path()).max_size(4));
    let effective = sources.effective(&paths);
    let mut info = git.detect().unwrap();

    // The installed Git reads the tree directly; a Git of unknown age must
    // fall back to the index and still agree.
    for version in [info.version.clone(), "git version unknown".to_owned()] {
        info.version = version;
        let (in_tree, direct) = git
            .attribute_pair_for_tree(&repository, &sources.tree, &paths, false, &info)
            .unwrap();
        assert!(in_tree.is_empty());
        assert_eq!(sorted(direct), effective, "{}", info.version);
    }
}

#[hegel::test(test_cases = 15)]
fn working_tree_queries_report_whether_any_rule_applies(tc: TestCase) {
    let sources = Sources::draw(&tc, false);
    let repository = sources.fixture.repository();
    let git = Git::default();
    let paths = tc.draw(gs::vecs(path()).max_size(4));
    let effective = sources.effective(&paths);

    assert_eq!(
        sorted(
            git.effective_attributes_for_paths(&repository, &paths)
                .unwrap()
        ),
        effective
    );
    assert_eq!(
        git.paths_have_effective_attributes(&repository, &paths)
            .unwrap(),
        !effective.is_empty()
    );
    assert_eq!(
        git.info_attributes_path(&repository).unwrap(),
        repository.join(".git/info/attributes")
    );
}
