# APFS deferred read-ahead evaluation, 2026-10-09

Separating read-ahead from file cloning improved cached creation in local
serial and four-way tests, without removing Git content checks. **This is
not yet a merge recommendation:** independent CI and higher-concurrency
evidence are still pending.

This follows the [original inline experiment](apfs-read-ahead-2026-10-09.md),
which regressed four-way creation on a hosted runner. Those negative results
and the unsuccessful budget variants remain recorded; this is a different
scheduling approach, not a reinterpretation of their results.

## Change

The old candidate issued optional read-ahead immediately after each file clone
and permission update. The new candidate finishes **all** file clones first,
then runs a separate bounded pass of optional hints. A failed clone phase
issues no hints. Both passes join before returning. The same worker limit,
64 KiB/file and 64 MiB/operation request caps, no-follow regular-file handle
checks, and best-effort error handling remain in place.

File lengths come from the existing permissions stat, retained in each clone
job. There is no additional metadata walk to obtain lengths. There is a second
worker-team startup, an atomic length per job and a temporary vector of job
references; memory still scales with file count. Hint calls still open and
inspect descriptors. The request budget is not a kernel page-cache memory cap
and does not coordinate different creator processes.

Git's full initial content check, immutable-base verification, final clean
check, permissions, COW isolation, journals, rollback and recovery are unchanged.
There is no daemon, Git configuration change, new user option or persisted
format. Linux and Windows are unchanged.

## Local paired results

Baseline `a58006d7c7b4989a055a967e65d5654281b77ea4` versus the release-built
runtime committed as `f6b32a4fca40136d4548c60e2180d6b12dd42d9b`.
Apple M1, 16 GiB RAM, Darwin 25.2.0/APFS, Apple Git 2.50.1. The host had
changing background load, swap use and limited free disk space; it was not
a controlled quiet-host experiment. No build or download was deliberately
run alongside the three final-candidate cached comparisons below.

Eight alternating-order pairs per workload. Serial values are whole-add
medians; four-way values measure the whole batch until the last child exits.
These compare **Riftri against Riftri, not plain Git**.

| Cached fixture | Baseline | Deferred hints | Reduction | Faster pairs |
| --- | ---: | ---: | ---: | ---: |
| 4,096 × 8 KiB files + symlink | 1,625.11 ms | 841.39 ms | 48.23% | 8/8 |
| assistant-ui source, serial | 3,153.39 ms | 2,016.55 ms | 36.05% | 8/8 |
| assistant-ui source, four simultaneous views | 5,110.79 ms | 3,786.24 ms | 25.92% | 8/8 |
| 32 × 8 KiB files + symlink | 278.29 ms | 271.89 ms | 2.30% | 6/8 |
| 64 × 1 MiB files + symlink | 914.37 ms | 857.34 ms | 6.24% | 7/8 |

The tiny-tree difference is small and does not establish a meaningful speedup;
its clone-and-hint phase rose by about 1.8 ms. These follow-up runs completed
after the workflow comparison and package tests, without an overlapping build.

