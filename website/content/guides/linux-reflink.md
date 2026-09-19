# Linux reflinks

Riftri supports **Btrfs** and **reflink-enabled XFS**: worktrees share
unchanged file data while edits stay private.

## Check and create

From your repository:

```sh
riftri doctor --destination ../app-auth
riftri worktree add ../app-auth -b feature/auth main
```

Creation actively verifies reflink support before changing Git metadata; XFS
may need that even when the read-only diagnostic cannot confirm support.

## Requirements

- Riftri state and the new worktree must be on the same filesystem volume.
- Every file must clone natively; failures never silently fall back to a full copy.

No reflinks? [OverlayFS](linux-overlayfs.md) may work; Riftri checks it
separately — the OS name alone does not establish support.
