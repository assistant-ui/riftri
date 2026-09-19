# Is Riftri right for you?

Use Riftri when you keep several worktrees of a repository and want unchanged
files to share storage on a supported filesystem.

| Choose | When you need |
| --- | --- |
| Riftri | Real Git worktrees with shared working-file storage |
| `git worktree` | The simplest setup, without Riftri's storage requirements |
| Separate clones | Independent repositories and branch state |
| Containers or VMs | Process isolation and a security boundary |

## The differences

- Git worktrees already share repository history; Riftri also shares
  unchanged working-file data. Branches, commits, and everyday Git commands
  stay the same.
- Riftri does not guarantee faster creation, and dependencies and build
  outputs still take their own space; for a small repository or a few
  worktrees, ordinary Git may be enough.
- Copying a folder can share file data, but a copied `.git` is not a
  correctly registered linked worktree. Riftri handles real registration,
  clean-checkout verification, and recoverable cleanup.
- Riftri is not a sandbox; run untrusted code in a container or VM.

[Check compatibility](filesystem-compatibility.md) or
[install Riftri](installation.md) to try one worktree.
