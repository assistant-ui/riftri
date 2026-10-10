# Adaptive APFS clone workers: rejected after independent replication

This is a failed experiment, not a speedup claim or a production change. The
prototype has been removed from the runtime. It must not be merged or released
on the evidence below.

## Candidate and method

Both binaries start from main `a58006d7c7b4989a055a967e65d5654281b77ea4`.
Neither issues read-ahead. The candidate retains the existing clone-worker
limit for a lone caller and uses at most two workers if another caller owns a
nonblocking admission on the same base directory. Unsupported admission keeps
the existing limit. This scheduling hint does not replace base validation,
Git checks, lifecycle locks, or journals. All clone workers join before the
admission is explicitly released.

The local Apple M1/macOS run used a fresh 4 GiB APFS sparsebundle and release
binaries. Eight alternating pairs were planned for each of four fixtures:
4,097 synthetic entries (4,096 files of 8 KiB plus a symlink), serial and
four-way; and the unchanged assistant-ui tree, serial and four-way. The source
commit was `038cd9f82b418afe9e6d0080648738f78586fbca`, tree
`c4de7922b24126e860cb77652f5d080e04c8c396`, with 5,864 entries.

Each fixture began with a cold baseline anchor. Measured adds reused its base.
The harness checked Git cleanliness/HEAD, every tracked byte, executable mode,
symlink target, private-write isolation, clean removal, and final GC. It kept
all attempts and stopped after an operational failure; no failed pair is
included in a completed-fixture median. The historical candidate label
`advice` does **not** mean this prototype performs read-ahead.

Predeclared requirements were: complete verification and no timeout;
concurrent medians at least 5% lower with at least six of eight faster pairs;
serial median and median paired ratio no more than 2% higher; no paired ratio
above 2; and median CPU per view no more than 5% higher. Every fixture had to
pass independently. Local success alone would not have authorized adoption.

## Results

Batch wall-clock medians, in milliseconds:

| Fixture | Baseline | Candidate | Faster pairs | Decision |
| --- | ---: | ---: | ---: | --- |
| Synthetic, serial | 2,064.76 | 2,256.77 | 2/8 | 9.30% slower; failed |
| Synthetic, four-way | 7,439.84 | 6,608.56 | 6/8 | 11.17% lower; local gate passed |
| Reference, serial | 3,682.50 | 3,602.63 | 5/8 | 2.22x paired tail; failed |
| Reference, four-way | — | — | — | Four candidate deadline failures in round 6 |

Across the sequence there were 144 attempts and 58 completed batches: 140
successful CLI exits and four candidate timeouts, with no baseline timeout.
The first three fixtures completed verification and cleanup. In the last
fixture the five completed pairs passed their verification/removal checks;
the anchor and four unfinished views were retained. That fixture has no valid
full-comparison median or final-GC result.

The four failed workers had all passed `view-created` and
`git-pointer-restored`. Three Git traces end inside `index/refresh`; the fourth
records a roughly 270-second reset and reaches `index-synchronized`. The
configured deadline was 120 seconds, but observed worker settlement took
about 273 seconds. Do not describe the timer as an enforced 120-second upper
bound on a heavily stalled host, or infer the kernel cause from these traces.

The host was not quiet: memory availability and load varied sharply, and load
averages exceeded 40 later in the run. No local builds/tests/other benchmarks
overlapped timing. A one-second diagnostic of the Node driver during prolonged
post-add verification found 201 of 205 main-thread observations in filesystem
read. There were no driver children and the same 13 samples/three batches
before and after that observation. It is not a Riftri-worker profile, a
per-read duration, or proof that admission caused the stalls.

## Retained evidence and next action

The [compressed record](apfs-adaptive-clones-2026-10-09.json.gz) contains every
raw result, failed-worker Git trace, predeclared plan, exact prototype patch,
harness and acceptance-gate sources, and the verification-driver diagnostic.
Embedded sources have individual SHA-256 digests. The compressed record's
SHA-256 is `e8a770a9c97ba5810bb19b041416fd23edc1bccb972d55d89e8b3df12617bf68`.

Binary SHA-256 identities:

- Baseline: `33240072e0f00873ecf588f0917dab1e198a5f9bd246bc3a3d195ad981f1c80a`.
- Candidate: `d81c7dbe050163c540627cbf7221ac888d6922e6fca8099a01089b1bf1f938f6`.
- Exact patch: `bad2251b5708114f5302bc3c2e1253f2469fb572e59401543ffe99abab192660`.

The prototype passed formatting, all-target/all-feature Clippy, the workspace
suite, and npm tests (266 passed, four skipped) before timing. Those checks
did not prevent the end-to-end performance failure. Added tests covered
contention, unsupported admission, independent bases, errors, unwinding,
descriptor duplication, and a separate contender process.

## Same-binary calibration

