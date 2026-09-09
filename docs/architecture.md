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
2. Repository opt-in plus process-scoped activation: `riftri enable`, then
   `riftri exec -- claude`, optionally with `--worktree <path>`
3. Repository opt-in plus explicit shell activation: evaluate
   `riftri shell hook zsh`, then use normal `git` commands

The process-scoped mode places a small Git shim at the front of `PATH` only for
the selected process and its children. Repository consent is stored in local Git
configuration as `riftri.enabled=true`. Normal Git commands and all commands in
repositories without that marker are immediately delegated to the exact real
Git executable resolved before the shim is installed. Supported worktree adds
in enabled repositories are routed through Riftri. `RIFTRI_BYPASS=1` provides an
explicit escape hatch to ordinary Git without disabling the repository.

`riftri exec --worktree <path> -- <command>` is an agent-neutral convenience
for binding the child process to an existing worktree. Riftri canonicalizes the
path, requires it to equal the repository root, and confirms that root is a live
entry in Git's structured worktree inventory before launching the command. The
binding does not imply repository enablement and does not create a new workspace
abstraction.

The shell hook installs a versioned shim link in the user's cache and prints
Bourne-compatible environment assignments. Evaluating those assignments puts
the shim first on `PATH` for that shell and its descendants. The hook is never
evaluated automatically and Riftri never edits shell startup files. Once it is
active, `riftri enable` and `riftri disable` are the repository-specific switch;
disabled repositories and commands outside repositories still delegate to the
real Git executable captured before `PATH` changes. A user may deliberately put
the hook evaluation in a shell profile, which globally activates the shim for
that user's new shells, but this does not globally enable optimization:
repository-local consent is still required.

The shim accepts optimized `worktree add` with `-b <new-branch>` or `--detach`
and routes the ordinary no-option `worktree remove <path>` and `worktree move
<source> <destination>` forms through Riftri when the target has an active
Riftri add journal. A no-option `worktree prune` first verifies that every
managed view is present and registered. Move and prune progress is durable and
recoverable. Unsupported add forms fail before mutation instead of silently
falling back to a full checkout. Lifecycle options continue to use Git for
unmanaged worktrees but fail closed when they could mutate managed Riftri state
outside a supported journal.

## Components

### CLI and Git shim

The CLI exposes diagnostics and explicit operations. The optional shim preserves
the `git worktree` user experience inside an activated process or shell while
repository-local configuration controls whether an add is optimized.

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

Versioned JSON add-operation journals are written atomically and preserve native
path units. They are the current recovery authority for the APFS prototype.
SQLite remains the planned Milestone 3 registry for bases, views, mounts, and
reference counts.

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
by `rolled-back`. `active` and `rolled-back` are terminal for an add operation.

## Removal-operation journal state machine

Removal uses separate journals under `removals/`:

```text
intent-recorded
  -> clean-verified
  -> worktree-removed
  -> base-released
  -> complete
```

Riftri validates cleanliness before recording intent and rechecks cleanliness
when resuming before Git removes a still-registered view. Git performs removal
without `--force`, so a concurrent dirtying write is also rejected. A missing
view plus missing Git registration is treated as an idempotently completed
removal step. Inconsistent or changed paths are preserved for manual attention.

## Move-operation journal state machine

Managed moves use separate journals under `moves/`:

```text
intent-recorded
  -> worktree-moved
  -> add-journal-updated
  -> complete
```

Git performs the directory and administrative-metadata move. Recovery accepts
only an intact registered source or an intact registered destination, preserves
all inconsistent states, and atomically relocates the active add-journal
reference. The source and destination must be on the same APFS volume.

## Prune-operation journal state machine

Managed-state pruning uses separate journals under `prunes/`:

```text
intent-recorded
  -> git-metadata-pruned
  -> complete
```

Before invoking Git, Riftri verifies that every active managed view exists and
is registered and that no add, removal, or move is incomplete. Repeating Git's
prune after an interruption is safe because managed registrations are
revalidated first.

## Base-collection journal state machine

Garbage collection is read-only unless the user passes `--apply`. Each selected
zero-reference base then uses a separate journal under `collections/`:

```text
intent-recorded
  -> marker-removed
  -> base-quarantined
  -> complete
```

An operation may instead become `cancelled` if reference revalidation finds a
live or incomplete add. Collection takes the same per-base lock as construction,
reloads add and removal journals under that lock, and treats every non-rolled-back
add without a completed removal as a reference. The completion marker is removed
before the base is made writable for atomic quarantine, preventing another add
from reusing a base once collection starts. Recovery deletes only the exact
journaled quarantine path and leaves a newly rebuilt base untouched.

Before Milestone 2 performs its first mutation, journal persistence must write
the intent first and replace each state atomically using a temporary file,
`fsync`, rename, and parent-directory `fsync` where the platform supports them.
The persisted path encoding must round-trip platform-native paths, including
non-UTF-8 Unix bytes. Recovery may repeat the cleanup associated with a recorded
state, so every rollback action must be idempotent and validate its exact target.
SQLite registration remains a Milestone 3 concern; the per-operation journal is
the crash-recovery authority while an add is incomplete.

The APFS prototype builds bases only from exact Git objects through an isolated
temporary index. A SHA-256 repository bucket, tree object ID, restricted checkout
profile version, and same-volume state placement implement the base-key
boundaries. A per-base file lock serializes construction, atomic rename exposes
the finished tree, and a synced completion marker prevents reuse of a partially
prepared base.

Native `clonefile` is used for every regular file in an APFS view. An error is
returned if APFS cannot clone; there is no byte-copy path. Directory structure,
symlinks, and executable modes are preserved. The base is made read-only and the
cloned view restores owner write permission before Git index synchronization.

Recovery validates every recorded cleanup path. It removes a visible incomplete
view only when Git reports it clean or a byte/mode/symlink comparison proves it
still equals the immutable base. Otherwise it retains the view and journal for
manual attention.

Storage accounting is derived from add/removal journals and completion markers.
It reports active views, retained bases, per-base reference counts, logical
bytes, and filesystem-allocated bytes. Allocated bytes can include shared APFS
blocks and are not an exclusive-space measurement; the volume-delta benchmark
remains the authoritative sharing check. Bases reaching zero references remain
cached until the explicit garbage collector independently proves deletion is
safe and records its intent. The collector is never part of normal worktree file
access or Git command passthrough.
`riftri repair` resolves the repository's state directory and applies the same
conservative journal recovery as the explicit-state `riftri recover` command;
it does not infer or delete unjournaled paths.

Status also inventories the state layout itself. Valid journals explain their
temporary, staging, and quarantine paths; completion markers explain retained
immutable bases; object-ID lock files explain coordination metadata. Anything
else is reported as a state issue, as are active journals whose worktree, base,
or completion marker is missing or unsafe. This diagnostic pass is read-only:
neither repair nor garbage collection guesses that an unexplained path is safe
to delete.

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
2. Explicit APFS worktree creation on macOS. (complete)
3. Removal, recovery inventory, and disk accounting. (initial slice complete)
4. Process-scoped Git shim. (add/clean-remove slice complete)
5. Linux reflink and OverlayFS backends.
6. Compaction and compatibility expansion.
7. Windows native backends.
