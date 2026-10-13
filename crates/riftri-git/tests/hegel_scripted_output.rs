//! Property tests that hand single Git replies to the operations parsing
//! them, through a scripted stand-in, and compare the answer with what the
//! reply says.
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use riftri_git::{Git, GitError, ObjectId};

#[allow(dead_code)]
mod support;

use support::{End, Fixture, ScriptedGit};

struct Scene {
    fixture: Fixture,
    scripted: ScriptedGit,
}

thread_local! {
    /// The scripted replies never change the repository, so the cases of
    /// one test share it. The test thread drops it.
    static SCENE: Scene = {
        let fixture = Fixture::committed();
        support::git(
            &fixture.repository(),
            &["sparse-checkout", "set", "--cone", "directory"],
        );
        Scene {
            fixture,
            scripted: ScriptedGit::new(),
        }
    };
}

/// Run `operation` with invocation `index` replaced by a successful reply.
fn with_reply<T>(index: usize, reply: &[u8], operation: impl FnOnce(&Git, &Fixture) -> T) -> T {
    SCENE.with(|scene| {
        scene.scripted.reset();
        scene.scripted.script(index, reply, End::Exit(0));
        operation(&scene.scripted.git(), &scene.fixture)
    })
}

fn is_invalid_output<T>(result: &Result<T, GitError>) -> bool {
    matches!(result, Err(GitError::InvalidOutput { .. }))
}

fn line_endings() -> impl PrintableGenerator<String> {
    gs::sampled_from(vec!["", "\n", "\r\n", "\n\n"]).map(str::to_owned)
}

#[hegel::test(test_cases = 40)]
fn repository_identity_needs_a_flag_line_and_a_path(tc: TestCase) {
    let flag = tc.draw(gs::sampled_from(vec![
        "true", "false", "", "TRUE", "true ", "1",
    ]));
    let separator = tc.draw(gs::sampled_from(vec!["\n", ""]));
    // The common directory is a native path and may contain line breaks.
    let directory = tc.draw(gs::binary().max_size(8));
    let terminator = tc.draw(gs::sampled_from(vec!["\n", ""]));
    let reply = [
        flag.as_bytes(),
        separator.as_bytes(),
        &directory,
        terminator.as_bytes(),
    ]
    .concat();

    let (info, root) = with_reply(0, &reply, |git, fixture| {
        (
            git.inspect_repository(&fixture.repository()),
            fixture.repository(),
        )
    });

    // What a reader holding only the reply can establish.
    let split = reply.iter().position(|byte| *byte == b'\n');
    let expected = split.and_then(|split| {
        let path = &reply[split + 1..];
        let path = path.strip_suffix(b"\n").unwrap_or(path);
        let bare = match &reply[..split] {
            b"true" => true,
            b"false" => false,
            _ => return None,
        };
        (!path.is_empty()).then(|| (bare, path.to_vec()))
    });
    match expected {
        None => assert!(is_invalid_output(&info), "{info:?}"),
        Some((bare, path)) => {
            let info = info.expect("a flag line and a path");
            assert_eq!(info.is_bare, bare);
            assert_eq!(info.identity.common_git_dir.as_os_str().as_bytes(), path);
            assert_eq!(info.root, (!bare).then_some(root));
        }
    }
}

#[hegel::test(test_cases = 40)]
fn single_value_replies_are_trimmed_and_checked(tc: TestCase) {
    let word = tc.draw(gs::sampled_from(vec![
        "true",
        "false",
        "",
        "TRUE",
        "tru",
        "0",
        "17",
        "18446744073709551616",
        "git-lfs/3.7.1",
        "1 7",
        "x",
    ]));
    let reply = format!("{word}{}", tc.draw(line_endings()));

    let version = with_reply(0, reply.as_bytes(), |git, fixture| {
        git.lfs_version(&fixture.repository())
    });
    if word.is_empty() {
        assert!(is_invalid_output(&version), "{version:?}");
    } else {
        assert_eq!(version.unwrap(), Some(word.as_bytes().to_vec()));
    }

    let flag = with_reply(0, reply.as_bytes(), |git, fixture| {
        git.local_config_bool(&fixture.repository(), "probe.flag")
    });
    match word {
        "true" => assert_eq!(flag.unwrap(), Some(true)),
        "false" => assert_eq!(flag.unwrap(), Some(false)),
        _ => assert!(is_invalid_output(&flag), "{flag:?}"),
    }

    let blob = ObjectId::parse("1".repeat(40)).unwrap();
    let size = with_reply(0, reply.as_bytes(), |git, fixture| {
        git.blob_size(&fixture.repository(), &blob)
    });
    match word.parse::<u64>() {
        Ok(expected) => assert_eq!(size.unwrap(), expected),
        Err(_) => assert!(is_invalid_output(&size), "{size:?}"),
    }

    let path = with_reply(0, reply.as_bytes(), |git, fixture| {
        git.info_attributes_path(&fixture.repository())
    });
    // Only one trailing newline is Git's; the rest belongs to the path.
    let directory = reply.strip_suffix('\n').unwrap_or(&reply);
    if directory.is_empty() {
        assert!(is_invalid_output(&path), "{path:?}");
    } else {
        assert_eq!(
            path.unwrap(),
            PathBuf::from(directory).join("info/attributes")
        );
    }
}

