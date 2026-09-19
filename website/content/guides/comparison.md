# Is Riftri right for you?

Use Riftri when you keep several worktrees of a repository and want unchanged
files to share storage on a supported filesystem.

| Choose | When you need |
| --- | --- |
| Riftri | Real Git worktrees with shared working-file storage |
| `git worktree` | The simplest setup, without Riftri's storage requirements |
| Separate clones | Independent repositories and branch state |
| Containers or VMs | Process isolation and a security boundary |

## Compared with Git worktrees

Git worktrees already share repository history. Riftri also shares unchanged
working-file data. Branches, commits, and everyday Git commands stay the same.

For a small repository or only a few worktrees, ordinary Git may be all you
need. Riftri does not guarantee faster creation, and dependencies and build
outputs still take their own space.

## Compared with copying a folder

A filesystem clone can share file data, but copying `.git` does not register a
new linked worktree correctly. Riftri handles real Git registration, clean
checkout verification, and recoverable cleanup.

## When to skip it

Use ordinary Git if your filesystem or repository features are unsupported.
Use a sandbox, container, or VM for untrusted code; Riftri does not replace one.

[Check compatibility](filesystem-compatibility.md) or
[install Riftri](installation.md) to try one worktree.
