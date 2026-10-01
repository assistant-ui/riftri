# Assistant-ui cached-add diagnostic, 2026-10-01

This is an incomplete performance investigation, **not a latency baseline or
speedup claim**. The paired release harness completed all sixteen serial cached
samples and verified and removed those views. The full ten-agent comparison was
stopped under heavy host pressure. Its batch timings and volume deltas are not
performance evidence. This record does not replace the September 25 baseline.

The baseline was the SHA-256-checked macOS arm64 v0.5.1 release archive
(`564cc37bbb342ec6e88e829ff6c87ba78ca09f62`). The candidate was
`085f9296b8cf83f5e1bf7bb2d86d986b3b6fad24`, built with
`cargo build --release --locked -p riftri-cli --features tui`.
Both used a full checkout on host APFS, an Apple M1 with 16 GiB RAM,
Darwin 25.2.0, and Apple Git 2.50.1.

An independent export of assistant-ui commit
`038cd9f82b418afe9e6d0080648738f78586fbca` reproduced exact tree
`c4de7922b24126e860cb77652f5d080e04c8c396`: 5,864 tracked files and
73,235,130 logical bytes. The source repository was only read. Four rounds
alternated binary order for explicit and process-scoped adds. Every serial view
passed clean-status and tracked-content, executable-mode, and symlink checks.
All serial samples, including outliers, are in the
[diagnostic JSON](assistant-ui-diagnostic-2026-10-01.json).

## Findings

- Git Trace2 recorded 17 to 14 Git starts per explicit cached add, and 21 to 18
  through the shim, consistently across all four rounds. These counts describe
  this fixture and these two revisions.
- In the first candidate explicit sample (2.501 seconds), the observed index
  synchronization phase took 1.347 seconds. Git Trace2 attributed 1.297 seconds
  to its `index/refresh` region. Index initialization merits further profiling.
- Candidate explicit samples ranged from 2.285 to 10.112 seconds; shim samples
  also varied sharply. The host reported load averages above 26 and more than
  17 GiB of used swap. This prevents a reliable latency improvement conclusion.
  Earlier attempts hit disk exhaustion and are excluded as failed runs.

## Adoption follow-ups, in priority order

1. Repeat the paired release benchmark on a quiet volume with ample free space,
   including cold adds and ten-agent batches. Include ordinary Git when
   evaluating adoption tradeoffs and report disk savings separately. CI's
   single-file/debug fixtures cannot replace this many-file workload.
2. Profile index initialization and refresh. Candidates must preserve real Git
   index contents, modes, attributes, sparse skip-worktree bits, corruption
   refusal, and final clean verification. Measure the entire add to avoid
   confusing work moved between phases with an actual improvement.
3. Measure the supported cone workflow on this repository and document task
   directory selection. Reducing materialized files may help monorepo users
   more than small allocation optimizations. Keep selection explicit and full
   checkout defaults unchanged.

The manual harness now canonicalizes output-directory aliases before comparing
journal paths and permits five minutes per subprocess. Timeouts still fail the
run; partial output cannot substantiate a speedup.
