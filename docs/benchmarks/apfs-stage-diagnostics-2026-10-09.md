# APFS stage diagnosis, 2026-10-09

The [deferred read-ahead candidate](apfs-deferred-read-ahead-2026-10-09.md)
remains unready: hosted concurrent regressions and candidate timeouts are not
resolved. This follow-up separates costs that ordinary progress receipts group
together. It is a diagnostic experiment, **not another speedup claim**.

## Instrumentation, not shipped behavior

The [diagnostic patch](apfs-stage-diagnostic.patch), based on `0463503`, prints
start/end events around directory layout, file cloning plus permission updates,
read-ahead, directory modes and each journal `advance`. Journal timing still
includes serialization, file sync, atomic replacement and directory sync; none
of those operations is removed. A [second patch](apfs-stage-control.patch)
switches off optional hints in a separately built control. That control retains
the same helper, worker passes and instrumentation; it is not unmodified main.

The production Rust files do **not** contain these probes. The one-off CI job
applies the patches only in its disposable checkout, builds both executables
before observing either, and never installs them. Normal quality jobs test the
unmodified product source. Remove the temporary job after preserving its data.

`RIFTRI_BENCH_STAGE_DIAGNOSTICS=1` enables an observer in the manual benchmark,
not in Riftri. The temporary build emits its own PID. After 15 seconds, the
observer rechecks the executable path using `ps` before attempting a one-second
macOS stack sample. It never searches globally by process name. Sampling has a
10-second process-group timeout, cannot overlap itself for one worker, and is
awaited before that worker's observation finishes. Capture errors are retained;
they do not turn a failed Riftri add into a success. The original 120-second
workload timeout and process-group termination remain intact.

Sampling and logging can perturb timings. Reports mark `stageDiagnostics`, and
the retained raw logs include every start/end event and any stack receipts.
Neither this mode nor its no-hint control should be used for headline latency
comparisons. Native stack capture may fail because of platform permissions;
that is an unavailable observation, not evidence that a process was not stuck.

## Local smoke diagnosis

Four alternating pairs of four simultaneous adds used the same pinned
assistant-ui tree (`038cd9f`, 5,864 entries) as the earlier evaluation. Both
instrumented binaries were release-built before timing. The full 33 samples
(including the initial anchor) and all eight batches are in the
[local diagnostic JSON](apfs-stage-local-2026-10-09.json), with source-patch,
binary and raw-receipt checksums.

Median stage duration per cached view:

| Stage | No-hint control | Hints enabled |
| --- | ---: | ---: |
| Directory layout | 244.14 ms | 253.43 ms |
| Native file clones and modes | 815.65 ms | 1,326.84 ms |
| Read-ahead pass | 0.70 ms | 268.90 ms |
| Persist `ViewCreated` | 30.61 ms | 21.08 ms |

The enabled hint pass reached 1,784.68 ms in one view; `ViewCreated` persistence
reached 195.34 ms. No worker reached the stack-observation threshold and no
timeout reproduced locally. All bytes, executable bits, symlinks, Git initial
scans, clean states, private-write isolation, removals and final-GC checks passed.
The native stack-capture helper was tested separately against an owned child.

The local verification run also exposed an intermittent fixture-launch failure:
`parallel_views_share_one_base_and_keep_commits_isolated` received OS error 22
(`Invalid argument`) from `Command::output` while launching `git`. That run used
the installed Riftri shim first in PATH and overlapped package tests; it does
not identify which of those conditions, if either, caused the error. The exact
test passed on its own without code changes. A full system-Git-first run is
being checked separately; the original failure is not reclassified as a pass.

The longer clone stage in the enabled run is a useful lead, not a causal proof:
one process can start hints while another is still cloning, and host load can
change. Per-process deferral does not separate those phases across processes.
The independent runner is needed to distinguish clone/hint contention from
journal persistence in the slow workload. Do not skip checks, relax durability,
or raise the workload deadline to hide the problem.

## Reproduce safely

Use a disposable checkout at the report's source revision. Apply the diagnostic
patch, build and copy the first binary aside, apply the control patch and build
the second. Never install either executable or apply these patches over work
you need to preserve. Run the sampler tests explicitly on macOS:

```sh
RIFTRI_BENCH_STAGE_SAMPLE_TEST=1 node --test docs/benchmarks/stage-sampler.test.mjs
RIFTRI_BENCH_STAGE_DIAGNOSTICS=1 node docs/benchmarks/apfs-read-ahead.mjs CONTROL NEW_OUTPUT 4096 4 HINTS 8192 REFERENCE_REPO 4
```

Use a new output directory. Inspect `complete`, `failure`, per-worker outcomes
and `stackSamples` before drawing any conclusion. Failed fixtures remain for
inspection; this diagnostic never force-cleans a changed or unfinished view.