After the candidate run terminated, two fresh serial fixtures used the exact
unchanged baseline executable for both labels. Both completed all eight pairs,
verification and cleanup: 34 successful creates and 32 batches. No build, test,
other benchmark or observer overlapped these controls. The
[calibration record](apfs-adaptive-calibration-2026-10-09.json.gz) retains every
sample and the plan and evaluation sources written before timing. Its SHA-256
is `7c939bf774b20dc2a26a4f556ae10e87b2b2b52697e61c2f7684bc8a71f3f33b`.

| Identical-binary control | First median | Second median | Median paired ratio | Maximum paired ratio |
| --- | ---: | ---: | ---: | ---: |
| Synthetic serial | 1,553.44 ms | 1,553.73 ms | 1.030 | 2.287 |
| Reference serial | 3,360.04 ms | 2,863.22 ms | 0.910 | 1.239 |

The same code produced an apparent 14.79% median reduction on the reference
tree, while synthetic controls crossed the serial paired/tail limits. Local
timing variation is substantial. This does not explain the four-way timeouts,
prove the candidate equivalent, or reverse its rejection.

## Independent replication method

The temporary `Adaptive APFS evaluation` workflow reconstructed the exact
checksummed prototype in an isolated checkout of the pinned main commit; it
did not change this branch's Rust runtime. Three macOS runners built both
binaries and completed prototype quality gates before timing. Each ran the two
same-binary controls, then the same four candidate fixtures, without concurrent
benchmark jobs on that runner. Fixture directories are fresh, and any
operational failure stops the sequence without retrying; unrun cases fail the
evidence gate. Logs, results, Git traces and add journals are uploaded even on
failure. Eight-minute step and 45-minute job limits bound a stalled harness.

The candidate acceptance limits above are unchanged. Additionally, each
same-binary hosted control must complete, match its pinned built executable,
keep both its median ratio and median paired ratio within 5% of 1, keep every
paired ratio between 0.5 and 2, and keep median CPU within 5%. These symmetric
control limits are predeclared before hosted timing, not fitted to its results.
All controls and candidate fixtures must pass on every runner. Do not pool
results to hide a failure. Even success still requires cold/tiny/large-file,
allocation and fresh cross-platform validation before production adoption.

## Independent replication results

All three jobs in [run 38016437732](https://github.com/assistant-ui/riftri/actions/runs/38016437732)
finished. All 18 fixtures completed their eight alternating pairs and full
verification/cleanup: **594 successful creates, 288 completed batches, and no
CLI timeout** on either label. This includes 18 cold baseline anchors; the
paired measurements are cached adds, not cold-creation comparisons.

The jobs failed their predeclared performance gates, not their correctness
checks. Only the three synthetic serial non-regression cases passed among the
12 candidate comparisons. No concurrent comparison qualified. Only one of
six same-binary stability controls passed. Results are kept separate below;
positive reduction means a lower candidate median, not an accepted speedup.

| Candidate median reduction | Runner 1 | Runner 2 | Runner 3 |
| --- | ---: | ---: | ---: |
| Synthetic serial | 0.52% | 1.90% | 11.27% |
| Synthetic four-way | -8.68% | -4.68% | 10.58% |
| Reference serial | 2.04% | 2.88% | -3.91% |
| Reference four-way | 1.40% | -6.06% | -10.79% |

Runner 3's apparently faster synthetic four-way median still had only four
of eight faster pairs and a 10.61x paired tail. All reference serial cases
crossed the 2x tail limit. The reference four-way cases had 3/8, 4/8 and 5/8
faster pairs and maximum paired ratios of 9.45x, 5.61x and 10.52x respectively.
Runner 2 also exceeded the CPU allowance in synthetic four-way creation.

Controls independently demonstrated substantial variability. The same binary
showed a 6.74x synthetic paired tail on runner 1 and a 6.00x reference paired
tail on runner 2. Runner 3 had a reference ratio of 0.132 and an apparent
11.34% synthetic median reduction with unchanged code. This does not establish
the source of the stalls, excuse candidate regressions, or justify loosening
the acceptance limits.

The [hosted evidence archive](apfs-adaptive-replication-2026-10-09.json.gz)
retains the terminal run/job record, all three exact build identities,
evaluations, results, worker logs and Git traces, plus the exact workflow and
gate sources from `f08762a445c2163fab9c16a008c2f02f601e45d8`. Its SHA-256 is
`d695be8d487d8f49cd5dcb1f919096f63ea7354d2091f71347c2b47bfd18769f`.
Every retained file has its own digest, and offline reevaluation exactly
matches each hosted result. All final fixtures report zero active views,
bases and diagnostic issues; each job detached its owned test image without
forcing. The temporary workflow has been removed after retaining the results.

Decision: **reject the adaptive scheduling prototype**. The research branch
contains evidence and gate tests only; production Rust remains identical to
the pinned main commit. A new proposal needs a distinct hypothesis and fresh
controls, not another run selected for a favorable median. Keep full Git and
immutable-base verification. In particular, do not weaken Git's first full
scan to hide the remaining startup cost.
