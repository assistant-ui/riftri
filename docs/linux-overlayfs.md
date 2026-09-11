# Linux OverlayFS capability

Riftri's first OverlayFS slice is a destination-specific active capability
probe. It does not yet create a persistent OverlayFS-backed Git worktree.

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

Probe results are conservative:

- `supported` means an actual mount, read, copy-up, isolation check, and unmount
  succeeded on the destination volume.
- `unsupported` means the kernel or destination rejected the OverlayFS
  primitive as incompatible.
- `unavailable` means the probe could not prove support, including when the
  current process lacks mount or namespace permission.

The persistent backend still needs durable upper/work placement, a visible
merged worktree mount, journaled creation and unmount, least-privilege launch
integration, and restart recovery. Riftri will not select OverlayFS worktree
creation until those lifecycle guarantees exist.

See the Linux kernel's
[OverlayFS documentation](https://docs.kernel.org/filesystems/overlayfs.html)
for the underlying mount and layer requirements.
