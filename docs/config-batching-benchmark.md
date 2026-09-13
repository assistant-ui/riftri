# Checkout configuration batching

Riftri now fetches the eleven checkout-profile settings through one exact-key,
NUL-delimited Git query instead of eleven separate `git config --get` processes.
Values retain their original bytes and precedence, including the distinction
between missing and present-but-empty settings. The compatibility checks, hash
order, captured checkout configuration, base identity, and Git index setup are
unchanged. The snapshot is operation-local, not a persistent configuration cache.

## Local measurement — September 13, 2026

Compared release builds of baseline `a71f8a8f00b8c9c008581cef071d388191e90673`
and this change on a MacBookPro17,1 (8 logical CPUs), macOS 26.2, ordinary APFS,
Git 2.50.1 (Apple Git-155), and Node 23.11.0.

The fixture was an independent export of Farm.js commit
`ab7b0184985f8bd742f0d4636961ec90c355cbd9`, tree
`aa5966b43f429773a941fd65dcbbce8bf2575a5a`: 2,005 tracked files and 13,781,534
logical bytes. Original source files and Git configuration were not changed.
The fixture's global/system Git configuration was isolated. Both binaries used
the same fixture and retained base; order alternated between paired rounds.
Cold runs explicitly collected the base first. Warm means a reusable base,
not a controlled operating-system cache state.

| Operation | Samples per build | Before median | After median | Interpretation |
| --- | ---: | ---: | ---: | --- |
| Destination-aware `doctor` | 10 | 1.343 s | 0.874 s | 34.9% lower; after was faster in all ten pairs |
| Cold explicit creation | 3 | 3.544 s | 4.537 s | 28.0% higher in this sample; no cold-start improvement established |
| Warm explicit creation | 6 | 2.021 s | 2.022 s | Essentially unchanged |
| Warm process-scoped creation | 6 | 2.366 s | 2.170 s | 8.3% lower median; after was faster in five of six pairs |
| Nine concurrent warm process-scoped creations | 3 batches | 8.694 s | 6.607 s | Mixed results; not a reliable concurrency speedup |

The concurrent before/after pairs were **9.284 / 5.423 s**, **6.121 / 12.233 s**,
and **8.694 / 6.607 s**. Despite the lower median, total elapsed batch time across
all three rounds was slightly higher after the change. Cold after-times ranged
from 2.361 to 12.426 s. No samples were removed. These shared-machine results
are too variable and too few to isolate a causal cold/concurrent regression or
claim a general creation-latency improvement; they should be repeated on a
quiet host before making broader speed claims.

Git Trace2 provides the deterministic result: **30 → 20 direct Git calls** for
warm explicit creation, **37 → 27** for warm process-scoped creation, **32 → 22**
for cold explicit creation, and **28 → 18** for destination-aware doctor. Nested
Git subprocesses are recorded separately by the harness. Only the eleven
checkout settings were batched; activation and other Git queries remain intact.

All **84 created views** passed Git cleanliness, tracked-byte SHA-256, symlink,
and executable-mode checks and were normally removed. Each concurrent batch
also exercised a same-size edit: Git detected it and sibling file contents
remained unchanged. Final fixture status reported zero active views, retained
bases, and state issues. Twenty additional doctor calls were read-only.

Binary SHA-256:

- Before: `fd4825ff8f598c6f99a5ad656619475cb17e7ffbc0ce9bc56f6a0cad03b92940`
- After: `16564a7e305e6ffe1a132d2dae83cc2b407216865aae691cead3c61859406cba`

## Reproduce

Build both revisions with `cargo build --release --locked -p riftri-cli` and
copy the executables to separate locations before rebuilding. On a supported
native-COW volume, with ordinary Git on PATH, run:

```sh
node benchmarks/config-batching.mjs \
  /absolute/path/riftri-before \
  /absolute/path/riftri-after \
  /absolute/path/farm.js \
  ab7b0184985f8bd742f0d4636961ec90c355cbd9 \
  /absolute/path/new-benchmark-output
```

The output directory must be new, outside the source repository, and have an
existing parent. The harness supports UTF-8 fixture paths and requires the
archive's bytes/tree to agree with the fixture's Git checkout; it fails on an
incompatible fixture rather than modifying the original project. It writes
`results.json`, per-case logs and Git Trace2 events, and a final status report.
Failed or dirty experimental worktrees are preserved for inspection. Successful
runs remove generated linked views and collect bases, but retain the independent
fixture repository and evidence.

Unit regression coverage compares batched results with individual Git reads,
including includes, duplicate values, missing/empty/valueless values, multiline
and non-UTF-8 value bytes, regex-literal subsection names, linked-worktree
configuration, global/system/command overrides, and malformed configuration.
Another test compares the checkout profile and captured values against the
previous individual-read sequence. A process-count regression was first
observed failing with 11 starts and now requires exactly one, without a flaky
wall-clock threshold.
