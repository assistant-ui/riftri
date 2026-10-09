# Optional Git FSMonitor: October 8, 2026

FSMonitor improved repeated status checks on a large synthetic worktree, not
Riftri startup. Keep it an explicit Git opt-in, off unless the user enables it.
The [setup guide](../fsmonitor.md) includes scope, compatibility, and reversal.

## Environment and method

Apple M1, macOS/APFS (Darwin 25.2.0), Apple Git 2.50.1, published Riftri 0.6.4.
This compares Git settings on the **same binary**, not two Riftri implementations.
The [JSON](fsmonitor-2026-10-08.json) retains every timing sample and the
nearest-rank p50/p95 summaries, without dropping outliers. Absolute temporary
paths and daemon tokens are omitted from the published data.

Each disposable repository contained 8 KiB text files plus four additional
tracked entries including an executable and a symlink. The 4,100-file fixture
had 32 MiB of text; the 20,004-file fixture had about 156 MiB. File contents
were repeated. Neither fixture represents T3 application startup, dependencies,
or a physical-allocation benchmark.

Three configurations were tested: FSMonitor off, on, and on with Git's untracked
cache. Global/system Git configuration was isolated. Creation used three
alternating cold pairs and six rotated cached rounds on 4,100 files. Cold means
an empty Riftri base cache, not a cold OS cache; cached adds reused an anchor's
exact immutable base. The larger run measured status only.

Each status scenario used four prior warm-ups and 20 rotated paired rounds per
setting. Times include Git process startup. The command used porcelain v1,
NUL delimiters, `--untracked-files=all`, and no rename detection. After all
three timed commands in a round, each output was compared with an independent
FSMonitor-off/untracked-cache-off check using `--no-optional-locks` to avoid
rewriting the monitored index. Three-edit rounds rewrote the same three tracked
paths. Three-untracked rounds created three files initially, then rewrote them;
this is not a high-churn directory test.

## Results

All times below are p50 milliseconds; raw JSON also includes p95 and all samples.

| Files | Operation | Off | FSMonitor | FSMonitor + untracked cache |
| ---: | --- | ---: | ---: | ---: |
| 4,100 | Cold creation, n=3 | 2,038.8 | 2,148.1 | Not measured |
| 4,100 | Cached creation, n=6 | 1,356.6 | 1,401.1 | 1,440.9 |
| 4,100 | Clean status, n=20 | 23.7 | 28.9 | 29.7 |
| 4,100 | Three edits, n=20 | 23.5 | 29.3 | 30.5 |
| 4,100 | Three untracked, n=20 | 23.1 | 28.5 | 29.2 |
| 20,004 | Clean status, n=20 | 61.5 | 45.3 | 45.8 |
| 20,004 | Three edits, n=20 | 66.7 | 46.6 | 46.7 |
| 20,004 | Three untracked, n=20 | 63.7 | 46.0 | 46.3 |

The larger fixture saved 16–20 ms per check (26–30%). The smaller fixture was
slower. No creation speedup or extra untracked-cache benefit was demonstrated.
These two sizes do not establish a universal enablement threshold.

Trace2 after three edits showed `preload/sum_lstat=20004` with FSMonitor off,
versus `3` with it on. Both had `refresh/sum_scan=3`: ordinary Git already avoided
content scans of unchanged files. FSMonitor saved metadata checks, not 20,001
content reads. Per-sample trace counters are included in the JSON.

## Correctness and cleanup

All 360 measured status outputs matched the normal-scan oracle. Both full-size
runs also passed same-size/restored-mtime edits, atomic replacement, deletion,
rename, executable changes, symlink changes, nested untracked files, staged plus
unstaged edits, and changes made while the watcher was stopped. These additional
cases used FSMonitor plus the untracked cache. Peer and immutable-base contents
were preserved; normal dirty removal was refused.

The committed macOS CLI regression runs those nine cases with the untracked
cache both off and on, checks default-off behavior, exercises Git interception,
and verifies opt-out and clean removal. Watchers are stopped during unwinding
as well as on success. The benchmark's CI smoke test checks all three variants
and cleanup without any timing threshold.

Successful benchmark runs stopped watchers, removed clean views through Riftri,
garbage-collected bases, verified empty active state, and retired only their
own fixtures. An initial smoke attempt failed on an incorrect harness
assumption that stopping a non-running daemon succeeds. The helper was fixed
before the measured runs; that was not a product failure. Memory discovery in
the original full runs was incomplete, so no aggregate memory/CPU claim is
made. The committed harness correlates observed daemon-token PIDs instead.
It also isolates IPC sockets in a short disposable directory rather than Git's
long-path fallback under the user's home directory; it removes that directory
after successful cleanup. Those harness refinements do not rewrite the recorded
measurements above.

## Reproduce

Build or select a release binary, then provide a **new** output directory on
local APFS. The script uses its own disposable repositories and socket directory, preserves
fixtures on failure, and retains logs/JSON/Trace2 results on success. Its free-space
guard scales with fixture size; reserve at least 2 GiB for the larger run.

```sh
cargo build --release -p riftri-cli
node docs/benchmarks/fsmonitor.mjs target/release/riftri /tmp/riftri-fsmonitor-4100
BENCH_FILES=20000 BENCH_CREATION_ROUNDS=0 BENCH_COLD_ROUNDS=0 \
  node docs/benchmarks/fsmonitor.mjs target/release/riftri /tmp/riftri-fsmonitor-20004
```

Keep the machine awake and avoid other benchmarks during timing. Defaults are
4,096 text files, three cold rounds, six cached rounds, and 20 status rounds.
`BENCH_STATUS_ROUNDS` also permits a short smoke run. This report is macOS/APFS
evidence only; representative real projects and other platforms need separate
measurements before any wider recommendation.
