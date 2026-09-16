# Native filesystem metadata compatibility

Riftri worktrees use native filesystem paths after creation. Metadata changes
must therefore stay private to one view just like file-content changes.
The normative cross-backend contract is in
[Backend guarantees and metadata profiles](backend-guarantees.md); this page
records the platform-specific metadata tests behind that contract.

## Certified behavior

The native integration matrix verifies the following behavior on APFS, Btrfs,
reflink-enabled XFS, and both direct and installed-helper OverlayFS views:

| Operation | Required result |
|---|---|
| Change a tracked executable bit | Git sees only that view as changed; the base and sibling retain their mode |
| Replace a tracked symlink | The replacement stays in that view; the base and sibling retain the original target |
| Add a user extended attribute | The attribute stays in that view and does not make Git report a content change |
| Edit tracked file data | The base and sibling retain the original bytes |
| Remove after restoring tracked Git state | The normal clean-removal lifecycle succeeds |

The installed OverlayFS helper test also begins with an 8 MiB tracked file and
asserts that permission restoration allocates less than 1 MiB in the private
upper layer. This guards the metadata-only copy-up behavior: unchanged file data
must remain shared until the first data write.

## Platform boundaries

Git does not store extended attributes, so Riftri does not promise to recreate
xattrs from a commit. The guarantee is that xattrs added while working remain
private to that worktree. The certified xattr names use the native user space:
`com.assistantui.riftri.private` on macOS and `user.riftri.private` on Linux.

Windows ReFS continues to run the shared content-isolation, Git-cleanliness,
allocation, lifecycle, and crash-recovery suite. Unix executable bits and Unix
xattrs do not have a direct ReFS equivalent. Windows symlink checkout remains
subject to Git's `core.symlinks` setting and the operating system privilege or
Developer Mode required to create symlinks; Riftri still fails closed when an
active checkout configuration is outside its supported profile.

Case folding and Unicode normalization are destination-filesystem properties.
A tree that is valid on a case-sensitive volume may be unrepresentable on a
case-insensitive one, so Riftri decides per destination instead of modelling
Unicode itself: any tree containing a non-ASCII path, or an ASCII path that
case-folds onto another, is replayed into a throwaway directory beside the
destination before anything is created. Two tree paths that the volume folds
together fail that replay, and the add stops with `cannot coexist on the
destination filesystem`.

This covers non-ASCII case aliases (`Ä.txt` against `ä.txt`) and both Unicode
normalization forms of one grapheme — composed `café.txt` (U+00E9) against
decomposed `café.txt` (U+0065 U+0301) — including collisions in directory
components rather than file names. A colliding directory pair is refused even when its leaf files
differ, because the checkout would otherwise place two distinct Git
directories in one filesystem directory. Identical rules apply to Windows.
Trees whose non-ASCII paths stay distinct on the destination are unaffected.
