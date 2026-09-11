# Linux reflink verification

Riftri's experimental Linux backend supports Btrfs and reflink-enabled XFS. It
uses the kernel `FICLONE` ioctl, so unchanged file data shares physical storage
and ordinary reads and writes stay on the native filesystem path.

Before an add mutates Git metadata, Riftri creates two unnamed files on the
destination volume, reflinks one into the other, and verifies write isolation.
The files have no directory entries and are reclaimed by the kernel on exit or
interruption. A failed probe stops the operation; Riftri never replaces it with
a byte copy.

The CI matrix creates disposable Btrfs and reflink-enabled XFS loop volumes and
runs the Rust workspace with `RIFTRI_REQUIRE_REFLINK=1`. The Linux tests require:

- a successful active capability probe;
- real linked-worktree registration and clean Git status;
- reusable exact-tree immutable bases;
- private writes that do not change another view or its base;
- eight simultaneous adds that converge on one base and keep parallel commits
  isolated;
- dirty-worktree removal refusal and clean journaled removal;
- injected interruption and repeated recovery at every durable add, remove,
  garbage-collection, move, and prune transition;
- explicit base garbage collection; and
- materially lower volume growth than the logical size of a cached view.

At runtime, `riftri backends <path>` remains a read-only diagnostic. It can
identify Btrfs directly, while XFS is reported as requiring an active probe.
`riftri worktree add` performs that probe automatically and reports `Linux
reflink` when selected.

The state directory and worktree must live on the same filesystem volume.
OverlayFS is a separate Milestone 5 backend because mounts, upper/work
directories, privilege boundaries, and restart recovery require a different
lifecycle transaction.
