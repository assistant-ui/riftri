# Backend guarantees

A successful Riftri creation gives you:

- A real linked worktree registered with Git.
- The requested committed files and a clean initial Git status.
- Native storage sharing for unchanged data.
- Private edits that never change the base or sibling worktrees.
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

Mounted OverlayFS moves and compaction, and sparse-worktree compaction, are
not supported.

## Not promised

Copying uncommitted changes, untracked dependencies, or source-directory
metadata; faster creation; fixed space savings; a security boundary.

Start with [filesystem compatibility](filesystem-compatibility.md). For the
complete metadata and lifecycle support matrix, open **View .md**.
