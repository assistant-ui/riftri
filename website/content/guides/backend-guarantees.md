# Backend guarantees

A successful Riftri creation gives you:

- A real linked worktree registered with Git.
- The requested committed files and a clean initial Git status.
- Native storage sharing for unchanged data.
- Private edits that do not change the base or sibling worktrees.
- Recorded lifecycle operations that can be inspected and recovered.

Unsupported configurations stop with an explanation rather than weakening
these guarantees.

## Filesystems differ

| Backend | How it shares storage |
| --- | --- |
| APFS | Cloned file-data blocks |
| Btrfs / reflink-enabled XFS | Shared file extents |
| OverlayFS | A shared base with a private writable layer |
| ReFS | Aligned file extents; small tails may be copied |

Not every backend supports every maintenance operation. Mounted OverlayFS
moves and compaction, and sparse-worktree compaction, are not supported.

## What is not promised

Riftri does not copy uncommitted source changes, untracked dependencies, or
arbitrary source-directory metadata. It does not promise faster creation,
fixed space savings, or a security boundary.

Start with [filesystem compatibility](filesystem-compatibility.md).
Use **Agent .md** for the complete metadata and lifecycle support matrix.
