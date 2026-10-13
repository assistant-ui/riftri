//! Property tests that write configuration through the real Git and check
//! every reader returns exactly what Git stored.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use riftri_git::{Git, GitError};

#[allow(dead_code)]
mod support;

use support::Fixture;

fn name() -> impl PrintableGenerator<String> {
    gs::text().alphabet("abXY09-").max_size(3)
}

/// A `section[.subsection].variable` key in the spelling a caller might
/// use, with the lowercase form Git reports for it.
fn key(tc: &TestCase) -> (String, String) {
    let section = format!("s{}", tc.draw(name()));
    let subsection = tc.draw(gs::optional(
        name().filter(|name: &String| !name.is_empty()),
    ));
    let variable = format!("V{}", tc.draw(name()));
    let middle = subsection.map_or(String::new(), |subsection| format!(".{subsection}"));
    (
        format!("{section}{middle}.{variable}"),
        format!(
            "{}{middle}.{}",
            section.to_ascii_lowercase(),
            variable.to_ascii_lowercase()
        ),
    )
}

/// A configuration value: any bytes but NUL. A leading dash is left out
/// because Git would read the argument as an option.
fn value() -> impl PrintableGenerator<Vec<u8>> {
    gs::vecs(hegel::one_of!(
        gs::sampled_from(b"\n\r\t \"\\#;=a\xff".to_vec()),
        gs::integers::<u8>().min_value(1)
    ))
    .max_size(10)
    .filter(|value: &Vec<u8>| value.first() != Some(&b'-'))
}

fn os(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}

#[hegel::test(test_cases = 25)]
fn a_local_value_reads_back_byte_for_byte_from_every_reader(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let (key, normalized) = key(&tc);
    let recased = key.chars().rev().collect::<String>().to_ascii_uppercase();
    let recased = recased.chars().rev().collect::<String>();
    let recased = if recased.contains('.') && key.matches('.').count() == 1 {
        recased
    } else {
        // Only a key without a subsection is case-insensitive throughout.
        key.clone()
    };

    for stored in tc.draw(gs::vecs(value()).min_size(1).max_size(2)) {
        git.set_local_config(&repository, &key, &os(&stored))
            .expect("store the value");

        for spelling in [&key, &recased] {
            assert_eq!(
                git.local_config_value(&repository, spelling).unwrap(),
                Some(stored.clone())
            );
            assert_eq!(
                git.config_value(&repository, spelling).unwrap(),
                Some(stored.clone())
            );
            let batch = git.config_values(&repository, &[spelling]).unwrap();
            assert_eq!(
                batch.values,
                BTreeMap::from([(normalized.clone(), stored.clone())])
            );
            assert!(!batch.has_conditional_includes);
        }
        let origins = git.config_value_origins(&repository, &key).unwrap();
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].scope, b"local");
        assert_eq!(origins[0].origin, b"file:.git/config");
        assert_eq!(origins[0].value, stored);
    }

    for _ in 0..2 {
        // Unsetting a key that is already gone is not an error.
        git.unset_local_config(&repository, &key).expect("unset");
        assert_eq!(git.local_config_value(&repository, &key).unwrap(), None);
        assert_eq!(git.config_value(&repository, &key).unwrap(), None);
        assert!(
            git.config_values(&repository, &[&key])
                .unwrap()
                .values
                .is_empty()
        );
        assert!(
            git.config_value_origins(&repository, &key)
                .unwrap()
                .is_empty()
        );
    }
}

