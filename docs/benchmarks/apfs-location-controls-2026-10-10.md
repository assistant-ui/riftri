# Same-binary APFS location controls, October 10

No runtime optimization was tested or accepted. Fourteen complete controls
all failed the predeclared timing-stability check, on both ordinary host APFS
and a nested APFS image. Moving the benchmark out of the image did not produce
a consistently quiet control. The reverse-order run was also incomplete.

## Method and provenance

[CI run 38021509234](https://github.com/assistant-ui/riftri/actions/runs/38021509234)
used controller commit `8973ea5171c542b27f7dda26784a842cd6433bb1` on two separate
macOS runners, with opposite case orders. Each built production commit
`a58006d7c7b4989a055a967e65d5654281b77ea4` once, then used that identical binary
for both labels in every case. No candidate patch was applied. The data-only
source was assistant-ui commit `038cd9f82b418afe9e6d0080648738f78586fbca`, tree
`c4de7922b24126e860cb77652f5d080e04c8c396`: 5,864 entries and 73,235,115 regular
file bytes. No source-project code was executed.

The whole fixture (repository, state and views) lived either on host APFS or
inside a dedicated 4 GiB APFS sparsebundle stored on the same host filesystem.
Mount identities and distinct devices were verified. Both runners had over
42 GiB available on the backing filesystem before measurement. Local timing
was not started because the development machine had about 1.1 GiB available.

Each runner planned eight cases: serial and four-way, twice per location,
with eight alternating pairs per case and a fresh cold anchor. The forward
order was host serial, image serial, image four-way, host four-way, image
serial, host serial, host four-way, image four-way. The other runner reversed
that sequence. Cases never overlapped on one runner.

Complete cases required exact bytes, executable modes, symlink targets, clean
Git state, private-write isolation, removal and GC. The separate stability
check required both median and paired-median ratios within 5% of one, each
paired ratio in `[0.5, 2]`, and median CPU per view within 5% of one. No outliers
were excluded and no failed measurement was retried.

## All cases, including incomplete work

A and B are the same executable. Medians are complete creation batches in
seconds (four simultaneous views for four-way cases), not per-view latency.
Ratios are B/A. Every complete row is **unstable**, not an optimization result.

| Order | Case | Median A | Median B | Paired median | Paired range | CPU/view ratio |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Forward | host serial 1 | 2.494 | 2.402 | 0.970 | 0.32–1.09 | 0.963 |
| Forward | image serial 1 | 2.615 | 3.036 | 1.180 | 0.78–7.61 | 1.108 |
| Forward | image four-way 1 | 5.944 | 6.371 | 0.996 | 0.75–1.32 | 1.084 |
| Forward | host four-way 1 | 4.530 | 4.478 | 1.018 | 0.09–8.52 | 0.961 |
| Forward | image serial 2 | 2.457 | 2.462 | 0.954 | 0.81–6.30 | 0.970 |
| Forward | host serial 2 | 2.738 | 2.842 | 1.006 | 0.17–1.24 | 1.014 |
| Forward | host four-way 2 | 6.536 | 8.365 | 1.269 | 0.93–5.84 | 1.030 |
| Forward | image four-way 2 | 9.523 | 9.081 | 0.932 | 0.18–4.08 | 0.979 |
| Reverse | image four-way 2 | 9.542 | 9.855 | 1.037 | 0.76–6.77 | 0.956 |
| Reverse | host four-way 2 | 7.480 | 7.524 | 1.004 | 0.89–5.93 | 1.024 |
| Reverse | host serial 2 | 3.491 | 3.624 | 1.020 | 0.96–5.98 | 1.034 |
| Reverse | image serial 2 | 3.853 | 4.034 | 1.012 | 0.19–5.00 | 1.031 |
| Reverse | host four-way 1 | 16.474 | 6.905 | 0.612 | 0.15–1.01 | 0.977 |
| Reverse | image four-way 1 | 7.662 | 8.722 | 1.089 | 0.98–10.72 | 1.091 |
| Reverse | image serial 1 | incomplete | incomplete | — | — | — |
| Reverse | host serial 1 | not started | not started | — | — | — |

The forward runner completed all eight cases and detached its image. The
reverse runner completed six cases before the 35-minute measurement-step
deadline interrupted its seventh case. Its non-forced detach failed with
`Resource busy`; job cleanup subsequently reported orphan `riftri` and
`diskimages-help` processes. The records do not establish the exact cause of
the busy mount. No successful cleanup is claimed for that partial case.

Across both runners, 632 successful CLI exits were recorded. Of these, 622
belong to the 14 fully verified, cleaned-up fixtures. The incomplete fixture
contains ten recorded exits and nine completed batches, plus a further started
attempt visible in a Git trace and operation journal without a settled sample.
There are 233 completed batches in all, versus 256 planned; the final host
case never started. All 633 available Git traces, 632 completed add logs,
11 remaining operation journals, controller records and the full CI log are
retained. The missing final add log was never written before interruption.

## What this establishes

Large stalls occurred outside disk images too. For example, all four workers
in forward host four-way 1, round 3, took about 44.7 seconds, using roughly
2.2 CPU seconds each; Git's `reset --mixed` accounted for about 41 seconds.
The image case also showed similar clustered delays. These are observed
locations of elapsed time, not proof of a particular kernel or hardware cause.

The study does not isolate cache history, host load, filesystem housekeeping,
or a particular APFS operation. Whole cases were sequenced rather than
interleaving locations batch by batch, and one runner did not finish. There
is no trustworthy quiet-layout selection or product-speedup claim from these
results. A later candidate still needs full correctness, end-to-end comparisons,
unchanged-code controls and independent replication; a faster subphase is not
enough.

Before another long hosted experiment, its outer cancellation must settle its
known child processes and preserve attempt records before image teardown. The
35-minute CI deadline bypassed the per-create supervisor here. This is a
benchmark-harness follow-up, not permission to change Riftri's safety checks or
to retry this run while discarding its failures. The temporary CI workflow has
been removed; its exact source is included in the archive.

## Reproducible evidence

[`apfs-location-controls-2026-10-10.json.gz`](apfs-location-controls-2026-10-10.json.gz)
contains exact UTF-8 artifact contents with individual hashes, both terminal
job records, the CI log, control evaluations and executed sources. No result
file selects executable code during offline reevaluation.

- Compressed SHA-256: `d6bc1d803c2a688e391735e3140f4a3ea30b3ef85dca5c0dd0538e94ebb8ee23`
- Uncompressed SHA-256: `7d31ab2c37f0beb33a3acd3aa38f026d1e7bba05d04fbbf3c1dc8732b654899d`

Run `node --test docs/benchmarks/apfs-location-controls-evidence.test.mjs` to
verify identities, retained failures, sample accounting and every complete
hosted evaluation, and to reject the partial and missing cases again.
