# Riftri roadmap

This roadmap orders work by correctness risk. A milestone is complete only when
its acceptance criteria pass on its supported platforms.

## Milestone 0: architecture scaffold — complete

Outcome: establish the repository without performing worktree mutations.

- Rust workspace and crate boundaries.
- Read-only Git repository diagnostics.
- Planned storage-backend reporting.
- Product and architecture documentation.
- Formatting, Clippy, and test quality gates.

## Milestone 1: capability and Git-semantics foundation — complete

Outcome: determine exactly what Riftri can safely do before creating files.

- Define the internal storage contract and capability result types.
- Probe the destination volume rather than assuming support from the OS name.
- Identify repositories by their common Git directory.
- Resolve commit and tree object IDs through real Git.
- Model checkout profiles and per-volume base keys.
- Parse stable, NUL-delimited Git worktree porcelain output.
- Add fixtures for normal, bare, unborn, detached, and linked-worktree states.
- Specify the operation journal and state transitions.

Acceptance criteria:

- All functionality is still read-only.
- Diagnostics distinguish supported, unsupported, and unavailable backends.
- Paths with spaces and non-UTF-8 bytes remain representable internally.
- Git discovery works from the main tree and a linked worktree.

## Milestone 2: explicit APFS prototype — complete

Outcome: create the first real COW-backed linked worktree through an explicit
Riftri command on a supported APFS volume.

- Implement tracked-file base materialization for an exact Git tree.
- Create linked-worktree metadata with checkout suppressed.
- Clone the immutable base with native APFS primitives.
- Restore the worktree `.git` pointer and synchronize its index.
- Verify clean state before reporting success.
- Refuse unsupported filters and configurations with a clear explanation.
- Keep full-copy fallback disabled unless explicitly requested.

Acceptance criteria:

- `git worktree list` recognizes the result.
- `git status` is clean immediately after creation.
- Editing one view does not modify the base or another view.
- Physical allocation demonstrates meaningful sharing.
- An interrupted creation can be rolled back or repaired.

The allocation methodology and recorded development result are documented in
[`docs/allocation-evidence.md`](docs/allocation-evidence.md).

## Milestone 3: removal, recovery, and accounting (complete)

Outcome: make the APFS prototype safe for repeated local use.

Clean managed worktrees can be
removed explicitly or through the enabled Git shim, removal intent and progress
are journaled separately from adds, interrupted removals resume conservatively,
`riftri repair` provides repository-aware conservative recovery, and `riftri
status` derives base reference counts plus logical and allocated bytes with
actionable lifecycle explanations. `riftri gc` plans zero-reference cleanup,
while `riftri gc --apply` revalidates references under the per-base lock and
uses a recoverable collection journal before deleting an immutable base.
Status now reports state paths that are unjournaled, structurally unsafe, or
missing despite an active journal, while preserving them for manual inspection.
Deterministic failure injection covers every recoverable persisted creation,
removal, and collection phase, including idempotent repeated recovery. All
Milestone 3 acceptance criteria are covered by the macOS integration suite.

- Implement explicit worktree removal while preserving Git dirty-state checks.
- Extend local state and operation journals to removal and base lifecycle.
- Add `status`, `doctor`, `repair`, and garbage-collection behavior.
- Track base reference counts.
- Report logical and physical storage use.
- Test failures at every creation and removal transition.

Acceptance criteria:

- Dirty worktrees are never silently deleted.
- Bases still in use are never collected.
- Cleanup is idempotent after a crash.
- Diagnostics explain every retained directory.

## Milestone 4: process-scoped transparent Git — complete

Outcome: let agents use ordinary `git worktree` commands without relying on a
prompt or skill.

Initial add/remove slice complete: repository-local `riftri enable` and
`riftri disable`, `riftri exec -- <command>`, optional exact-worktree process
binding, exact real-Git delegation, an explicitly evaluated sh/bash/zsh hook,
supported `worktree add`, clean managed `worktree remove`, managed
`worktree move`, and guarded `worktree prune` routing, and `RIFTRI_BYPASS=1`
are implemented. Move and prune have forward-only durable journals and
idempotent recovery. Shell status distinguishes hook activation from
repository consent, and explicitly evaluated deactivation restores the current
shell without editing a user's profile. Unsupported forced/configured lifecycle
forms remain fail-closed for managed state. The compatibility matrix covers
installed sh/bash/zsh shells, disabled repositories, brand-neutral child
process inheritance, Claude/Codex-named harnesses, passthrough standard I/O and
exit status, repeated activation, deactivation, base reuse, and clean real Git
worktrees. A manual, non-gating latency probe reports the global shim's
per-command cost without imposing a host-load-sensitive threshold.

