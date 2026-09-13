# Direct-download CLI: ten-agent assistant-ui experiment

## Result

The public macOS ARM64 `riftri 0.1.1` binary created ten isolated worktrees
using **87.0% less new volume allocation** than ordinary Git in the initial
creation comparison. It was **slower to create them** in this run. Ten distinct
coding agents subsequently edited and tested their own views successfully.
This is one local experiment, not a general speed claim.

| Initial creation measurement | Ordinary Git | Riftri / APFS |
| --- | ---: | ---: |
| Worktrees | 10 | 10 |
| Tracked files per view | 5,346 | 5,346 |
| Logical tracked bytes per view | 63,493,268 | 63,493,268 |
| New volume allocation, all ten | 811,606,016 B (774.01 MiB) | 105,504,768 B (100.62 MiB) |
| First worktree creation | 0.89 s | 3.98 s, including cold base |
| Following nine, launched concurrently | 9.01 s | 18.46 s |
| Creation time, excluding allocation-sampling pauses | 9.90 s | 22.44 s |

Riftri's first view plus immutable base used 78.45 MiB. The following nine
added 22.17 MiB in total, or 2.46 MiB per cached view on average. Status reported
one retained base with ten references, ten active APFS views, and no pending
operations or state issues. Both approaches began with clean Git worktrees.

After the ten agents' edits and tests, the measured additional volume growth
was 258,048 bytes (0.246 MiB). This includes filesystem/Git metadata and is not
an exact count of exclusive private-file bytes or peak test-process usage.

## Source and important compatibility caveat

The local `assistant-ui` repository supplied source commit
`8723a15e2adb4a00f0fd44abbb823ea09295e771`, tree
`2c897d97c7bb848aa321d568fb74dc56a9cac01d`. It had 5,346 tracked files and
approximately 60.6 MiB of tracked content; dependency directories are not part
of a normal Git checkout and are not included in the savings.

Its shallow/partial history could not be cloned locally because required
historical objects were missing. `git archive` successfully exported the exact
requested source tree. Importing that archive into an independent disposable
repository reproduced the original tree ID before any adjustment. The user's
original checkout, branches, configuration, and uncommitted files were not
changed by the experiment.

**The unmodified source was rejected by Riftri.** Its `.gitattributes` contains
`pnpm-lock.yaml linguist-generated`, which is outside the current deterministic
checkout allowlist. An attempted optimized add failed before creating its
destination. Only that GitHub display hint was removed in the disposable
fixture; `* text=auto eol=lf` was retained. Both storage lanes used the same
adjusted tree, `faf5d0cac6b381500f53677876ab7a4191260f9c`.

These results therefore do not certify the unmodified repository as compatible.
Supporting verified checkout-neutral GitHub metadata attributes is a concrete
follow-up; disabling arbitrary attribute validation is not the fix.

## Distribution and test method

