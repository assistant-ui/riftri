use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use hegel::TestCase;
use hegel::generators as gs;

use super::try_for_each_bounded;

#[hegel::test]
fn one_worker_behaves_like_a_sequential_loop(tc: TestCase) {
    let fails = tc.draw(gs::vecs(gs::booleans()).max_size(32));
    // Limits of zero and one both mean a single inline worker.
    let limit = tc.draw(gs::integers::<usize>().max_value(1));
    let seen = Mutex::new(Vec::new());

    let result = try_for_each_bounded((0..fails.len()).collect(), limit, |item| {
        seen.lock().unwrap().push(item);
        if fails[item] { Err(item) } else { Ok(()) }
    });

    let first_failure = fails.iter().position(|fails| *fails);
    assert_eq!(result, first_failure.map_or(Ok(()), Err));
    let reached = first_failure.map_or(fails.len(), |index| index + 1);
    assert_eq!(*seen.lock().unwrap(), (0..reached).collect::<Vec<_>>());
}

#[hegel::test]
fn bounded_workers_run_items_at_most_once_and_surface_a_real_failure(tc: TestCase) {
    let fails = tc.draw(gs::vecs(gs::booleans()).max_size(48));
    let limit = tc.draw(gs::integers::<usize>().max_value(8));
    let runs = fails
        .iter()
        .map(|_| AtomicUsize::new(0))
        .collect::<Vec<_>>();
    let active = AtomicUsize::new(0);
    let most_active = AtomicUsize::new(0);

    let result = try_for_each_bounded((0..fails.len()).collect(), limit, |item| {
        most_active.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        runs[item].fetch_add(1, Ordering::SeqCst);
        // Give sibling workers a chance to overlap without sleeping.
        std::thread::yield_now();
        active.fetch_sub(1, Ordering::SeqCst);
        if fails[item] { Err(item) } else { Ok(()) }
    });

    let runs = runs
        .iter()
        .map(|count| count.load(Ordering::SeqCst))
        .collect::<Vec<_>>();
    assert!(runs.iter().all(|count| *count <= 1), "{runs:?}");
    assert!(most_active.load(Ordering::SeqCst) <= limit.max(1));
    match result {
        // Success means nothing was skipped.
        Ok(()) => {
            assert!(!fails.contains(&true));
            assert!(runs.iter().all(|count| *count == 1), "{runs:?}");
        }
        // The reported error belongs to an item that ran and failed.
        Err(item) => assert!(fails[item] && runs[item] == 1),
    }
}