- Add `riftri exec -- <command>` and explicit shell activation.
- Add a small Git shim scoped to the child process environment.
- Immediately delegate non-worktree Git commands to real Git.
- Route add, remove, move, prune, and repair through tested lifecycle behavior.
- Add an explicit bypass environment variable.
- Preserve stdout, stderr, signals, and exit statuses.

Acceptance criteria:

- Git outside an explicitly activated process or shell is untouched.
- Normal Git commands behave identically inside an activated process or shell.
- Claude, Codex, and a plain shell can create optimized worktrees without
  Riftri-specific prompts.

The repeatable matrix and global-activation boundaries are documented in
[`docs/global-activation.md`](docs/global-activation.md).

## Milestone 5: Linux native backends

Outcome: support common Linux development environments without putting FUSE on
the hot path.

The native reflink slice is implemented: Linux actively validates `FICLONE`
with unnamed files, creates Btrfs and reflink-enabled XFS views through the same
journaled lifecycle as APFS, exposes the selected backend, and runs Git
correctness and allocation checks on disposable Btrfs and XFS volumes in CI.
The initial OverlayFS capability slice actively proves mount permission,
copy-up, and lower-layer isolation against the destination volume without
leaving a mount in the caller's namespace. The storage layer can now prepare
journal-owned upper/work directories, mount directly at a requested merged
path, persist an exact boot/namespace/mount identity, reject foreign unmounts,
and recover a mount after its creator process exits. Core add journals can now
carry validated OverlayFS layout intent, a recovery token, the pre-mount boot
and namespace context, and an optional exact mount identity without breaking
older journals. A second active probe proves that the caller's current
namespace can host a persistent view, and a private recovery marker closes the
crash window between the mount syscall and mount-ID persistence. Transaction
execution and least-privilege activation remain open, so the milestone as a
whole is not yet complete.

The capability contract and remaining lifecycle boundary are documented in
[`docs/linux-overlayfs.md`](docs/linux-overlayfs.md).

- Add Btrfs/XFS reflink probing and creation.
- Add kernel OverlayFS capability probing. (complete)
- Add durable private-layer placement and identity-checked mount primitives.
  (complete)
- Record backward-compatible OverlayFS mount intent and identity in add
  journals and diagnose unowned private-layer roots. (complete)
- Add caller-namespace preflight and crash-safe mount adoption. (complete)
- Execute immutable lower, private upper/work, and merged mounts through the
  existing add and removal transactions.
- Add least-privilege mount activation and reboot recovery.
- Define fallback behavior when user namespaces or mounts are unavailable.

Acceptance criteria:

- The same Git correctness suite passes for reflink and OverlayFS views.
- Mount recovery survives process termination and reboot simulation.
- Backend choice is visible and never silently downgraded.

## Milestone 6: compatibility and lifecycle efficiency

Outcome: cover real-world repositories and long-lived worktrees.

- Git LFS and custom filter compatibility.
- Sparse-checkout profiles.
- Submodule policy and support.
- Symlinks, executable modes, xattrs, and case-sensitivity testing.
- Clean-worktree compaction onto a new immutable base.
- Shared dependency and build-cache guidance without sharing unsafe writable
  directories.

## Milestone 7: Windows and managed environments

Outcome: expand the same storage contract without changing the Git UX.

The first Windows-native slice is implemented: Riftri actively verifies ReFS
block cloning and private-write isolation on the destination volume, creates
real linked worktrees through the existing journaled lifecycle, and exercises
explicit and process-scoped Git commands on a disposable ReFS volume in CI.
Windows shell-profile integration, ordinary-NTFS alternatives, and managed
environments remain open, so the milestone as a whole is not yet complete.

- ReFS block-clone capability and implementation. (complete)
- Evaluate differencing VHDX and ProjFS only where native block cloning is
  unavailable.
- Managed VM or microVM integration.
- Temporary Git credentials and lifecycle callbacks.
- Usage metering based on compute time, retained private data, and egress.

Acceptance criteria:

- The shared Git correctness and crash-recovery suites pass on a real ReFS
  volume.
- An active destination probe verifies block cloning before mutation.
- Cached views consume materially less new physical space than their logical
  size.
- Unsupported Windows filesystems stop without a silent full-copy fallback.

## Deferred ideas

These are intentionally outside the early milestones:

- System-wide Git interception by default.
- A Riftri commit, merge, or pull-request workflow.
- A userspace filesystem as the primary backend.
- Automatic deletion of dirty or unpushed worktrees.
- Cross-machine writable worktree sharing over NFS.
