# APFS bulk directory clone evaluation

Date: 2026-09-13

This experiment compares Riftri's production APFS tree builder, which creates
directories explicitly and invokes native `clonefile` once per regular file,
with one recursive `clonefile` call for the whole directory. The candidate is
compiled only for tests and cannot be selected by the CLI.

## Correctness result

The deterministic test fixture contains nested directories, regular and
executable files, and a relative symlink. Both implementations preserve the
supported entry types, bytes, Git-relevant mode bits, and symlink target. A
write to the bulk-cloned view leaves the source unchanged, and source and view
files have different inode identities.

There is also an intentional regression test for a material difference: a
custom extended attribute on the source root is copied by the recursive call,
while Riftri's current directory construction does not copy it. This is enough
to keep the candidate out of the production backend. A future production change
would need an explicit directory-metadata policy and a complete preflight that
rejects unsupported entry types before the destination is created.

## Local measurements

The release-mode benchmark used an arm64 Mac running macOS 26.2 on APFS. Each
run cloned 2,049 files with 8,388,626 logical bytes. The five measured pairs
were run consecutively after compilation:

| Run | Per-file clone (µs) | Bulk directory clone (µs) |
| ---: | ---: | ---: |
| 1 | 145,647 | 14,955 |
| 2 | 198,886 | 23,297 |
| 3 | 154,447 | 16,589 |
| 4 | 150,362 | 13,031 |
| 5 | 159,841 | 15,544 |
| Median | 154,447 | 15,544 |

The local median for the bulk candidate was about 9.9 times faster. Both views
reported 8,392,704 allocated bytes through `st_blocks`. That accounting can
count shared blocks and is a parity observation, not exclusive physical usage.
These timings are evidence for further work, not a cross-machine performance
promise or a pass/fail threshold.

## Reproduce

Run on a writable APFS volume:

```console
$ cargo test --release -p riftri-storage --lib \
  apfs::tests::reports_bulk_directory_clone_comparison -- \
  --ignored --exact --nocapture
```

The ordinary storage test suite also runs the small behavior-parity and
directory-xattr cases without the ignored benchmark.

## Decision

Keep the per-file implementation as the production path. The bulk candidate is
promising enough to retain as a repeatable benchmark, but it must not ship until
Riftri can prove its full entry-type and directory-metadata policy before
mutation and retain the existing cleanup and private-write guarantees.
