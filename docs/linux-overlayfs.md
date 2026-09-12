# Linux OverlayFS backend

Riftri has a destination-specific active capability probe and a user-facing,
journaled persistent mount lifecycle. On Linux it prefers reflinks, then selects
OverlayFS only when the caller's current mount namespace passes the persistent
probe. Ordinary unprivileged shells that cannot mount OverlayFS still fail
before mutation until the least-privilege activation path is implemented.

The probe creates temporary lower, upper, work, and merged directories on the
destination volume. A short-lived child enters a private mount namespace,
mounts the lower and writable layers, reads a file through the merged view, and
writes different bytes through that view. Success requires the immutable lower
file to remain unchanged and the private bytes to appear in the upper layer.
The child then unmounts and the parent removes the complete probe directory.

A separate caller-namespace probe uses the durable mount primitive without
`unshare(2)` and repeats the lower-read, copy-up, lower-isolation, unmount, and
artifact-cleanup checks. User-facing selection must pass this stricter probe;
the isolated probe alone cannot prove that a long-lived view will be visible to
ordinary Git and agent processes.

The probe and persistent views use `userxattr`, `index=off`, `metacopy=off`, and
`redirect_dir=nofollow`. Worktree creation, file copy-up, Git status, and clean
removal are covered in the dedicated namespace suite. Mounted worktree moves
remain fail-closed until relocation has its own identity-safe mount transaction.

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
Before any future mount mutation, it can persist the exact private-layer root,
a 256-bit recovery token, and the current boot and mount-namespace context;
after mounting, the same record can carry the kernel identity. A token-bound
regular file in the private upper layer lets recovery identify a mount that
survived a crash before its kernel mount ID reached the journal. The marker is
removed only after that identity is durable. Missing mount intent on an
OverlayFS journal, mount intent on another backend, malformed tokens, and
layout paths outside
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

The user-facing backend executes the prepared mount through the existing add
and removal journals. It stages Git's `.git` pointer into the upper layer,
persists mount identity before clearing the recovery marker, and validates a
clean Git status before activation. Clean removal unmounts only the matching
identity, restores the pointer to the underlying directory, removes the exact
private-layer root, and then delegates removal to real Git. CI covers two-view
isolation, every persisted add/removal transition, and a creator process exiting
in the mount-ID gap.

## Reboot recovery

Kernel OverlayFS mounts disappear at reboot while the immutable lower and
private upper/work directories remain durable. `riftri repair` recognizes an
active journal from a prior boot only when no current mount occupies the exact
worktree path. It atomically replaces the stale boot and namespace identity
with remount intent, arms the existing private token marker, remounts in the
caller's current namespace, persists the new kernel identity, and then clears
the marker. A crash anywhere in this remount sequence can be retried.

Private upper contents are reused rather than reconstructed, so tracked,
untracked, and dirty changes survive. Repeated repair is a no-op once the mount
is active. A current mount at the destination, a same-boot mount in another
namespace, or an ownership-marker mismatch fails closed. CI simulates a reboot
by removing the kernel mount, aging the durable boot identity, and proving that
repair restores the real dirty Git worktree without changing its base.

A narrow least-privilege mount boundary remains an acceptance gate for ordinary
unprivileged Linux shells.

See the Linux kernel's
[OverlayFS documentation](https://docs.kernel.org/filesystems/overlayfs.html)
for the underlying mount and layer requirements.