#[hegel::test(test_cases = 25)]
fn a_batched_read_matches_single_reads_for_every_key(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let mut keys = BTreeMap::new();
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(4)) {
        let (key, normalized) = key(&tc);
        keys.insert(normalized, key);
    }
    for key in keys.values() {
        // Zero values leaves the key missing; several exercise precedence.
        for stored in tc.draw(gs::vecs(value()).max_size(2)) {
            let arguments = [OsStr::new("config"), OsStr::new("--add"), OsStr::new(key)];
            let status = support::isolated_git(&repository)
                .args(arguments)
                .arg(os(&stored))
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
    let conditional = tc.draw(gs::booleans());
    if conditional {
        support::git(
            &repository,
            &["config", "includeIf.gitdir:/elsewhere/.path", "/missing"],
        );
    }

    let requested = keys.values().map(String::as_str).collect::<Vec<_>>();
    let batch = git.config_values(&repository, &requested).unwrap();

    let mut expected = BTreeMap::new();
    for (normalized, key) in &keys {
        if let Some(stored) = git.config_value(&repository, key).unwrap() {
            expected.insert(normalized.clone(), stored);
        }
        assert_eq!(
            git.has_config_matching(
                &repository,
                &format!("^{}$", normalized.replace('.', r"\."))
            )
            .unwrap(),
            expected.contains_key(normalized)
        );
    }
    assert_eq!(batch.values, expected);
    assert_eq!(batch.has_conditional_includes, conditional);
}

#[hegel::test(test_cases = 25)]
fn keys_git_could_read_as_a_pattern_are_refused_before_git_runs(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let (key, _) = key(&tc);
    let inserted = tc.draw(gs::sampled_from(vec![
        "*", "^", "$", "|", "(", "[", "\\", " ", "\n", "=", "_", "..",
    ]));
    let index = tc.draw(gs::integers::<usize>().max_value(key.len()));
    let mut invalid = key.clone();
    invalid.insert_str(index, inserted);

    let batch = git.config_values(&repository, &[&key, &invalid]);
    assert!(
        matches!(batch, Err(GitError::InvalidOutput { .. })),
        "{batch:?}"
    );
    let origins = git.config_value_origins(&repository, &invalid);
    assert!(
        matches!(origins, Err(GitError::InvalidOutput { .. })),
        "{origins:?}"
    );
    // Git rejects a key without a section; that is not "nothing to unset".
    let section = key.split('.').next().expect("a section");
    assert!(git.unset_local_config(&repository, section).is_err());
    // An unbalanced pattern reaches Git, whose refusal must surface.
    let pattern = git.has_config_matching(&repository, &format!("{key}("));
    assert!(
        matches!(pattern, Err(GitError::CommandFailed { .. })),
        "{pattern:?}"
    );
}

#[hegel::test(test_cases = 25)]
fn path_lists_keep_order_and_remove_only_the_exact_value(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let (key, _) = key(&tc);
    // `~` and `%(prefix)` are expanded by Git and `:(optional)` is path
    // magic, so a stored path here starts with a separator.
    let path = || value().map(|name| PathBuf::from(os(&[b"/", &name[..]].concat())));
    let mut stored = tc.draw(gs::vecs(path()).min_size(1).max_size(4));
    for path in &stored {
        git.add_local_config_path(&repository, &key, path)
            .expect("append the path");
    }
    assert_eq!(git.local_config_paths(&repository, &key).unwrap(), stored);

    let removed = if tc.draw(gs::booleans()) {
        tc.draw(gs::sampled_from(stored.clone()))
    } else {
        tc.draw(path())
    };
    let result = git.unset_local_config_value(&repository, &key, removed.as_os_str());

    // The value is matched literally, never as the regular expression Git
    // would otherwise take it for.
    assert_eq!(result.is_ok(), stored.contains(&removed), "{result:?}");
    stored.retain(|path| *path != removed);
    assert_eq!(git.local_config_paths(&repository, &key).unwrap(), stored);

    git.unset_local_config(&repository, &key).expect("unset");
    assert!(
        git.local_config_paths(&repository, &key)
            .unwrap()
            .is_empty()
    );
}

#[hegel::test(test_cases = 25)]
fn local_booleans_follow_gits_vocabulary_and_refuse_the_rest(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let (key, _) = key(&tc);
    assert_eq!(git.local_config_bool(&repository, &key).unwrap(), None);

    let (word, expected) = tc.draw(gs::sampled_from(vec![
        ("true", Some(true)),
        ("yes", Some(true)),
        ("on", Some(true)),
        ("1", Some(true)),
        ("-1", Some(true)),
        ("2k", Some(true)),
        ("false", Some(false)),
        ("no", Some(false)),
        ("off", Some(false)),
        ("0", Some(false)),
        ("00", Some(false)),
        ("", Some(false)),
        ("maybe", None),
        ("tru", None),
        ("true ", None),
        ("1x", None),
    ]));
    let uppercase = tc.draw(
        gs::vecs(gs::booleans())
            .min_size(word.len())
            .max_size(word.len()),
    );
    let spelled = word
        .chars()
        .zip(uppercase)
        .map(|(letter, upper)| {
            // The size suffix of a number is the one case-sensitive part.
            if upper && !letter.is_ascii_digit() && letter != 'k' {
                letter.to_ascii_uppercase()
            } else {
                letter
            }
        })
        .collect::<String>();
    let arguments = ["config", "--local", "--", &key, &spelled];
    support::git(&repository, &arguments);

    let read = git.local_config_bool(&repository, &key);

    match expected {
        Some(expected) => assert_eq!(read.unwrap(), Some(expected), "{spelled:?}"),
        None => assert!(
            matches!(read, Err(GitError::CommandFailed { .. })),
            "{spelled:?}: {read:?}"
        ),
    }
}

#[hegel::test(test_cases = 25)]
fn conditional_includes_pass_only_when_every_included_key_is_allowed(tc: TestCase) {
    let fixture = Fixture::new("sha1");
    let repository = fixture.repository();
    let git = Git::default();
    let pool = ["core.autocrlf", "core.eol", "filter.lfs.clean", "user.name"];
    let allowed = tc.draw(gs::subsequences(pool.to_vec()));
    assert!(
        git.conditional_config_has_only(&repository, &allowed)
            .unwrap()
    );

    let mut expected = true;
    for index in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(2)) {
        // Half the targets hold only allowed keys, so both verdicts are
        // common.
        let included = if tc.draw(gs::booleans()) {
            tc.draw(gs::subsequences(allowed.clone()))
        } else {
            tc.draw(gs::subsequences(pool.to_vec()))
        };
        // A relative target is resolved against the including file.
        let relative = tc.draw(gs::booleans());
        let name = format!("included-{index}");
        let file = fixture.repository().join(".git").join(&name);
        let target = if relative {
            name
        } else {
            file.to_str().unwrap().to_owned()
        };
        // The condition does not match, and must be inspected regardless.
        let condition = format!("includeIf.gitdir:/elsewhere-{index}/.path");
        support::git(&repository, &["config", &condition, &target]);
        if tc.draw(gs::integers::<u8>().max_value(3)) == 0 {
            // A target that cannot be read can prove nothing.
            expected = false;
            continue;
        }
        std::fs::write(&file, "").unwrap();
        for key in &included {
            let recased = if tc.draw(gs::booleans()) {
                key.to_ascii_uppercase()
            } else {
                (*key).to_owned()
            };
            let recased = recased.replace(".LFS.", ".lfs.");
            support::git(
                &repository,
                &[
                    "config",
                    "--file",
                    file.to_str().unwrap(),
                    &recased,
                    "value",
                ],
            );
        }
        expected &= included.iter().all(|key| allowed.contains(key));
    }

    // Git reports section and variable names in lowercase, so an allowlist
    // spelled the way the documentation spells them must still match.
    let spelled = allowed
        .iter()
        .map(|key| {
            if tc.draw(gs::booleans()) {
                key.replace("core.", "Core.").replace(".eol", ".EOL")
            } else {
                (*key).to_owned()
            }
        })
        .collect::<Vec<_>>();
    let spelled = spelled.iter().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        git.conditional_config_has_only(&repository, &spelled)
            .unwrap(),
        expected
    );
}

#[hegel::test(test_cases = 25)]
fn plain_configuration_paths_expand_to_themselves(tc: TestCase) {
    let git = Git::default();
    let plain = tc.draw(value().filter(|path: &Vec<u8>| {
        !path.is_empty()
            && !path.starts_with(b"~")
            && !path.starts_with(b"%(")
            && !path.starts_with(b":(")
    }));

    let expanded = git.expand_config_path(&os(&plain)).unwrap();
    assert_eq!(expanded.as_os_str().as_bytes(), plain);

    let home = std::env::var_os("HOME").expect("HOME");
    let below_home = git
        .expand_config_path(&os(&[b"~/", &plain[..]].concat()))
        .unwrap();
    assert_eq!(
        below_home.as_os_str().as_bytes(),
        [home.as_bytes(), b"/", &plain].concat()
    );
}
