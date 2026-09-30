use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use riftri_git::{Git, ObjectId};

use super::{
    BaseReadLock, NativeCowCloner, acquire_base_read_lock, open_coordination_lock, prepare_base,
};
use fs2::FileExt;

#[test]
fn readers_share_ownership_and_exclude_writers_until_the_last_reader_exits() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("base.lock");
    let first = acquire_base_read_lock(&path).unwrap();
    let second = open_coordination_lock(&path, "test second reader").unwrap();
    FileExt::try_lock_shared(&second).expect("first reader must hold shared ownership");
    let second = BaseReadLock(second);
    let writer = open_coordination_lock(&path, "test writer").unwrap();
    for reader in [first, second] {
        let error = FileExt::try_lock_exclusive(&writer).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );
        drop(reader);
    }
    FileExt::try_lock_exclusive(&writer).unwrap();
    let contender = open_coordination_lock(&path, "test reader").unwrap();
    let error = FileExt::try_lock_shared(&contender).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
    FileExt::unlock(&writer).unwrap();
    FileExt::try_lock_shared(&contender).unwrap();
    assert!(path.is_file(), "coordination locks must never be unlinked");
}

#[cfg(unix)]
#[test]
fn reader_drop_releases_a_description_still_held_by_an_inherited_descriptor() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("base.lock");
    let reader = acquire_base_read_lock(&path).unwrap();
    // Like fork(), dup() retains the same Unix open-file description. Merely
    // closing the original descriptor would keep its flock alive here.
    let inherited = reader.0.try_clone().unwrap();
    drop(reader);
    let writer = open_coordination_lock(&path, "test writer").unwrap();
    FileExt::try_lock_exclusive(&writer).expect("reader ownership explicitly released");
    drop(inherited);
}

