# How Riftri works

Riftri changes how worktree files are stored. Git still owns branches,
commits, history, and worktree registration.

## Create once, share unchanged data

1. **Check support.** Riftri checks the requested checkout and destination.
2. **Prepare a base.** It builds or reuses an immutable copy of the exact Git tree.
3. **Create a worktree.** Git registers a real linked worktree, and the filesystem
   shares unchanged data from the base. Riftri verifies that Git reports it clean.

Compatible worktrees can reuse a base. Different trees, checkout settings,
repositories, or volumes do not accidentally share one.

## Edits stay private

On APFS, Linux reflink filesystems, and ReFS, native copy-on-write keeps
changed blocks private. OverlayFS stores changes in a separate writable layer.

Riftri is not in the normal file read/write path. Your editor and build tools
work directly with the filesystem. No always-running daemon is required.

## Cleanup is recorded

Riftri records lifecycle operations so an interruption can be recovered.
It removes shared bases only when nothing still references them. If ownership
or contents are uncertain, it stops and preserves the data.

See [safety](safety.md) for day-to-day precautions. **Agent .md** contains
the full component design, storage layout, and transaction state machines.
