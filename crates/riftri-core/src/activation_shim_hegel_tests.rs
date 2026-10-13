//! Property tests for the Git shim's own files and shell code: quoting a path
//! into shell source, removing the shim from `PATH`, the durable shim
//! directory, its `git` entry and real-Git marker, and finding the real Git
//! without ever selecting a shim.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hegel::TestCase;
use hegel::generators as gs;
use tempfile::tempdir;

use crate::journal::hegel_tests::entry_names;

use super::{
    ActivationError, PROCESS_SHIM_DIR_ENV, PROCESS_SHIM_DIR_PREFIX, REAL_GIT_MARKER_FILE,
    SHELL_SHIM_DIR_ENV, SHIM_ACTIVE_ENV, ensure_real_shell_shim_directory,
    install_durable_git_shim, posix_quote_path, prepare_posix_shell_deactivation_inner,
    read_real_git_marker, real_git_on_path, record_real_git_marker, require_real_shell_directory,
    resolve_stripped_real_git, set_private_directory_permissions, shell_shim_directory,
};

/// Each case starts a shell or builds a small directory fixture.
const SHIM_CASES: u64 = 100;

/// The POSIX shells a hook is evaluated in, by absolute path because some
/// cases hand the shell a `PATH` that names no real directory. zsh skips its
/// startup files so a user's `.zshenv` cannot change the result.
fn shells() -> Vec<Command> {
    [
        ("/bin/sh", None),
        ("/bin/zsh", Some("-f")),
        ("/bin/bash", None),
    ]
    .into_iter()
    .filter(|(shell, _)| Path::new(shell).is_file())
    .map(|(shell, option)| {
        let mut command = Command::new(shell);
        command.args(option);
        command
    })
    .collect()
}

/// A path as raw bytes: mostly text dense with shell syntax, sometimes
/// arbitrary bytes, occasionally with the colon `PATH` cannot hold. Never a
/// NUL, which no path or process argument can contain.
fn path_bytes(tc: &TestCase) -> Vec<u8> {
    let mut bytes = if tc.draw(gs::weighted_booleans(0.8)) {
        tc.draw(
            gs::text()
                .alphabet("a/ '\"$`\\;&|*?[]{}()<>~!#=%\n\t\u{e9}")
                .min_size(2)
                .max_size(16),
        )
        .into_bytes()
    } else {
        tc.draw(gs::binary().max_size(12))
    };
    if tc.draw(gs::weighted_booleans(0.1)) {
        bytes.push(b':');
    }
    bytes.retain(|byte| *byte != 0);
    bytes
}

fn make_executable(path: &Path) {
    fs::write(path, "#!/bin/sh\n").expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("mark executable");
}

/// A quoted shim path is spliced into shell source that the user evaluates.
/// Whatever the path contains, every shell reads it back as that one word,
/// and a path that cannot be spliced safely is refused instead.
// More cases than the fixture-building properties: each shell metacharacter
// has to turn up in some path, and a case only costs three short processes.
#[hegel::test(test_cases = 200)]
fn quoted_shim_paths_are_one_literal_shell_word(tc: TestCase) {
    let bytes = path_bytes(&tc);
    let path = PathBuf::from(OsString::from_vec(bytes.clone()));
    let quoted = match posix_quote_path(&path) {
        Ok(quoted) => quoted,
        Err(error) => {
            assert!(matches!(error, ActivationError::Shell(_)));
            // `PATH` cannot hold a colon, and the script is UTF-8 text.
            assert!(std::str::from_utf8(&bytes).is_err() || bytes.contains(&b':'));
            return;
        }
    };
    assert!(std::str::from_utf8(&bytes).is_ok() && !bytes.contains(&b':'));
    for mut shell in shells() {
        let script = format!("set -- {quoted}; printf '%s' \"$#\" \"$1\"");
        let output = shell.args(["-c", &script]).output().expect("run shell");
        assert!(output.status.success(), "{shell:?} rejected {quoted:?}");
        assert_eq!(
            output.stdout,
            [b"1", bytes.as_slice()].concat(),
            "{shell:?}"
        );
    }
}

