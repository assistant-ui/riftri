#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use riftri_core::progress::{ProgressEvent, set_progress_observer};
use riftri_core::{
    AddWorktreePhase, AddWorktreeRequest, WorktreeMode, add_worktree, recover_incomplete_operations,
};

mod support;

#[test]
fn a_same_size_edit_after_clone_read_ahead_is_not_trusted_as_clean() {
    let fixture = support::writable_tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).unwrap();
    let git = |arguments: &[&str]| {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.name", "Riftri Tests"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "core.autocrlf", "false"]);
    let original = vec![b'a'; 8192];
    let edited = vec![b'b'; 8192];
    fs::write(repository.join("tracked.txt"), &original).unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "--quiet", "-m", "read-ahead fixture"]);
    let request = |name: &str| AddWorktreeRequest {
        repository: repository.clone(),
        destination: fixture.path().join(name),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::Detached,
        state_dir: Some(state.clone()),
        sparse_directories: vec![],
    };
    let anchor = add_worktree(request("anchor")).unwrap();
    let changed = request("changed");
    let target = changed.destination.join("tracked.txt");
    let observed = Arc::new(AtomicBool::new(false));
    let observed_in_callback = Arc::clone(&observed);
    let callback_bytes = edited.clone();
    assert!(set_progress_observer(Box::new(move |event| {
        if *event
            == (ProgressEvent::AddPhase {
                phase: AddWorktreePhase::GitPointerRestored,
            })
        {
            // Native clone workers have finished advising reads, but Git has
            // not initialized its index. Keep length and mtime unchanged to
            // demonstrate that a hint never substitutes for Git reading bytes.
            let modified = fs::metadata(&target).unwrap().modified().unwrap();
            fs::write(&target, &callback_bytes).unwrap();
            fs::File::open(&target)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
            observed_in_callback.store(true, Ordering::SeqCst);
        }
    })));
    let destination = changed.destination.clone();
    add_worktree(changed).expect_err("the changed clone must not be activated as clean");
    assert!(observed.load(Ordering::SeqCst));
    assert!(
        !riftri_git::Git::default()
            .worktree_is_clean(&destination)
            .unwrap()
    );
    for _ in 0..2 {
        let report = recover_incomplete_operations(&state).unwrap();
        assert_eq!(report.recovered, 0);
        assert!(!report.errors.is_empty(), "{report:?}");
        assert_eq!(fs::read(destination.join("tracked.txt")).unwrap(), edited);
        assert_eq!(
            fs::read(anchor.destination.join("tracked.txt")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(anchor.base_path.join("tracked.txt")).unwrap(),
            original
        );
    }
}
