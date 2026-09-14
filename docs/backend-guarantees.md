# Backend guarantees and metadata profiles

This document defines what Riftri means when it accepts an optimized worktree
creation. It separates guarantees shared by every backend from details that are
necessarily filesystem-specific. A backend is not supported merely because an
operating system exposes a related API: the destination and checkout must pass
the checks described here.

## Common optimized-worktree guarantee

Every successful optimized add guarantees all of the following:

1. **Real Git worktree.** The destination is a linked worktree registered and
   managed by the installed Git executable.
2. **Exact checkout input.** Its immutable base is keyed by repository, exact
   Git tree, supported checkout profile, and destination volume.
3. **Clean initial state.** Riftri refreshes the real per-worktree index and
   requires Git to report the new worktree as clean before activation.
4. **Native sharing.** The selected backend uses its declared native sharing
   primitive. Riftri never silently substitutes a normal full checkout.
5. **Private changes.** A write in one view cannot change its immutable base or
   another view. OverlayFS changes go to the view's private upper layer.
6. **Recoverable lifecycle.** Add, clean or explicitly forced removal, move,
   prune, compaction, and base collection mutations are authorized by validated
   paths and durable journals. Inconsistent or changed paths are preserved for
   inspection.
7. **Visible limitations.** Unsupported checkout inputs, volume capabilities,
   and lifecycle forms stop with an explanation instead of weakening the
   guarantee.

These are correctness guarantees, not a sandbox boundary. A process running as
the user can still modify repositories, worktrees, and Riftri state that the
user can access. Keep important work committed or backed up.

## Backend profiles

| Backend | Admission evidence | Sharing unit | Important boundary |
| --- | --- | --- | --- |
| macOS APFS | Writable APFS volume inspection; every regular-file `clonefile` must then succeed | File data blocks | Source, state, and view must share the required APFS volume; a clone failure rolls the add back |
| Linux reflink | Active `FICLONE` and private-write probe using unnamed files | File data extents | Only Btrfs and reflink-enabled XFS are accepted; every view file must reflink |
| Linux OverlayFS | Active mount, lower-read, copy-up, isolation, unmount, and cleanup probe in the usable mount context | Immutable lower tree plus a per-view upper layer | Direct mounts require capability in the caller namespace; the optional helper accepts only caller-owned validated layouts |
| Windows ReFS | Active aligned block-clone and private-write probe using delete-on-close files | Aligned file extents | Aligned clone failure rolls back; the final sub-cluster tail is copied because ReFS cannot clone it |

An APFS add has no separate artifact-clean probe today. Its read-only diagnosis
is based on the destination volume identity, and the mutation transaction treats
every actual native clone as a strict gate. Linux reflink, OverlayFS, and ReFS
perform the active probes shown above before Git or Riftri mutation begins.

ReFS files smaller than one cluster may contain no cloneable aligned region and
can therefore be copied in full. This is an explicit API boundary, not a silent
fallback from a failed block-clone request. Larger files share aligned extents.

## Metadata profile: `git-checkout-v1`

The current shared metadata profile is intentionally limited to information
needed to reproduce an accepted Git checkout:

| Entry or metadata | Creation requirement |
| --- | --- |
| Regular file | Exact post-checkout bytes for the requested tree and accepted attributes |
| Directory | Same tree shape with usable traversal permissions; final Git-relevant mode behavior is preserved on Unix |
| Symlink | Same link target where the checkout profile and operating system allow symlinks |
| Executable bit | Preserved on macOS and Linux and visible correctly to Git |
| Special filesystem entry | Rejected; devices, sockets, and FIFOs are not valid immutable-base entries |
| Owner, group, timestamps | Not part of the cross-platform checkout identity |
| ACLs, resource forks, Finder metadata, arbitrary xattrs | Not reconstructed from Git and not part of base identity |

Only deterministic in-tree `text`, `eol`, and `binary` attribute semantics are
accepted. Canonical Git LFS paths are also accepted when they use strict v1
pointers and the referenced SHA-256-verified objects are already present in the
default local LFS store. Custom LFS storage, pointer extensions, custom filters,
working-tree encodings, ident substitution, legacy or unknown attributes,
external attributes, sparse checkout, and submodules remain outside this
profile and fail closed.

Git does not store arbitrary extended attributes. Riftri therefore does not
promise to reproduce a source directory's xattrs from a commit. The native
metadata integration tests instead guarantee that adding a user xattr to one
created view does not alter its base or sibling and does not itself make Git
report a content change. macOS and Linux use their respective user xattr
namespaces. Windows has no equivalent promise in this profile.

Windows executable and symlink behavior follows the accepted Git configuration
and operating-system capability. Creating Windows symlinks can require Developer
Mode or another applicable privilege; unsupported configurations stop before an
optimized checkout is activated.

## Lifecycle profile

| Operation | APFS | Reflink | OverlayFS | ReFS |
| --- | --- | --- | --- | --- |
| Add and recover | Journaled | Journaled | Journaled with mount identity and private-layer token | Journaled |
| Clean remove | Supported | Supported | Supported with identity-checked unmount | Supported |
| Snapshot-guarded forced remove | Supported | Supported | Supported with identity-checked unmount | Supported |
| Same-volume move | Supported | Supported | Rejected while mounted | Supported |
| Prune managed metadata | Journaled and fail-closed | Journaled and fail-closed | Journaled and fail-closed | Journaled and fail-closed |
| Compact pristine view | Journaled swap | Journaled swap | Not yet supported | Journaled swap |
| Explicit base collection | Reference-revalidated and journaled | Reference-revalidated and journaled | Reference-revalidated and journaled | Reference-revalidated and journaled |
| Recovery after reboot | No mount restoration needed | No mount restoration needed | Explicit repair remounts a validated view and retains its upper layer | No mount restoration needed |

Normal removal is deliberately clean-only. Forced managed removal is a separate
explicit path that records an exact content snapshot before deletion. If the
view changes after force intent is recorded, removal and recovery stop and
preserve it for inspection. Missing-but-inconsistent, foreign-mounted, or
otherwise unexplained views are likewise retained. `riftri repair` repeats only
actions authorized by a valid journal and revalidates the observed state first.

## Disk and performance claims

“Lightweight” means unchanged content uses the backend's native sharing model;
it does not mean a worktree consumes zero space. Each view still needs Git
metadata, directory entries, filesystem metadata, and private blocks. Disk use
grows as files diverge. OverlayFS can copy up an entire file on its first data
write, while clone and reflink backends generally allocate changed blocks.

`riftri status` reports filesystem-accounted bytes, which can count shared
blocks more than once. Controlled before/after volume growth on a quiet,
disposable volume is the acceptance evidence for physical sharing. Timing and
allocation measurements are platform-specific observations, not universal
performance promises.

## How a guarantee changes

Expanding this profile requires tests for clean Git state, private writes,
physical allocation, failure rollback, and recovery on every affected backend.
If the change alters base identity or accepted checkout inputs, it also requires
a versioned profile update and a decision record. Performance optimizations may
not bypass entry validation, metadata rules, or journal boundaries.
