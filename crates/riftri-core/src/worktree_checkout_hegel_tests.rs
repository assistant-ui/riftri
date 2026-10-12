//! Creation against real Git as the oracle: a view Riftri materializes holds
//! exactly the bytes `git worktree add` checks out, is clean, and is isolated
//! from its siblings and its base. A repository outside the deterministic
//! checkout policy is refused before anything is written.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::generators as gs;
use hegel::{HealthCheck, TestCase};
use riftri_git::Git;
use sha2::{Digest, Sha256};

use super::super::tests::{git, git_stdout};
use super::super::{
    RemoveWorktreeRequest, WorktreeError, add_worktree, force_remove_worktree, garbage_collect,
    inspect_repository_compatibility, remove_worktree, storage_accounting,
    validate_new_worktree_destination,
};
use super::{
    EXECUTABLE, Entry, Fixture, REGULAR, SYMLINK, TreeFile, component, portable_paths, registered,
    snapshot, status,
};
use crate::RepositoryCompatibilityBlockerKind as Blocker;

fn file_data(tc: &TestCase) -> Vec<u8> {
    match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => tc.draw(gs::binary().max_size(48)),
        1 => b"unix\nlines\n".to_vec(),
        2 => b"dos\r\nlines\r\n".to_vec(),
        _ => tc
            .draw(gs::text().alphabet("ab \n\r\t\u{e9}").max_size(24))
            .into_bytes(),
    }
}

/// A tree every supported filesystem can hold, with regular, executable, and
/// symlink entries under names that need quoting.
fn portable_tree(tc: &TestCase) -> Vec<TreeFile> {
    portable_paths(tc, 5)
        .into_iter()
        .map(|mut path| {
            path.push_str(tc.draw(gs::sampled_from(vec!["", ".txt", ".bin", ".bat", ".sh"])));
            match tc.draw(gs::integers::<u8>().max_value(5)) {
                0 => TreeFile {
                    path: path.into_bytes(),
                    mode: SYMLINK,
                    data: format!("../{}", component(tc)).into_bytes(),
                },
                1 => TreeFile {
                    path: path.into_bytes(),
                    mode: EXECUTABLE,
                    data: file_data(tc),
                },
                _ => TreeFile {
                    path: path.into_bytes(),
                    mode: REGULAR,
                    data: file_data(tc),
                },
            }
        })
        .collect()
}

/// `.gitattributes` lines inside the deterministic allowlist.
fn allowed_attributes(tc: &TestCase) -> Option<TreeFile> {
    let lines = tc.draw(gs::subsequences(vec![
        "* text=auto",
        "*.txt text",
        "*.bin binary",
        "*.bin -text -diff",
        "*.sh text eol=lf",
        "*.bat text eol=crlf",
        "* linguist-generated=true",
        "p0/** linguist-language=Rust",
        "p1* -merge",
    ]));
    (!lines.is_empty())
        .then(|| TreeFile::regular(".gitattributes", (lines.join("\n") + "\n").as_bytes()))
}

fn plain_checkout(fixture: &Fixture, name: &str, commit: &str) -> PathBuf {
    let plain = fixture.path(name);
    git(
        &fixture.repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            plain.to_str().expect("fixture paths are UTF-8"),
            commit,
        ],
    );
    plain
}

fn nothing_was_created(fixture: &Fixture, destination: &Path) {
    assert!(!destination.exists(), "a refused add left its destination");
    assert!(!registered(&fixture.repository, destination));
    if fixture.state.exists() {
        let accounting = storage_accounting(&fixture.state).expect("account refused add");
        assert_eq!(
            (
                accounting.active_views,
                accounting.pending_adds,
                accounting.bases.len()
            ),
            (0, 0, 0),
            "a refused add left durable state: {accounting:?}"
        );
    }
}

