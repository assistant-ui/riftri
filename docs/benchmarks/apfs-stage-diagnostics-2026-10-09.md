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
Fresh Windows quality and ReFS jobs passed at `1dedd89`. That overall run
remained red because Docker Hub returned HTTP 429 while the unchanged
dependency-check action tried to obtain its pinned image. The dependency check
did not run; one targeted retry hit the same rate limit. It was not disabled or
counted as a pass. A separate local `cargo-deny 0.20.2` run passed advisories,
bans, licenses and sources; that does not replace the failed hosted job.

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

## Time-budget follow-up: rejected

A second prototype limited admission of optional hints to 50 ms, shared across
workers and starting lazily with the hint pass rather than during cloning.
It preserved the existing byte limits and did not cancel an already running
kernel call. This was not a hard operation deadline. A regression failed before
deadline enforcement; the deadline, shared-worker and lazy-clock tests passed
on the prototype, along with the same-size/restored-timestamp mutation test.

The [complete time-budget record](apfs-time-admission-2026-10-09.json) retains
its exact patch, hashes, all 65 samples and 16 batches. Against the existing
deferred candidate (not main), the parallel batch median was
**4,160.60 → 4,501.99 ms (8.21% slower)** and only **2/8** pairs were faster.
All correctness, complete initial scans, private-write, removal and final-GC
checks passed. The 6,888.55 ms candidate batch remains in the data. The prototype
was reverted; no time-budget policy is shipped.

