# Linux OverlayFS backend

Riftri has a destination-specific active capability probe plus a storage-level
persistent mount lifecycle. It does not yet select OverlayFS for user-facing
Git worktree creation because core transaction execution and the
least-privilege activation path are not wired to that lifecycle.

The probe creates temporary lower, upper, work, and merged directories on the
destination volume. A short-lived child enters a private mount namespace,
mounts the lower and writable layers, reads a file through the merged view, and
writes different bytes through that view. Success requires the immutable lower
file to remain unchanged and the private bytes to appear in the upper layer.
The child then unmounts and the parent removes the complete probe directory.

The probe uses `userxattr`, `index=off`, `metacopy=off`, and
`redirect_dir=nofollow`. OverlayFS does not permit redirect creation with the
unprivileged xattr mode. The later worktree backend must therefore prove that
normal Git directory operations remain compatible or introduce a narrowly
scoped mount helper before Riftri can select this backend.

After namespace isolation, the child resolves the private probe root using its
native byte path in the new namespace. Only fixed relative layer names enter the
OverlayFS mount options. This preserves byte-oriented Linux paths, avoids
mount-option delimiters such as commas and colons, and does not grow with the
destination path length. The caller's mount namespace is never modified.

CI requires the positive probe to succeed on a disposable ext4 volume inside an
unprivileged user and mount namespace; the probe child further isolates its test
mount from the caller. The ordinary Ubuntu quality job also runs the probe
without requiring support, which verifies conservative failure and cleanup when
the current process lacks mount permission. Least-privilege activation for a
persistent mount remains a separate lifecycle gate.

## Persistent mount primitive

One prepared view owns this durable layout:

```text
<state>/overlays/v1/<operation-id>/
  upper/
  work/
```

The exact immutable base is the lower directory and the requested Git worktree
path is the merged mountpoint. The merged path is not a symlink or an alternate
workspace: it is the real path Git registers. Upper and work must be real
directories on the same filesystem, the initial work directory and mountpoint
must be empty, and unexpected state-root entries fail closed.

Persistent mounts use open directory descriptors for lower, upper, and work.
Only fixed `/proc/self/fd/<number>` values enter the mount option string, so
commas, colons, spaces, long paths, and non-UTF-8 path bytes cannot alter its
grammar. The merged path is passed as the mount syscall's separate target.

Each successful mount returns an identity containing the current Linux boot
ID, mount-namespace device and inode, kernel mount ID, and filesystem type.
Recovery can reload the durable layout after the creating process exits. It
will unmount only the exact matching OverlayFS mount and removes private layers
only after that mount is absent. A different namespace or a foreign mount at
the destination is retained for manual attention. The dedicated CI suite
proves copy-up isolation, identity-mismatch refusal, and recovery after the
creator process terminates.

The existing version 1 add journal now has an optional OverlayFS-only record.
Before any future mount mutation, it can persist the exact private-layer root
and a 256-bit recovery token; after mounting, the same record can carry the
kernel identity. Missing mount intent on an OverlayFS journal, mount intent on
another backend, malformed tokens, and layout paths outside
`overlays/v1/<operation-id>` all fail closed. Status recognizes journal-owned
layout roots and reports unowned roots without deleting them. Existing APFS,
reflink, and ReFS journals remain compatible because they omit this record.

Probe results are conservative:

- `supported` means an actual mount, read, copy-up, isolation check, and unmount
  succeeded on the destination volume.
- `unsupported` means the kernel or destination rejected the OverlayFS
  primitive as incompatible.
- `unavailable` means the probe could not prove support, including when the
  current process lacks mount or namespace permission.

The user-facing backend still needs to execute the prepared mount through the
existing add/removal transactions, restore Git's `.git` pointer in the upper
layer, validate clean status, and coordinate unmounts for remove and move. It
also needs a narrow least-privilege mount boundary that
makes the merged path visible to ordinary Git and agent processes. Reboot
simulation and explicit fallback policy remain acceptance gates. Riftri will
not select OverlayFS worktree creation until those guarantees exist.

See the Linux kernel's
[OverlayFS documentation](https://docs.kernel.org/filesystems/overlayfs.html)
for the underlying mount and layer requirements.