#[hegel::test(
    test_cases = 7,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn a_view_matches_a_plain_git_checkout_and_is_isolated(tc: TestCase) {
    let mut tree = portable_tree(&tc);
    tree.extend(allowed_attributes(&tc));
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &tree);
    let plain = plain_checkout(&fixture, "plain", &commit);
    let expected = snapshot(&plain);

    // A tree Git itself checks out dirty is never activated; see
    // `a_tree_git_checks_out_dirty_is_refused_without_residue`.
    tc.assume(status(&plain).is_empty());

    let first = fixture.path("first");
    let added =
        add_worktree(fixture.add_request(&first, &commit)).expect("a tree Git checks out cleanly");
    assert_eq!(
        snapshot(&first),
        expected,
        "the view differs from Git's checkout"
    );
    assert_eq!(status(&first), "", "a new view is not clean");
    assert_eq!(
        git_stdout(&first, &["rev-parse", "HEAD"]),
        commit,
        "the view is at another commit"
    );
    assert!(!added.reused_base);
    let base = snapshot(&added.base_path);

    let second = fixture.path("second");
    let reused = add_worktree(fixture.add_request(&second, &commit)).expect("second view");
    assert!(reused.reused_base, "an identical tree built a second base");
    assert_eq!(reused.base_path, added.base_path);

    // Change every kind of thing a user can change in the first view.
    for (path, entry) in &expected {
        let target = first.join(std::str::from_utf8(path).expect("portable path"));
        match (entry, tc.draw(gs::integers::<u8>().max_value(3))) {
            (Entry::File { .. }, 0) => fs::write(&target, b"changed in the first view\n").unwrap(),
            (Entry::File { .. }, 1) => fs::remove_file(&target).unwrap(),
            (Entry::File { executable, .. }, 2) => {
                let mode = if *executable { 0o644 } else { 0o755 };
                fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
            }
            (Entry::Symlink(_), 0) => {
                fs::remove_file(&target).unwrap();
                fs::write(&target, b"no longer a link\n").unwrap();
            }
            _ => {}
        }
    }
    fs::write(first.join("untracked-by-user"), b"new\n").unwrap();
    assert_eq!(
        snapshot(&second),
        expected,
        "a write leaked into a sibling view"
    );
    assert_eq!(
        snapshot(&added.base_path),
        base,
        "a write leaked into the base"
    );
    assert_eq!(status(&second), "");

    let request = |destination: &Path| RemoveWorktreeRequest {
        repository: fixture.repository.clone(),
        destination: destination.to_path_buf(),
        state_dir: Some(fixture.state.clone()),
    };
    remove_worktree(request(&first)).expect_err("an unforced removal of a changed view");
    assert!(first.join("untracked-by-user").is_file());
    remove_worktree(request(&second)).expect("remove the clean view");
    force_remove_worktree(request(&first)).expect("force-remove the changed view");
    assert!(!first.exists() && !second.exists());
    assert_eq!(snapshot(&added.base_path), base, "removal changed the base");

    let plan = garbage_collect(&fixture.state, false).expect("plan collection");
    assert_eq!(
        plan.candidates.len(),
        1,
        "the unreferenced base is collectible"
    );
    assert!(added.base_path.is_dir(), "a plan deleted the base");
    let applied = garbage_collect(&fixture.state, true).expect("collect");
    assert_eq!(applied.collected, vec![added.base_path.clone()]);
    assert!(!added.base_path.exists());
    assert_eq!(
        snapshot(&plain),
        expected,
        "collection touched a plain worktree"
    );
}

/// Attributes can make a tree dirty the moment Git checks it out: a blob
/// stored with CRLF under `text eol=crlf` would be renormalized by `git add`.
/// Riftri refuses to activate such a view, and a refused add leaves nothing.
#[hegel::test(
    test_cases = 8,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
#[ignore = "bug: rollback of a tree Git checks out dirty keeps the view and a journal repair cannot retire"]
fn a_tree_git_checks_out_dirty_is_refused_without_residue(tc: TestCase) {
    let attribute = tc.draw(gs::sampled_from(vec![
        "* text eol=crlf",
        "*.txt text",
        "* text eol=lf",
    ]));
    let mut tree = vec![
        TreeFile::regular(".gitattributes", format!("{attribute}\n").as_bytes()),
        TreeFile::regular("stored-with-crlf.txt", b"dos\r\nlines\r\n"),
    ];
    tree.extend(portable_tree(&tc));
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &tree);
    let plain = plain_checkout(&fixture, "plain", &commit);
    assert_ne!(status(&plain), "", "Git checks this tree out dirty");

    let destination = fixture.path("view");
    add_worktree(fixture.add_request(&destination, &commit))
        .expect_err("a view Git reports dirty is never activated");
    nothing_was_created(&fixture, &destination);
    let repair = super::super::recover_incomplete_operations(&fixture.state).expect("repair");
    assert!(repair.errors.is_empty(), "{:?}", repair.errors);
}

