//! Git owns FSMonitor; Riftri must preserve its explicit opt-in and Git safety.
//! The measured and CI-tested profile is macOS/APFS with built-in FSMonitor.
#![cfg(target_os = "macos")]

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[allow(dead_code)]
mod support;

fn isolated(program: &str, directory: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(directory);
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy();
        if key.starts_with("GIT_") || key.starts_with("RIFTRI_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    success(isolated("git", directory).args(arguments).output().unwrap())
}

fn monitor_status(directory: &Path) -> Output {
    isolated("git", directory)
        // Socket-directory selection must also work after persistent opt-out.
        .args(["-c", "core.fsmonitor=true", "fsmonitor--daemon", "status"])
        .output()
        .unwrap()
}

fn stop_monitor(directory: &Path) {
    if monitor_status(directory).status.success() {
        git(
            directory,
            &["-c", "core.fsmonitor=true", "fsmonitor--daemon", "stop"],
        );
    }
    assert_eq!(monitor_status(directory).status.code(), Some(1));
}

struct Fixture {
    root: support::WritableTempDir,
    // Use a short, disposable socket directory, not Git's HOME fallback.
    _sockets: tempfile::TempDir,
    repository: PathBuf,
    views: Vec<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        let root = support::writable_tempdir().unwrap();
        let sockets = tempfile::Builder::new()
            .prefix("rf-fsm-")
            .tempdir_in("/tmp")
            .unwrap();
        let repository = root.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        for (key, value) in [
            ("user.name", "FSMonitor fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgSign", "false"),
            ("core.autocrlf", "false"),
            ("core.untrackedCache", "false"),
            ("fsmonitor.socketDir", sockets.path().to_str().unwrap()),
        ] {
            git(&repository, &["config", "--local", key, value]);
        }
        fs::write(repository.join("tracked.txt"), "original\n").unwrap();
        fs::write(repository.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(repository.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("tracked.txt", repository.join("link")).unwrap();
        git(&repository, &["add", "."]);
        git(
            &repository,
            &["commit", "--quiet", "-m", "test: FSMonitor fixture"],
        );
        Self {
            root,
            _sockets: sockets,
            repository,
            views: Vec::new(),
        }
    }

    fn riftri(&self) -> Command {
        isolated(env!("CARGO_BIN_EXE_riftri"), &self.repository)
    }

    fn add(&mut self, name: &str, intercepted: bool) -> serde_json::Value {
        let view = self.root.path().join(name);
        self.views.push(view.clone()); // Stop any watcher even if creation panics.
        let mut command = self.riftri();
        if intercepted {
            command
                .args(["exec", "--", "git", "worktree", "add", "--detach"])
                .arg(&view);
            success(command.output().unwrap());
            serde_json::Value::Null
        } else {
            command
                .args(["worktree", "add", "--detach", "--json", "--no-progress"])
                .arg(&view);
            serde_json::from_slice(&success(command.output().unwrap())).unwrap()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Do not leave daemons holding CI's disposable volume after a failed assertion.
        for directory in self.views.iter().chain(std::iter::once(&self.repository)) {
            if !directory.join(".git").exists() {
                continue;
            }
            // Do not unwrap a process-spawn failure while already unwinding.
            let status = isolated("git", directory)
                .args(["-c", "core.fsmonitor=true", "fsmonitor--daemon", "status"])
                .output();
            if status.is_ok_and(|output| output.status.success()) {
                let output = isolated("git", directory)
                    .args(["-c", "core.fsmonitor=true", "fsmonitor--daemon", "stop"])
                    .output();
                if !output.is_ok_and(|output| output.status.success()) {
                    eprintln!(
                        "failed to stop fixture FSMonitor at {}",
                        directory.display()
                    );
                }
            }
        }
    }
}

fn status(view: &Path, full_scan: bool) -> Vec<u8> {
    let mut command = isolated("git", view);
    if full_scan {
        // The oracle must not rewrite the watched index and invalidate its cache.
        command.args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ]);
    }
    success(
        command
            .args([
                "-c",
                "status.renames=false",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ])
            .output()
            .unwrap(),
    )
}

#[test]
fn opt_in_fsmonitor_preserves_changes_isolation_restart_and_dirty_removal() {
    let mut fixture = Fixture::new();
    let first = fixture.add("first", false);
    let peer = fixture.views[0].clone();
    let base = PathBuf::from(first["base_path"].as_str().unwrap());
    // Neither creation nor interception opt-in enables a watcher by default.
    success(fixture.riftri().arg("enable").output().unwrap());
    assert_eq!(
        isolated("git", &peer)
            .args(["config", "--get", "core.fsmonitor"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(monitor_status(&peer).status.code(), Some(1));

    // The documented Git opt-in also works through ordinary Git passthrough.
    success(
        fixture
            .riftri()
            .args([
                "exec",
                "--",
                "git",
                "config",
                "--local",
                "core.fsmonitor",
                "true",
            ])
            .output()
            .unwrap(),
    );
    let second = fixture.add("watched", false);
    assert_eq!(first["base_path"], second["base_path"]);
    assert_eq!(second["reused_base"], true);
    let view = fixture.views[1].clone();
    fixture.add("intercepted", true);
    for path in &fixture.views {
        assert!(status(path, false).is_empty());
        assert!(status(path, true).is_empty());
        assert!(
            monitor_status(path).status.success(),
            "watcher must really be active"
        );
    }

    let target = view.join("tracked.txt");
    for untracked_cache in [false, true] {
        git(
            &view,
            &[
                "config",
                "--local",
                "core.untrackedCache",
                if untracked_cache { "true" } else { "false" },
            ],
        );
        for case in [
            "same-size-mtime",
            "replace",
            "delete",
            "rename",
            "mode",
            "symlink",
            "untracked",
            "staged",
            "restart",
        ] {
            assert!(status(&view, false).is_empty());
            let modified = fs::metadata(&target).unwrap().modified().unwrap();
            match case {
                "same-size-mtime" => {
                    fs::write(&target, "modified\n").unwrap();
                    File::options()
                        .write(true)
                        .open(&target)
                        .unwrap()
                        .set_times(FileTimes::new().set_modified(modified))
                        .unwrap();
                }
                "replace" => {
                    let replacement = view.join("replacement");
                    fs::write(&replacement, "modified\n").unwrap();
                    File::options()
                        .write(true)
                        .open(&replacement)
                        .unwrap()
                        .set_times(FileTimes::new().set_modified(modified))
                        .unwrap();
                    fs::rename(replacement, &target).unwrap();
                }
                "delete" => fs::remove_file(&target).unwrap(),
                "rename" => fs::rename(&target, view.join("renamed")).unwrap(),
                "mode" => {
                    fs::set_permissions(view.join("run.sh"), fs::Permissions::from_mode(0o644))
                        .unwrap()
                }
                "symlink" => {
                    fs::remove_file(view.join("link")).unwrap();
                    symlink("run.sh", view.join("link")).unwrap();
                }
                "untracked" => {
                    fs::create_dir(view.join("nested")).unwrap();
                    fs::write(view.join("nested/new"), "private\n").unwrap();
                }
                "staged" => {
                    fs::write(&target, "staged\n").unwrap();
                    git(&view, &["add", "tracked.txt"]);
                    fs::write(&target, "unstaged\n").unwrap();
                }
                "restart" => {
                    stop_monitor(&view);
                    fs::write(&target, "while stopped\n").unwrap();
                }
                _ => unreachable!(),
            }
            let watched = status(&view, false);
            let full = status(&view, true);
            assert!(!full.is_empty(), "{case} must really change Git status");
            assert_eq!(watched, full, "{case}, untracked_cache={untracked_cache}");
            assert!(monitor_status(&view).status.success());
            let refused = fixture
                .riftri()
                .args(["worktree", "remove", "--no-progress"])
                .arg(&view)
                .output()
                .unwrap();
            assert!(
                !refused.status.success(),
                "dirty removal must refuse: {case}"
            );
            assert!(view.exists());
            assert_eq!(status(&view, true), full, "refusal must preserve edits");
            for unchanged in [&peer, &base] {
                assert_eq!(
                    fs::read(unchanged.join("tracked.txt")).unwrap(),
                    b"original\n"
                );
                assert_eq!(
                    fs::read_link(unchanged.join("link")).unwrap(),
                    Path::new("tracked.txt")
                );
                assert_ne!(
                    fs::metadata(unchanged.join("run.sh"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o111,
                    0
                );
            }
            git(
                &view,
                &[
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    "tracked.txt",
                    "run.sh",
                    "link",
                ],
            );
            if case == "rename" {
                fs::remove_file(view.join("renamed")).unwrap();
            }
            if case == "untracked" {
                fs::remove_file(view.join("nested/new")).unwrap();
                fs::remove_dir(view.join("nested")).unwrap();
            }
            assert!(status(&view, false).is_empty());
        }
    }
    // Opt-out prevents automatic restart; stopping alone would not do that.
    git(
        &fixture.repository,
        &["config", "--local", "core.fsmonitor", "false"],
    );
    for path in fixture
        .views
        .iter()
        .chain(std::iter::once(&fixture.repository))
    {
        stop_monitor(path);
        assert!(status(path, false).is_empty());
        assert_eq!(monitor_status(path).status.code(), Some(1));
    }
    for path in &fixture.views {
        success(
            fixture
                .riftri()
                .args(["worktree", "remove", "--no-progress"])
                .arg(path)
                .output()
                .unwrap(),
        );
        assert!(!path.exists());
    }
    success(
        fixture
            .riftri()
            .args(["gc", "--apply", "--yes", "--no-progress"])
            .output()
            .unwrap(),
    );
    let receipt: serde_json::Value = serde_json::from_slice(&success(
        fixture
            .riftri()
            .args(["status", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(receipt["operations"]["active_views"], 0);
    assert_eq!(receipt["bases"], serde_json::json!([]));
    assert_eq!(receipt["diagnostic_issues"], serde_json::json!([]));
}
