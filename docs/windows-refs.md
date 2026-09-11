# Windows ReFS support

Riftri's experimental Windows backend creates real Git linked worktrees backed
by native ReFS block clones. It is available only when the repository,
destination, and Riftri state all resolve to the same writable ReFS volume.

## What Riftri verifies

Before mutating Git metadata or Riftri state, the active destination probe:

1. Confirms that the destination is on writable ReFS.
2. Creates two temporary delete-on-close files on that volume.
3. Calls `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for an aligned region.
4. Changes the cloned destination and confirms that the source is unchanged.

If any check fails, optimized creation stops. Riftri never silently creates a
normal full-copy worktree.

## Block-clone behavior

ReFS requires clone offsets and lengths to align to filesystem clusters and
limits each clone request to less than 4 GiB. Riftri temporarily marks each
destination sparse before extending it so that pre-sizing does not allocate a
full file of zero-backed clusters. It then clones valid aligned requests,
restores the source's sparse status, and preserves its integrity-stream setting.
The final unaligned tail of a file—less than one cluster—is copied because the
Windows API cannot block-clone that region. A failure while cloning an aligned
region aborts and rolls back the worktree.

Small files below one cluster may therefore be copied in full. Larger files
share their aligned physical blocks and allocate private blocks as a worktree
changes.

## Usage

Use the explicit command from PowerShell or Command Prompt:

```console
> riftri doctor --destination ..\app-auth
> riftri worktree add ..\app-auth -b feature/auth main
```

Process-scoped Git interception works for tools that spawn the Git executable:

```console
> riftri enable
> riftri exec -- claude
```

Inside that process tree, supported `git worktree add` commands for the enabled
repository use ReFS block cloning. Other Git commands pass through to the real
Git executable. Riftri does not install or edit a PowerShell profile.

## Current limits

- NTFS is not an optimized backend and fails closed.
- The checkout profile remains intentionally strict: Git LFS, custom filters,
  sparse checkout, submodules, and external attributes are rejected.
- A Windows account needs permission to create any symlink present in the tree.
- The implementation is experimental; keep important work committed or backed
  up and use `riftri status` and `riftri repair` when diagnosing interrupted
  lifecycle operations.

The ReFS API behavior and restrictions are documented by Microsoft in
[Block cloning on ReFS](https://learn.microsoft.com/windows-server/storage/refs/block-cloning)
and
[`FSCTL_DUPLICATE_EXTENTS_TO_FILE`](https://learn.microsoft.com/windows/win32/api/winioctl/ni-winioctl-fsctl_duplicate_extents_to_file).

CI creates a disposable ReFS VHDX and runs the shared Git correctness,
private-write isolation, recovery, cleanup, and physical-allocation tests on
every change.