The reference is an exact committed archive of
[assistant-ui commit 038cd9f](https://github.com/assistant-ui/assistant-ui/commit/038cd9f82b418afe9e6d0080648738f78586fbca),
tree `c4de7922b24126e860cb77652f5d080e04c8c396`: 5,864 tracked entries,
73,235,115 regular-file bytes, plus a 15-byte symlink target. The harness checks
the reproduced tree ID. No dependencies or build output are included.
The source repository is read only; all views belong to disposable fixtures.
This is not a T3 Code compatibility or performance result.

On the four-way reference, median initial Git refresh per view fell from
2,884.34 ms to 1,077.52 ms. The combined clone-and-hint phase grew from
1,057.72 ms to 1,275.71 ms. **Every refresh still scanned all 5,864 entries.**
Reduced elapsed time is not reduced total CPU: median user+system CPU per add
rose from 2.265 s to 2.360 s there; synthetic serial rose from 1.130 s to
1.160 s, and reference serial from 2.125 s to 2.185 s. Page-cache residency
and system-wide resource use were not measured.

## Retained exploratory evidence

A prototype before helper extraction and the final tests won all eight
synthetic four-way pairs: 2,260.98 ms → 1,665.49 ms (26.34% lower median).
Its binary differs from the final candidate; its exact source patch and hash
are retained separately. It is supporting evidence, not a final-binary repeat.

The prototype serial run overlapped the reference repository's Git fetch.
**That run is confounded and excluded from headline evidence**, even though
its samples passed correctness checks. Its full 17 samples are retained,
along with every other sample and outlier, instead of silently dropping it.

The [local measurement JSON](apfs-deferred-read-ahead-2026-10-09.json) contains
all 215 samples and 112 timed batches across seven runs, binary and raw-receipt
checksums, host load, CPU, phase timing and initial scan counts. Cold baseline
anchor samples are retained but are not a cold before/after comparison.

## Cold creation, Git-shim and plain-Git reference

A second local run used the same final binaries and exact assistant-ui tree
with [the existing workflow harness](checkout-config-batching.mjs). It ran
after the earlier measurements, with a system-only PATH so plain Git did not
accidentally invoke the user's installed shim. Eight alternating pairs cover
empty-base-cache and cached serial creation; four pairs cover four-way batches.
No concurrent local build or download was launched during these measurements.

| Workflow | Baseline median | Candidate median | Reduction | Faster pairs |
| --- | ---: | ---: | ---: | ---: |
| Cold base, explicit | 4.090 s | 2.989 s | 26.92% | 7/8 |
| Cached, explicit | 3.076 s | 2.069 s | 32.74% | 8/8 |
| Cached, Git shim | 3.098 s | 2.208 s | 28.71% | 8/8 |
| Four simultaneous, explicit | 5.243 s | 3.487 s | 33.49% | 4/4 |
| Four simultaneous, Git shim | 4.828 s | 3.601 s | 25.42% | 3/4 |

Cold means the immutable base was absent, **not** that the OS cache was purged.
The four-way shim's final paired round was slower: 4.555 s versus 4.327 s.
The lower median does not erase that loss or guarantee faster concurrent adds.

**Plain Git remained faster:** 1.326 s serial and 2.323 s for a four-view batch.
The revised explicit Riftri median therefore still adds about 0.743 s serial
and 1.163 s per four-way batch here. This experiment reduces Riftri's startup
overhead; it does not show that Riftri creates worktrees faster than Git.

All 137 views passed full tracked byte/mode/symlink and clean-status checks and
were removed. Batch private writes left peers and the base unchanged; final
status showed no active views, retained bases, pending operations or issues.
All 137 cases and 20 batches, including outliers and the slower pair, are
retained in the [workflow measurements](apfs-deferred-workflows-2026-10-09.json).
The tables use ordinary midpoint medians; the original harness's `p50` uses
nearest-rank selection. Host-volume allocation deltas are retained but are
not dedicated-volume physical-sharing evidence.

## Safety and verification

Every completed run checked all tracked bytes, executable bits and symlink
targets against the source and base; clean Git status and HEAD; private writes
without changing the anchor or base; clean removal; and final GC leaving zero
active views, bases and diagnostic issues. The harness waits for every worker
and kills timed-out process groups before reporting failure.

Two new tests failed with inline scheduling and passed with deferred scheduling:
no hint before every clone is writable, and no hint after a clone-phase failure.
A third checks that nonwritable clones do not issue hints. Existing tests cover
budget bounds, unsafe entry types and same-size/restored-mtime edits before Git
validation; those edits remain detected and preserved during recovery.

The final runtime passed formatting, all-target/all-feature Clippy and the
workspace test suite: 699 passed, zero failed, 23 ignored. All ordinary jobs in
[CI run 37983257049](https://github.com/assistant-ui/riftri/actions/runs/37983257049)
passed, including cross-platform quality and real-filesystem integration.
Its separate performance job has a failed assistant-ui four-way case; the
logs and remaining workloads are still pending. Do not merge on green quality
checks alone.

The same run's `native-cow-benchmark-macos-apfs` artifact reports a dedicated
APFS volume growing by only **49,152 bytes** for a cached 32 MiB payload view;
a 4 MiB private write grew it by 4,194,304 bytes. This debug-build storage check
supports continued physical sharing, not a release-build latency claim.

## Reproduce

Use [apfs-read-ahead.mjs](apfs-read-ahead.mjs), with separately built binaries
and a new output directory. For the reference fixture, provide a repository
whose HEAD is the pinned commit above. The harness archives it without creating
worktrees in that source repository.

```sh
node docs/benchmarks/apfs-read-ahead.mjs BASELINE NEW_OUTPUT 4096 8 CANDIDATE 8192 - 1
node docs/benchmarks/apfs-read-ahead.mjs BASELINE NEW_OUTPUT 4096 8 CANDIDATE 8192 REFERENCE_REPO 4
```

Read the JSON's `complete` and failure fields before using any timings. Never
turn a partial or failed concurrent batch into a successful comparison.
