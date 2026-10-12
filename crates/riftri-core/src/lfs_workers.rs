//! Scheduling for independent, unpublished LFS checkout destinations.

pub(super) fn run<E: Send>(
    count: usize,
    workers: usize,
    operation: impl Fn(usize) -> Result<(), E> + Sync,
) -> Result<(), E> {
    let workers = workers.clamp(1, 4).min(count);
    if workers <= 1 {
        return (0..count).try_for_each(operation);
    }
    // Admission and failure publication share a lock. Once an error is
    // observed, no further destinations are admitted. Earlier admitted work
    // still settles, and an earlier input error takes precedence over a later
    // one regardless of completion order.
    let queue = std::sync::Mutex::new((0..count, None::<(usize, E)>));
    let work = || loop {
        let index = {
            let mut queue = queue.lock().expect("LFS work queue poisoned");
            if queue.1.is_some() {
                break;
            }
            let Some(index) = queue.0.next() else {
                break;
            };
            index
        };
        if let Err(error) = operation(index) {
            let mut queue = queue.lock().expect("LFS work queue poisoned");
            if queue.1.as_ref().is_none_or(|(first, _)| index < *first) {
                queue.1 = Some((index, error));
            }
            break;
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            scope.spawn(work);
        }
        work();
    });
    // The scope joins every writer before the caller can remove staging.
    queue
        .into_inner()
        .expect("LFS work queue poisoned")
        .1
        .map_or(Ok(()), |(_, error)| Err(error))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    #[test]
    fn later_objects_can_progress_while_the_first_is_busy() {
        let ready = (Mutex::new(false), Condvar::new());
        super::run(8, 2, |index| {
            if index == 0 {
                let (guard, _) = ready
                    .1
                    .wait_timeout_while(ready.0.lock().unwrap(), Duration::from_secs(1), |ready| {
                        !*ready
                    })
                    .unwrap();
                if !*guard {
                    return Err("later objects stayed blocked");
                }
            } else if index == 1 {
                *ready.0.lock().unwrap() = true;
                ready.1.notify_all();
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn workers_are_bounded_and_every_object_is_visited_once() {
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let visits = (0..64).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();
        super::run(64, 100, |index| {
            let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(concurrent, Ordering::SeqCst);
            visits[index].fetch_add(1, Ordering::SeqCst);
            std::thread::yield_now();
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(maximum.load(Ordering::SeqCst) <= 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(visits.iter().all(|count| count.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn serial_work_runs_inline_and_stops_at_the_first_error() {
        let caller = std::thread::current().id();
        let visited = Mutex::new(Vec::new());
        assert_eq!(
            super::run(10, 1, |index| {
                assert_eq!(std::thread::current().id(), caller);
                visited.lock().unwrap().push(index);
                if index == 2 { Err(index) } else { Ok(()) }
            }),
            Err(2)
        );
        assert_eq!(*visited.lock().unwrap(), vec![0, 1, 2]);
        super::run(0, 4, |_| panic!("empty work")).unwrap_or_else(|_: ()| unreachable!());
    }

    #[test]
    fn failure_joins_admitted_work_and_reports_the_earliest_input_error() {
        let ready = (Mutex::new(false), Condvar::new());
        let finished = AtomicUsize::new(0);
        let result = super::run(2, 2, |index| {
            if index == 0 {
                let (guard, _) = ready
                    .1
                    .wait_timeout_while(ready.0.lock().unwrap(), Duration::from_secs(1), |ready| {
                        !*ready
                    })
                    .unwrap();
                assert!(*guard, "the other worker never started");
            } else {
                *ready.0.lock().unwrap() = true;
                ready.1.notify_all();
            }
            finished.fetch_add(1, Ordering::SeqCst);
            Err(index)
        });
        assert_eq!(result, Err(0));
        assert_eq!(finished.load(Ordering::SeqCst), 2);
    }
}
