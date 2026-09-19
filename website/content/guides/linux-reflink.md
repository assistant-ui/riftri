# Linux reflinks

Riftri supports **Btrfs** and **reflink-enabled XFS**. Reflinks let worktrees
share unchanged file data while keeping edits private.

## Check and create

From your repository:

```sh
riftri doctor --destination ../app-auth
riftri worktree add ../app-auth -b feature/auth main
```

Creation actively checks reflink support on the destination before changing
Git metadata. XFS may need that active check even when the read-only diagnostic
cannot confirm support.

## Keep in mind

- Riftri state and the new worktree must be on the same filesystem volume.
- Every file must use the supported native clone operation; failures do not
  silently fall back to a full copy.
- Normal reads, writes, builds, and Git commands use the native filesystem.

If reflinks are unavailable, [OverlayFS](linux-overlayfs.md) may work.
Riftri checks that separately; the OS name alone does not establish support.
