# Clone-worker reuse evaluation

The bounded clone-queue draft starts a new worker team for each 1,024-file
batch. This follow-up tested a team that sleeps between batches within one
clone operation. **It did not establish a consistent latency win and is not
enabled in production.** The prototype is compiled only for tests in
`crates/riftri-storage/src/parallel/reuse_evaluation.rs`.

## Local results

Apple M1, macOS/APFS, release build; clone-stage p50 in milliseconds:

| Regular files | Whole-tree queue | Restarted batches | Reused workers |
| --- | ---: | ---: | ---: |
| 9 | 3.980 | 3.970 | 3.993 |
| 2,049 | 81.893 | 84.502 | 83.324 |
| 16,385 | 979.781 | 1,118.132 | 1,079.551 |
| 2,049, repeated | 128.238 | 105.436 | 92.723 |

The first medium comparison improves only about 1.4% over restarted batches
and remains about 1.7% slower than the whole-tree queue. The repeat improves
more, but the baseline itself changes substantially. Large-tree worker reuse
is about 3.5% faster than restarted batches and about 10.2% slower than the
whole-tree queue. Timing variability is large: the largest whole-tree sample
is 6.27 seconds. No slow samples were discarded.

These observations do not justify shipping a more complex scheduler or
claiming an end-to-end startup improvement. They also show why the previous
single large-tree median should not be treated as a universal batching win.
PR #660 remains a draft. The file-job bound is independently proven; it is
not a bound on directory metadata, whole-process memory, or peak RSS.

## Method and reproducibility

The ignored `reports_batched_clone_comparison` test compares all three
executors in the same release binary. Twelve rounds rotate through all six
execution-order permutations twice. Each fixture has 4 KiB regular files,
an additional readme, and a symlink. Sources are read-only; views restore
writable permissions. Each timed path includes validation and its own umask
lookup. File bytes, entry names, modes, symlink targets, and distinct file
inodes are checked after timing. The medium case was repeated after the large
case, with every raw sample retained.

```sh
RIFTRI_CLONE_BENCH_DIRS=32 RIFTRI_CLONE_BENCH_FILES=64 RIFTRI_CLONE_BENCH_ROUNDS=12 \
  cargo test --release -p riftri-storage --lib reports_batched_clone_comparison -- --ignored --nocapture
```

Use `1`/`8` for the small fixture and `128`/`128` for the large fixture. The
benchmark has no pass/fail timing threshold. The
[raw results](clone-worker-reuse-2026-10-09.json) include p50/p95 and per-round
candidate/baseline ratios; `clone-batches.test.mjs` recomputes these summaries.
They also retain a separate six-round medium smoke run after moving the
prototype behind `cfg(test)`, without pooling it into the table above.

The prototype keeps batches synchronous with traversal, joins workers before
returning, and creates no permanent pool or daemon. Tests cover worker reuse,
the 1,024-job retention bound, the partial tail, inline single-worker cases,
later-batch errors, and propagation of worker/producer panics without leaving
unfinished work behind. Real APFS cloning checks passed as part of the timed
comparison. Other filesystems and end-to-end creation still need separate
performance evidence before any runtime adoption.
