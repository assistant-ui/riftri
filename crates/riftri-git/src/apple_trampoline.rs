//! Skipping Apple's `/usr/bin/git` trampoline for Riftri's own Git queries.
//!
//! On macOS, `/usr/bin/git` is not Git. It is one of the `xcrun` stubs that
//! Apple ships on the sealed system volume: each start asks `xcrun` which
//! developer directory is active (`DEVELOPER_DIR`, then the `xcode-select`
//! choice), finds `git` there, adds compiler and manual variables (`SDKROOT`,
//! `CPATH`, `LIBRARY_PATH`, `MANPATH`) to the environment, and executes the
//! real binary. That costs about 8 ms per Git process, and a lifecycle command
//! starts a dozen or more.
//!
//! When the Git Riftri would start is that stub, and the caller has allowed
//! it, Riftri asks `xcrun --find git` once per process which binary the stub
//! would run right now and starts that binary directly, for read-only queries
//! only. The added variables are read by compilers and `man`, not by Git, but a
//! Git command that can start another program (a hook, a filter, an FSMonitor
//! hook, a credential helper) would hand them on, so every such command keeps
//! going through the stub. See D046 in `docs/decisions.md`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Apple's trampoline. It lives on the read-only, sealed system volume, so
/// nothing but Apple's stub can be found at this path.
const TRAMPOLINE: &str = "/usr/bin/git";
/// Apple's own `xcrun`, named absolutely so `PATH` cannot substitute another.
const XCRUN: &str = "/usr/bin/xcrun";

/// The binary the trampoline would run, asked for at most once per process.
/// Never cached across processes: `xcode-select` and `DEVELOPER_DIR` can
/// change between two Riftri commands.
static RESOLVED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The binary to start instead of `recorded` for a read-only query, or `None`
/// to start `recorded` itself.
pub(crate) fn query_executable(recorded: &Path) -> Option<PathBuf> {
    let search_path = std::env::var_os("PATH");
    let spawned = spawned_executable(recorded, search_path.as_deref())?;
    if !is_trampoline(&spawned, Path::new(TRAMPOLINE)) {
        return None;
    }
    RESOLVED
        .get_or_init(|| resolve(Path::new(XCRUN), Path::new(TRAMPOLINE)))
        .clone()
}