/// A name the destination volume cannot keep apart from `path`, or cannot
/// store at all.
fn hazard(tc: &TestCase, path: &str) -> Vec<u8> {
    match tc.draw(gs::integers::<u8>().max_value(2)) {
        // Every portable path starts with a lowercase `p`, so this is always
        // a distinct ASCII case alias of its first component.
        0 => format!("P{}", &path[1..]).into_bytes(),
        1 => [path.as_bytes(), b"\xff\xfe"].concat(),
        // The same name once precomposed and once decomposed.
        _ => format!("{path}e\u{301}").into_bytes(),
    }
}

#[hegel::test(
    test_cases = 16,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn preflight_and_add_agree_and_refusals_leave_nothing(tc: TestCase) {
    let mut tree = portable_tree(&tc);
    let hazardous = tc.draw(gs::booleans());
    if hazardous {
        let index = tc.draw(gs::integers::<usize>().max_value(tree.len() - 1));
        let path = String::from_utf8(tree[index].path.clone()).expect("portable path");
        let aliased = hazard(&tc, &path);
        if aliased.ends_with("e\u{301}".as_bytes()) {
            tree[index].path = format!("{path}\u{e9}").into_bytes();
        }
        tree.push(TreeFile {
            path: aliased,
            mode: REGULAR,
            data: b"alias\n".to_vec(),
        });
    }
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &tree);
    let destination = fixture.path("view");

    let preflight = validate_new_worktree_destination(
        &fixture.repository,
        &destination,
        std::ffi::OsStr::new(&commit),
    );
    assert!(
        !destination.exists(),
        "the preflight created its destination"
    );
    assert!(
        !fixture.state.exists(),
        "the preflight created durable state"
    );
    let added = add_worktree(fixture.add_request(&destination, &commit));
    tc.note(&format!("preflight {preflight:?}, add {added:?}"));

    if hazardous {
        // macOS volumes are case- and normalization-insensitive and require
        // UTF-8 names; elsewhere only the invalid name can be refused.
        if cfg!(target_os = "macos") {
            assert!(matches!(added, Err(WorktreeError::Unsupported(_))));
        }
    } else {
        added.as_ref().expect("a portable tree");
    }
    match added {
        Ok(_) => {
            preflight.expect("the preflight refused an add that succeeded");
            assert_eq!(status(&destination), "");
        }
        Err(error) => {
            assert_eq!(
                preflight
                    .expect_err("the preflight accepted a doomed add")
                    .to_string(),
                error.to_string(),
                "the preflight and the add explain the refusal differently"
            );
            nothing_was_created(&fixture, &destination);
        }
    }
}