Both scheduling reductions lost locally. The next lead is removing repeated
clone operations, not tuning another hint constant. A
[test-only writable bulk-clone evaluation](apfs-bulk-directory-clone-2026-09-13.md#writable-follow-up-october-9)
includes entry-type preflight, permission restoration and the same hints, and
measures the storage phase only. Its directory-metadata parity gate remains
open, and the follow-up platform-contract review finds that Apple discourages
direct directory cloning while recursive force-cloning is not supported by
the recommended copy API. It remains a diagnostic comparison, not the next
production implementation. Bounded directory-relative per-file cloning was
the next candidate measured below. The existing PR remains draft.

After reverting the prototype, formatting, all-target/all-feature Clippy and
the full system-Git-first Rust workspace passed (699 tests, 23 ignored).
Package verification passed 282 tests with four skips, including recomputation
of the retained summaries and byte-exact stack checksums. These correctness
results do not turn the losing prototype into a performance improvement.

After the time-budget experiment was reverted and the bulk helper remained
test-only, all local gates passed again: formatting, all-target/all-feature
Clippy, 702 Rust tests (24 ignored), and 284 package tests (four skipped).
A fresh ordinary release build reproduced the existing deferred candidate's
SHA-256 (`d3ace58ebedba2ae332c756e4c8b7573aa1d714ed1eca5fb8bcb4421844ac4c1`),
confirming these follow-ups did not change the CLI executable.

## Directory-handle reuse: rejected

This prototype kept strict per-file native cloning, but used `clonefileat`
with one reusable source/destination directory pair per worker. It dropped the
old pair before opening another, bounding retained handles independently of
tree depth or file count. Explicit directory creation, actual cloned-file
mode restoration, deferred hints, full base checks and Git checks were unchanged.

The [complete record](apfs-directory-handles-2026-10-09.json) retains its source
patch, executable hashes, all 65 observations and all 16 batches. Against the
existing deferred candidate, **not main**, eight alternating real-project
four-way pairs produced **3,503.14 → 4,075.73 ms (16.34% slower)**, with only
**2/8** faster pairs. No samples were excluded. Full content/mode/symlink
comparisons, 5,864-entry initial Git scans, private writes, clean removals and
final GC passed. These results do not identify the source of the slowdown or
resolve the earlier hosted timeout.

A worker-lifetime regression first failed with per-item state initialization.
Prototype checks cover state reuse, error-path joins and resource release,
directory-pair replacement, occupied destinations, Unicode names, preserved
native error paths, symlink-parent refusal, xattrs and private writes. An initial
invalid-UTF-8 creation fixture failed at setup because APFS rejected its name;
it was corrected to check Unicode success and byte-preserving error reporting.
The low-file-descriptor deep-tree regression also passed. The runtime prototype
and its implementation-specific tests were reverted: the additional lifetime
complexity is not justified by this losing comparison.

## Git refresh handoff: unconditional shortcut rejected

The next scan checked deferring the refresh inside `reset --mixed` to the
mandatory clean-status check, **not removing the content check**. Git documents
[`--no-refresh`](https://git-scm.com/docs/git-reset) and explains that
[`GIT_OPTIONAL_LOCKS=0`](https://git-scm.com/docs/git/2.50.0#Documentation/git.txt-GIT_OPTIONAL_LOCKS)
prevents status from persisting optional index updates. This suggested a risk
of shifting cost onto repeated status commands rather than eliminating it.

A [reproducible mechanism fixture](git-refresh-handoff.mjs) and its
[complete results](git-refresh-handoff-2026-10-09.json) confirm that risk on
installed Git 2.50.1. Each case starts with its own missing temporary index in
a disposable 257-file repository; old file modification times avoid a racy
timestamp fixture. The source repository's regular index is left alone.
Git Trace2 reports these content-scan counts:

| Reset mode | Optional index writes | Reset | First status | Repeated status |
| --- | --- | ---: | ---: | ---: |
| Current mixed reset | Disabled | 257 | 0 | 0 |
| Mixed reset, no refresh | Disabled | — | 257 | 257 |
| Current mixed reset | Enabled | 257 | 0 | 0 |
| Mixed reset, no refresh | Enabled | — | 257 | 0 |

The dash means reset emitted no refresh counter. Index checksums confirm that
status only persisted the initially missing stat data in the enabled-write
case; every status was clean. This is **not a startup benchmark or a native
COW lifecycle test**, and no latency gain is claimed. Unconditionally adding
the flag would make repeated status more expensive for a valid environment.
A future guarded experiment would still need version support, inherited
optional-lock handling, sparse behavior, FSMonitor, corruption detection and
recovery tests. No Git behavior is changed by this follow-up.

The latest ordinary CI at `de17971` again passed all jobs except the dependency
action, which hit Docker Hub HTTP 429 before its check ran. This does not change
the PR's failed performance evidence or its draft status.

After reverting directory handles, formatting, all-target/all-feature Clippy
and the full Rust workspace passed (702 tests, 24 ignored). The release binary
again matched `d3ace58ebedba2ae332c756e4c8b7573aa1d714ed1eca5fb8bcb4421844ac4c1`.
The final package suite passed 286 tests with four skips, including both new
evidence checks. The retained evidence is not a new production optimization.

## Size-selective hints: not adopted

The next experiment removes optional requests for regular files smaller than
4 KiB, while retaining the existing 64 KiB per-file and 64 MiB per-operation
caps. The pinned reference tree has 5,863 regular files, one empty. Based on
its sizes, the cutoff reduces eligible requests from 5,862 to 2,046 (65.10%)
and requested bytes from 35,139,308 to 29,790,921 (84.78% retained). These are
eligibility calculations, **not traced syscall counts or a measured speedup**.
The 8 KiB synthetic fixture remains eligible, so this does not simply disable
the feature on the workload that previously benefited.

The [build-only patch](apfs-large-file-hints.patch) retains the exact prototype
and its tests. The minimum-size regression failed before the change; the
patched storage suite and release build then passed locally. The measured
executable is `07909e07801f1e28690092bc5c5c4c53bc23874c854e3ceb978477d5ebc2ae09`.
Ordinary Rust source is restored to the existing deferred candidate. The patch
is applied only in disposable evaluation checkouts, never by the CLI.

The [complete local record](apfs-large-file-hints-2026-10-09.json) retains every
one of the 65 samples and all 16 batches. Eight alternating four-way pairs
against the prior deferred candidate, **not main**, produced a batch median of
**7,189.77 → 7,916.22 ms (10.10% slower)**, with **4/8** faster pairs. Median CPU
per cached view was **2.720 → 2.745 seconds**, not a CPU saving either. All
full content/mode/symlink, 5,864-entry Git-scan, private-write, clean-removal and
final-GC checks passed. No samples were excluded, including the 23.70-second
baseline batch and 22.70-second candidate batch. Fewer eligible syscalls did
not establish a whole-startup improvement.

Both variants had long delays before hints started; progress receipts include
journal persistence and do not isolate the cause. Both binaries were built
before timing, and no local build or other benchmark overlapped. Shortly before
building the candidate, disk pressure required Cargo to remove 4.4 GiB of this
task's rebuildable development artifacts; source, release binaries and prior
evidence were preserved. Host activity was not controlled. These are loaded-host
observations, not a universal slowdown estimate.

A CI comparison against main was prepared but not launched after this completed
result. The cutoff is removed from normal source; only the reproducible patch
and evidence are retained. It also leaves hints unchanged for 8 KiB files, so
it cannot be claimed to fix the older synthetic four-way regression. No merge
readiness or resolution of the earlier timeout follows from these checks.

The next scheduling lead is to overlap optional hints with Git's initial full
check after all native cloning and permission updates finish, then stop issuing
new hints when Git finishes. That could avoid making Git wait for the entire
hint pass. It has not been implemented or measured; a valid experiment must
join every admitted hint worker before cleanup, preserve error handling and
full checks, and account for an in-flight kernel call that cannot be cancelled.

The preceding ordinary CI at `39560b0` passed every job, including the dependency
check that previously hit Docker Hub's rate limit. That does not resolve the
older performance regression or timeout.

After reverting the cutoff, formatting, all-target/all-feature Clippy and the
full Rust workspace passed (702 tests, 24 ignored). The package suite passed
287 tests with four skips. The rebuilt release executable again matched
`d3ace58ebedba2ae332c756e4c8b7573aa1d714ed1eca5fb8bcb4421844ac4c1`, confirming
that this follow-up retains evidence without changing the candidate's runtime.

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

The refresh-handoff mechanism fixture is independent of the patched builds.
On macOS with Git supporting `--no-refresh`, give it a new absolute output path:

```sh
node docs/benchmarks/git-refresh-handoff.mjs /absolute/path/to/new-refresh-fixture
```

It creates its own repository and temporary indexes, not a user worktree.
