use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use riftri_git::{Git, ObjectId};

fn isolated_git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(repository)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
    command
}

fn git(repository: &Path, args: &[&str]) -> Output {
    let output = isolated_git(repository).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn required_helper_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
}

/// A caller-set `GIT_ALTERNATE_OBJECT_DIRECTORIES` pointing at a foreign
/// object store must not let materialization read objects that are absent
/// from the source repository. The assertion runs in a helper process so the
/// override exists in the process environment the isolated materialization
/// strips, without mutating this test process.
#[test]
fn materialization_ignores_caller_alternate_object_directories() {
    let fixture = tempfile::tempdir().unwrap();
    let foreign = fixture.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.name", "Riftri Tests"],
        &["config", "user.email", "riftri@example.invalid"],
        &["config", "core.autocrlf", "false"],
    ] {
        git(&foreign, args);
    }
    fs::write(foreign.join("pinned.txt"), "foreign object store\n").unwrap();
    git(&foreign, &["add", "."]);
    git(&foreign, &["commit", "--quiet", "-m", "initial"]);
    let tree = String::from_utf8(git(&foreign, &["rev-parse", "HEAD^{tree}"]).stdout).unwrap();
    // The target repository shares no objects with the foreign store.
    let target = fixture.path().join("target");
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "--quiet"]);

    let status = Command::new(std::env::current_exe().expect("integration test executable"))
        .arg("--exact")
        .arg("caller_alternates_materialization_helper")
        .arg("--nocapture")
        .env("RIFTRI_GIT_ALTERNATES_HELPER", "1")
        .env("RIFTRI_GIT_HELPER_FOREIGN", &foreign)
        .env("RIFTRI_GIT_HELPER_TARGET", &target)
        .env("RIFTRI_GIT_HELPER_TREE", tree.trim())
        .env("RIFTRI_GIT_HELPER_SCRATCH", fixture.path())
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            foreign.join(".git/objects"),
        )
        .status()
        .expect("run alternates helper process");
    assert!(status.success(), "alternates helper failed: {status}");
}

#[test]
fn caller_alternates_materialization_helper() {
    if std::env::var_os("RIFTRI_GIT_ALTERNATES_HELPER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    assert!(
        std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").is_some(),
        "helper must run with the caller override set"
    );
    let foreign = required_helper_path("RIFTRI_GIT_HELPER_FOREIGN");
    let target = required_helper_path("RIFTRI_GIT_HELPER_TARGET");
    let scratch = required_helper_path("RIFTRI_GIT_HELPER_SCRATCH");
    let tree = ObjectId::parse(std::env::var("RIFTRI_GIT_HELPER_TREE").unwrap()).unwrap();
    let git = Git::new("git");

    // Control: the same tree materializes from the repository that owns its
    // objects, so a refusal below cannot be a broken fixture.
    let control = scratch.join("control-view");
    fs::create_dir(&control).unwrap();
    git.materialize_tree(&foreign, &tree, &control, &scratch.join("control-index"))
        .expect("materialize from the owning repository");
    assert_eq!(
        fs::read(control.join("pinned.txt")).unwrap(),
        b"foreign object store\n"
    );

    // The target repository does not contain the tree; the caller override
    // must not smuggle the foreign store into the isolated materialization.
    let smuggled = scratch.join("smuggled-view");
    fs::create_dir(&smuggled).unwrap();
    git.materialize_tree(&target, &tree, &smuggled, &scratch.join("smuggled-index"))
        .expect_err("foreign alternates must not satisfy materialization");
    assert!(
        !smuggled.join("pinned.txt").exists(),
        "no foreign content may be materialized"
    );
}
