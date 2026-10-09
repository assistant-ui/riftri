//! Experimental executor for benchmark comparison only; not in release binaries.

use super::{FILE_CLONE_BATCH_SIZE, try_for_each_bounded};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

pub(crate) fn try_for_each_batched_reusing<T, E, F, P>(
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
    std::thread::scope(|scope| {
        // Lazily create the team only for a full batch. Small trees retain the
        // original executor, including its inline zero/one-worker fast path.
        // Dropping these senders also releases idle workers if produce panics.
        let mut workers = Vec::new();
        let mut run_batch = |items: Vec<T>| {
            if worker_limit <= 1
                || items.len() <= 1
                || (workers.is_empty() && items.len() < FILE_CLONE_BATCH_SIZE)
            {
                return try_for_each_bounded(items, worker_limit, &operation);
            }
            if workers.is_empty() {
                for _ in 0..worker_limit.min(FILE_CLONE_BATCH_SIZE) {
                    let (sender, receiver) = mpsc::channel::<(
                        Arc<CloneBatch<T, E>>,
                        mpsc::Sender<std::thread::Result<()>>,
                    )>();
                    let operation = &operation;
                    scope.spawn(move || {
                        while let Ok((batch, completed)) = receiver.recv() {
                            // Transport panics to the caller only after all
                            // admitted work finishes; never strand it awaiting
                            // a completion from a worker that has unwound.
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    batch.run(operation)
                                }));
                            if result.is_err() {
                                batch.failed.store(true, Ordering::Release);
                            }
                            drop(batch);
                            let _ = completed.send(result);
                        }
                    });
                    workers.push(sender);
                }
            }
            let batch = Arc::new(CloneBatch::new(items));
            let (completed, results) = mpsc::channel();
            for worker in &workers {
                worker
                    .send((Arc::clone(&batch), completed.clone()))
                    .expect("clone worker disconnected");
            }
            drop(completed);
            let mut first_panic = None;
            for result in results {
                if let Err(panic) = result {
                    first_panic.get_or_insert(panic);
                }
            }
            if let Some(panic) = first_panic {
                std::panic::resume_unwind(panic);
            }
            batch.result()
        };
        let mut items = Vec::new();
        produce(&mut |item| {
            items.push(item);
            if items.len() == FILE_CLONE_BATCH_SIZE {
                run_batch(std::mem::take(&mut items))?;
            }
            Ok(())
        })?;
        run_batch(items)
    })
}

struct CloneBatch<T, E> {
    queue: Mutex<VecDeque<T>>,
    failed: AtomicBool,
    first_error: Mutex<Option<E>>,
}

impl<T, E> CloneBatch<T, E> {
    fn new(items: Vec<T>) -> Self {
        Self {
            queue: Mutex::new(VecDeque::from(items)),
            failed: AtomicBool::new(false),
            first_error: Mutex::new(None),
        }
    }

    fn run(&self, operation: &impl Fn(T) -> Result<(), E>) {
        loop {
            if self.failed.load(Ordering::Acquire) {
                break;
            }
            let item = self
                .queue
                .lock()
                .expect("clone work queue poisoned")
                .pop_front();
            let Some(item) = item else {
                break;
            };
            if let Err(error) = operation(item) {
                self.failed.store(true, Ordering::Release);
                self.first_error
                    .lock()
                    .expect("clone error slot poisoned")
                    .get_or_insert(error);
                break;
            }
        }
    }

    fn result(&self) -> Result<(), E> {
        match self
            .first_error
            .lock()
            .expect("clone error slot poisoned")
            .take()
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn clone_batches_reuse_the_same_workers() {
        let workers = std::sync::Mutex::new(std::collections::HashSet::new());
        let barrier = Barrier::new(2);
        super::try_for_each_batched_reusing(
            2,
            |item: usize| {
                // Require both workers in every batch, regardless of scheduling.
                if item % super::FILE_CLONE_BATCH_SIZE < 2 {
                    barrier.wait();
                }
                workers.lock().unwrap().insert(std::thread::current().id());
                Ok::<_, ()>(())
            },
            |submit| {
                for item in 0..super::FILE_CLONE_BATCH_SIZE * 3 + 17 {
                    submit(item)?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(workers.into_inner().unwrap().len(), 2);
    }

    #[test]
    fn clone_batches_release_idle_workers_when_production_panics() {
        let completed = AtomicUsize::new(0);
        let panic = std::panic::catch_unwind(|| {
            let _ = super::try_for_each_batched_reusing(
                2,
                |_| {
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(())
                },
                |submit| {
                    for item in 0..super::FILE_CLONE_BATCH_SIZE {
                        submit(item)?;
                    }
                    panic!("traversal panic after a finished batch");
                },
            );
        });
        assert!(panic.is_err());
        assert_eq!(
            completed.load(Ordering::SeqCst),
            super::FILE_CLONE_BATCH_SIZE
        );
    }

    #[test]
    fn clone_batches_join_admitted_work_before_propagating_a_worker_panic() {
        let completed = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        let panic = std::panic::catch_unwind(|| {
            let _ = super::try_for_each_batched_reusing(
                2,
                |item| {
                    if item < 2 {
                        barrier.wait();
                    }
                    if item == 0 {
                        panic!("clone panic");
                    }
                    if item == 1 {
                        std::thread::sleep(Duration::from_millis(10));
                        completed.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok::<_, ()>(())
                },
                |submit| {
                    for item in 0..super::FILE_CLONE_BATCH_SIZE * 2 {
                        submit(item)?;
                    }
                    Ok(())
                },
            );
        });
        assert!(panic.is_err());
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn clone_batches_keep_single_worker_and_single_item_inline() {
        let caller = std::thread::current().id();
        for (limit, count) in [(0, 2049), (1, 2049), (8, 1), (8, 0)] {
            let completed = AtomicUsize::new(0);
            super::try_for_each_batched_reusing(
                limit,
                |_| {
                    assert_eq!(std::thread::current().id(), caller);
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(())
                },
                |submit| {
                    for item in 0..count {
                        submit(item)?;
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(completed.load(Ordering::SeqCst), count);
        }
    }

    #[test]
    fn clone_batches_stop_and_join_after_a_reused_worker_fails() {
        let completed = AtomicUsize::new(0);
        let admitted = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        let error = super::try_for_each_batched_reusing(
            2,
            |item| {
                if (super::FILE_CLONE_BATCH_SIZE..super::FILE_CLONE_BATCH_SIZE + 2).contains(&item)
                {
                    barrier.wait();
                }
                if item == super::FILE_CLONE_BATCH_SIZE {
                    return Err("later clone failure");
                }
                if item == super::FILE_CLONE_BATCH_SIZE + 1 {
                    std::thread::sleep(Duration::from_millis(10));
                }
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |submit| {
                for item in 0..super::FILE_CLONE_BATCH_SIZE * 3 {
                    admitted.fetch_add(1, Ordering::SeqCst);
                    submit(item)?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error, "later clone failure");
        assert_eq!(
            admitted.load(Ordering::SeqCst),
            super::FILE_CLONE_BATCH_SIZE * 2
        );
        // The peer can finish more jobs before observing cancellation. What
        // matters is that admitted work has joined and no next batch is sent.
        assert!(completed.load(Ordering::SeqCst) > super::FILE_CLONE_BATCH_SIZE);
    }

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
        super::try_for_each_batched_reusing(
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
}
