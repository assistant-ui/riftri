# APFS bounded read-ahead evaluation, 2026-10-09

Historical inline-hint candidate and budget experiments. The current follow-up
uses [deferred scheduling](apfs-deferred-read-ahead-2026-10-09.md); the results
below remain evidence about the earlier variants, not the revised runtime.
Progress-derived clone-phase intervals also include the following durable
journal transition; they are not isolated native-clone or hint syscall timings.

Best-effort read-ahead improved **serial cached creation** on the measured
many-file fixtures while retaining full Git validation. **Keep this candidate
in draft:** a separate hosted-runner repeat found an approximately 11% slower
four-way concurrent median. The serial result is not a universal creation-speed
claim, and the current request budget is not ready for default adoption.

## Approach and safety boundary

An initial phase trace of 4,096 regular files plus one symlink attributed roughly
half of cached creation time to Git's initial index/content check. Sampling Git
also found substantial time inside file reads, not just hashing. The experiment
asks macOS to prepare those reads during native clone work, before Git needs
them serially.

After restoring a cloned regular file's permissions, a worker issues
[`F_RDADVISE`](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/fcntl.2),
an asynchronous advisory read with no userspace copy. It requests at most the
first 64 KiB of each file and 64 MiB total across one clone operation. The
operation-local atomic budget cannot be overdrawn by concurrent workers. Empty
files and exhausted budgets open no additional handles.

Each hint uses a read-only, no-follow, nonblocking handle, checks that the opened
entry is a regular file, and closes it before the worker's next file. Failed or
unsupported hints are ignored. This adds an open, descriptor metadata check,
hint, and close for each admitted file; it is not free CPU work. The request
budget is **not a bound on kernel page-cache residency** or on the combined
requests of multiple creator processes.

Base hashing, Git's index/content checks, final clean verification, COW cloning,
permissions, journal synchronization, and recovery remain unchanged. No Git
configuration, daemon, user-facing option, cache identity, or persisted format
changes. Linux and Windows are unchanged. This is not whole-directory cloning
and does not copy file data into a new allocation.

## Method

- Baseline: `a58006d7c7b4989a055a967e65d5654281b77ea4`; candidate: this change.
  Both were built with `cargo build --release --locked -p riftri-cli`.
  Binary SHA-256 values are recorded in the [measurement JSON](apfs-read-ahead-2026-10-09.json).
- Apple M1, 16 GiB RAM, host APFS, Darwin 25.2.0, Apple Git 2.50.1.
  The machine had significant swap use and changing load. No benchmark or build
  was deliberately run alongside a timed comparison, but unrelated host work
  was not controlled.
- One cold baseline add retains an anchor and warms the exact-tree base. Cached
  pairs alternate baseline/candidate order. Eight pairs per serial fixture;
  four pairs of four simultaneous adds for the concurrent fixture. The cold
  warm-up is retained in the data but is not a cold before/after comparison.
- Timings cover the entire CLI add. Stderr progress and Git Trace2 attribute
  phases and scans; `/usr/bin/time -l` records child resource use. Concurrent
  batch timing ends after the last child exits, before content verification and
  cleanup. All launched workers settle before any failure is reported.
- After every successful add: check clean Git status and HEAD, compare every
  tracked file's bytes, executable bits, and symlink target against the fixture
  and base; make a private write, check it is dirty and does not affect the
  anchor/base, restore it, and remove the clean view. Final GC leaves no active
  views, bases, or diagnostic issues. Failures retain their fixture for
  inspection instead of force-cleaning it.
- The real-source fixture is a committed archive of Riftri at the baseline
  revision: 313 tracked files and 4,710,420 logical bytes. It does not include
  dependencies or build output. It is not a T3 Code or assistant-ui measurement.
- All timed samples and outliers are retained. The initial prototype lacked the
  final descriptor metadata check; its separate eight-pair result is recorded
  as exploratory and is not used in the table below.

## Results

Median whole-add time, in milliseconds; concurrent rows use whole-batch time.
Percentages compare medians, not a guaranteed per-add reduction.

| Fixture | Baseline | Read-ahead | Difference | Faster paired rounds |
| --- | ---: | ---: | ---: | ---: |
| 4,096 × 8 KiB files + symlink | 1,987.45 | 990.66 | 50.15% lower | 8/8 |
| Riftri committed source tree | 547.93 | 410.81 | 25.02% lower | 8/8 |
| 64 × 1 MiB files + symlink | 841.70 | 789.58 | 6.19% lower | 7/8 |
| 32 × 8 KiB files + symlink | 269.48 | 265.55 | 1.46% lower | 6/8 |
| Four concurrent 4,097-entry views | 2,661.77 | 2,134.91 | 19.79% lower median; mixed pairs | 2/4 |