#[hegel::test(test_cases = 40)]
fn a_git_directory_maps_to_its_first_live_checkout_or_to_none(tc: TestCase) {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Listed {
        Bare,
        Live,
        Gone,
    }
    let listed = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            Listed::Bare,
            Listed::Live,
            Listed::Gone,
        ]))
        .max_size(4),
    );

    let (root, live) = SCENE.with(|scene| {
        let live = scene.fixture.repository();
        let mut reply = Vec::new();
        for (index, kind) in listed.iter().enumerate() {
            let path = match kind {
                Listed::Live => live.clone(),
                _ => scene.fixture.root().join(format!("absent-{index}")),
            };
            reply.extend_from_slice(b"worktree ");
            reply.extend_from_slice(path.as_os_str().as_bytes());
            reply.extend_from_slice(if *kind == Listed::Bare {
                b"\0bare\0\0"
            } else {
                b"\0detached\0\0"
            });
        }
        scene.scripted.reset();
        scene.scripted.script(0, &reply, End::Exit(0));
        let root = scene
            .scripted
            .git()
            .worktree_root_from_git_dir(&live.join(".git"));
        (root, live)
    });

    if listed.iter().all(|kind| *kind == Listed::Bare) {
        assert_eq!(root.unwrap(), None);
    } else if listed.contains(&Listed::Live) {
        assert_eq!(root.unwrap(), Some(live));
    } else {
        // Registered checkouts exist, but none can be used.
        assert!(is_invalid_output(&root), "{root:?}");
    }
}

#[hegel::test(test_cases = 40)]
fn sparse_directories_are_the_non_empty_lines_git_lists(tc: TestCase) {
    let lines: Vec<Vec<u8>> = tc.draw(
        gs::vecs(hegel::one_of!(
            gs::sampled_from(vec![
                b"".to_vec(),
                b"directory".to_vec(),
                b"nested/directory".to_vec(),
                b"sp ace".to_vec(),
                "é雪".as_bytes().to_vec(),
                b"\xff".to_vec(),
            ]),
            gs::binary().max_size(4)
        ))
        .max_size(4),
    );
    let ending = tc.draw(gs::sampled_from(vec!["\n", "\r\n"]));
    let reply = lines
        .iter()
        .flat_map(|line| [line, ending.as_bytes()].concat())
        .collect::<Vec<_>>();

    let state = with_reply(1, &reply, |git, fixture| {
        git.sparse_checkout_state(&fixture.repository())
    });

    // A reader of the reply sees newline-separated UTF-8 names.
    let expected = reply
        .split(|byte| *byte == b'\n')
        .map(|line| {
            let end = line
                .iter()
                .rposition(|byte| !matches!(byte, b'\n' | b'\r'))
                .map_or(0, |last| last + 1);
            std::str::from_utf8(&line[..end]).map(str::to_owned)
        })
        .filter(|line| line.as_ref().map_or(true, |line| !line.is_empty()))
        .collect::<Result<Vec<_>, _>>();
    match expected {
        Ok(directories) => {
            let state = state.expect("UTF-8 directory names");
            assert!(state.enabled && state.cone);
            assert_eq!(state.directories, directories);
        }
        Err(_) => assert!(is_invalid_output(&state), "{state:?}"),
    }
}

#[hegel::test(test_cases = 40)]
fn sparse_state_is_enabled_only_by_a_true_setting(tc: TestCase) {
    let setting = || {
        gs::optional(gs::sampled_from(vec![
            "true", "TRUE", "True", "false", "", "1",
        ]))
    };
    let (sparse, cone) = (tc.draw(setting()), tc.draw(setting()));
    let mut reply = Vec::new();
    for (key, value) in [
        ("core.sparsecheckout", sparse),
        ("core.sparsecheckoutcone", cone),
    ] {
        if let Some(value) = value {
            reply.extend(format!("{key}\n{value}\0").into_bytes());
        }
    }
    // An empty reply with exit 0 is still a reply: nothing is configured.
    let state = with_reply(0, &reply, |git, fixture| {
        git.sparse_checkout_state(&fixture.repository())
    })
    .expect("well-formed configuration records");

    let on = |value: Option<&str>| value.is_some_and(|value| value.eq_ignore_ascii_case("true"));
    assert_eq!(state.enabled, on(sparse));
    assert_eq!(state.cone, on(sparse) && on(cone));
    assert_eq!(state.directories.is_empty(), !state.cone);
}