#[cfg(unix)]
#[test]
fn reader_rejects_a_symlinked_lock_without_changing_its_target() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("preserve");
    std::fs::write(&target, b"preserve\n").unwrap();
    let path = fixture.path().join("base.lock");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(acquire_base_read_lock(&path).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_waiter_rechecks_a_newly_completed_base_under_shared_ownership() {
    let fixture = tempfile::tempdir().unwrap();
    let base_parent = fixture.path().join("bases");
    fs::create_dir(&base_parent).unwrap();
    let tree = ObjectId::parse("1111111111111111111111111111111111111111").unwrap();
    let base = base_parent.join(tree.as_str());
    let complete = base_parent.join(format!("{}.complete", tree.as_str()));
    let lock_path = base_parent.join(format!("{}.lock", tree.as_str()));

    // Keep one reader alive after the worker's initial cache miss, forcing it
    // to wait for exclusive ownership. The fixture then publishes the base as
    // though another process finished building it during that wait.
    let blocker = acquire_base_read_lock(&lock_path).unwrap();
    let (missed_tx, missed_rx) = mpsc::sync_channel(0);
    let (continue_tx, continue_rx) = mpsc::sync_channel(0);
    let observed_shared = Arc::new(AtomicBool::new(false));
    let worker_base = base.clone();
    let worker_lock = lock_path.clone();
    let worker_observed_shared = Arc::clone(&observed_shared);
    let repository = fixture.path().to_path_buf();
    let worker = thread::spawn(move || {
        let _hook = crate::test_hooks::install(
            crate::test_hooks::FilesystemRacePoint::BaseReadMiss,
            move |_| {
                missed_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
                let next_hook = crate::test_hooks::install(
                    crate::test_hooks::FilesystemRacePoint::BaseReuseAfterExclusiveWait,
                    move |_| {
                        let contender =
                            open_coordination_lock(&worker_lock, "test shared base recheck")
                                .unwrap();
                        FileExt::try_lock_shared(&contender)
                            .expect("completed-base recheck must not retain exclusive ownership");
                        worker_observed_shared.store(true, Ordering::SeqCst);
                    },
                );
                // The next hook consumes itself before this worker exits.
                std::mem::forget(next_hook);
            },
        );
        prepare_base(
            &Git::default(),
            &repository,
            &tree,
            &worker_base,
            &repository.join("base-staging"),
            &repository.join("temporary-index"),
            &[],
            &[],
            &[],
        )
    });

    missed_rx.recv().unwrap();
    fs::create_dir(&base).unwrap();
    fs::write(base.join("tracked"), b"contents\n").unwrap();
    NativeCowCloner::make_tree_read_only(&base).unwrap();
    fs::write(&complete, crate::base_integrity::marker_v2(&base).unwrap()).unwrap();
    continue_tx.send(()).unwrap();
    drop(blocker);

    assert!(worker.join().unwrap().unwrap());
    assert!(observed_shared.load(Ordering::SeqCst));
    NativeCowCloner::make_tree_owner_writable(&base).unwrap();
}

/// Reproducible comparison for the exact bottleneck this lock transition
/// changes. Every reader still hashes the same full base; only the ownership
/// mode differs. Keep this threshold-free because filesystem caches and host
/// load move absolute timings.
#[test]
#[ignore = "manual release-mode concurrent base-verification benchmark"]
fn reports_exclusive_and_shared_base_verification_latency() {
    const DIRECTORIES: usize = 96;
    const FILES_PER_DIRECTORY: usize = 64;
    const PAYLOAD_BYTES: usize = 12 * 1024;
    const READERS: usize = 10;
    const ROUNDS: usize = 5;

    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().join("base");
    fs::create_dir(&base).unwrap();
    let payload = vec![b'x'; PAYLOAD_BYTES];
    for directory in 0..DIRECTORIES {
        let directory = base.join(format!("directory-{directory:03}"));
        fs::create_dir(&directory).unwrap();
        for file in 0..FILES_PER_DIRECTORY {
            fs::write(directory.join(format!("file-{file:03}")), &payload).unwrap();
        }
    }
    let expected = crate::base_integrity::marker_v2(&base).unwrap();
    let lock_path = fixture.path().join("base.lock");
    let mut exclusive_samples = Vec::with_capacity(ROUNDS);
    let mut shared_samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let modes = if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for shared in modes {
            let barrier = Arc::new(Barrier::new(READERS + 1));
            let mut readers = Vec::with_capacity(READERS);
            for _ in 0..READERS {
                let barrier = Arc::clone(&barrier);
                let base = base.clone();
                let lock_path = lock_path.clone();
                let expected = expected.clone();
                readers.push(thread::spawn(move || {
                    let lock = open_coordination_lock(&lock_path, "benchmark base lock").unwrap();
                    barrier.wait();
                    if shared {
                        FileExt::lock_shared(&lock).unwrap();
                    } else {
                        FileExt::lock_exclusive(&lock).unwrap();
                    }
                    assert_eq!(crate::base_integrity::marker_v2(&base).unwrap(), expected);
                }));
            }
            let started = Instant::now();
            barrier.wait();
            for reader in readers {
                reader.join().unwrap();
            }
            let elapsed = started.elapsed().as_micros();
            if shared {
                shared_samples.push(elapsed);
            } else {
                exclusive_samples.push(elapsed);
            }
        }
    }
    let mut exclusive_sorted = exclusive_samples.clone();
    exclusive_sorted.sort_unstable();
    let mut shared_sorted = shared_samples.clone();
    shared_sorted.sort_unstable();
    println!(
        "RIFTRI_BASE_RECHECK_BENCHMARK files={} logical_bytes={} readers={} exclusive_median_microseconds={} shared_median_microseconds={} exclusive_samples_microseconds={exclusive_samples:?} shared_samples_microseconds={shared_samples:?}",
        DIRECTORIES * FILES_PER_DIRECTORY,
        DIRECTORIES * FILES_PER_DIRECTORY * PAYLOAD_BYTES,
        READERS,
        exclusive_sorted[ROUNDS / 2],
        shared_sorted[ROUNDS / 2],
    );
}
