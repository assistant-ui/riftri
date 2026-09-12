# Native COW benchmark

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