#[hegel::test(
    test_cases = 7,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn a_sparse_view_matches_git_cone_mode(tc: TestCase) {
    let directories = ["docs", "src", "src/deep", "tools", "tools/a b"];
    let mut tree = vec![TreeFile::regular("root.txt", b"root\n")];
    for directory in directories {
        for index in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(2)) {
            tree.push(TreeFile::regular(
                &format!("{directory}/f{index}-{}", component(&tc).trim_matches(' ')),
                &file_data(&tc),
            ));
        }
    }
    let mut cones = tc.draw(gs::subsequences(directories.to_vec()));
    tc.assume(!cones.is_empty());
    if tc.draw(gs::booleans()) {
        cones[0] = "src/";
    }
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &tree);

    let plain = fixture.path("plain");
    let plain_text = plain.to_str().expect("fixture paths are UTF-8");
    git(
        &fixture.repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "--no-checkout",
            "--detach",
            plain_text,
            &commit,
        ],
    );
    let mut arguments = vec!["sparse-checkout", "set", "--cone", "--"];
    arguments.extend(cones.iter().copied());
    git(&plain, &arguments);
    git(&plain, &["checkout", "--quiet", "HEAD"]);
    let expected = snapshot(&plain);

    let view = fixture.path("view");
    let mut request = fixture.add_request(&view, &commit);
    request.sparse_directories = cones.iter().map(|cone| (*cone).to_owned()).collect();
    add_worktree(request.clone()).expect("sparse view");
    assert_eq!(snapshot(&view), expected, "the cone differs from Git's");
    assert_eq!(status(&view), "");
    assert_eq!(
        git_stdout(&view, &["sparse-checkout", "list"]),
        git_stdout(&plain, &["sparse-checkout", "list"]),
        "Git sees a different cone in the view"
    );

    // The same selection spelled in another order shares the base; the full
    // tree at the same commit must not.
    request.destination = fixture.path("again");
    request.sparse_directories.reverse();
    let again = add_worktree(request).expect("reordered sparse view");
    assert!(again.reused_base, "one cone built two bases");
    let full =
        add_worktree(fixture.add_request(&fixture.path("full"), &commit)).expect("full view");
    assert_ne!(
        full.base_path, again.base_path,
        "a sparse base served a full view"
    );

    let mut missing = fixture.add_request(&fixture.path("missing"), &commit);
    missing.sparse_directories = vec![
        tc.draw(gs::sampled_from(vec![
            "absent", "root.txt", "src/*", "../src", ".git", "/src",
        ]))
        .to_owned(),
    ];
    let error = add_worktree(missing).expect_err("an invalid cone");
    assert!(matches!(error, WorktreeError::InvalidRequest(_)), "{error}");
    assert!(!fixture.path("missing").exists());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyHazard {
    Config(&'static str, &'static str, Option<Blocker>),
    InfoAttributes(&'static str),
    InfoAttributesSymlink,
    TreeAttributes(&'static str, bool),
    Submodule,
}

const POLICY_HAZARDS: &[PolicyHazard] = &[
    PolicyHazard::Config(
        "core.autocrlf",
        "true",
        Some(Blocker::CheckoutConfiguration),
    ),
    PolicyHazard::Config(
        "core.autocrlf",
        "input",
        Some(Blocker::CheckoutConfiguration),
    ),
    PolicyHazard::Config("core.autocrlf", "FALSE", None),
    PolicyHazard::Config("core.eol", "crlf", Some(Blocker::CheckoutConfiguration)),
    PolicyHazard::Config("core.eol", "LF", None),
    PolicyHazard::Config("core.eol", "native", None),
    PolicyHazard::Config(
        "core.symlinks",
        "false",
        Some(Blocker::CheckoutConfiguration),
    ),
    PolicyHazard::Config("core.symlinks", "true", None),
    PolicyHazard::Config("core.filemode", "true", None),
    PolicyHazard::Config(
        "core.attributesfile",
        "/dev/null",
        Some(Blocker::EffectiveAttributes),
    ),
    PolicyHazard::Config("core.sparsecheckout", "true", Some(Blocker::SparseCheckout)),
    PolicyHazard::Config("core.sparsecheckout", "false", None),
    PolicyHazard::Config(
        "core.sparsecheckoutcone",
        "true",
        Some(Blocker::SparseCheckout),
    ),
    PolicyHazard::InfoAttributes(""),
    PolicyHazard::InfoAttributes("* text=auto\n"),
    PolicyHazard::InfoAttributes("unrelated-name -text\n"),
    PolicyHazard::InfoAttributesSymlink,
    PolicyHazard::TreeAttributes("* text=auto\n", true),
    PolicyHazard::TreeAttributes("*.txt ident\n", false),
    PolicyHazard::TreeAttributes("* filter=custom\n", false),
    PolicyHazard::TreeAttributes("* working-tree-encoding=UTF-16\n", false),
    PolicyHazard::TreeAttributes("* crlf\n", false),
    PolicyHazard::TreeAttributes("* eol=cr\n", false),
    PolicyHazard::TreeAttributes("* filter=lfs\n", false),
    PolicyHazard::TreeAttributes("* made-up-attribute\n", false),
    PolicyHazard::Submodule,
];

/// The repository-wide policy: every setting or attribute that could change
/// checkout bytes outside the immutable tree blocks the add, each with its
/// documented blocker kind, and nothing else does.
#[hegel::test(
    test_cases = 20,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn checkout_policy_blocks_exactly_the_documented_hazards(tc: TestCase) {
    let hazards = tc.draw(gs::subsequences(POLICY_HAZARDS).max_size(3));
    let fixture = Fixture::new();
    let mut tree = vec![
        TreeFile::regular("a.txt", b"text\n"),
        TreeFile::regular("dir/b.bin", b"\0binary"),
    ];
    let mut expected = BTreeSet::new();
    let mut config = std::collections::BTreeMap::new();
    let mut tree_attributes = None;
    let mut info_attributes = None;
    for hazard in &hazards {
        match *hazard {
            PolicyHazard::Config(key, value, _) => {
                config.insert(key, value);
            }
            PolicyHazard::InfoAttributes(_) | PolicyHazard::InfoAttributesSymlink => {
                info_attributes = Some(*hazard);
            }
            PolicyHazard::TreeAttributes(content, allowed) => {
                tree_attributes = Some((content, allowed));
            }
            PolicyHazard::Submodule => {
                tree.push(TreeFile::regular(".gitmodules", b""));
                expected.insert(Blocker::Submodules.as_str());
            }
        }
    }
    // Later picks of one key or file replace earlier ones, as in Git.
    for (key, value) in &config {
        git(&fixture.repository, &["config", key, value]);
        let blocker = POLICY_HAZARDS.iter().find_map(|hazard| match hazard {
            PolicyHazard::Config(k, v, blocker) if k == key && v == value => Some(*blocker),
            _ => None,
        });
        if let Some(blocker) = blocker.flatten() {
            expected.insert(blocker.as_str());
        }
    }
    if let Some((content, allowed)) = tree_attributes {
        tree.push(TreeFile::regular(".gitattributes", content.as_bytes()));
        if !allowed {
            expected.insert(Blocker::InTreeAttributes.as_str());
        }
    }
    let info = fixture.repository.join(".git/info/attributes");
    match info_attributes {
        Some(PolicyHazard::InfoAttributes(content)) => {
            fs::write(&info, content).unwrap();
            if !content.is_empty() {
                expected.insert(Blocker::EffectiveAttributes.as_str());
            }
        }
        Some(PolicyHazard::InfoAttributesSymlink) => {
            std::os::unix::fs::symlink("/dev/null", &info).unwrap();
            expected.insert(Blocker::EffectiveAttributes.as_str());
        }
        _ => {}
    }
    let commit = fixture.commit("main", &tree);
    tc.note(&format!("expected blockers {expected:?}"));

    let common = fixture.repository.join(".git");
    let report = inspect_repository_compatibility(
        &Git::default(),
        &fixture.repository,
        &common,
        std::ffi::OsStr::new(&commit),
    )
    .expect("inspect compatibility");
    let reported = report
        .blockers
        .iter()
        .map(|blocker| blocker.kind.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(reported, expected, "{:?}", report.blockers);
    assert_eq!(report.compatible, expected.is_empty());

    let destination = fixture.path("view");
    match add_worktree(fixture.add_request(&destination, &commit)) {
        Ok(_) => {
            assert!(expected.is_empty(), "an add ignored blockers {expected:?}");
            let plain = plain_checkout(&fixture, "plain", &commit);
            assert_eq!(snapshot(&destination), snapshot(&plain));
            assert_eq!(status(&destination), "");
        }
        Err(error) => {
            assert!(
                !expected.is_empty(),
                "refused a compatible repository: {error}"
            );
            assert!(matches!(error, WorktreeError::Unsupported(_)), "{error}");
            nothing_was_created(&fixture, &destination);
        }
    }
}

/// `post-checkout` runs for a managed add exactly as Git runs it for its own:
/// same arguments, same working directory, and a failing hook keeps the view.
#[hegel::test(
    test_cases = 10,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn the_post_checkout_hook_runs_as_git_runs_it(tc: TestCase) {
    let fixture = Fixture::new();
    let commit = fixture.commit("main", &[TreeFile::regular("a.txt", b"text\n")]);
    let exit_code = tc.draw(gs::sampled_from(vec![0, 1, 7]));
    let executable = tc.draw(gs::booleans());
    let startable = tc.draw(gs::booleans());
    let hooks = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => fixture.repository.join(".git/hooks"),
        1 => {
            git(
                &fixture.repository,
                &["config", "core.hooksPath", "relative hooks"],
            );
            fixture.repository.join("relative hooks")
        }
        _ => {
            let hooks = fixture.path("absolute-hooks");
            git(
                &fixture.repository,
                &["config", "core.hooksPath", hooks.to_str().unwrap()],
            );
            hooks
        }
    };
    fs::create_dir_all(&hooks).unwrap();
    let log = fixture.path("hook.log");
    let interpreter = if startable {
        "/bin/sh"
    } else {
        "/nonexistent/interpreter"
    };
    let hook = hooks.join("post-checkout");
    fs::write(
        &hook,
        format!(
            "#!{interpreter}\nprintf '%s|%s\\n' \"$*\" \"$(basename \"$PWD\")\" >> '{}'\nexit {exit_code}\n",
            log.display()
        ),
    )
    .unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&hook, fs::Permissions::from_mode(mode)).unwrap();

    // Git reports a failing hook through its exit status but keeps the view.
    Command::new("git")
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(fixture.path("view"))
        .arg(&commit)
        .current_dir(&fixture.repository)
        .output()
        .expect("run git worktree add");
    let by_git = fs::read_to_string(&log).unwrap_or_default();
    git(
        &fixture.repository,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.path("view").to_str().unwrap(),
        ],
    );
    let _ = fs::remove_file(&log);

    let added = add_worktree(fixture.add_request(&fixture.path("view"), &commit))
        .expect("a hook never fails the add");
    let by_riftri = fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(by_riftri, by_git, "the hook saw a different invocation");
    assert_eq!(status(&fixture.path("view")), "");
    match added.post_checkout {
        None => assert!(!executable, "an executable hook was skipped"),
        Some(outcome) => {
            assert!(executable, "a hook Git would not run was reported");
            assert_eq!(outcome.hook, hook);
            assert_eq!(outcome.started, startable);
            assert_eq!(outcome.exit_code, startable.then_some(exit_code));
            assert_eq!(outcome.succeeded(), startable && exit_code == 0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LfsHazard {
    None,
    PointerExtension,
    PointerUppercaseOid,
    PointerPaddedSize,
    ObjectMissing,
    ObjectWrongSize,
    ObjectSymlink,
    PointerIsSymlink,
    Config(&'static str, Option<&'static str>),
    Sparse,
}

const LFS_CONFIG: [(&str, &str); 4] = [
    ("filter.lfs.clean", "git-lfs clean -- %f"),
    ("filter.lfs.smudge", "git-lfs smudge -- %f"),
    ("filter.lfs.process", "git-lfs filter-process"),
    ("filter.lfs.required", "true"),
];

/// Git LFS paths are materialized from the local object store only for strict
/// v1 pointers, verified objects, and the standard filter configuration.
#[hegel::test(
    test_cases = 16,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn lfs_paths_materialize_only_from_strict_pointers_and_verified_objects(tc: TestCase) {
    let hazard = tc.draw(gs::sampled_from(vec![
        LfsHazard::None,
        LfsHazard::None,
        LfsHazard::PointerExtension,
        LfsHazard::PointerUppercaseOid,
        LfsHazard::PointerPaddedSize,
        LfsHazard::ObjectMissing,
        LfsHazard::ObjectWrongSize,
        LfsHazard::ObjectSymlink,
        LfsHazard::PointerIsSymlink,
        LfsHazard::Config("filter.lfs.required", Some("false")),
        LfsHazard::Config("filter.lfs.required", None),
        LfsHazard::Config("filter.lfs.smudge", Some("cat")),
        LfsHazard::Config("filter.lfs.clean", None),
        LfsHazard::Config("lfs.storage", Some("elsewhere")),
        LfsHazard::Config("filter.lfs.required", Some("TRUE")),
        LfsHazard::Config("filter.lfs.process", None),
        LfsHazard::Sparse,
    ]));
    let content = tc.draw(gs::binary().min_size(1).max_size(64));
    let executable = tc.draw(gs::booleans());
    let fixture = Fixture::new();
    if Git::default()
        .lfs_version(&fixture.repository)
        .expect("probe git lfs")
        .is_none()
    {
        // Without the standard `git lfs` command every LFS tree is refused.
        return;
    }
    for (key, value) in LFS_CONFIG {
        git(&fixture.repository, &["config", key, value]);
    }
    let blocks = match hazard {
        LfsHazard::Config(key, value) => {
            match value {
                Some(value) => git(&fixture.repository, &["config", key, value]),
                None => git(&fixture.repository, &["config", "--unset", key]),
            }
            // The long-running process filter is optional, and booleans are
            // compared as Git compares them.
            !matches!(
                (key, value),
                ("filter.lfs.process", None) | (_, Some("TRUE"))
            )
        }
        LfsHazard::None => false,
        _ => true,
    };

    let oid = Sha256::digest(&content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let pointer = match hazard {
        LfsHazard::PointerExtension => format!(
            "version https://git-lfs.github.com/spec/v1\next-0-x sha256:{oid}\noid sha256:{oid}\nsize {}\n",
            content.len()
        ),
        LfsHazard::PointerUppercaseOid => format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
            oid.to_ascii_uppercase(),
            content.len()
        ),
        LfsHazard::PointerPaddedSize => format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 0{}\n",
            content.len()
        ),
        _ => format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
            content.len()
        ),
    };
    let mode = match hazard {
        LfsHazard::PointerIsSymlink => SYMLINK,
        _ if executable => EXECUTABLE,
        _ => REGULAR,
    };
    let commit = fixture.commit(
        "main",
        &[
            TreeFile::regular(
                ".gitattributes",
                b"*.bin filter=lfs diff=lfs merge=lfs -text\n",
            ),
            TreeFile {
                path: b"assets/large.bin".to_vec(),
                mode,
                data: pointer.into_bytes(),
            },
            TreeFile::regular("assets/plain.txt", b"plain\n"),
        ],
    );
    let objects = fixture
        .repository
        .join(".git/lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4]);
    fs::create_dir_all(&objects).unwrap();
    match hazard {
        LfsHazard::ObjectMissing => {}
        LfsHazard::ObjectWrongSize => {
            fs::write(objects.join(&oid), [&content[..], b"+"].concat()).unwrap()
        }
        LfsHazard::ObjectSymlink => {
            fs::write(fixture.path("outside-object"), &content).unwrap();
            std::os::unix::fs::symlink(fixture.path("outside-object"), objects.join(&oid)).unwrap();
        }
        _ => fs::write(objects.join(&oid), &content).unwrap(),
    }

    let destination = fixture.path("view");
    let mut request = fixture.add_request(&destination, &commit);
    if hazard == LfsHazard::Sparse {
        request.sparse_directories = vec!["assets".to_owned()];
    }
    match add_worktree(request) {
        Ok(_) => {
            assert!(!blocks, "an add accepted {hazard:?}");
            assert_eq!(
                fs::read(destination.join("assets/large.bin")).unwrap(),
                content
            );
            let mode = fs::metadata(destination.join("assets/large.bin"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o100 != 0, executable);
            assert_eq!(
                status(&destination),
                "",
                "the materialized object is not clean"
            );
            // Writes in the view must never reach the local LFS store.
            fs::write(destination.join("assets/large.bin"), b"overwritten").unwrap();
            assert_eq!(fs::read(objects.join(&oid)).unwrap(), content);
        }
        Err(error) => {
            assert!(blocks, "refused the standard LFS profile: {error}");
            assert!(matches!(error, WorktreeError::Unsupported(_)), "{error}");
            nothing_was_created(&fixture, &destination);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadRequest {
    ExistingBranchMissing,
    NewBranchExists,
    BranchCheckedOut,
    BranchHeldByDeletedWorktree,
    UnknownRevision,
    DestinationStillRegistered,
    DestinationInWorktreeMetadata,
    DestinationOccupied,
    DestinationIsFile,
    DestinationInsideState,
    StateInsideDestination,
    StateIsFile,
    StateIsSymlink,
    StateClimbsThroughMissing,
    SparsePattern,
    SparseMissing,
}

const BAD_REQUESTS: [BadRequest; 16] = [
    BadRequest::ExistingBranchMissing,
    BadRequest::NewBranchExists,
    BadRequest::BranchCheckedOut,
    BadRequest::BranchHeldByDeletedWorktree,
    BadRequest::UnknownRevision,
    BadRequest::DestinationStillRegistered,
    BadRequest::DestinationInWorktreeMetadata,
    BadRequest::DestinationOccupied,
    BadRequest::DestinationIsFile,
    BadRequest::DestinationInsideState,
    BadRequest::StateInsideDestination,
    BadRequest::StateIsFile,
    BadRequest::StateIsSymlink,
    BadRequest::StateClimbsThroughMissing,
    BadRequest::SparsePattern,
    BadRequest::SparseMissing,
];

/// A request that can never succeed is refused as a request error before any
/// journal, branch, registration, or directory exists, whatever the tree.
#[hegel::test(
    test_cases = 2,
    derandomize = true,
    suppress_health_check = [HealthCheck::TooSlow]
)]
fn impossible_add_requests_are_refused_before_anything_exists(tc: TestCase) {
    let tree = portable_tree(&tc);
    for bad in BAD_REQUESTS {
        refuse_impossible_add(&tc, &tree, bad);
    }
}

fn refuse_impossible_add(tc: &TestCase, tree: &[TreeFile], bad: BadRequest) {
    let fixture = Fixture::new();
    let commit = fixture.commit("main", tree);
    git(&fixture.repository, &["branch", "side", &commit]);
    let mut request = fixture.add_request(&fixture.path("view"), &commit);
    let branch = |name: &str| std::ffi::OsString::from(name);
    match bad {
        BadRequest::ExistingBranchMissing => {
            request.mode = super::super::WorktreeMode::ExistingBranch(branch("absent"));
        }
        BadRequest::NewBranchExists => {
            request.mode = super::super::WorktreeMode::NewBranch(branch("side"));
        }
        BadRequest::BranchCheckedOut | BadRequest::BranchHeldByDeletedWorktree => {
            let holder = plain_holder(&fixture, "side");
            if bad == BadRequest::BranchHeldByDeletedWorktree {
                fs::remove_dir_all(holder).unwrap();
            }
            request.revision = branch("side");
            request.mode = super::super::WorktreeMode::ExistingBranch(branch("side"));
        }
        BadRequest::UnknownRevision => request.revision = branch("no-such-revision"),
        BadRequest::DestinationStillRegistered => {
            let holder = plain_holder(&fixture, "side");
            fs::remove_dir_all(&holder).unwrap();
            request.destination = holder;
        }
        BadRequest::DestinationInWorktreeMetadata => {
            request.destination = fixture.repository.join(".git/worktrees/view");
        }
        BadRequest::DestinationOccupied => {
            fs::create_dir(&request.destination).unwrap();
            fs::write(request.destination.join("kept"), b"kept\n").unwrap();
        }
        BadRequest::DestinationIsFile => fs::write(&request.destination, b"kept\n").unwrap(),
        BadRequest::DestinationInsideState => {
            request.destination = fixture.state.join("view");
        }
        BadRequest::StateInsideDestination => {
            request.state_dir = Some(request.destination.join("state"));
        }
        BadRequest::StateIsFile => fs::write(&fixture.state, b"kept\n").unwrap(),
        BadRequest::StateIsSymlink => {
            fs::create_dir(fixture.path("real-state")).unwrap();
            std::os::unix::fs::symlink(fixture.path("real-state"), &fixture.state).unwrap();
        }
        BadRequest::StateClimbsThroughMissing => {
            request.state_dir = Some(fixture.path("missing/../state"));
        }
        BadRequest::SparsePattern => request.sparse_directories = vec!["p0*".to_owned()],
        BadRequest::SparseMissing => request.sparse_directories = vec!["absent".to_owned()],
    }
    let observe = || {
        (
            git_stdout(&fixture.repository, &["worktree", "list", "--porcelain"]),
            git_stdout(&fixture.repository, &["for-each-ref"]),
            snapshot(&fixture.path("")),
        )
    };
    let before = observe();

    let error = add_worktree(request).expect_err("an impossible request");
    tc.note(&format!("{bad:?}: {error}"));
    assert!(
        !matches!(
            error,
            WorktreeError::Io { .. }
                | WorktreeError::Git(_)
                | WorktreeError::OperationAndRollback { .. }
        ),
        "a request error surfaced as an operational failure: {error:?}"
    );
    let after = observe();
    assert_eq!(
        after.0, before.0,
        "a refused add changed Git's worktree list"
    );
    assert_eq!(after.1, before.1, "a refused add changed a ref");
    for (path, entry) in &before.2 {
        assert_eq!(
            after.2.get(path),
            Some(entry),
            "a refused add changed a file"
        );
    }
    let created = after
        .2
        .keys()
        .filter(|path| !before.2.contains_key(*path))
        .map(|path| String::from_utf8_lossy(path).into_owned())
        // Advisory locks in the Git directory are not state.
        .filter(|path| !path.ends_with(".lock"))
        .collect::<Vec<_>>();
    assert!(created.is_empty(), "a refused add created {created:?}");
}

/// A plain Git worktree that has `branch` checked out.
fn plain_holder(fixture: &Fixture, branch: &str) -> PathBuf {
    let holder = fixture.path("holder");
    git(
        &fixture.repository,
        &[
            "worktree",
            "add",
            "--quiet",
            holder.to_str().unwrap(),
            branch,
        ],
    );
    holder
}
