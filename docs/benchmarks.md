# Native COW benchmark

## Performance regression checks

On macOS, the release-mode CLI checks exercise 1,025-file cold, cached, and
existing-branch adds, verify every file and private-write isolation, and remove
the views through Riftri. They also cover 16 local LFS pointers through creation
and compaction. CI runs these without requiring any new user configuration:

```sh
cargo test --release --locked -p riftri-cli \
  --test git_invocation_budget --test checkout_compatibility
```

The many-file fixture retains the 19/14/17 Git-process budgets. LFS pointer
size inspection is one batch regardless of pointer count; pointer bodies remain
individually read only after their sizes pass the existing bound. Status indexes
one inventory snapshot without caching it across commands or skipping checks.

Two optional paired probes isolate size-query overhead and worktree lookup
costs (including index construction):

```sh
cargo test --release -p riftri-git --lib reports_blob_size_batch_latency -- --ignored --nocapture
cargo test --release -p riftri-core --lib reports_inventory_lookup_latency -- --ignored --nocapture
```

These probes alternate old/new operation order and check result parity. Their
timings are not end-to-end add/status speedups; use the full CLI workload below
for those claims. The lookup probe runs on macOS, or with the
`native-cow-integration` feature on supported Linux/Windows test volumes.

Local diagnostic on October 1, 2026 (Apple M1, macOS, release profile): the four
paired samples below passed result parity. The host was busy with other builds
and tests; these are component observations, not a quiet-machine baseline or a
whole-command speedup claim. All samples are retained, in microseconds:

```text
64 blob sizes, individual: 2722253, 1591882, 3615255, 3368587
64 blob sizes, batched:      26695,   38438,   41424,   64565
1000 lookups, linear:      223819, 257709, 303160, 243345
1000 lookups, indexed:       1109,    889,    875,    768
```

The deterministic improvement is fewer subprocesses (64 size requests become
one) and indexed rather than repeated linear lookup. Pointer parsing, local
object validation, base integrity, and final Git cleanliness checks remain in
place. No command syntax, opt-in requirement, or checkout default changes.

## Real-project comparisons

For journal-history scaling, build the baseline and candidate with the same
release profile, then run this Unix/native-COW fixture:

```sh
node docs/benchmarks/journal-history.mjs /path/to/before /path/to/after \
  /path/to/new-output-directory 100
```

It creates the requested number of real add/remove cycles while retaining a
live anchor, alternates four paired `status` and GC-plan measurements, and
requires identical complete JSON reports. It then applies retirement, verifies
the live view, removes it, collects its base, runs repair, and checks empty
status. A failed run retains its fixture and is not a completed benchmark.
Use larger cycle counts (up to 1,000) when evaluating long-lived repositories.

The ignored `reports_journal_relationship_index_latency` core test isolates
grouping 10,000 records, including index construction. A local release run
measured 45,191–48,072 microseconds for linear grouping and 591–776 microseconds
for indexed grouping. This does not establish an end-to-end improvement:
100-cycle command timings were dominated by other costs/noise and did not
demonstrate a clear benefit. Fresh under-lock deletion validation is unchanged.

For a sparse/full comparison on an exported source tree, run:

```sh
node docs/benchmarks/sparse-cone.mjs /path/to/riftri /path/to/source \
  EXACT_COMMIT /path/to/new-output-directory packages/react
```

The [sparse harness](benchmarks/sparse-cone.mjs) verifies that its independent
fixture has the source's exact Git tree ID. Ordinary Git supplies the expected
cone shape and skip-worktree bits. It checks every created view against those
semantics, keeps sparse and full bases separate, records one cold and three
cached samples for each profile, then removes the views and collects the bases.
Logical bytes/file counts, latency, and volume-level allocation deltas are
separate fields. Volume deltas are noisy on a busy host; use a quiet volume with
ample free space. Dependencies and builds are not included, and incomplete runs
are not benchmark evidence. The selected directory must exist in the exact tree.

For a real-project, ten-agent run using the public direct-download CLI, see
[the assistant-ui experiment](benchmarks/assistant-ui-ten-agents-2026-09-12.md).
It records both allocation savings and slower creation, along with the
repository compatibility adjustment and workload limitations.

For a historical cached-creation baseline on a real repository, see
[the assistant-ui cached creation baseline](benchmarks/assistant-ui-cached-creation-2026-09-25.md).
It records serial and ten-way concurrent latencies, reports the two physical
allocation measurements separately, and retains an outlier rather than dropping
it. It is a baseline for #211, not a comparison between versions.

The [October 1 diagnostic](benchmarks/assistant-ui-diagnostic-2026-10-01.md)
records serial process counts and next profiling targets. Heavy host pressure
prevented a complete comparison, so it makes no latency claim.

For the narrower checkout-configuration optimization, see
[the batching comparison](benchmarks/checkout-config-batching-2026-09-13.md).
It includes process counts, paired timings, correctness checks, and raw samples.

For the follow-up shared-reader base-lock change, see
[the concurrent verification comparison](benchmarks/shared-base-readers-2026-09-13.md).
It measures the incremental change on top of configuration batching and records
both the median improvement and slower individual pairs.

