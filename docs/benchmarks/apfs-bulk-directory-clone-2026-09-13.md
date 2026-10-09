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

## Writable follow-up, October 9

The original result excluded writable-permission restoration. A new test-only
helper includes a complete entry-type walk before mutation, a recursive native
clone, restoration of each file's actual writable mode, the same bounded
deferred hints as the current candidate, and final directory modes. It remains
inside the storage crate's test module: **the CLI cannot select it**.

The mode-parity regression first failed with the raw recursive clone and then
passed with restoration. Tests also cover private-write isolation, unsupported
entries rejected before destination creation, and the remaining directory-xattr
difference. This is not a complete metadata eligibility policy: inherited ACLs,
directory xattrs, flags and owner/group behavior need an explicit compatibility
decision and tests before any production path can change.

Eight alternating pairs on the local Apple M1/APFS host used a read-only base
with 4,097 regular files, 32 flat subdirectories, one symlink and 32 MiB plus
18 bytes of regular-file payload. Compilation finished before measurement.

| Measured storage path | Median |
| --- | ---: |
| Current per-file clone, permissions and hints | 218.88 ms |
| Bulk clone with entry preflight, permissions and hints | 140.11 ms |

The bulk path was **35.99% faster in this storage-phase fixture**, winning
**8/8** pairs. All pairs matched bytes, modes, symlinks and distinct file inode
identities. Private writes to each bulk view left both its source and the
per-file view unchanged. The [retained record](apfs-writable-bulk-2026-10-09.json)
contains every sample, the exact test-only source patch, executable/log
checksums and the unresolved correctness gates.

This does **not** measure Git initialization, base verification, journal writes,
end-to-end startup, real-project directory shapes or physical volume allocation.
It is one warm-source machine, not an independent-runner result. The extra
directory-metadata eligibility work is not implemented or included. The earlier
9.9-times readonly result must not be presented as a writable startup speedup.
Keep the current production path until those gaps are resolved.

The platform contract adds a separate reason not to promote this prototype.
[Apple's `clonefile` documentation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/clonefile.2)
discourages cloning directory hierarchies directly. Its recommended
[`copyfile` API](https://github.com/apple-oss-distributions/copyfile/blob/main/copyfile.3)
supports recursive best-effort cloning, but not recursive `COPYFILE_CLONE_FORCE`;
best-effort cloning permits a byte-copy fallback, contrary to Riftri's contract.
Metadata parity tests alone therefore would not justify adopting this bulk
path. Treat it as a diagnostic comparison. The next candidate is reusing bounded
directory handles with per-file `clonefileat`, whose documented semantics match
per-file `clonefile` while allowing directory-relative paths. That candidate
has not been implemented or measured here.

```sh
cargo test --release --locked -p riftri-storage --lib \
  apfs::tests::reports_writable_bulk_directory_clone_comparison -- \
  --ignored --exact --nocapture
```
