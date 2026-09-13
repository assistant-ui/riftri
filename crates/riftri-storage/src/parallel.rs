use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(target_os = "linux", target_os = "windows"))]
const MAX_FILE_CLONE_WORKERS: usize = 8;

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) fn file_clone_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_FILE_CLONE_WORKERS)
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
