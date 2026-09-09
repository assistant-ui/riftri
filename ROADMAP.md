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

## Milestone 4: process-scoped transparent Git

Outcome: let agents use ordinary `git worktree` commands without relying on a
prompt or skill.

Initial add/remove slice complete: repository-local `riftri enable` and
`riftri disable`, `riftri exec -- <command>`, optional exact-worktree process
binding, exact real-Git delegation, an explicitly evaluated sh/bash/zsh hook,
supported `worktree add` and clean managed `worktree remove` routing, and
`RIFTRI_BYPASS=1` are implemented. Managed forced/configured removals, moves,
and prunes are guarded from unjournaled passthrough. The milestone remains
incomplete until the remaining lifecycle commands use safe journaled paths.

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

## Milestone 5: Linux native backends

Outcome: support common Linux development environments without putting FUSE on
the hot path.

- Add Btrfs/XFS reflink probing and creation.
- Add kernel OverlayFS capability probing.
- Add immutable lower, private upper, work, and merged-directory management.
- Add least-privilege mount support and restart recovery.
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

- ReFS block-clone capability and implementation.
- Evaluate differencing VHDX and ProjFS only where native block cloning is
  unavailable.
- Managed VM or microVM integration.
- Temporary Git credentials and lifecycle callbacks.
- Usage metering based on compute time, retained private data, and egress.

## Deferred ideas

These are intentionally outside the early milestones:

- System-wide Git interception by default.
- A Riftri commit, merge, or pull-request workflow.
- A userspace filesystem as the primary backend.
- Automatic deletion of dirty or unpushed worktrees.
- Cross-machine writable worktree sharing over NFS.
