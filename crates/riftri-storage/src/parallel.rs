use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(target_os = "linux", target_os = "windows"))]
const MAX_FILE_CLONE_WORKERS: usize = 8;

#[cfg(target_os = "macos")]
const MAX_FILE_CLONE_WORKERS: usize = 4;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn file_clone_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_FILE_CLONE_WORKERS)
}

// Bound queued path pairs without keeping workers alive across traversal or
// cleanup. Directory restoration metadata is intentionally not part of this
// bound; permissions must still be restored after all descendant files finish.
pub(crate) const FILE_CLONE_BATCH_SIZE: usize = 1024;

#[cfg(test)]
pub(crate) mod reuse_evaluation;

pub(crate) fn try_for_each_batched<T, E, F, P>(
    worker_limit: usize,
    operation: F,
    produce: P,
) -> Result<(), E>
where
    T: Send,
    E: Send,
    F: Fn(T) -> Result<(), E> + Sync,
    P: FnOnce(&mut dyn FnMut(T) -> Result<(), E>) -> Result<(), E>,
{
    let mut items = Vec::new();
    produce(&mut |item| {
        items.push(item);
        if items.len() == FILE_CLONE_BATCH_SIZE {
            try_for_each_bounded(std::mem::take(&mut items), worker_limit, &operation)?;
        }
        Ok(())
    })?;
    try_for_each_bounded(items, worker_limit, operation)
}

pub(crate) fn try_for_each_bounded<T, E, F>(
    items: Vec<T>,
    worker_limit: usize,
    operation: F,
) -> Result<(), E>
where
    T: Send,
    E: Send,
    F: Fn(T) -> Result<(), E> + Sync,
{
    if items.is_empty() {
        return Ok(());
    }

    let worker_count = worker_limit.max(1).min(items.len());
    // One effective worker needs neither a thread nor a shared queue. This
    // also covers a one-file checkout on a host with many available CPUs.
    if worker_count == 1 {
        return items.into_iter().try_for_each(operation);
    }
    let queue = Mutex::new(VecDeque::from(items));
    let failed = AtomicBool::new(false);
    let first_error = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if failed.load(Ordering::Acquire) {
                        break;
                    }
                    let item = queue.lock().expect("clone work queue poisoned").pop_front();
                    let Some(item) = item else {
                        break;
                    };
                    if let Err(error) = operation(item) {
                        failed.store(true, Ordering::Release);
                        let mut slot = first_error.lock().expect("clone error slot poisoned");
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                        break;
                    }
                }
            });
        }
    });

    match first_error.into_inner().expect("clone error slot poisoned") {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::try_for_each_bounded;

    #[test]
    fn clone_batches_bound_retained_work_and_process_the_tail() {
        struct Item<'a>(&'a AtomicUsize);
        impl Drop for Item<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let count = super::FILE_CLONE_BATCH_SIZE * 3 + 17;
        super::try_for_each_batched(
            4,
            |_: Item<'_>| {
                completed.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            |submit| {
                for _ in 0..count {
                    peak.fetch_max(live.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    submit(Item(&live))?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), count);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert!(peak.load(Ordering::SeqCst) <= super::FILE_CLONE_BATCH_SIZE);
    }

    #[test]
    fn clone_batches_stop_production_and_join_workers_before_returning_an_error() {
        let admitted = AtomicUsize::new(0);
        let finished = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        let error = super::try_for_each_batched(
            2,
            |item| {
                if item < 2 {
                    barrier.wait();
                }
                if item == 0 {
                    Err("clone failed")
                } else {
                    std::thread::sleep(Duration::from_millis(2));
                    finished.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            |submit| {
                for item in 0..super::FILE_CLONE_BATCH_SIZE * 2 {
                    admitted.fetch_add(1, Ordering::SeqCst);
                    submit(item)?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error, "clone failed");
        assert_eq!(
            admitted.load(Ordering::SeqCst),
            super::FILE_CLONE_BATCH_SIZE
        );
        assert!(finished.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn clone_batches_discard_pending_work_when_traversal_fails() {
        let completed = AtomicUsize::new(0);
        let error = super::try_for_each_batched(
            1,
            |_| {
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |submit| {
                for item in 0..super::FILE_CLONE_BATCH_SIZE + 1 {
                    submit(item)?;
                }
                Err("traversal failed")
            },
        )
        .unwrap_err();
        assert_eq!(error, "traversal failed");
        assert_eq!(
            completed.load(Ordering::SeqCst),
            super::FILE_CLONE_BATCH_SIZE
        );
    }

    #[test]
    fn single_worker_runs_inline_and_stops_at_the_first_error() {
        let caller = std::thread::current().id();
        for limit in [0, 1] {
            let seen = std::sync::Mutex::new(Vec::new());
            let error = try_for_each_bounded(vec![0, 1, 2], limit, |item| {
                assert_eq!(std::thread::current().id(), caller);
                seen.lock().unwrap().push(item);
                if item == 1 { Err("failed") } else { Ok(()) }
            })
            .unwrap_err();
            assert_eq!(error, "failed");
            assert_eq!(*seen.lock().unwrap(), [0, 1]);
        }
        // A high worker limit still needs no thread for a single item.
        try_for_each_bounded(vec![()], 8, |_| -> Result<(), ()> {
            assert_eq!(std::thread::current().id(), caller);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn bounds_parallel_work_and_processes_every_item() {
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);

        try_for_each_bounded((0..32).collect(), 4, |_| -> Result<(), ()> {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2));
            active.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("bounded work succeeds");

        assert_eq!(completed.load(Ordering::SeqCst), 32);
        assert!(maximum.load(Ordering::SeqCst) <= 4);
    }

    #[test]
    fn waits_for_admitted_work_after_the_first_error() {
        let started = Arc::new(Barrier::new(2));
        let completed = AtomicUsize::new(0);

        let error = try_for_each_bounded(vec![0, 1], 2, |item| {
            started.wait();
            if item == 0 {
                Err("clone failed")
            } else {
                std::thread::sleep(Duration::from_millis(10));
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .expect_err("first worker fails");

        assert_eq!(error, "clone failed");
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }
}