/// The file the operating system would execute for `command`, or `None`
/// when that cannot be decided without starting it.
///
/// A command with a directory component is used as given. A bare name is
/// looked up the way `execvp` does, in `PATH` order, and the first entry that
/// holds anything by that name decides: whatever it is, the lookup does not
/// continue past it. An empty or relative `PATH` entry, or no `PATH` at all,
/// makes the answer depend on the child's working directory or the C
/// library's default search path, so it gives up instead.
pub(crate) fn spawned_executable(command: &Path, search_path: Option<&OsStr>) -> Option<PathBuf> {
    if command.is_absolute() {
        return Some(command.to_path_buf());
    }
    if command.components().count() != 1 {
        return None;
    }
    for directory in std::env::split_paths(search_path?) {
        if !directory.is_absolute() {
            return None;
        }
        let candidate = directory.join(command);
        if std::fs::symlink_metadata(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}

/// Whether `executable` is the trampoline itself or a link to it.
pub(crate) fn is_trampoline(executable: &Path, trampoline: &Path) -> bool {
    match (
        std::fs::canonicalize(executable),
        std::fs::canonicalize(trampoline),
    ) {
        (Ok(executable), Ok(trampoline)) => executable == trampoline,
        _ => false,
    }
}

/// Ask `xcrun` which Git the trampoline would run, honouring the caller's
/// `DEVELOPER_DIR`, `xcode-select` choice, and `TOOLCHAINS` exactly as the
/// trampoline does. Anything unexpected answers `None`, which keeps every
/// query on the trampoline.
pub(crate) fn resolve(xcrun: &Path, trampoline: &Path) -> Option<PathBuf> {
    let output = Command::new(xcrun)
        .args(["--find", "git"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    validated_resolution(&output.stdout, trampoline)
}

/// Accept only one absolute path to an executable regular file named `git`
/// that is not the trampoline itself.
pub(crate) fn validated_resolution(stdout: &[u8], trampoline: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let line = stdout.strip_suffix(b"\n")?;
    if line.is_empty() || line.contains(&b'\n') || line.contains(&0) {
        return None;
    }
    let path = PathBuf::from(OsStr::from_bytes(line));
    if !path.is_absolute() || path.file_name() != Some(OsStr::new("git")) {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return None;
    }
    if is_trampoline(&path, trampoline) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_bare_name_is_decided_by_the_first_path_entry_that_holds_it() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        executable(&second.join("git"), "#!/bin/sh\n");
        let search = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            spawned_executable(Path::new("git"), Some(&search)),
            Some(second.join("git"))
        );

        // Anything by that name stops the search, even one that cannot run:
        // the trampoline further along must not be assumed.
        fs::create_dir(first.join("git")).unwrap();
        assert_eq!(
            spawned_executable(Path::new("git"), Some(&search)),
            Some(first.join("git"))
        );
    }

    #[test]
    fn an_ambiguous_lookup_gives_up() {
        let fixture = tempfile::tempdir().unwrap();
        let absolute = fixture.path().join("bin");
        fs::create_dir(&absolute).unwrap();
        executable(&absolute.join("git"), "#!/bin/sh\n");
        for entries in [
            vec![PathBuf::from("relative"), absolute.clone()],
            vec![PathBuf::new(), absolute.clone()],
        ] {
            let search = std::env::join_paths(entries).unwrap();
            assert_eq!(spawned_executable(Path::new("git"), Some(&search)), None);
        }
        assert_eq!(spawned_executable(Path::new("git"), None), None);
        // A relative path with a directory depends on the working directory.
        assert_eq!(spawned_executable(Path::new("bin/git"), None), None);
        assert_eq!(
            spawned_executable(&absolute.join("git"), None),
            Some(absolute.join("git"))
        );
    }

    #[test]
    fn a_link_to_the_trampoline_is_the_trampoline() {
        let fixture = tempfile::tempdir().unwrap();
        let trampoline = fixture.path().join("trampoline");
        executable(&trampoline, "#!/bin/sh\n");
        let link = fixture.path().join("git");
        symlink(&trampoline, &link).unwrap();
        let copy = fixture.path().join("copy");
        executable(&copy, "#!/bin/sh\n");
        assert!(is_trampoline(&trampoline, &trampoline));
        assert!(is_trampoline(&link, &trampoline));
        assert!(!is_trampoline(&copy, &trampoline));
        assert!(!is_trampoline(&fixture.path().join("missing"), &trampoline));
    }

    #[test]
    fn resolution_accepts_only_one_executable_git_that_is_not_the_trampoline() {
        let fixture = tempfile::tempdir().unwrap();
        let trampoline = fixture.path().join("trampoline");
        executable(&trampoline, "#!/bin/sh\n");
        let developer = fixture.path().join("Developer/usr/bin");
        fs::create_dir_all(&developer).unwrap();
        let git = developer.join("git");
        executable(&git, "#!/bin/sh\n");
        let line = |path: &Path| format!("{}\n", path.display()).into_bytes();

        assert_eq!(
            validated_resolution(&line(&git), &trampoline),
            Some(git.clone())
        );

        let not_executable = fixture.path().join("plain/git");
        fs::create_dir_all(not_executable.parent().unwrap()).unwrap();
        fs::write(&not_executable, "").unwrap();
        let other_name = developer.join("git-real");
        executable(&other_name, "#!/bin/sh\n");
        let loops_back = fixture.path().join("loop/git");
        fs::create_dir_all(loops_back.parent().unwrap()).unwrap();
        symlink(&trampoline, &loops_back).unwrap();
        for rejected in [
            Vec::new(),
            b"\n".to_vec(),
            format!("{}", git.display()).into_bytes(),
            format!("{0}\n{0}\n", git.display()).into_bytes(),
            b"usr/bin/git\n".to_vec(),
            line(&fixture.path().join("missing/git")),
            line(&developer),
            line(&not_executable),
            line(&other_name),
            line(&loops_back),
        ] {
            assert_eq!(
                validated_resolution(&rejected, &trampoline),
                None,
                "{:?}",
                String::from_utf8_lossy(&rejected)
            );
        }
    }

    #[test]
    fn resolution_falls_back_when_xcrun_fails_or_cannot_start() {
        let fixture = tempfile::tempdir().unwrap();
        let trampoline = fixture.path().join("trampoline");
        executable(&trampoline, "#!/bin/sh\n");
        let git = fixture.path().join("git");
        executable(&git, "#!/bin/sh\n");

        let answering = fixture.path().join("answering-xcrun");
        executable(
            &answering,
            &format!(
                "#!/bin/sh\n[ \"$*\" = '--find git' ] || exit 64\nprintf '%s\\n' '{}'\n",
                git.display()
            ),
        );
        assert_eq!(resolve(&answering, &trampoline), Some(git.clone()));

        let failing = fixture.path().join("failing-xcrun");
        executable(
            &failing,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\necho 'xcrun: error' >&2\nexit 1\n",
                git.display()
            ),
        );
        assert_eq!(resolve(&failing, &trampoline), None);
        assert_eq!(resolve(&fixture.path().join("missing"), &trampoline), None);
    }

    /// The real trampoline, when this Mac has one that works.
    fn working_trampoline() -> Option<PathBuf> {
        let resolved = resolve(Path::new(XCRUN), Path::new(TRAMPOLINE))?;
        Command::new(TRAMPOLINE)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| resolved)
    }

    #[test]
    fn only_the_trampoline_is_replaced() {
        assert_eq!(query_executable(Path::new("/bin/sh")), None);
        let Some(resolved) = working_trampoline() else {
            return;
        };
        assert_eq!(query_executable(Path::new(TRAMPOLINE)), Some(resolved));
    }

    #[test]
    fn the_resolved_git_reports_the_trampolines_version_byte_for_byte() {
        // The version string is a checkout-profile input, so a base keyed
        // through either path must be found again through the other.
        let Some(resolved) = working_trampoline() else {
            return;
        };
        let version = |program: &Path| {
            let output = Command::new(program).arg("--version").output().unwrap();
            assert!(output.status.success());
            output.stdout
        };
        assert_eq!(version(&resolved), version(Path::new(TRAMPOLINE)));
    }

    #[test]
    fn the_resolved_git_is_the_binary_the_trampoline_runs() {
        let Some(resolved) = working_trampoline() else {
            return;
        };
        assert!(!is_trampoline(&resolved, Path::new(TRAMPOLINE)));
        // The trampoline starts the developer binary by its full path, which
        // is also what a direct start passes, so even the paths Git derives
        // from its own location agree.
        let locations = |program: &Path| {
            ["--exec-path", "--html-path", "--man-path", "--info-path"]
                .map(|option| {
                    let output = Command::new(program).arg(option).output().unwrap();
                    assert!(output.status.success());
                    output.stdout
                })
                .concat()
        };
        assert_eq!(locations(&resolved), locations(Path::new(TRAMPOLINE)));
    }
}