/// Deactivation removes the durable shim, the recorded process shim, and any
/// stacked `riftri exec` shim from `PATH`, keeps every other entry in order
/// (empty ones included), and unsets its own variables.
#[hegel::test(test_cases = SHIM_CASES)]
fn deactivation_removes_exactly_the_shim_entries_from_path(tc: TestCase) {
    let shim = shell_shim_directory().expect("shell shim directory");
    let shim = shim.to_str().expect("UTF-8 shim directory").to_owned();
    let script = prepare_posix_shell_deactivation_inner().expect("render deactivation code");
    let process_shim = tc.draw(gs::sampled_from(vec![
        None,
        Some(""),
        Some("/tmp/scope/riftri-git-shim-recorded"),
        Some("/opt/renamed scope"),
    ]));
    let candidates = vec![
        shim.clone(),
        format!("{shim}/"),
        "/tmp/scope/riftri-git-shim-recorded".to_owned(),
        "/opt/renamed scope".to_owned(),
        "/tmp/nested/riftri-git-shim-stacked".to_owned(),
        "riftri-git-shim-relative".to_owned(),
        "/opt/riftri-git-shim-tools/bin".to_owned(),
        "/opt/not-riftri-git-shim-x".to_owned(),
        "/usr/bin".to_owned(),
        String::new(),
        "/opt/it's \"$HOME\" *".to_owned(),
        "-n".to_owned(),
    ];
    let entries = tc.draw(gs::vecs(gs::sampled_from(candidates)).max_size(6));
    let expected = entries
        .iter()
        .filter(|entry| {
            let leaf = entry.rsplit('/').next().unwrap_or(entry);
            **entry != shim
                && process_shim.is_none_or(|scope| scope.is_empty() || *entry != scope)
                && !leaf.starts_with(PROCESS_SHIM_DIR_PREFIX)
        })
        .cloned()
        .collect::<Vec<_>>()
        .join(":");

    let report = format!(
        "eval \"$RIFTRI_TEST_DEACTIVATION\"\nprintf '%s\\n' \"$PATH\" \"${{{}-unset}}\" \"${{{PROCESS_SHIM_DIR_ENV}-unset}}\" \"${{{SHIM_ACTIVE_ENV}-unset}}\" \"${{{SHELL_SHIM_DIR_ENV}-unset}}\" \"${{_riftri_shim-unset}}\" \"${{_riftri_entry-unset}}\"",
        riftri_git::REAL_GIT_ENV
    );
    for mut command in shells() {
        command
            .args(["-c", &report])
            .env("RIFTRI_TEST_DEACTIVATION", &script)
            .env("PATH", entries.join(":"))
            .env(SHIM_ACTIVE_ENV, "1")
            .env(SHELL_SHIM_DIR_ENV, &shim)
            .env(riftri_git::REAL_GIT_ENV, "/usr/bin/git")
            .env_remove(PROCESS_SHIM_DIR_ENV);
        if let Some(scope) = process_shim {
            command.env(PROCESS_SHIM_DIR_ENV, scope);
        }
        let output = command.output().expect("evaluate deactivation");
        let shell = command.get_program().to_owned();
        assert!(output.status.success(), "{shell:?} failed");
        assert_eq!(
            String::from_utf8(output.stdout).expect("UTF-8 shell output"),
            format!("{expected}\nunset\nunset\nunset\nunset\nunset\nunset\n"),
            "{shell:?} with PATH {entries:?} and scope {process_shim:?}"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Absent,
    Directory,
    File,
    SymlinkToDirectory,
    DanglingSymlink,
}

impl Entry {
    fn draw(tc: &TestCase) -> Self {
        tc.draw(gs::sampled_from(vec![
            Self::Absent,
            Self::Directory,
            Self::File,
            Self::SymlinkToDirectory,
            Self::DanglingSymlink,
        ]))
    }

    /// Create this kind of entry at `path`; symlinks point at `outside`.
    fn create(self, path: &Path, outside: &Path) {
        match self {
            Self::Absent => {}
            Self::Directory => fs::create_dir(path).expect("create directory"),
            Self::File => fs::write(path, b"occupied").expect("create file"),
            Self::SymlinkToDirectory => symlink(outside, path).expect("create symlink"),
            Self::DanglingSymlink => symlink(outside.join("absent"), path).expect("dangling"),
        }
    }
}

/// The durable shim lives in `<cache>/shims/v1`. Each level is created when
/// missing and must otherwise already be a real directory: a file or a
/// symlink at any level is refused, and nothing is created through a symlink.
#[hegel::test(test_cases = SHIM_CASES)]
fn the_shim_directory_is_only_ever_a_chain_of_real_directories(tc: TestCase) {
    let fixture = tempdir().expect("fixture");
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    let cache = fixture.path().join("cache");
    let levels = [cache.clone(), cache.join("shims"), cache.join("shims/v1")];
    let mut kinds = [Entry::Absent; 3];
    for (kind, level) in kinds.iter_mut().zip(&levels) {
        *kind = Entry::draw(&tc);
        kind.create(level, &outside);
        if *kind != Entry::Directory {
            // Nothing can be placed below a level that is not a directory.
            break;
        }
    }
    let standalone = fixture.path().join("standalone");
    let standalone_kind = Entry::draw(&tc);
    standalone_kind.create(&standalone, &outside);
    assert_eq!(
        require_real_shell_directory(&standalone).is_ok(),
        standalone_kind == Entry::Directory
    );

    let ensured = ensure_real_shell_shim_directory(&levels[2]);
    let creatable = kinds
        .iter()
        .all(|kind| matches!(kind, Entry::Absent | Entry::Directory));
    assert_eq!(ensured.is_ok(), creatable, "{kinds:?}: {ensured:?}");
    assert!(entry_names(&outside).is_empty(), "wrote through a symlink");
    let Err(error) = ensured else {
        for level in &levels {
            let metadata = fs::symlink_metadata(level).expect("created level");
            assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        }
        let mode = tc.draw(gs::sampled_from(vec![0o700, 0o711, 0o750, 0o755, 0o777]));
        fs::set_permissions(&levels[2], fs::Permissions::from_mode(mode)).expect("loosen mode");
        set_private_directory_permissions(&levels[2]).expect("secure the shim directory");
        let secured = fs::metadata(&levels[2]).expect("metadata").permissions();
        assert_eq!(secured.mode() & 0o7777, 0o700);
        ensure_real_shell_shim_directory(&levels[2]).expect("an existing chain is accepted");
        return;
    };
    assert!(matches!(error, ActivationError::Shell(_)));
    assert!(matches!(
        set_private_directory_permissions(&fixture.path().join("absent")),
        Err(ActivationError::Shell(_))
    ));
}

/// A directory the user cannot write to cannot gain a shim level; that is a
/// shell-activation failure, not a panic or a partial directory chain.
#[hegel::test(test_cases = SHIM_CASES)]
fn an_unwritable_cache_refuses_a_new_shim_directory(tc: TestCase) {
    let fixture = tempdir().expect("fixture");
    let cache = fixture.path().join("cache");
    let existing = tc.draw(gs::integers::<usize>().max_value(1));
    let locked = if existing == 0 {
        cache.clone()
    } else {
        cache.join("shims")
    };
    fs::create_dir_all(&locked).expect("existing levels");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("lock directory");
    // Root ignores directory permissions, so there is nothing to observe.
    let enforced = fs::write(locked.join("probe"), b"").is_err();

    let ensured = ensure_real_shell_shim_directory(&cache.join("shims/v1"));
    let created = entry_names(&locked);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock directory");
    if enforced {
        assert!(matches!(ensured, Err(ActivationError::Shell(_))));
        assert!(created.is_empty(), "{created:?}");
    }
}

/// Installing the durable shim replaces whatever `git` entry was there with
/// a symlink to the running executable, atomically: afterwards the entry is
/// exactly that link, or the old entry is untouched, and no temporary file
/// remains either way.
#[hegel::test(test_cases = SHIM_CASES)]
fn the_durable_shim_is_replaced_atomically(tc: TestCase) {
    let fixture = tempdir().expect("fixture");
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    let directory = fixture.path().join("v1");
    let has_directory = tc.draw(gs::integers::<u8>().max_value(9)) > 0;
    let previous = Entry::draw(&tc);
    if has_directory {
        fs::create_dir(&directory).expect("shim directory");
        previous.create(&directory.join("git"), &outside);
    }
    let mut executable = path_bytes(&tc);
    executable.insert(0, b'/');
    let executable = PathBuf::from(OsString::from_vec(executable));

    let installed = install_durable_git_shim(&directory, &executable);
    let replaceable = has_directory && previous != Entry::Directory;
    assert_eq!(
        installed.is_ok(),
        replaceable,
        "{previous:?}: {installed:?}"
    );
    if !has_directory {
        assert!(!directory.exists());
        return;
    }
    assert_eq!(entry_names(&directory), ["git"].map(OsString::from));
    let shim = directory.join("git");
    match installed {
        Ok(path) => {
            assert_eq!(path, shim);
            assert_eq!(fs::read_link(&shim).expect("shim link"), executable);
        }
        Err(error) => {
            assert!(matches!(error, ActivationError::Shell(_)));
            assert!(fs::symlink_metadata(&shim).expect("old entry").is_dir());
        }
    }
    assert!(entry_names(&outside).is_empty(), "wrote through a symlink");
}

/// The marker bakes the real Git path into a shim directory as native bytes,
/// so a shim with a stripped environment reads back exactly the path that was
/// recorded, and a failed write leaves no temporary file behind.
#[hegel::test(test_cases = SHIM_CASES)]
fn the_real_git_marker_round_trips_native_paths(tc: TestCase) {
    let fixture = tempdir().expect("fixture");
    let directory = fixture.path().join("shim");
    let previous = tc.draw(gs::sampled_from(vec![
        Entry::Absent,
        Entry::File,
        Entry::Directory,
    ]));
    let has_directory = tc.draw(gs::integers::<u8>().max_value(9)) > 0;
    if has_directory {
        fs::create_dir(&directory).expect("shim directory");
        previous.create(&directory.join(REAL_GIT_MARKER_FILE), fixture.path());
    }
    let real_git = PathBuf::from(OsString::from_vec(path_bytes(&tc)));

    let recorded = record_real_git_marker(&directory, &real_git);
    let writable = has_directory && previous != Entry::Directory;
    assert_eq!(recorded.is_ok(), writable, "{previous:?}: {recorded:?}");
    if let Err(error) = &recorded {
        assert!(matches!(error, ActivationError::Process(_)));
    }
    assert_eq!(
        read_real_git_marker(&directory),
        writable.then_some(real_git)
    );
    if has_directory {
        assert_eq!(
            entry_names(&directory),
            [REAL_GIT_MARKER_FILE].map(OsString::from)
        );
    }
}

/// What one `PATH` directory offers a search for the real Git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    Nothing,
    RealGit,
    NotExecutable,
    /// A `git` that is the running executable under another name.
    Itself,
    /// A shim: its own `git`, and a marker naming this real Git.
    ShimNaming(MarkerTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerTarget {
    BakedGit,
    Missing,
    Itself,
}

struct SearchFixture {
    _fixture: tempfile::TempDir,
    directories: Vec<(PathBuf, Offer)>,
    baked_git: PathBuf,
    missing: PathBuf,
    running: PathBuf,
}

impl SearchFixture {
    fn draw(tc: &TestCase) -> Self {
        let fixture = tempdir().expect("fixture");
        let root = fs::canonicalize(fixture.path()).expect("canonical fixture");
        let (baked_git, running) = (root.join("baked-git"), root.join("riftri"));
        make_executable(&baked_git);
        make_executable(&running);
        let missing = root.join("missing-git");
        let offers = vec![
            Offer::Nothing,
            Offer::RealGit,
            Offer::NotExecutable,
            Offer::Itself,
            Offer::ShimNaming(MarkerTarget::BakedGit),
            Offer::ShimNaming(MarkerTarget::Missing),
            Offer::ShimNaming(MarkerTarget::Itself),
        ];
        let mut directories = Vec::new();
        for (index, offer) in tc
            .draw(gs::vecs(gs::sampled_from(offers)).max_size(5))
            .into_iter()
            .enumerate()
        {
            let directory = root.join(format!("path-{index}"));
            fs::create_dir(&directory).expect("PATH directory");
            let git = directory.join("git");
            match offer {
                Offer::Nothing => {}
                Offer::RealGit => make_executable(&git),
                Offer::NotExecutable => fs::write(&git, "data").expect("plain file"),
                Offer::Itself => symlink(&running, &git).expect("link to the executable"),
                Offer::ShimNaming(target) => {
                    symlink(&running, &git).expect("shim entry");
                    let target = match target {
                        MarkerTarget::BakedGit => &baked_git,
                        MarkerTarget::Missing => &missing,
                        MarkerTarget::Itself => &running,
                    };
                    fs::write(
                        directory.join(REAL_GIT_MARKER_FILE),
                        target.as_os_str().as_bytes(),
                    )
                    .expect("marker");
                }
            }
            directories.push((directory, offer));
        }
        Self {
            _fixture: fixture,
            directories,
            baked_git,
            missing,
            running,
        }
    }

    fn path(&self) -> OsString {
        std::env::join_paths(self.directories.iter().map(|(directory, _)| directory))
            .expect("join PATH")
    }

    /// The first real Git along `PATH`: a shim contributes only what its
    /// marker names, and the running executable is never a candidate.
    fn first_real_git(&self, skipped: Option<&Path>) -> Option<PathBuf> {
        self.directories
            .iter()
            .filter(|(directory, _)| Some(directory.as_path()) != skipped)
            .find_map(|(directory, offer)| match offer {
                Offer::RealGit => Some(directory.join("git")),
                Offer::ShimNaming(MarkerTarget::BakedGit) => Some(self.baked_git.clone()),
                _ => None,
            })
    }
}

/// Whatever `PATH` looks like, the Git recorded for later delegation is the
/// first real one: never a shim's own entry, never the running executable,
/// and nothing at all when no real Git is reachable.
#[hegel::test(test_cases = SHIM_CASES)]
fn the_real_git_lookup_never_selects_a_shim_or_itself(tc: TestCase) {
    let search = SearchFixture::draw(&tc);
    let found = real_git_on_path(&search.path(), Some(search.running.clone()));
    assert_eq!(found, search.first_real_git(None));
}

/// A shim that lost its environment looks, in order, at the recorded real
/// Git, the marker of its own process scope, and then `PATH` without that
/// scope's directory.
#[hegel::test(test_cases = SHIM_CASES)]
fn a_stripped_shim_resolves_the_real_git_in_documented_order(tc: TestCase) {
    let search = SearchFixture::draw(&tc);
    let recorded = tc.draw(gs::sampled_from(vec![
        None,
        Some(OsString::new()),
        Some(search.baked_git.clone().into_os_string()),
        Some(search.missing.clone().into_os_string()),
        Some(search.running.clone().into_os_string()),
    ]));
    let scope = if search.directories.is_empty() || tc.draw(gs::booleans()) {
        None
    } else {
        let index = tc.draw(gs::integers::<usize>().max_value(search.directories.len() - 1));
        Some(search.directories[index].clone())
    };
    let path = tc.draw(gs::booleans()).then(|| search.path());

    let found = resolve_stripped_real_git(
        recorded.clone(),
        scope.as_ref().map(|(directory, _)| directory.clone()),
        path.clone(),
        Some(search.running.clone()),
    );
    let expected = if recorded.as_deref() == Some(search.baked_git.as_os_str())
        || matches!(scope, Some((_, Offer::ShimNaming(MarkerTarget::BakedGit))))
    {
        Some(search.baked_git.clone())
    } else {
        path.and_then(|_| {
            search.first_real_git(scope.as_ref().map(|(directory, _)| directory.as_path()))
        })
    };
    assert_eq!(found, expected);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn powershell_code_is_only_rendered_on_windows() {
    for rendered in [
        super::prepare_powershell_hook(),
        super::prepare_powershell_deactivation(),
    ] {
        assert!(matches!(rendered, Err(ActivationError::Shell(_))));
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_overlayfs_helper_is_only_installed_on_linux() {
    for replace in [false, true] {
        assert!(matches!(
            super::install_overlayfs_helper(replace),
            Err(ActivationError::Process(_))
        ));
    }
}
