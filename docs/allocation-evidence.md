# APFS allocation evidence

Riftri's macOS backend calls `clonefile` for every regular file and returns an
error when the native clone cannot be created. There is no byte-copy fallback in
the APFS implementation. This makes successful view creation itself evidence
that file data was cloned copy-on-write, but the repository also contains an
opt-in volume-level allocation check.

Run the current platform-neutral benchmark on an otherwise quiet writable APFS
volume:

```console
$ RIFTRI_BENCHMARK_OUTPUT=riftri-benchmark.json \
  cargo test -p riftri-core --test native_cow_benchmark \
  reports_cold_cached_and_private_write_costs -- \
  --ignored --exact --nocapture
```

See [Native COW benchmark](benchmarks.md) for the JSON fields, Linux and Windows
commands, CI artifacts, and interpretation guidance.

The fixture writes a 32 MiB pseudo-random tracked file, creates the first
worktree to prime the immutable base, records available bytes on the APFS
volume, and then creates a second view from the cached base. It fails if the
second view consumes 25% or more of its logical file size.

## Recorded development run

On 2026-09-08, an arm64 Mac running macOS 26.2 on APFS reported:

| Measurement | Bytes |
|---|---:|
| Cached view logical payload | 33,554,432 |
| APFS volume growth | 49,152 |
| Growth as a share of logical payload | 0.146% |

This single run is evidence for the implementation and acceptance fixture, not
a universal performance promise. APFS metadata, unrelated filesystem activity,
file count, clone fragmentation, and later private writes all affect allocation.
For that reason the volume-level test is ignored in the normal deterministic
suite and is run explicitly when validating storage behavior.

The regular integration suite separately proves that both views are clean real
Git worktrees, edits in one view do not alter the other view or immutable base,
and symlink/executable semantics survive materialization. Those checks prevent a
small allocation number from masking an unusable or incorrectly shared view.
