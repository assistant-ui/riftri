# Riftri architecture

## Objective

Riftri makes ordinary Git linked worktrees copy-on-write. It is not a source
control system and it does not introduce a second workspace abstraction.

The central rule is:

> Riftri participates in setup and cleanup; the native filesystem handles the
> normal read/write path.

## Opt-in modes

Riftri is never a global Git replacement by default.

1. Explicit operation: `riftri worktree add ...`
2. Process-scoped activation: `riftri exec -- claude`
3. Optional repository-scoped shell integration

The process-scoped mode places a small Git shim at the front of `PATH` only for
the selected process and its children. Normal Git commands are immediately
delegated to the real Git executable. Worktree lifecycle commands are routed
through Riftri.

## Components

### CLI and Git shim

The CLI exposes diagnostics and explicit operations. The optional shim preserves
the `git worktree` user experience inside an enabled process.

The npm distribution layer does not implement product behavior. Its launcher
selects an exact platform package, executes the Rust CLI, and preserves the
native process result. Platform packages are optional dependencies so npm only
installs the applicable binary. Missing or unsupported native packages fail
visibly; the launcher never substitutes an ordinary JavaScript implementation.

### Git coordinator

The coordinator invokes the installed Git executable. Git remains responsible
for linked-worktree metadata, per-worktree `HEAD` and index files, branch rules,
configuration, filters, and hooks.

### Immutable base cache

A base represents the checked-out files for an exact Git tree and checkout
profile. Bases are immutable and shared by compatible worktrees. They are keyed
by repository identity, tree object ID, checkout profile, and filesystem volume.

Repository identity is the platform-native path to Git's common directory, so a
main worktree and all linked worktrees resolve to the same repository. Object IDs
are validated as SHA-1 or SHA-256 hex values returned by the real Git process.

The checkout profile is a versioned, canonical sequence of raw byte key/value
inputs. The collector must account for every external input that can change
checkout results, including line-ending configuration, external attributes,
filter drivers, and sparse-checkout configuration. In-tree attributes are
already covered by the exact tree ID. If Riftri cannot account for an active
external input, it must reject optimized creation rather than reuse an
ambiguous base.

### Storage engine

The storage engine selects the fastest supported native primitive:

- macOS: APFS clones.
- Linux: reflinks where supported, otherwise kernel OverlayFS.
- Windows: ReFS block cloning where supported, with later native alternatives.
- Unsupported configuration: explicit ordinary-worktree fallback.

FUSE and network filesystems are not part of the default hot path.

Capability results have three meanings:

- `supported`: read-only checks establish that the destination volume supplies
  the primitive;
- `unsupported`: the inspected destination is incompatible; and
- `unavailable`: Riftri cannot establish a reliable answer without a later
  active probe or because inspection failed.

The destination itself may not exist. Probing uses its nearest existing ancestor
to identify the volume. A full-copy result is always marked as requiring an
explicit fallback policy, even when the volume could hold an ordinary Git
worktree.

### State and recovery

Local metadata records bases, views, mounts, reference counts, and incomplete
operations. SQLite is the planned registry; small per-operation journals provide
recovery breadcrumbs if creation is interrupted.

## Worktree-add transaction

For an enabled `git worktree add <path> <ref>` operation, the intended sequence is:

1. Resolve the requested revision and Git tree using real Git.
2. Validate the destination and probe its filesystem capabilities.
3. Create the real linked-worktree metadata with checkout suppressed.
4. Find or build an immutable base for the exact Git tree.
5. Create a private writable layer or native clone at the requested path.
6. Restore the linked worktree's `.git` pointer in the visible view.
7. Populate or refresh the per-worktree index.
8. Verify that `git status` reports the expected clean state.
9. Atomically record the operation as active.

Failures are rolled back from an operation journal. Riftri must not silently
fall back to a full copy unless the user explicitly allows that policy.

## Add-operation journal state machine

The version 1 add journal advances through these durable states:

```text
intent-recorded
  -> git-metadata-created
  -> base-ready
  -> view-created
  -> git-pointer-restored
  -> index-synchronized
  -> clean-verified
  -> active
```

Every incomplete forward state may transition to `rollback-pending`, followed
by `rolled-back`. `active` and `rolled-back` are terminal for an add operation;
removal will use a separate transaction.

Before Milestone 2 performs its first mutation, journal persistence must write
the intent first and replace each state atomically using a temporary file,
`fsync`, rename, and parent-directory `fsync` where the platform supports them.
The persisted path encoding must round-trip platform-native paths, including
non-UTF-8 Unix bytes. Recovery may repeat the cleanup associated with a recorded
state, so every rollback action must be idempotent and validate its exact target.
SQLite registration remains a Milestone 3 concern; the per-operation journal is
the crash-recovery authority while an add is incomplete.

## Fast path

Once a worktree is ready, Riftri is absent from ordinary file operations:

```text
agent -> native filesystem -> shared base/private changes
```

It must not become:

```text
agent -> userspace Riftri callback -> filesystem
```

For normal Git commands, the shim should directly execute the real Git process
without opening Riftri state. Cached bases are built once and reused by
concurrent worktree requests.

## Safety constraints

- Never use a mutable working directory as a shared lower layer.
- Never delete a worktree without respecting Git's dirty-worktree safeguards.
- Never silently create a full copy when the selected policy requires COW.
- Never run the complete Riftri process as root.
- Never place local state or writable SQLite WAL files on a network filesystem.
- Treat submodules, sparse checkout, filters, and Git LFS as explicit
  compatibility features with safe fallback behavior.

## Initial delivery sequence

1. Read-only diagnostics and storage capability model. (complete)
2. Explicit APFS worktree creation on macOS.
3. Process-scoped Git shim.
4. Linux reflink and OverlayFS backends.
5. State recovery, compaction, and compatibility expansion.
6. Windows native backends.