- Downloaded the macOS ARM64 archive from the public
  [v0.1.1 release](https://github.com/assistant-ui/riftri/releases/tag/v0.1.1),
  verified its SHA-256 before extraction, and executed that binary. Its binary
  SHA-256 was `db9738daac94e6a299ad829d09c80cf62a5ce87a9bb1b87bb6bda1f8e028b398`.
- The six public archive URLs all returned HTTP 200 and matched `SHA256SUMS`.
  Only macOS ARM64 was executed locally; the original tagged CI run had
  built and smoke-tested all six targets.
- Executed the Unix installation guide as written, installing the CLI in a
  previously unused per-user executable path. No npm launcher, shell-profile
  modification, administrator install, or security-policy bypass was needed.
  The PowerShell installation example was not executed on this Mac.
- Used a dedicated 2 GiB APFS sparsebundle on the same Mac, with indexing
  disabled by its `.metadata_never_index` marker. The host had limited free
  disk space, so a media-heavy repository or a ten-way dependency installation
  was not used.
- Measured volume allocation as `(statfs.blocks - statfs.bfree) * statfs.bsize`
  after `sync`, with four samples 300 ms apart. Each initial comparison and
  post-agent measurement had four identical samples. Per-file `du`/allocated
  totals count shared extents repeatedly and were not used to claim savings.
- Created one worktree, then nine concurrently in each lane. Removed the ten
  clean ordinary worktrees before measuring Riftri. Riftri adds used ordinary
  `git worktree add --detach` inside `riftri exec` in the enabled fixture.
- OS caches were not flushed. "Cold" means no existing Riftri base, not cold
  hardware caches. Background host work, including a brief overlapping Rust
  validation run, means wall times are observations rather than isolated
  performance thresholds.

## Actual agent work

Ten separate coding agents ran in batches of at most three, not ten
simultaneous model sessions. All ten worktrees existed together. Each agent
read the project instructions, ran an existing project test suite, added one
test to its own tracked test file, appended a unique marker to its own tracked
README, and reran the suite through `riftri exec --worktree` with Node 24.19.0.

| Agent | Project test suite | Baseline passed | Final passed |
| --- | --- | ---: | ---: |
| 01 | Workspace dependency ranges | 12 | 13 |
| 02 | Unmanaged dependency pins | 14 | 15 |
| 03 | Changeset checks | 18 | 19 |
| 04 | Changeset semantic-version cascade | 27 | 28 |
| 05 | Built declaration checks | 8 | 9 |
| 06 | Workspace manifest helpers | 4 | 5 |
| 07 | Changeset checks, separate contract | 18 | 19 |
| 08 | Workspace ranges, separate contract | 12 | 13 |
| 09 | Unmanaged pins, separate contract | 14 | 15 |
| 10 | Workspace helpers, separate contract | 4 | 5 |
| Total test executions across agents | | 131 | 141 |

The totals include repeated suites in different views; they are not counts of
unique tests. Final runs had zero failures. No product code was changed.

One initial declaration run failed because the ambient system TypeScript was
too old for `moduleResolution: Bundler`; using the project's existing 7.0.2
compiler read-only produced the passing baseline and final runs above. Agent
07's first API-surface workload required absent `tsdown`, so that failure was
recorded and a dependency-free changeset workload substituted. No dependencies
were installed or shared as writable workspace directories. This experiment
does not claim a full monorepo build, application-server run, or ten independent
dependency/build-cache installations. Tests' temporary fixture directories
outside the benchmark volume were not included in its allocation readings.

## Isolation, replay, and cleanup

- Every README contained only its own agent marker. The fixture source and
  immutable-base READMEs remained unchanged; a full tracked-content comparison
  of the base against the fixture commit also passed.
- A normal Git removal through Riftri refused a dirty view and preserved its
  edits. Garbage-collection planning found no eligible base while views used it.
- Saved all ten patches and replayed the identical edits on ten newly created
  ordinary Git worktrees. All 141 final test executions passed there too.
  The replay's volume samples drifted during measurement; its allocation and
  timing figures are excluded from the headline comparison.
- Preserved patches and logs, verified there were no unknown or untracked
  edits, reversed only the saved patches, and removed all twenty comparison
  worktrees without force. Explicit garbage collection removed the one
  unreferenced base. Final status had zero active views, retained bases,
  pending operations, or state issues; repair required no manual attention.

## Follow-up optimization work

1. Validate a narrowly defined set of checkout-neutral GitHub attributes so
   real projects like this do not require fixture adjustments.
2. Profile creation latency on a quiet host: base integrity hashing, filename
   preflight, per-file cloning, index synchronization, and coordination waits
   are candidate phases, not proven bottlenecks from this experiment. Preserve
   integrity and dirty-worktree checks when optimizing them.
3. Add a larger, repeatable workload with independent dependency installs and
   build outputs once disk capacity permits. Report private growth and peak
   usage separately from the shared tracked-source saving.
