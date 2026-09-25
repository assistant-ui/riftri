# assistant-ui cached creation baseline

## Scope

This is the baseline [#211](https://github.com/assistant-ui/riftri/issues/211)
asks for before any cached-creation optimization: what a cached add costs today,
serially and with ten created at once, on a real repository rather than a
synthetic payload. It is **not** a comparison between two versions and contains
no claimed improvement.

Fixture: an independent, exact-tree export of assistant-ui commit
`038cd9f82b418afe9e6d0080648738f78586fbca`, tree
`c4de7922b24126e860cb77652f5d080e04c8c396` — 5,864 tracked files and 73,235,130
logical bytes. The export was produced with `git archive` and its tree hash
matches the source tree exactly; the assistant-ui repository and its working
files were not modified.

Release `riftri` 0.5.0 ran on ordinary host APFS, macOS (Darwin 25.2.0), arm64,
full checkout profile, `apfs-clone` backend. Three runs, each one cold add, five
serial cached adds, and one batch of ten concurrent cached adds. **All samples
are retained, including outliers**, and the volume was not quiet — other work
was running on the machine throughout.

## The unmodified repository is now accepted

This matters for reading the numbers below, and it closes a limitation the
earlier record left open.

[`assistant-ui-ten-agents-2026-09-12.md`](assistant-ui-ten-agents-2026-09-12.md)
had to **remove** `pnpm-lock.yaml linguist-generated` from `.gitattributes`
before Riftri would accept the repository, ran against an adjusted tree
`faf5d0cac6b381500f53677876ab7a4191260f9c`, and stated plainly that its results
"do not certify the unmodified repository as compatible."

This run needed no adjustment. The fixture's tree hash
`c4de7922b24126e860cb77652f5d080e04c8c396` is the source tree exactly,
`linguist-generated` and all, and the optimized add succeeded with a clean Git
status. Riftri 0.5.0 ignores attribute names that cannot affect checkout content
instead of refusing the repository, so the figures below describe the real
assistant-ui tree rather than an edited one.

## Creation results

| Metric | Run 1 | Run 2 | Run 3 |
| --- | ---: | ---: | ---: |
| Cold add (base construction) | 3.50 s | 4.03 s | 3.63 s |
| Serial cached add, median of 5 | 2.53 s | 2.51 s | 2.43 s |
| Ten concurrent, batch wall clock | 6.82 s | 7.36 s | 7.89 s |
| Ten concurrent, median per-view latency | 6.62 s | 7.02 s | 7.63 s |

Across all fifteen serial cached samples: median **2.51 s**, min 2.41 s, max
5.03 s. The 5.03 s sample is the same one that produced the volume-delta outlier
below.

## What the baseline says

**Caching saves about 31% of wall clock, not most of it.** A cold add that must
build the base takes a median 3.63 s; a cached add that reuses a verified base
takes 2.51 s. Since the cached path does no base construction, that remaining
2.51 s is almost entirely *not* data copying — it is Git invocations, base
integrity verification over 5,864 files, the writable-permissions traversal, and
index initialization. This is consistent with
[#339](https://github.com/assistant-ui/riftri/issues/339) and is where #211's
optimization has room.

**Concurrency trades per-view latency for throughput.** Ten views released
together finish in a median 7.02 s batch, i.e. 0.74 s amortized per view against
2.51 s serial — **3.4× the throughput**. But each individual view now takes a
median 7.02 s, **2.8× its serial latency**. A harness creating ten worktrees at
once gets them sooner in total; any single agent waiting on its own worktree
waits nearly three times longer. Both numbers are true and neither alone
describes the behaviour.

## Physical allocation

Reported separately from latency, because the two available measurements differ
by three orders of magnitude and answer different questions.

| Measurement | Value | What it means |
| --- | ---: | --- |
| Base allocated (riftri accounting) | 84.17 MB | Identical in all three runs, against 73,235,130 logical bytes — the difference is block rounding across 5,864 files. |
| Marginal per cached view (riftri accounting) | 4,096 B | 65,536 B across all sixteen views, byte-identical in all three runs. APFS does not charge a clone for blocks it shares with the base, so the view's file *data* costs one block. |
| Marginal per cached view (volume delta) | 2.88 MB median | Whole-filesystem free-space change, so it also counts directory metadata, the Git index, and the `.git` administrative files a real worktree needs. |

Neither figure is wrong; quoting only the 4,096 B would overstate the saving,
and quoting only the 2.88 MB would hide that the tracked content itself is
genuinely shared. The 2.88 MB median independently reproduces the ~2.7 MB
measured for assistant-ui in the earlier clean-room comparison.

## Why `du` must not be used for this

After the measurements, the sixteen view directories were removed and the
free-space change recorded:

| | |
| --- | ---: |
| `du -sk` reports for the sixteen views | 1,347 MB |
| Free space actually recovered | 12.4 MB |
| Overstatement | **109x** |

`du` sums each file's apparent size and cannot see that a clone shares its blocks
with the base, so it reports sixteen full checkouts where the filesystem holds
roughly one. Any savings figure derived from `du`, `ls -l`, or Git's own object
sizes is meaningless for copy-on-write views. That is why this record quotes
Riftri's own accounting and whole-volume deltas and never a directory walk.

## Retained outlier

One serial sample in run 2 recorded a 533.6 MB volume delta and a 5.03 s
duration. Nothing in a cached add can allocate half a gigabyte; this was
unrelated activity on the machine during the measurement window. It is retained
rather than discarded, and it is the reason the volume-delta column should be
read as a median over many samples rather than a per-run figure.

## Methodology notes and limits

- Volume deltas use a settle loop that waits for free space to stop moving,
  because APFS reclaims deleted space asynchronously.
- The machine was **not** quiet. Latency figures proved stable regardless — the
  serial cached median moved 4% across three runs — but volume deltas did not.
- Host APFS was used rather than a dedicated sparse image, matching
  `shared-base-readers-2026-09-13.md`.
- Git invocation counts are not measured here; they are a hard contract in
  `crates/riftri-cli/tests/git_invocation_budget.rs` (24 cold, 18 cached).
- Machine-readable results: `assistant-ui-cached-creation-2026-09-25.json`.