#[hegel::test(test_cases = 40)]
fn submodules_are_found_only_where_a_gitlink_is_checked_out(tc: TestCase) {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Entry {
        File,
        AbsentGitlink,
        CheckedOutGitlink,
        GitlinkWithoutPath,
    }
    let entries = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            Entry::File,
            Entry::AbsentGitlink,
            Entry::CheckedOutGitlink,
            Entry::GitlinkWithoutPath,
        ]))
        .max_size(4),
    );
    let id = "1".repeat(40);
    let mut reply = Vec::new();
    for entry in &entries {
        reply.extend(
            match entry {
                Entry::File => format!("100644 {id} 0\tfile\0"),
                Entry::AbsentGitlink => format!("160000 {id} 0\tabsent\0"),
                // The fixture's own root holds a `.git`, as a submodule would.
                Entry::CheckedOutGitlink => format!("160000 {id} 0\t.\0"),
                Entry::GitlinkWithoutPath => format!("160000 {id} 0\0"),
            }
            .into_bytes(),
        );
    }

    // Git keeps the repositories of absorbed submodules in this store.
    let has_store = tc.draw(gs::booleans());

    let found = with_reply(1, &reply, |git, fixture| {
        let store = fixture.repository().join(".git/modules");
        if has_store {
            std::fs::create_dir_all(&store).unwrap();
        } else if store.exists() {
            std::fs::remove_dir(&store).unwrap();
        }
        git.worktree_has_submodules(&fixture.repository())
    });
    if has_store {
        assert!(found.unwrap());
        return;
    }

    // The first entry that settles the question wins.
    let decisive = entries
        .iter()
        .find(|entry| matches!(entry, Entry::CheckedOutGitlink | Entry::GitlinkWithoutPath));
    match decisive {
        Some(Entry::GitlinkWithoutPath) => assert!(is_invalid_output(&found), "{found:?}"),
        Some(_) => assert!(found.unwrap()),
        None => assert!(!found.unwrap()),
    }
}

#[hegel::test(test_cases = 40)]
fn a_staged_change_check_refuses_an_index_that_is_not_a_file(tc: TestCase) {
    let kind = tc.draw(gs::sampled_from(vec![
        "missing",
        "directory",
        "symlink",
        "file",
    ]));
    let checked = SCENE.with(|scene| {
        let repository = scene.fixture.repository();
        let link = scene.fixture.root().join("index-link");
        if !link.exists() {
            std::os::unix::fs::symlink(repository.join(".git/index"), &link).unwrap();
        }
        let index = match kind {
            "missing" => scene.fixture.root().join("no-index"),
            "directory" => repository.clone(),
            "symlink" => link,
            _ => repository.join(".git/index"),
        };
        let reply = [index.as_os_str().as_bytes(), b"\n"].concat();
        scene.scripted.reset();
        scene.scripted.script(0, &reply, End::Exit(0));
        scene.scripted.git().worktree_index_has_changes(&repository)
    });

    match kind {
        // No index yet is not a staged deletion of everything.
        "missing" => assert!(!checked.unwrap()),
        "file" => assert!(!checked.unwrap()),
        _ => assert!(is_invalid_output(&checked), "{kind}: {checked:?}"),
    }
}

#[hegel::test(test_cases = 40)]
fn an_unresolvable_head_is_unborn_only_when_its_branch_is_also_missing(tc: TestCase) {
    let target = "2".repeat(40);
    let ends = vec![
        End::Exit(0),
        End::Exit(1),
        End::Exit(tc.draw(gs::integers::<u8>().min_value(2))),
        End::Signal(libc::SIGKILL),
    ];
    let base = tc.draw(gs::sampled_from(ends));

    let info = SCENE.with(|scene| {
        scene.scripted.reset();
        // `HEAD^{commit}` does not resolve; the follow-up asks about `HEAD`.
        scene.scripted.script(2, b"", End::Exit(1));
        scene
            .scripted
            .script(3, format!("{target}\n").as_bytes(), base);
        scene
            .scripted
            .git()
            .inspect_repository(&scene.fixture.repository())
    });

    match base {
        End::Exit(1) => assert_eq!(info.expect("an unborn HEAD").head_commit, None),
        // HEAD names an object that cannot be read: corruption, not absence.
        End::Exit(0) => match info {
            Err(GitError::UnreadableObject {
                target: reported, ..
            }) => assert_eq!(reported, target),
            other => panic!("expected an unreadable object, got {other:?}"),
        },
        _ => assert!(
            matches!(info, Err(GitError::CommandFailed { .. })),
            "{info:?}"
        ),
    }
}