For the test-only APFS directory-clone candidate, see
[the bulk directory evaluation](benchmarks/apfs-bulk-directory-clone-2026-09-13.md).
It records behavioral parity, a directory-metadata difference, allocation, and
five local timing samples. The candidate is deliberately not used in production.

The [paired creation harness](benchmarks/checkout-config-batching.mjs) can also
compare a proposed optimization with a baseline on an exact exported Git tree:

```sh
RIFTRI_BENCH_SINGLE_ROUNDS=4 RIFTRI_BENCH_BATCH_ROUNDS=2 \
RIFTRI_BENCH_WORKERS=10 node docs/benchmarks/checkout-config-batching.mjs \
  /path/to/baseline/riftri /path/to/candidate/riftri \
  /path/to/source EXACT_COMMIT /path/to/new-output-directory
```

Use matching build profiles and a quiet volume with ample free space. The
harness records cold samples, alternating cached serial samples, and ten-way
cached batches through both explicit and process-scoped interfaces. It records
Git Trace2 process counts, observed CLI phase timestamps, and batch volume
deltas separately. Phase timestamps are observed at stderr receipt, not an
internal profiler; the shim does not emit the explicit CLI's phase stream.
The fixture must have the source's exact tree ID, and every successful view is
checked against the tracked-content manifest and removed through Riftri. Each
concurrent batch also checks private-write isolation. A failed or disk-full run
is incomplete evidence; do not use its partial samples for a speedup claim.

Riftri includes one ignored integration benchmark for comparing worktree
creation and physical allocation on a real supported destination filesystem.
It exercises the same public core transaction used by the CLI; no synthetic
storage implementation is involved.

Run it on an otherwise quiet APFS volume:

```console
$ RIFTRI_BENCHMARK_OUTPUT=riftri-benchmark.json \
  cargo test -p riftri-core --test native_cow_benchmark \
  reports_cold_cached_and_private_write_costs -- \
  --ignored --exact --nocapture
```

Linux and Windows runs also enable the integration feature and place their
temporary directory on the volume under test:

```console
$ TMPDIR=/path/on/supported-volume \
  RIFTRI_BENCHMARK_OUTPUT=riftri-benchmark.json \
  cargo test -p riftri-core --features native-cow-integration \
  --test native_cow_benchmark reports_cold_cached_and_private_write_costs -- \
  --ignored --exact --nocapture
```

The JSON report records:

- the selected backend and host platform;
- cold creation time, including immutable-base materialization;
- cached creation time, reusing that exact-tree base;
- destination-volume growth for both creations;
- volume growth after a private 4 MiB write; and
- filesystem-accounted bytes for the cached view before and after that write.

The per-view accounting fields are `null` for helper-backed OverlayFS because
its journal-owned kernel work directory is deliberately root-owned and not
scannable by the calling user. Volume-growth evidence remains available for
that backend; the benchmark does not relax helper permissions to obtain an
extra metric.

Timing values are observations, not pass/fail thresholds. Host load, Git and
filesystem caches, antivirus software, file count, and storage hardware all
affect them. The correctness gate remains deterministic: both worktrees must be
clean and isolated, the second add must reuse the base, and a cached 32 MiB view
must consume less than 25% of its logical payload in new volume allocation.

Filesystem-accounted bytes can count shared blocks more than once and are not
exclusive physical disk use. The before/after free-space deltas are the primary
physical-sharing evidence and should be measured on a quiet disposable volume.
OverlayFS may allocate the full backing file on its first data write because
copy-up happens at file granularity; that is expected and is reported rather
than hidden.

CI uploads one JSON artifact for APFS, Btrfs, reflink-enabled XFS,
helper-backed OverlayFS, and ReFS. Artifact names start with
`native-cow-benchmark-`, making runs directly comparable without treating one
shared hosted runner as a permanent performance baseline.

## Cached creation baseline

`reports_cached_creation_baseline_serial_and_concurrent` in
`crates/riftri-core/tests/native_cow_benchmark.rs` records what a cached
creation costs, serially and with ten views released together against one
already-verified base. It exists for #211, whose first requirement is a current
baseline before anything is optimized.

The concurrent phase is the interesting one: a barrier releases all ten threads
at once so they overlap rather than queue, every per-view latency is reported
rather than a mean, and the run asserts that all ten reused the single base
instead of rebuilding it.

**This harness is not itself a published baseline.** It runs under `cargo test`
with debug binaries, unlike the records in `docs/benchmarks/`, which use release
binaries, alternating pairs, and a real repository fixture. Two runs on a busy
laptop moved every absolute number by a factor of two to four, so figures taken
that way are worth nothing as a baseline. CI records it on the disposable APFS
volume and uploads `cached-creation-baseline-macos-apfs`; a baseline worth
optimizing against should come from there, or from a run following the
methodology the `docs/benchmarks/` records already use.

The Git invocation counts the issue also asks about are a hard contract
elsewhere: `crates/riftri-cli/tests/git_invocation_budget.rs` caps a cold add at
19 invocations and a detached cached add at 14; a cached existing-branch add
is capped at 17. These budgets cover the test's attribute-free fixture, rather
than every checkout profile, and the test fails when a budget grows.
