# Shared immutable-base readers: follow-up to checkout-config batching

## Scope

This compares commit `cde51fe4a044d13ee52ca0b1b786e80b8da25471`
([PR #97](https://github.com/assistant-ui/riftri/pull/97)) with that same code plus
shared read locks for existing-base integrity verification. Both builds already
include batched checkout configuration reads. This is **not** a comparison with
the published 0.1.1 release, and the percentages from the two PRs must not simply
be added together.

Cache hits still recompute the complete integrity digest. Construction,
incomplete-base cleanup, and collection retain exclusive ownership of the same
stable lock file. Cold callers release their read lock before acquiring the
write lock and repeat all checks after the ownership gap. There is no index,
clone, permission, cache-key, journal-format, or durability optimization here.

## Creation results

The fixture is an independent, exact-tree export of Farm.js commit
`ab7b0184985f8bd742f0d4636961ec90c355cbd9`, tree
`aa5966b43f429773a941fd65dcbbce8bf2575a5a`: 2,005 tracked files and 13,781,534
logical bytes. The original repository and its mutable working files were not
modified. A baseline-created anchor kept the same immutable base alive for
both versions throughout the run.

Release binaries ran on ordinary host APFS, macOS 26.2, arm64. Four alternating
pairs ran per single-create mode; six alternating pairs ran with nine concurrent
process-scoped creations. All samples are retained, including outliers.

| Operation | Baseline median | Shared-reader median | Change |
| --- | ---: | ---: | ---: |
| One cached explicit creation, 4 runs/version | 1.985 s | 2.028 s | 2.2% higher |
| One cached process-scoped creation, 4 runs/version | 1.956 s | 1.806 s | 7.7% lower |
| Nine concurrent cached creations, 6 batches/version | 5.956 s | 5.518 s | 7.4% lower |

The concurrent pairs were:

| Pair | Run order | Baseline | Shared readers |
| --- | --- | ---: | ---: |
| 1 | before, after | 4.730 s | 4.550 s |
| 2 | after, before | 5.128 s | 5.338 s |
| 3 | before, after | 5.921 s | 5.081 s |
| 4 | after, before | 5.991 s | 7.596 s |
| 5 | before, after | 6.352 s | 5.698 s |
| 6 | after, before | 11.482 s | 6.481 s |

Shared readers won four pairs and lost two. The baseline's last batch and the
candidate's fourth batch were notably slow. The host was busy and had only about
2 GiB free; CPU load, filesystem caches, and background activity were not
controlled. Single-create results are mixed. These samples suggest a modest
median concurrency benefit, **not a statistically established or universal
speedup**. Reader admission is more concurrent, but hashing, cloning, Git index
initialization, and storage contention remain. No lock-fairness guarantee is
introduced.

Concurrent batch timing includes the harness collecting completed child results;
content verification, private edits, and worktree removals are outside the
timed region. Git starts remain 20 per explicit add and 27 per process-scoped
add in both versions, as expected after #97.

## Correctness evidence

- The regression fixture holds an external shared base lock. Cached creation
  timed out on the baseline while that reader remained held, then completed
  after it was released. The candidate completes while the reader still holds
  ownership. This is an ownership test, not a claimed 15-second latency saving.
- New tests retain exclusive-writer exclusion, wait before rebuilding an
  incomplete base, refuse an invalid integrity marker under shared ownership,
  and block collection until readers release ownership.
- Lock-level tests check exclusion until the last reader exits, symlink refusal
  on Unix, and explicit release even when a duplicated Unix descriptor remains
  alive, modeling an inherited descriptor after fork.
- Existing parallel cold-add tests still build one base and keep private edits
  and commits isolated. The existing corruption and lifecycle recovery tests
  remain required; the optimization does not skip them.
- All **125** benchmark views passed Git cleanliness and full tracked-content,
  symlink-target, and executable-mode comparisons, then normal removal. All
  twelve concurrent groups passed a private-write check against their peers,
  anchor, and base. Final state reported zero active views, retained bases,
  pending adds/removals/collections, or state issues.

The existing native COW allocation benchmark also passed on a disposable 1 GiB
APFS volume. A cached view with a 32 MiB payload added **53,248 bytes (52 KiB)**
of volume allocation; a 4 MiB private overwrite added approximately 4 MiB while
the peer stayed clean. Raw allocation counters are included in the JSON below.
This preserves physical sharing; it does not claim better allocation than #97.
That separate test used an unoptimized build, and its timing counters must not
be compared with the release-binary creation measurements above.

Local validation includes formatting, Clippy with all targets/features and
warnings denied, the full workspace suite with default concurrency and serial
execution, and the isolated allocation test. Incremental compilation and debug
information were disabled to conserve disk space. Linux/ReFS lifecycle results
must come from platform CI; local APFS evidence does not establish their timing.

## Reproduction and raw samples

[Raw samples and summary](shared-base-readers-2026-09-13.json) record every
single-create and concurrent-create duration, each batch, process counts, and
the candidate's `worktree.rs` SHA-256. Build the baseline commit and candidate
with `cargo build --release --locked -p riftri-cli`, preserving separate binaries.
Then use the shared exact-tree harness:

```sh
RIFTRI_BENCH_SINGLE_ROUNDS=4 \
RIFTRI_BENCH_BATCH_ROUNDS=6 \
RIFTRI_BENCH_WORKERS=9 \
node docs/benchmarks/checkout-config-batching.mjs \
  /path/to/before/riftri /path/to/after/riftri \
  /path/to/farm.js ab7b0184985f8bd742f0d4636961ec90c355cbd9 \
  /path/to/new-output-directory
```

The harness requires a fresh output directory, isolates fixture Git
configuration, enables Riftri only in that fixture, and retains logs and Trace2
events. It restores its private test edit before normal removal. On failure,
inspect and recover the disposable fixture rather than forcing removal.
