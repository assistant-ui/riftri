//! Ownership routing must not require a storage-accounting walk or opt-in.

#[cfg(target_os = "macos")]
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod support;
use support::writable_tempdir as tempdir;

fn git(repository: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn owner(repository: &Path, destination: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "owner", "--json", "--json-errors", "--"])
        .arg(destination)
        .current_dir(repository)
        .output()
        .unwrap()
}

fn inspect(repository: &Path, destinations: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "inspect", "--json", "--json-errors", "--"])
        .args(destinations)
        .current_dir(repository)
        .output()
        .unwrap()
}

#[test]
fn batch_inspection_reports_unmanaged_paths_without_creating_state() {
    let fixture = tempdir().unwrap();
    let repository = fixture.path();
    git(repository, &["init", "--quiet"]);
    let first = repository.join("-first");
    let second = repository.join("second");
    let output = inspect(repository, &[&first, &second]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    let worktrees = report["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 2);
    for (view, path) in worktrees.iter().zip([&first, &second]) {
        assert_eq!(view["path"], path.display().to_string());
        assert!(view["state_directory"].is_null());
        assert!(view["backend"].is_null());
        assert!(view["mount_status"].is_null());
        assert!(view["path_native_hex"].is_string());
    }
    assert!(!repository.join(".git/riftri").exists());
    let stale = repository.join("missing-state");
    git(
        repository,
        &["config", "riftri.stateDirectory", stale.to_str().unwrap()],
    );
    let failed = inspect(repository, &[&first, &second]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
}

#[test]
fn owner_reports_unmanaged_without_creating_state_and_refuses_stale_registrations() {
    let fixture = tempdir().unwrap();
    let repository = fixture.path();
    git(repository, &["init", "--quiet"]);
    let destination = repository.join("missing-view");
    let output = owner(repository, &destination);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["state_directory"], serde_json::Value::Null);
    assert_eq!(
        report["state_directory_native_hex"],
        serde_json::Value::Null
    );
    assert!(report["native_path_encoding"].is_string());
    assert!(!repository.join(".git/riftri").exists());
    assert!(!destination.exists());

    let stale = repository.join("missing-state");
    git(
        repository,
        &["config", "riftri.stateDirectory", stale.to_str().unwrap()],
    );
    let failed = owner(repository, &destination);
    assert!(!failed.status.success());
    assert!(
        failed.stdout.is_empty(),
        "failed lookup must not claim unmanaged"
    );
    let receipt: serde_json::Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(receipt["operation"], "worktree-owner");
    assert!(!stale.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn owner_finds_custom_state_aliases_and_refuses_incomplete_adds() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempdir().unwrap();
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "fixture"]);
    let state = fixture.path().join("custom-state-💾");
    let destination = fixture.path().join("managed");
    let added = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["worktree", "add", "--detach", "--json", "--state-dir"])
        .arg(&state)
        .arg(&destination)
        .arg("HEAD")
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    let add: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    let alias = fixture.path().join("alias");
    std::os::unix::fs::symlink(&destination, &alias).unwrap();
    // Ownership does not require clean contents or a repository enable flag.
    fs::write(destination.join("tracked.txt"), "private edit\n").unwrap();
    let unreadable = destination.join("private-directory");
    fs::create_dir(&unreadable).unwrap();
    for path in [&destination, &alias] {
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o0)).unwrap();
        let output = owner(&repository, path);
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(output.status.success(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["state_directory"],
            state.canonicalize().unwrap().display().to_string()
        );
        let hex: String = state
            .canonicalize()
            .unwrap()
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(report["state_directory_native_hex"], hex);
    }

    let journal_path = add["journal_path"].as_str().unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o0)).unwrap();
    let inspection = inspect(
        &repository,
        &[&destination, &alias, &repository.join("ordinary")],
    );
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(inspection.status.success(), "{inspection:?}");
    let report: serde_json::Value = serde_json::from_slice(&inspection.stdout).unwrap();
    for view in &report["worktrees"].as_array().unwrap()[..2] {
        assert_eq!(view["backend"], "apfs-clone");
        assert!(view["mount_status"].is_null());
        assert_eq!(
            view["state_directory"],
            state.canonicalize().unwrap().display().to_string()
        );
    }
    assert!(report["worktrees"][2]["state_directory"].is_null());
    let original = fs::read(journal_path).unwrap();
    let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
    journal["phase"] = serde_json::json!("index-synchronized");
    fs::write(journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let failed = owner(&repository, &destination);
    assert!(
        !failed.status.success(),
        "unfinished add must not become ordinary Git: {failed:?}"
    );
    assert!(failed.stdout.is_empty());
    let failed = inspect(&repository, &[&repository.join("ordinary"), &destination]);
    assert!(!failed.status.success());
    assert!(
        failed.stdout.is_empty(),
        "a partial batch must not appear successful"
    );
    assert_eq!(
        fs::read_to_string(destination.join("tracked.txt")).unwrap(),
        "private edit\n"
    );
    fs::write(journal_path, original).unwrap();
}
