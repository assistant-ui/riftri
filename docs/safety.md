# How Riftri stays safe

Riftri asks for real trust: it deletes worktree directories, intercepts
`git worktree` commands in opted-in repositories, and on Linux can install a
root-owned set-user-ID OverlayFS mount helper. This page collects the concrete
mechanisms that justify that trust — what Riftri refuses to do, how those
refusals are tested on real filesystems, and how the performance numbers are
kept honest. Every claim here is traceable to the linked documents, the CI
configuration, or the code.

Two boundaries up front, from the [security policy](../SECURITY.md): Riftri is
experimental, pre-release software, and its copy-on-write isolation is not a
security sandbox — untrusted processes still belong in a container, VM, or
operating-system sandbox.

## Fail-closed by design

Riftri's rule is that when an operation cannot be performed safely and
exactly, it stops with an explanation instead of degrading to a slower or
approximate result ([troubleshooting](troubleshooting.md)). Concretely:

- **No silent full copies.** If the destination lacks a supported
  copy-on-write primitive, or a checkout profile cannot be reproduced exactly
  (custom filters, external attributes, sparse checkout, submodules,
  non-canonical [Git LFS](git-lfs.md) setups), creation fails before any
  mutation. Riftri never substitutes an ordinary full checkout unless the
  user explicitly selects that policy
  ([architecture — safety constraints](architecture.md#safety-constraints)).
- **Every mutation is journaled.** Add, removal, move, prune, compaction, and
  base collection each advance through a durable state machine persisted with
  atomic write-`fsync`-rename steps. The journal is the crash-recovery
  authority: rollback actions are idempotent and validate their exact targets
  ([architecture — journal state machines](architecture.md#add-operation-journal-state-machine)).
- **Removal refuses dirty worktrees.** Clean removal revalidates cleanliness
  through Git's own non-`--force` path, so a concurrent write is rejected. An
  explicit `--force` first records an exact content snapshot, then re-hashes
  the view at the final delete boundary and during recovery; any change after
  consent stops deletion and preserves the view. Recreating a deleted path
  cannot turn old force consent into deletion of a new path
  ([CLI reference](cli.md), [architecture](architecture.md#removal-operation-journal-state-machine)).
- **Recovery preserves ambiguous state.** `riftri repair` removes an
  incomplete view only when Git reports it clean or a byte/mode/symlink
  comparison proves it still equals the immutable base; anything else is
  retained with its journal for manual attention. It never infers or deletes
  unjournaled paths, and `riftri status` reports unexplained state-directory
  entries instead of guessing they are disposable.
- **Garbage collection is read-only by default.** `riftri gc` only previews;
  `--apply` takes the same per-base lock as construction, revalidates
  references under that lock, and cancels the collection if a live or
  incomplete add still uses the base. A prune first verifies that every
  managed view is present and registered.
- **OverlayFS state is identity-checked.** Mounts record the Linux boot ID,
  mount-namespace device and inode, and kernel mount ID. Recovery unmounts or
  deletes private layers only when that full identity matches or is provably
  absent; a foreign mount at the destination is preserved
  ([Linux OverlayFS](linux-overlayfs.md)).
- **The elevated helper is deliberately narrow.** The complete Riftri process
  never runs as root. The optional helper installed by
  `sudo riftri overlayfs install-helper` accepts only a validated mount, an
  exact identity-checked unmount, and a journal-owned work-directory reset; it
  starts with an empty environment, requires every layer and mountpoint to be
  a real directory owned by the requesting UID, and mounts with
  `nodev,nosuid`. It cannot dispatch normal Riftri or Git commands.
- **Interception is opt-in twice.** The Git shim activates only for a
  selected process or shell, and optimization additionally requires
  repository-local `riftri.enabled` consent. Riftri never edits shell startup
  files, and `RIFTRI_BYPASS=1` is an explicit escape hatch to ordinary Git.
- **Failures are machine-readable.** Every command accepts `--json-errors`
  and emits one versioned failure receipt with a stable code, category,
  durable phase, cleanup disposition, and recovery guidance, so automation
  can react to a refusal instead of retrying blindly
  ([agent integration](agent-integration.md)).

## Verified on real filesystems

Copy-on-write behavior cannot be tested on a filesystem that does not have
it. GitHub runners default to filesystems without the primitives Riftri uses
— there is no APFS `clonefile`, `FICLONE` reflink, or ReFS block clone on a
stock runner temp directory — so a CI suite that stayed there would only ever
exercise the refusal paths. Instead, [`ci.yml`](../.github/workflows/ci.yml)
creates a disposable real volume per backend and runs the integration suites
and lifecycle smoke tests on it:

- **macOS/APFS**: an `hdiutil` APFS sparsebundle volume.
- **Linux reflink**: loop-mounted Btrfs and reflink-enabled XFS (`mkfs.xfs -m
  reflink=1`) images, with `RIFTRI_REQUIRE_REFLINK=1` so a skipped reflink
  path fails the job instead of passing quietly.
- **Linux OverlayFS**: a loop-mounted ext4 volume, exercised both inside an
  unprivileged user/mount namespace and — separately — from an ordinary shell
  through the actually installed set-user-ID helper, including the installed
  npm workflow. CI also simulates a reboot and proves `riftri repair`
  restores a dirty worktree without changing its base
  ([Linux OverlayFS](linux-overlayfs.md)).
- **Windows/ReFS**: a `diskpart`-created ReFS VHDX volume, with the job
  failing unless the volume really is ReFS.

The same conservatism applies at runtime: the Linux and Windows backends are
accepted only after an active probe succeeds on the actual destination volume
— an unnamed-file `FICLONE` check for reflinks, a real OverlayFS
mount/read/copy-up probe, and a delete-on-close
`FSCTL_DUPLICATE_EXTENTS_TO_FILE` check for ReFS — and a capability result
that cannot be established reliably is reported as `unavailable` rather than
assumed ([architecture](architecture.md#storage-engine),
[backend guarantees](backend-guarantees.md)).

## Honest benchmarks

The performance story is designed to be hard to flatter
([benchmark guide](benchmarks.md)):

- The benchmark drives the same public core transaction the CLI uses; there
  is no synthetic storage implementation.
- Timing values are reported as observations, not pass/fail thresholds. The
  deterministic correctness gate is about behavior: both worktrees clean and
  isolated, the base reused, and a cached 32 MiB view consuming less than 25%
  of its logical payload in new allocation.
- Savings are claimed from volume-level free-space deltas on a quiet
  disposable volume. Per-file accounted bytes can count shared blocks more
  than once and are explicitly not used as sharing evidence
  ([allocation evidence](allocation-evidence.md)).
- Unflattering results are published, not hidden: OverlayFS may allocate a
  full backing file on the first write because copy-up is file-granular, and
  the helper-backed benchmark reports `null` per-view accounting rather than
  relaxing the helper's root-owned permissions to obtain a metric.
- The headline [assistant-ui ten-agent experiment](benchmarks/assistant-ui-ten-agents-2026-09-12.md)
  records that creation was *slower* than ordinary Git in that run, that the
  unmodified repository was rejected by the checkout-profile allowlist and
  needed a one-attribute fixture adjustment, and that it is one local
  experiment, not a general speed claim.
- CI uploads a `native-cow-benchmark-*` JSON artifact for APFS, Btrfs, XFS,
  helper-backed OverlayFS, and ReFS on every run, so numbers are comparable
  without treating one shared hosted runner as a performance baseline.

## Supply chain

For a tool that deletes directories and installs a root-owned helper, the
build pipeline is treated as part of the safety surface:

- [`ci.yml`](../.github/workflows/ci.yml) runs `cargo-deny` (advisories,
  licenses, bans, sources) on every push and pull request, checks the
  workspace on the declared minimum supported Rust toolchain (1.88), and
  builds and tests with `--locked` so lockfile drift cannot slip in. All
  GitHub Actions are pinned to full commit SHAs.
- [`release.yml`](../.github/workflows/release.yml) verifies the exact
  expected asset set and every SHA-256 checksum before publishing, then
  attests build provenance for all release tarballs and `SHA256SUMS` with
  `actions/attest-build-provenance`. Publishing an existing release fails
  safely rather than replacing published assets. The npm publish job requests
  an OpenID Connect identity for npm trusted publishing; a bootstrap token is
  used only when one is explicitly configured.
- The [installers](install.md) verify SHA-256 and the binary version before
  installing to a per-user directory; they do not use sudo, edit shell
  profiles, or activate Git interception.
- A [scheduled lychee workflow](../.github/workflows/links.yml) checks every
  Markdown link, including repository blob links, so documentation claims do
  not silently go stale.

## Reporting a problem

Behavior that could delete, overwrite, expose, or cross-contaminate worktree
data should be reported privately; see the
[security policy](../SECURITY.md). For non-security failures, start with
[troubleshooting](troubleshooting.md) — a refused operation usually names the
exact fail-closed rule that triggered it.
