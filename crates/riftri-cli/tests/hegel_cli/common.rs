//! Fixtures and process helpers shared by the end-to-end property tests.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

use crate::contract::{assert_receipt_keys, exit_code_for};
use crate::support::{WritableTempDir, writable_tempdir};

/// A command that sees neither the developer's Git configuration nor an
/// enclosing Riftri activation.
fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null());
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "RIFTRI_BYPASS",
        "RIFTRI_CACHE_DIR",
        "RIFTRI_REAL_GIT",
        "RIFTRI_SHIM_ACTIVE",
        "RIFTRI_SHELL_SHIM_DIR",
        "RIFTRI_PROCESS_SHIM_DIR",
    ] {
        command.env_remove(name);
    }
    command
}

pub(crate) fn riftri() -> Command {
    isolated(env!("CARGO_BIN_EXE_riftri"))
}

pub(crate) fn git(directory: &Path) -> Command {
    let mut command = isolated("git");
    command.current_dir(directory).args([
        "-c",
        "user.name=Riftri Tests",
        "-c",
        "user.email=riftri@example.invalid",
        "-c",
        "commit.gpgSign=false",
    ]);
    command
}

/// Run Git and return its stdout, failing the test if Git fails.
pub(crate) fn git_ok(directory: &Path, arguments: &[&str]) -> String {
    let output = git(directory).args(arguments).output().expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output")
}

/// A scratch directory that can hold repositories and their worktrees.
pub(crate) struct Scratch {
    directory: WritableTempDir,
}

impl Scratch {
    pub(crate) fn new() -> Self {
        Self {
            directory: writable_tempdir().expect("fixture directory"),
        }
    }

    /// The canonical root, so paths compare equal to the ones Git reports.
    pub(crate) fn root(&self) -> PathBuf {
        self.directory
            .path()
            .canonicalize()
            .expect("canonical fixture root")
    }

    /// Create a repository named `name` holding one committed file.
    pub(crate) fn repository(&self, name: &str) -> PathBuf {
        let repository = self.root().join(name);
        std::fs::create_dir(&repository).expect("create repository");
        git_ok(&repository, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(repository.join("tracked.txt"), "tracked\n").expect("write tracked file");
        git_ok(&repository, &["add", "tracked.txt"]);
        git_ok(&repository, &["commit", "--quiet", "-m", "fixture"]);
        repository
    }
}

/// Whether this machine can create copy-on-write worktrees under `scratch`.
/// The suites that require it set a `RIFTRI_REQUIRE_*` variable to fail
/// instead of skipping.
pub(crate) fn copy_on_write_supported(repository: &Path) -> bool {
    let output = riftri()
        .args(["doctor", "--json", "--repository"])
        .arg(repository)
        .arg("--destination")
        .arg(repository.with_extension("probe"))
        .output()
        .expect("run doctor");
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    let supported = report["destination_readiness"]["copy_on_write"] == true;
    let required = std::env::vars_os()
        .any(|(key, value)| key.to_string_lossy().starts_with("RIFTRI_REQUIRE_") && value == "1");
    assert!(supported || !required, "native COW is required: {report}");
    supported
}

/// The one receipt a failed `--json-errors` invocation leaves on stderr,
/// checked against the documented shape and the process exit code.
pub(crate) fn receipt(output: &Output) -> Value {
    let stderr = String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8");
    let line = stderr
        .strip_suffix('\n')
        .unwrap_or_else(|| panic!("stderr does not end in a newline: {stderr:?}"));
    assert!(
        !line.contains('\n'),
        "stderr carries more than the receipt: {stderr}"
    );
    let receipt: Value = serde_json::from_str(line)
        .unwrap_or_else(|error| panic!("stderr is not one JSON receipt ({error}): {stderr}"));
    assert_receipt_keys(&receipt);
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["outcome"], "failed");
    assert_eq!(
        output.status.code(),
        Some(exit_code_for(
            receipt["category"].as_str().expect("category")
        )),
        "exit code and category disagree: {receipt}"
    );
    receipt
}

/// Run `command` with a pseudo-terminal on all three standard streams, type
/// `input`, then end the input. Returns the exit code and everything the
/// terminal showed.
pub(crate) fn run_on_terminal(mut command: Command, input: &[u8]) -> (Option<i32>, String) {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty writes two fresh descriptors; the optional name,
    // termios, and window-size pointers are null.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "open a pseudo-terminal");
    // SAFETY: openpty returned both descriptors with exclusive ownership.
    let (mut master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    command
        .stdin(Stdio::from(slave.try_clone().expect("terminal stdin")))
        .stdout(Stdio::from(slave.try_clone().expect("terminal stdout")))
        .stderr(Stdio::from(slave));
    let mut child = command.spawn().expect("run on a terminal");
    // The child now holds the only open ends of the terminal's far side.
    drop(command);

    let mut reader = master.try_clone().expect("terminal reader");
    let shown = std::thread::spawn(move || {
        let mut shown = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => shown.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                // Linux reports the closed far side as EIO rather than EOF.
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&shown).into_owned()
    });
    // End the input at the start of a line, so a further prompt reads nothing
    // instead of waiting on the test. A command that exits without reading
    // may already have closed the terminal, which is not an error here.
    let _ = master
        .write_all(input)
        .and_then(|()| master.write_all(b"\x04"));
    let status = child.wait().expect("wait for the terminal command");
    (status.code(), shown.join().expect("terminal output"))
}
