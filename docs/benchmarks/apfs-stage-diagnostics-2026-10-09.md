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
unmodified product source. The temporary job was removed after preserving its
completed observations below.

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
test passed on its own without code changes. A separate system-Git-first full
workspace run passed 699 tests (23 ignored); the original failure is not
reclassified as a pass. Formatting and all-target/all-feature Clippy passed,
as did all nine explicitly run diagnostic/process-group/APFS failure tests.

The longer clone stage in the enabled run is a useful lead, not a causal proof:
one process can start hints while another is still cloning, and host load can
change. Per-process deferral does not separate those phases across processes.
The independent runner provides the additional observations below. Do not skip
checks, relax durability, or raise the workload deadline to hide the problem.

## Hosted diagnosis

[Run 37988551532](https://github.com/assistant-ui/riftri/actions/runs/37988551532),
commit `533da94`, completed both diagnostic fixtures and detached the disposable
APFS volume without forcing. The [retained hosted JSON](apfs-stage-ci-2026-10-09.json)
contains every one of the 82 samples, all 20 completed batches, both binary
checksums, and all three native stack reports with their original checksums.
The real-project fixture ran six alternating pairs; synthetic ran four.
All initial scans, byte/mode/symlink comparisons, isolation, clean removal and
final-GC checks passed. No worker timed out. **This does not explain or resolve
the earlier candidate timeout.**

Median duration per cached view, in milliseconds (instrumented builds only):

| Stage | Real-project control | Real-project hints | Synthetic control | Synthetic hints |
| --- | ---: | ---: | ---: | ---: |
| Native file clones and modes | 1,322.94 | 967.87 | 678.16 | 597.22 |
| Read-ahead pass | 1.68 | 1,927.88 | 0.88 | 82.53 |
| Persist `ViewCreated` | 27.51 | 106.13 | 22.49 | 17.67 |
| Git initial index refresh | 2,420.81 | 1,237.58 | 598.67 | 264.60 |

The real-project hint pass reached 3,867.81 ms. Unlike the local observation,
native cloning was not slower in its hint-enabled samples. Prefetching reduced
the later Git read time, but imposed substantial work before it. Medians from
overlapping stages must not be added or subtracted to claim an end-to-end gain.

Synthetic round two had four **no-hint control** workers taking about 21.2
seconds. Git Trace2 measured their `reset` commands at 10.63–16.60 seconds;
individual journal transitions also reached 879.40 ms. The three captured
parent stacks contain `poll` and `fcntl` frames, with one `posix_spawn` frame.
Their Rust frames are stripped, they were captured near completion, and the
child Git processes were not sampled. They do not identify the cause of the
earlier long candidate stall, and `fcntl` here cannot be called evidence of
`F_RDADVISE`: these builds had hints disabled. The complete stack text is
retained, not just a selected frame.

The diagnostic job passed, but the **overall CI run failed** its Windows
package checksum test. Git converted the newly added `.patch` files to CRLF;
recreating that conversion reproduced the exact failing hash. `.gitattributes`
now pins patch files to LF, preserving the original artifact checksums rather
than weakening their checks. A real `core.autocrlf=true` checkout fixture now
reproduces the old hash and preserves the exact original bytes with the fix.
This correction still requires fresh Windows CI.

## Single-issuer follow-up: rejected

The next experiment kept native cloning parallel but issued deferred hints from
one caller thread. Its thread-identity regression failed before the change and
passed on the prototype. Both uninstrumented release binaries were built before
the comparison; the baseline was the current deferred multi-issuer candidate,
**not main**. The [single-issuer record](apfs-single-issuer-2026-10-09.json)
retains the exact prototype patch, binary hashes, every one of the 65 samples
and all 16 batches.

Eight alternating four-way pairs on the same real-project tree produced a
batch median of **3,321.75 → 3,604.82 ms (8.52% slower)**, with only **2/8**
faster pairs. The 6,343.99 ms candidate batch is retained. All full content,
initial Git scans, private-write checks, clean removals and final GC passed.
The prototype was **reverted**, including its implementation-specific test;
it does not justify changing shipped scheduling. No extra hosted evaluation
was spent on this locally losing variant.

The remaining question is whether optional prefetch issuance can stop early
under slow I/O, instead of relying only on a byte budget. That needs a separate
bounded experiment: a wall-time admission limit cannot interrupt an already
running kernel call and must not be represented as a hard operation deadline.
No such runtime limit is implemented here. The existing PR remains draft.

After reverting the prototype, formatting, all-target/all-feature Clippy and
the full system-Git-first Rust workspace passed (699 tests, 23 ignored).
Package verification passed 282 tests with four skips, including recomputation
of the retained summaries and byte-exact stack checksums. These correctness
results do not turn the losing prototype into a performance improvement.

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