#[hegel::test(test_cases = 40)]
fn conditional_includes_are_rejected_unless_their_target_can_be_checked(tc: TestCase) {
    let (origin, target, verdict) = SCENE.with(|scene| {
        let git_dir = scene.fixture.repository().join(".git");
        let included = git_dir.join("included");
        if !included.exists() {
            std::fs::write(&included, "[core]\n\teol = lf\n").unwrap();
        }
        let config = format!("file:{}", git_dir.join("config").display());
        tc.draw(gs::sampled_from(vec![
            (config.clone(), "included".to_owned(), Some(true)),
            (config.clone(), included.display().to_string(), Some(true)),
            (config.clone(), "not-there".to_owned(), Some(false)),
            // A relative target needs an including file to resolve against.
            (
                "command line:".to_owned(),
                "included".to_owned(),
                Some(false),
            ),
            ("file:".to_owned(), "included".to_owned(), Some(false)),
            (config.clone(), "/".to_owned(), Some(false)),
            (config, String::new(), None),
        ]))
    });
    let allowed = tc.draw(gs::subsequences(vec!["core.eol", "core.autocrlf"]));
    let reply = match verdict {
        Some(_) => format!("{origin}\0includeif.gitdir:/elsewhere/.path\n{target}\0"),
        // A record without a value separator has no include path at all.
        None => format!("{origin}\0includeif.gitdir:/elsewhere/.path\0"),
    };

    let checked = with_reply(0, reply.as_bytes(), |git, fixture| {
        git.conditional_config_has_only(&fixture.repository(), &allowed)
    });

    match verdict {
        None => assert!(is_invalid_output(&checked), "{checked:?}"),
        Some(readable) => assert_eq!(
            checked.unwrap(),
            readable && allowed.contains(&"core.eol"),
            "{origin} {target}"
        ),
    }
}

#[hegel::test(test_cases = 12)]
fn a_batch_answer_is_refused_when_git_never_read_the_whole_request(tc: TestCase) {
    // A small request is read in full before the reply. A large one gets a
    // reply without being read, and as it is larger than any pipe buffer it
    // cannot have been delivered in full. A small request that is never read
    // is left out: whether it reached the pipe before Git exited is a race.
    let request_count = tc.draw(hegel::one_of!(
        gs::integers::<usize>().min_value(1).max_value(20),
        gs::integers::<usize>().min_value(4000).max_value(5000)
    ));
    let blob = ObjectId::parse("1".repeat(40)).unwrap();
    let objects = vec![blob.clone(); request_count];
    let reply = format!("{} blob 7\n", blob.as_str()).repeat(request_count);

    let sizes = SCENE.with(|scene| {
        scene.scripted.reset();
        scene.scripted.script(0, reply.as_bytes(), End::Exit(0));
        if request_count <= 20 {
            scene.scripted.drain_input(0);
        }
        scene
            .scripted
            .git()
            .blob_sizes(&scene.fixture.repository(), &objects)
    });

    if request_count <= 20 {
        assert_eq!(sizes.unwrap(), vec![7; request_count]);
    } else {
        assert!(
            matches!(sizes, Err(GitError::WriteInput { .. })),
            "{sizes:?}"
        );
    }
}

#[hegel::test(test_cases = 40)]
fn a_git_lfs_that_does_not_answer_is_reported_as_absent(tc: TestCase) {
    let end = tc.draw(hegel::one_of!(
        gs::integers::<u8>().min_value(1).map(End::Exit),
        gs::sampled_from(vec![End::Signal(libc::SIGKILL), End::Signal(libc::SIGTERM)])
    ));
    let reply = tc.draw(gs::binary().max_size(12));

    let version = SCENE.with(|scene| {
        scene.scripted.reset();
        scene.scripted.script(0, &reply, end);
        scene
            .scripted
            .git()
            .lfs_version(&scene.fixture.repository())
    });

    // Whatever an unhealthy `git lfs` printed is not a version.
    assert_eq!(version.unwrap(), None);
}

#[hegel::test(test_cases = 40)]
fn a_failed_conditional_include_listing_is_never_read_as_none_found(tc: TestCase) {
    let code = tc.draw(gs::integers::<u8>().min_value(1));
    let reply = tc.draw(gs::binary().max_size(6));

    let checked = SCENE.with(|scene| {
        scene.scripted.reset();
        scene.scripted.script(0, &reply, End::Exit(code));
        scene
            .scripted
            .git()
            .conditional_config_has_only(&scene.fixture.repository(), &[])
    });

    // Only a silent exit 1 means "no conditional includes".
    if code == 1 && reply.is_empty() {
        assert!(checked.unwrap());
    } else {
        assert!(
            matches!(checked, Err(GitError::CommandFailed { .. })),
            "{checked:?}"
        );
    }
}