On the hardened 4,097-entry fixture, median Git `index/refresh` fell from
1,124.87 ms to 162.62 ms. **Every initial refresh still scanned all 4,097 entries.**
The improvement is not a skipped check or work merely moved to final status;
the whole-add measurement includes the extra read-ahead work.

Resource results are workload-dependent. Median user+system CPU per add was
1.240 s versus 1.135 s on that many-file repeat, 0.270 s versus 0.280 s on Riftri,
and 1.170 s versus 1.235 s in the concurrent run. These measurements do not
establish a universal CPU saving. Child RSS is recorded, but does not include
the kernel cache populated by hints.

Concurrent candidate batches were 2.241, 1.763, 4.374, and 2.029 seconds versus
2.067, 2.395, 2.928, and 3.533 seconds for their paired baselines. The two losses
are material. **Do not present the lower concurrent median as an established
concurrent speedup.** Likewise, the tiny-tree difference is too small to support
an adoption claim on this host.

## Independent CI repeat: do not merge yet

[CI run 37975831382](https://github.com/assistant-ui/riftri/actions/runs/37975831382)
compared the same baseline with candidate `ce68f1a`, using release binaries on
a `macos-15` Apple M1 virtual runner with 7 GiB RAM and Apple Git 2.39.5. Each
fixture used a dedicated 4 GiB APFS sparsebundle. This separates fixture state,
not the hosted runner's underlying I/O. All ordinary platform quality and
filesystem integration checks passed; the separate evaluation job failed.

| Cached fixture | Baseline median | Read-ahead median | Difference | Faster paired rounds |
| --- | ---: | ---: | ---: | ---: |
| 4,096 × 8 KiB files + symlink | 1,604.30 ms | 1,399.92 ms | 12.74% lower | 6/8 |
| Four simultaneous views of that tree | 5,480.65 ms | 6,070.56 ms | 10.76% higher | 2/4 |
| Ten simultaneous views | Incomplete | Not run | No valid comparison | None |

Read-ahead reduced the four-way median Git refresh from 2,717.43 ms to
300.84 ms per view, but increased the clone phase from 1,212.47 ms to
4,508.85 ms. Median CPU per add also rose from 1.595 s to 1.765 s.
This is evidence that the added read requests can cost more during cloning
than they save later. It does not justify skipping either integrity check.

The complete four-way baseline includes a 44.68-second batch outlier; it has
not been discarded. In the first ten-way **baseline** batch, five time wrappers
reached the harness's 120-second timeout. Five other workers produced complete
samples, including an 89.61-second sample. This is an incomplete failed batch,
not a ten-way baseline median. Candidate ten-way timing, both assistant-ui
reference runs, and the T3 eligibility diagnostic never ran.

Failed-worker logs contain later successful Riftri receipts, but no time-wrapper
resource receipt. The harness timed out the wrapper without explicitly stopping
its child process group, and did not store the failed samples' signal or elapsed
time. Preserve this limitation; neither success receipts nor the surviving
samples turn the failed batch into a valid result. No successful cleanup is
claimed for that fixture. The disposable volume was detached without forcing.

The harness now uses a separate POSIX process group, sends TERM on timeout and
then KILL after a bounded grace period, including when the wrapper has already
closed its pipes. A regression that failed with the old timeout proves that a
signal-ignoring child stops before the result is returned. A real APFS test
also verifies that both failed concurrent workers keep their elapsed time,
exit status, signal and failure flag without creating a successful batch.
These fixes do not retroactively repair the incomplete CI measurements.

The [retained CI measurements](apfs-read-ahead-ci-2026-10-09.json) include every
recorded sample, paired batch, failure and source-file checksum. The linked
[original run's artifact](https://github.com/assistant-ui/riftri/actions/runs/37975831382)
named `apfs-read-ahead-paired-evaluation` contains the original receipts, full
Git traces and all worker logs (artifact downloads require GitHub sign-in). The
temporary evaluation job was removed after retaining its evidence; it must not
add benchmark cost to unrelated pull requests.

Follow-ups, in order:

1. Repeat with the corrected timeout handling. Run real-source and eligibility
   cases independently so one failed stress case cannot hide them.
2. Investigate separating hint issuance from cloning. Smaller budgets and
   fewer simultaneous issuing workers were evaluated below; neither established
   a concurrent improvement. No tested setting is ready for default adoption.
3. Add paired cold-creation cases and a plain-Git reference. Do not advertise
   current T3 support: its pinned tree contains gitlinks that Riftri refuses;
   removing those entries would measure a different repository.

## Follow-up budget experiments: still no concurrent winner

Two release-built prototypes based on `3a2e2e2` were compared with `a58006d`
on the local Apple M1 host. Both use the corrected process-group harness and
the same 4,097-entry synthetic fixture. Eight alternating pairs cover serial
creation; four pairs cover four simultaneous creates. The runtime source was
restored after building each prototype; **neither is enabled in this PR**.

- **8 MiB budget:** reduce only the per-operation request cap from 64 MiB to
  8 MiB; leave the 64 KiB per-file cap unchanged.
- **Single issuing worker:** keep the 64 MiB cap, but use a nonblocking atomic
  admission flag around `advise`. If another worker is issuing a hint, skip the
  optional hint and keep cloning. This limits issuing calls, not outstanding
  asynchronous kernel reads, and coordinates only workers in one process.

| Prototype / workload | Baseline median | Candidate median | Difference | Faster pairs |
| --- | ---: | ---: | ---: | ---: |
| 8 MiB / serial | 1,247.08 ms | 1,120.01 ms | 10.19% lower | 7/8 |
| 8 MiB / four simultaneous | 2,000.63 ms | 1,988.49 ms | 0.61% lower; inconclusive | 2/4 |
| Single issuing worker / serial | 1,723.37 ms | 1,093.10 ms | 36.57% lower | 8/8 |
| Single issuing worker / four simultaneous | 2,236.95 ms | 2,332.66 ms | 4.28% higher | 2/4 |

Every view passed byte/mode/symlink, Git cleanliness, private-write isolation,
removal and final-GC checks. Every initial Git refresh still scanned all 4,097
entries. All 100 samples, including cold baseline warm-ups, and all 48 timed
batches are retained in the [budget experiment JSON](apfs-read-ahead-budgets-2026-10-09.json),
with binary checksums, exact prototype descriptions and source receipt hashes.
The changing baseline times demonstrate why these are paired local experiments,
not comparisons between separate runs or proof of a quiet-host speedup. Neither
the near-zero budget-only median change nor the single-worker serial win proves
that either prototype should be adopted for concurrent agent creation.

## Verification and follow-ups

Unit tests cover per-file and concurrent per-tree limits, empty files, failed
hints, Unicode/native paths, unchanged bytes/modes/allocation, and refusal to
follow symlinks or block on FIFOs. A real APFS integration test changes a file
after cloning/read-ahead but before index creation, preserving its size and
mtime: Git detects the edit, add refuses activation, and repeated recovery
preserves the changed view while its anchor and base remain untouched.

The full local gate run passed formatting, all-target/all-feature Clippy, 696
Rust tests (23 ignored), and 268 npm tests (4 skipped). A separate explicitly
run allocation integration test on a dedicated 1 GiB APFS sparsebundle passed:
a cached 32 MiB payload view grew the volume by **53,248 bytes**, and a 4 MiB
private write grew it by 4,177,920 bytes. The plain `st_blocks`-derived view
accounting still includes shared blocks; the isolated volume delta is the
sharing evidence. This debug-build correctness run is not a latency comparison.

Before claiming broad startup improvements, repeat on a quiet host with ample
free space and on real T3 Code/assistant-ui trees; compare cold creation,
four/ten simultaneous adds, and plain Git. Investigate concurrent I/O and
page-cache pressure rather than removing integrity checks. FSMonitor remains a
separate optimization for later Git commands, not a substitute for initial
validation.

## Reproduce

Use the [manual harness](apfs-read-ahead.mjs) on macOS/APFS with separately built
baseline and candidate binaries. Supply a **new** output directory; the harness
refuses an existing one and preserves unexpected state.

```console
node docs/benchmarks/apfs-read-ahead.mjs /absolute/baseline /new/output 4096 8 /absolute/candidate
node docs/benchmarks/apfs-read-ahead.mjs /absolute/baseline /new/real-output 4096 8 /absolute/candidate 8192 /path/to/source-repository
node docs/benchmarks/apfs-read-ahead.mjs /absolute/baseline /new/concurrent-output 4096 4 /absolute/candidate 8192 - 4
```

The source repository is only read; its committed HEAD is archived into the
isolated fixture. The harness rejects archives whose attributes change the
source tree, and its manual manifest requires UTF-8 paths. Product low-level
APIs continue to accept native paths. Detailed phase logs, Git Trace2 output,
resource lines, and the complete result receipt remain in the output directory.
