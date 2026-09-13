#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, garbage_collect,
    remove_worktree, storage_accounting,
};

mod support;
use support::writable_tempdir as tempdir;

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .expect("run Git");
    assert!(output.status.success(), "{:?}", output);
    String::from_utf8(output.stdout).expect("UTF-8 fixture output")
}

fn repository(root: &Path) -> std::path::PathBuf {
    let repository = root.join("repository");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Tests"]);
    git(
        &repository,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "base\n").unwrap();
    git(&repository, &["add", "--", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "initial"]);
    repository
}

fn request(repository: &Path, root: &Path, name: &str) -> AddWorktreeRequest {
    AddWorktreeRequest {
        repository: repository.to_path_buf(),
        destination: root.join(name),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::Detached,
        state_dir: Some(root.join("state")),
    }
}

fn base_lock(base: &Path) -> fs::File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(base.with_extension("lock"))
        .unwrap()
}

fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn remove(repository: &Path, root: &Path, destination: &Path) {
    remove_worktree(RemoveWorktreeRequest {
        repository: repository.to_path_buf(),
        destination: destination.to_path_buf(),
        state_dir: Some(root.join("state")),
    })
    .unwrap();
}

#[test]
fn cached_add_completes_while_another_reader_holds_the_base_lock() {
    let fixture = tempdir().unwrap();
    let repository = repository(fixture.path());
    let first = add_worktree(request(&repository, fixture.path(), "first")).unwrap();
    let lock = base_lock(&first.base_path);
    FileExt::lock_shared(&lock).unwrap();

    let second_request = request(&repository, fixture.path(), "second");
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = add_worktree(second_request);
        let _ = send.send(result);
    });
    // This is an ownership assertion, not a latency benchmark. Release the
    // external reader even on failure so the worker cannot deadlock the suite.
    let concurrent = receive.recv_timeout(Duration::from_secs(15));
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    let second = concurrent
        .expect("cached add must not wait for another base reader")
        .expect("create cached view");
    assert!(second.reused_base);
    assert_eq!(first.base_path, second.base_path);
    assert!(git(&second.destination, &["status", "--porcelain=v1"]).is_empty());
    fs::write(second.destination.join("tracked.txt"), "private\n").unwrap();
    for path in [&first.base_path, &first.destination, &repository] {
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"base\n");
    }
}

#[test]
fn cached_add_waits_for_an_exclusive_base_owner() {
    let fixture = tempdir().unwrap();
    let repository = repository(fixture.path());
    let first = add_worktree(request(&repository, fixture.path(), "first")).unwrap();
    let lock = base_lock(&first.base_path);
    FileExt::lock_exclusive(&lock).unwrap();
    let second_request = request(&repository, fixture.path(), "second");
    let destination = second_request.destination.clone();
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = send.send(add_worktree(second_request));
    });
    let metadata_created = wait_until(|| destination.join(".git").exists());
    let waiting = receive.recv_timeout(Duration::from_millis(200));
    let no_view_files = !destination.join("tracked.txt").exists();
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    assert!(metadata_created);
    assert!(matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(no_view_files);
    let second = receive.recv().unwrap().unwrap();
    assert!(second.reused_base);
    assert!(git(&second.destination, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn incomplete_base_is_not_rebuilt_under_a_shared_lock() {
    let fixture = tempdir().unwrap();
    let repository = repository(fixture.path());
    let first = add_worktree(request(&repository, fixture.path(), "first")).unwrap();
    remove(&repository, fixture.path(), &first.destination);
    let marker = first.base_path.with_extension("complete");
    fs::remove_file(&marker).unwrap();
    let lock = base_lock(&first.base_path);
    FileExt::lock_shared(&lock).unwrap();
    let second_request = request(&repository, fixture.path(), "second");
    let destination = second_request.destination.clone();
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = send.send(add_worktree(second_request));
    });
    let metadata_created = wait_until(|| destination.join(".git").exists());
    let waiting = receive.recv_timeout(Duration::from_millis(200));
    let still_incomplete = !marker.exists();
    let original_preserved = fs::read(first.base_path.join("tracked.txt")).ok();
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    assert!(metadata_created);
    assert!(matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(still_incomplete);
    assert_eq!(original_preserved.as_deref(), Some(b"base\n".as_slice()));
    let second = receive.recv().unwrap().unwrap();
    assert!(!second.reused_base);
    assert!(marker.is_file());
    assert!(git(&second.destination, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn cached_reader_still_rejects_an_invalid_integrity_marker() {
    let fixture = tempdir().unwrap();
    let repository = repository(fixture.path());
    let first = add_worktree(request(&repository, fixture.path(), "first")).unwrap();
    let marker = first.base_path.with_extension("complete");
    fs::write(&marker, b"invalid integrity marker\n").unwrap();
    let lock = base_lock(&first.base_path);
    FileExt::lock_shared(&lock).unwrap();
    let second_request = request(&repository, fixture.path(), "second");
    let destination = second_request.destination.clone();
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = send.send(add_worktree(second_request));
    });
    let concurrent = receive.recv_timeout(Duration::from_secs(15));
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    let error = concurrent
        .expect("invalid base must be rejected without waiting for another reader")
        .expect_err("invalid integrity marker must not be reused");
    assert!(error.to_string().contains("integrity"), "{error}");
    assert!(!destination.exists());
    assert_eq!(fs::read(&marker).unwrap(), b"invalid integrity marker\n");
    assert_eq!(
        fs::read(first.base_path.join("tracked.txt")).unwrap(),
        b"base\n"
    );
    assert!(git(&first.destination, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn garbage_collection_waits_until_base_readers_release_the_lock() {
    let fixture = tempdir().unwrap();
    let repository = repository(fixture.path());
    let first = add_worktree(request(&repository, fixture.path(), "first")).unwrap();
    remove(&repository, fixture.path(), &first.destination);
    let lock = base_lock(&first.base_path);
    FileExt::lock_shared(&lock).unwrap();
    let state = fixture.path().join("state");
    let worker_state = state.clone();
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = send.send(garbage_collect(&worker_state, true));
    });
    let intent_recorded = wait_until(|| {
        storage_accounting(&state).is_ok_and(|report| report.pending_collections == 1)
    });
    let waiting = receive.recv_timeout(Duration::from_millis(200));
    let marker_preserved = first.base_path.with_extension("complete").is_file();
    let original_preserved = fs::read(first.base_path.join("tracked.txt")).ok();
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    assert!(intent_recorded);
    assert!(matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(marker_preserved);
    assert_eq!(original_preserved.as_deref(), Some(b"base\n".as_slice()));
    assert_eq!(
        receive.recv().unwrap().unwrap().collected,
        vec![first.base_path.clone()]
    );
    assert!(!first.base_path.exists());
    let status = storage_accounting(&state).unwrap();
    assert_eq!(status.active_views, 0);
    assert!(status.bases.is_empty());
    assert_eq!(status.pending_collections, 0);
}
