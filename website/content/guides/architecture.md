# How Riftri works

Riftri changes how worktree files are stored. Git still owns branches,
commits, history, and worktree registration.

## Create once, share unchanged data

1. **Check support** for the requested checkout and destination.
2. **Prepare a base**: an immutable copy of the exact Git tree, built or reused.
3. **Create a worktree**: Git registers a real linked worktree, the
   filesystem shares unchanged data from the base, and Riftri verifies Git
   reports it clean.

Only compatible worktrees reuse a base; different trees, checkout settings,
repositories, or volumes never share one.

## Edits stay private

APFS, Linux reflink filesystems, and ReFS keep changed blocks private through
native copy-on-write; OverlayFS stores changes in a separate writable layer.
Riftri is not in the normal read/write path and needs no daemon.

## Cleanup is recorded

Lifecycle operations are recorded so interruptions can be recovered. Shared
bases are removed only when unreferenced; uncertain ownership or contents
stop cleanup and preserve the data.

See [safety](safety.md) for precautions. For the full component design,
storage layout, and transaction state machines, open **View .md**.
