# Checkout configuration batching

## Scope

This change replaces eleven individual checkout-configuration reads with one
allowlisted, NUL-delimited `git config --get-regexp` call. Git still resolves
configuration files, includes, overrides, and precedence. The reader retains
the last occurrence of each requested key, matching the previous `--get`
behavior, and preserves raw value bytes. Missing keys remain distinct from
present empty values. It deliberately accepts only simple `section.variable`
keys; existing single-key APIs continue to support subsection keys.

The result exists only within the current compatibility analysis. No persistent
configuration cache is introduced. The repository-local consent check, both
attribute inspections, checkout-profile input ordering, Git index initialization,
native clone operations, integrity verification, locks, and durability journals
are unchanged.

Git documents the relevant precedence and NUL-delimited output behavior in
[git-config](https://git-scm.com/docs/git-config).

## Safety evidence

- A process-count regression test first failed at eleven processes and passes at
  one after batching.
- Differential tests compare the batch reader against the original single-key
  reader for duplicate/case-varied keys, missing/empty/implicit/false values,
  multiline and non-UTF-8 bytes, global/local/worktree configuration, included
  files, environment overrides, and command-line overrides.
- Tests reject malformed output, invalid selectors, and Git failures, and prove
  that later calls see updated settings. Global enablement does not become
  repository-local consent.
- A core regression test reconstructs the old checkout profile using individual
  reads, checks identical profile hashes and captured configuration ordering,
  and verifies that unsupported checkout settings still produce blockers before
  lifecycle state is created.
- Local validation: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
  `cargo test --workspace` pass, as does a serial full-suite run. Earlier test attempts hit
  host disk exhaustion; the successful full rerun completed after space became
  available. No test or safety gate was disabled to obtain a pass.

## Measured results

The isolated configuration-read benchmark ran 30 alternating pairs and compared
identical answers on every iteration. Median time for all eleven settings fell
from **111.7 ms to 10.1 ms (91.0% lower)**. Each pair verified eleven Git process
starts for the individual reader and one for the batch reader.

The end-to-end Farm.js results are more modest and noisy:

| Warm creation | Before median | After median | Median reduction | Git starts before/after |
| --- | ---: | ---: | ---: | ---: |
| Explicit, eight runs each | 1.816 s | 1.711 s | 5.8% | 30 / 20 |
| Process-scoped, eight runs each | 1.917 s | 1.868 s | 2.6% | 37 / 27 |

The new binary was faster in six of eight explicit pairs and seven of eight
process-scoped pairs. There was also a 4.077 s explicit post-change sample:
**the arithmetic mean of explicit creation did not improve** (1.839 s before,
2.048 s after). That sample is retained in the evidence, not excluded.
Process-scoped means were 2.232 s before and 1.996 s after.

Concurrent results were mixed: the first nine-way pair was 4.493 s before and
4.692 s after; the second was 4.645 s before and 4.108 s after. These observations
do not establish a consistent concurrency speedup. The defensible conclusion is
that the configuration-reading step is substantially cheaper, while total
creation remains sensitive to other work and host conditions.

All **69 worktrees** in the completed experiment passed verification and normal
removal. Both binaries reused the same baseline-created base. Four concurrent
groups passed private-write isolation checks, and final status reported zero
active views, retained bases, pending adds/removals, and state issues.

[Raw measurements](checkout-config-batching-2026-09-13.json) include every sample
from the completed run and the configuration-only benchmark. An earlier pilot
stopped because the harness expected the shim to print the explicit command's
stdout format; its views were normally removed, its base was collected, and the
corrected full run used a fresh fixture. Pilot timings are not pooled here.

## Benchmark method

The [manual harness](checkout-config-batching.mjs) compares optimized binaries
from main commit `a71f8a8f00b8c9c008581cef071d388191e90673` and this change on
ordinary host APFS. It uses an independent snapshot of Farm.js commit
`ab7b0184985f8bd742f0d4636961ec90c355cbd9`, exact tree
`aa5966b43f429773a941fd65dcbbce8bf2575a5a`: 2,005 tracked files and
13,781,534 logical bytes. The source repository is only read, never used as the
fixture's mutable worktree or Git metadata directory.

Both versions reuse the same baseline-created immutable base. Each version
performs eight warm explicit creations, eight warm process-scoped creations,
and two batches of nine concurrent process-scoped creations. Version order
alternates; initial base creation, verification, private writes, and cleanup are
excluded from single-create timings. Concurrent batch timing includes the
harness's small result-recording overhead. Git Trace2 is enabled for both
binaries; its process counts provide a deterministic check separate from timing.

Every created view must have a clean Git status and match the fixture's complete
file-content hashes, executable bits, and symlink targets. Concurrent groups
also exercise private edits, verify untouched peers and the base, restore the
edited file, and remove views normally. Garbage collection must finish with
zero active views, retained bases, pending adds/removals, and state issues.

Timing results are local observations, not CI thresholds or universal speedup
guarantees. In particular, two concurrent batches per version are not sufficient
to establish a reliable concurrency speedup. This experiment does not remeasure
exclusive physical allocation or eliminate the separate index-refresh bottleneck.

## Reproduce

Run the configuration-only comparison in release mode:

```console
cargo test -p riftri-git --release reports_batched_configuration_read_latency \
  -- --ignored --nocapture
```

Build release binaries from the baseline and this change, preserve both under
distinct paths, and run on macOS/APFS with sufficient free space and no competing
build/test workload:

```console
node docs/benchmarks/checkout-config-batching.mjs \
  /absolute/path/to/riftri-before /absolute/path/to/riftri-after \
  /absolute/path/to/farm.js ab7b0184985f8bd742f0d4636961ec90c355cbd9 \
  /absolute/path/to/new-benchmark-output
```

The output directory must not already exist. The harness records per-case logs,
Git Trace2 events, `results.json`, and final storage status. It removes all created
views normally on success, but leaves the independent fixture and evidence for
inspection. If interrupted or a check fails, preserve the fixture for diagnosis
and use normal Riftri recovery/removal; do not force-delete managed views.
