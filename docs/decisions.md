# Project decisions

This file records foundational choices so future work does not repeatedly reopen
the product boundary. Change a settled decision only with a documented reason,
compatibility impact, and migration plan.

## Settled decisions

### D001: Riftri accelerates real Git worktrees

Riftri does not create a competing workspace model. A Riftri-backed directory is
a real linked worktree recognized by ordinary Git.

### D002: Git remains the source of truth

Cloning, fetching, branching, checkout, committing, merging, rebasing, and
pushing remain Git operations. Riftri may invoke Git plumbing during worktree
setup but does not reproduce Git semantics.

### D003: activation is opt-in

The explicit `riftri worktree` interface is the baseline. Transparent command
handling is scoped to a process launched through Riftri or to a repository the
user explicitly enables. System-wide interception is not the default.

### D004: no prompt dependency

Correctness cannot rely on Claude, Codex, or another agent remembering a skill or
instruction. Process environment or harness integration provides transparent
activation when desired.

### D005: native filesystems own the hot path

Riftri coordinates creation and cleanup. Normal reads and writes go directly to
APFS, a reflink-capable filesystem, OverlayFS, ReFS, or an explicitly chosen
fallback. FUSE and NFS are not primary backends.

### D006: shared bases are immutable

A live working directory cannot serve as a mutable lower layer. Each shared base
represents an exact Git tree and checkout profile and remains unchanged while a
view references it.

### D007: backend capability is destination-specific

Backend selection is based on the actual target volume and its capabilities,
not merely the operating-system name. Clone and reflink bases may need to exist
on the same volume as the requested worktree.

### D008: implementation language is Rust

The core, Git shim, storage engine, and any mount coordinator are native Rust.
JavaScript may be used for distribution or optional integrations, not for the
filesystem data path.

### D009: full-copy fallback is explicit

If COW cannot be provided, Riftri explains why and follows the configured
policy. It does not silently consume the space of an ordinary checkout.

### D010: filesystem isolation is not sandboxing

Riftri isolates worktree changes. It does not by itself protect the host from
untrusted code. Managed untrusted execution requires a separate VM, microVM,
container, or operating-system sandbox boundary.

### D011: capability results are destination-specific and conservative

Riftri reports `supported`, `unsupported`, or `unavailable` for each backend on
the proposed destination volume. A missing destination is mapped through its
nearest existing ancestor. A result is `unavailable`, rather than optimistic,
when read-only inspection cannot prove a required feature. Full-copy fallback
always requires explicit policy even when it is technically supported.

### D012: repository identity is Git's common directory

Repository identity is the platform-native absolute path returned for Git's
common directory. This is shared by the main and linked worktrees and does not
require interpreting a worktree path as UTF-8. Git object IDs are accepted only
after validating the real Git executable's SHA-1 or SHA-256 output.

### D013: checkout profiles use versioned canonical inputs

The base key contains the repository identity, exact Git tree, destination
volume, and a versioned checkout profile. The profile is a sorted set of raw
byte key/value inputs so discovery order and text encoding cannot change the
identity. If an external checkout influence cannot be represented, the backend
must refuse optimized creation rather than guess that two profiles match.

### D014: add and removal use separate operation journals

An add journal records intent before mutation, advances atomically after each
recoverable step, and has an explicit rollback path from every incomplete
state. Active adds and completed rollbacks are terminal. Removal has its own
forward state machine under `removals/` so cleanup cannot be confused with
reversal of an incomplete add. Journal path encoding preserves platform-native
paths.

### D015: npm is a distribution layer for native Rust binaries

The public `riftri` npm package contains a small Node.js launcher and exact
optional dependencies on platform-native packages. The launcher selects by OS,
CPU, and Linux libc, then executes the Rust CLI while preserving standard I/O
and exit behavior. It never reimplements Riftri behavior in JavaScript and never
downloads an executable during an install script. Unsupported targets fail with
an explicit explanation.

### D016: the first APFS base is materialized only from Git objects

The explicit APFS prototype uses a temporary isolated Git index to materialize
the exact requested tree. It never seeds a base from a mutable working directory.
Base construction is serialized by a file lock and exposed through atomic rename
plus a durable completion marker. The initial checkout profile is intentionally
narrow: attributes, filters/Git LFS, sparse checkout, submodules, and ambiguous
checkout-changing configuration are rejected rather than approximated.

### D017: repository activation is local Git configuration plus explicit scope

`riftri enable` writes `riftri.enabled=true` to the repository's local Git
configuration, which is shared by its linked worktrees. It does not edit shell
startup files or replace Git globally. `riftri exec` prepends a temporary Git
shim only to the selected child process tree and records the exact real Git
executable before changing `PATH`. Its optional `--worktree` binding accepts an
exact live root from Git's worktree inventory, changes only the child working
directory, and remains independent of any agent brand. As an explicit alternative,
`riftri shell hook <sh|bash|zsh>` prepares a versioned shim in the user's cache
and prints environment changes for the user to evaluate. Riftri never evaluates
the hook or edits a shell profile itself. In either scope, the shim delegates
commands outside enabled repositories and non-worktree Git commands unchanged.
Unsupported optimized add forms fail visibly; `RIFTRI_BYPASS=1` is the explicit
ordinary-Git escape hatch.

Shell activation and repository consent remain deliberately independent.
Status reports both. Deactivation is emitted as shell code because a child
process cannot modify its parent environment; Riftri never claims that running
the command without `eval` changes the current shell. If a user added the hook
to a profile for global per-user activation, only that user removes the profile
line.

### D018: removal moves forward and accounting is journal-derived

Riftri removal records intent only after an initial clean check, invokes Git
without `--force`, and moves forward to completion rather than trying to
reconstruct a deleted writable view. Recovery rechecks a still-present view and
preserves it if it changed. Active reference counts are derived from terminal
add and removal journals instead of maintained as a second mutable counter.
Zero-reference bases remain cached until the explicit garbage collector is
applied; collection is journaled and independently validates that no active or
incomplete operation references the base.
Filesystem-allocated byte totals are diagnostic and may count shared blocks;
volume-delta tests remain the proof of physical sharing.

### D019: garbage collection is explicit, locked, and recoverable

`riftri gc` is a read-only plan unless the user supplies `--apply`. Applied
collection records intent before mutation, takes the same per-base lock as base
construction, and reloads durable add/removal journals before removing a base.
Every non-rolled-back add without a completed removal protects its base,
including interrupted adds and removals. The collector removes the completion
marker before making the base directory writable and atomically quarantining it;
recovery then removes only the exact journaled quarantine path. A reference that
appears before mutation cancels collection rather than risking a live base.

### D020: unexplained state is diagnosed but not inferred safe

`riftri status` compares on-disk state with versioned layout roots, decoded
journals, immutable-base completion markers, and object-ID coordination locks.
It reports unmatched paths and missing active-journal targets with an actionable
reason. Diagnostics do not turn an unjournaled artifact into a cleanup target;
automatic repair and garbage collection remain limited to paths authorized by a
validated durable journal.

### D021: managed move and prune use forward-only journals

A managed `worktree move` records its source, destination, repository, and
source add operation before asking Git to move the linked worktree. Recovery
reconciles the two safe observable states, then atomically updates the active add
journal and completes the move journal. Cross-volume moves are rejected because
their storage semantics are not an atomic same-volume rename. Before `worktree prune`, Riftri
requires every active managed view to exist and remain in Git's structured
inventory and refuses to proceed while another lifecycle journal is pending.
Prune can then be repeated safely during recovery. Unsupported configured or
forced forms fail closed for managed state.

### D022: deterministic in-tree attributes are resolved by Git

Riftri asks the installed Git executable to resolve attributes from the exact
requested tree through an isolated temporary index; it does not parse attribute
patterns itself. The APFS backend accepts only the built-in `text`, `eol`, and
`binary` checkout semantics, including the checkout-neutral `diff` and `merge`
records emitted by `binary`. The tree ID already makes these rules part of the
immutable-base identity. Git LFS, custom filters, working-tree encodings, ident
substitution, legacy or unknown attributes, and any effective repository-local,
global, or system attribute source remain fail-closed. This deliberately
narrows D016's initial blanket rejection without changing its exact-tree or
external-input safety requirements; existing bases need no migration because
an attributed tree was previously rejected before base creation.

### D023: Linux reflinks use an active unnamed-file probe

Linux worktree creation supports Btrfs and reflink-enabled XFS through the
kernel `FICLONE` ioctl. Before Git metadata or Riftri state is mutated, Riftri
actively clones between unnamed files on the destination volume and verifies
that a private write does not change the source. Unnamed files make probe
cleanup automatic after normal exit or interruption. The state directory is
then required to resolve to the same volume, and every view file must reflink;
an ioctl failure rolls the journaled operation back and never triggers a byte
copy. Read-only diagnostics remain conservative for XFS because its per-volume
reflink feature cannot be proven from the filesystem name alone.

### D024: Windows ReFS uses an active destination probe and aligned block clones

Windows worktree creation supports ReFS through
`FSCTL_DUPLICATE_EXTENTS_TO_FILE`. Before Git metadata or Riftri state is
mutated, Riftri block-clones between delete-on-close files on the destination
volume and verifies that a private write does not change the source. View files
are temporarily made sparse before they are pre-sized, avoiding allocation of
zero-backed clusters that would immediately be replaced by cloned extents. They
then inherit the source's sparse and integrity-stream settings. Aligned data is
cloned in requests below 4 GiB; only the final unaligned tail, which is smaller
than one filesystem cluster, is copied to satisfy the Windows API contract. A
failed aligned clone aborts and rolls the journaled operation back instead of
silently creating a full copy. ReFS is the only supported Windows mutation
filesystem in this slice; alternatives for ordinary NTFS remain an explicit
design question.

### D025: Git worktree metadata mutation is repository-serialized

Git's shared `worktrees/` administration is not safe to mutate through multiple
simultaneous `git worktree add` processes on every supported Git/platform pair.
Riftri therefore holds a repository-local lock around each Git worktree
metadata mutation and the corresponding journal transition. Immutable-base
preparation and native COW view creation remain independently concurrent. The
lock lives in the common Git directory so separate Riftri state directories
cannot race the same repository.

### D026: custom state directories have repository-local discovery locators

An explicit add that uses a non-default state directory records its canonical
absolute path as a multi-valued `riftri.stateDirectory` entry in the
repository's local Git configuration before operation intent is written. This
locator lets the process-scoped and shell Git shims find lifecycle journals
that must remain on another COW-capable volume. Remove and move route through
the state directory that owns the matched add journal. Prune revalidates every
registered state directory before Git changes shared worktree metadata.
Missing, relative, or unsafe registered paths fail closed; a locator never
authorizes deletion and does not replace the operation journals as recovery
authority.

### D027: OverlayFS support starts with an isolated active probe

Linux OverlayFS cannot be accepted from `/proc/filesystems` alone because
mount permission, upper/work filesystem compatibility, and copy-up behavior are
destination- and execution-context-specific. Riftri therefore creates temporary
lower, upper, work, and merged directories on the destination volume and tests
an actual mount in a forked private mount namespace. The probe verifies a lower
read, a private write, the unchanged lower file, and the copied-up upper file,
then unmounts and removes its paths. The child resolves the probe root after
entering its new mount namespace, then fixed relative names identify the layers.
This prevents stale references to the caller's namespace and avoids OverlayFS
option-delimiter ambiguity. The initial options were explicit (`userxattr`,
`index=off`, `metacopy=off`, and `redirect_dir=nofollow`). D034 replaces that
initial metadata policy so a real ordinary-user checkout can retain a
permission-protected lower layer. A permission failure is `unavailable`, not
evidence that the kernel or volume is unsupported. This decision authorizes
capability detection only; persistent views require journaled mount paths,
unmount/removal semantics, and restart recovery first.

### D028: persistent OverlayFS mounts require exact kernel identity

One prepared OverlayFS view owns `upper` and `work` directories under a single
`overlays/v1/<operation-id>` state root; the exact immutable base remains the
lower and the requested Git worktree path is the merged mountpoint. Riftri
opens all three layers before mounting and passes fixed `/proc/self/fd` paths to
the kernel, keeping native user paths out of OverlayFS's delimiter-sensitive
option string. A successful mount is identified by the Linux boot ID, mount
namespace device and inode, kernel mount ID, and OverlayFS filesystem type.
Unmount and private-layer deletion fail closed if the caller is in another
namespace or a different mount occupies the destination. A prior-boot mount is
considered absent only when no current mount occupies that exact path. This
storage primitive does not choose the privilege boundary: persistent CLI
activation still requires journal wiring and a narrow way to create the mount
in the namespace where ordinary Git and agent processes can see it.

### D029: OverlayFS mount intent extends the version 1 add journal

An add journal whose backend is OverlayFS must contain an OverlayFS-specific
record; other backends must not contain one. The record preserves the native
path to the exact `overlays/v1/<operation-id>` private-layer root, a 256-bit
hexadecimal recovery token written with the original operation intent, and an
optional kernel mount identity populated only after mounting. This optional
field is backward-compatible with existing version 1 journals, which never
selected OverlayFS and therefore omit it. Recovery path validation rejects a
root outside the operation's exact state location. State diagnostics recognize
live journal-owned roots, report unowned roots, and never infer that an unknown
private layer is safe to delete. This schema closes the durable-intent
prerequisite only; selecting and mutating the OverlayFS backend remains gated
on transaction and mount-activation work.

### D030: OverlayFS closes the mount-ID crash gap with namespace intent and a private marker

The kernel assigns a mount ID only after `mount(2)` succeeds, so a process can
exit while the mount exists but the journal still has no ID. Before mounting,
Riftri persists the Linux boot ID and mount-namespace device and inode, then
creates a token-bound regular file in the journal-owned upper layer. Recovery
adopts the live mount only in that same boot and namespace, at the exact merged
path, with filesystem type `overlay`, and when the marker has identical bytes
through both the private upper and merged views. The marker is removed only
after the exact mount identity is durable. Missing, changed, foreign-namespace,
or foreign-filesystem state fails closed. A caller-namespace active probe is a
separate selection gate because the isolated capability probe does not prove
that a persistent mount will be visible to ordinary Git processes.

### D031: Linux prefers reflinks and selects OverlayFS only in a proven caller namespace

Linux probes reflinks first because they require no long-lived mount. When that
probe is not supported, Riftri may select OverlayFS only after an active mount,
copy-up, lower-isolation, unmount, and cleanup cycle succeeds in the caller's
current mount namespace. Add intent and namespace context are durable before
mounting; exact mount identity is durable before the private recovery marker is
removed. Clean removal unmounts only that identity, restores Git's pointer from
the upper layer, removes only the journal-owned private layers, and invokes the
installed Git executable for metadata removal. Recovery adopts only a
token-proven mount in the original namespace. Mounted moves fail before
mutation until relocation has a dedicated mount transaction. This enables
containers and already-capable namespaces without pretending that ordinary
unprivileged shells have the still-planned activation helper.

### D032: explicit repair remounts active OverlayFS views after reboot

An OverlayFS mount cannot survive a Linux reboot, but its immutable lower and
private upper/work state can. When an active journal's boot identity is stale
and no current mount occupies the exact destination, `riftri repair` atomically
replaces the old context and mount ID with new remount intent. The existing
token-bound marker then protects the crash window around remount and identity
persistence. Recovery recreates the disposable kernel work directory but reuses
the private upper, so dirty and untracked work is preserved and repeated repair
is idempotent. A mount in the current boot, a
same-boot namespace mismatch, or a foreign destination mount remains a hard
stop. This is explicit recovery rather than an always-on daemon; making the
mount available to ordinary unprivileged shells requires the separate
least-privilege activation boundary settled in D033.

### D033: ordinary Linux shells use a narrow installed mount helper

A rootless private user/mount namespace cannot make a persistent mount visible
back in the shell or editor that launched it, so wrapping only the agent process
would produce a misleading, session-scoped worktree. Riftri instead offers an
explicit installer for a root-owned set-user-ID copy of the same release. Any
elevated invocation is forced into a fixed helper protocol before normal CLI or
Git-shim dispatch. The protocol clears its environment and permits only a
mount, an exact journal-identity-checked unmount, or resetting the exact
journal-owned disposable work directory after unmount. Probe files and copy-up
verification remain in the unprivileged parent. All participating paths must be
canonical real directories owned by the requesting UID, and the mount retains
`nodev,nosuid`.

Direct reflinks and already-capable mount namespaces remain preferred. The
helper is trusted only when its absolute executable path and every ancestor are
root-owned and non-writable by group or others, and the executable is
set-user-ID. Installation is an explicit system-wide capability action, not
global Git interception: each repository still requires its own `riftri enable`
consent. If the helper is missing or fails validation, OverlayFS remains
unavailable and the worktree add stops before mutation without a full-copy
fallback.

### D034: OverlayFS restores checkout modes with profile-bound metadata copy-up

Permission-protected immutable bases expose regular files without owner-write
bits. A process mapped to root in a private user namespace can bypass those
modes, which allowed the original test suite to miss that an ordinary host user
could not edit a helper-mounted view. Riftri now makes the active probes begin
with a read-only lower file and restores ordinary checkout modes through the
merged mount before activation.

On installed-helper mounts, OverlayFS `metacopy=on` makes this permission
restoration copy only inode metadata into the private upper; unchanged file
contents remain in the lower until their first data write. Because the kernel
warns against accepting forged metacopy and redirect attributes from untrusted
layers, the root helper does not use `userxattr`. Its root-created mount uses
the protected `trusted.overlay.*` namespace, which the requesting user cannot
populate. Rootless mounts retain the previously proven `userxattr`,
`metacopy=off`, and `redirect_dir=nofollow` combination; their mapped-root
caller needs no permission-copy-up pass. The chosen rootless or privileged
profile is persisted in both the pre-mount context and mount identity and must
be reused during repair. Riftri never remounts an upper under the other
attribute interpretation.

### D035: potential ASCII case aliases are preflighted on the destination

Git trees can contain path sets that the destination filesystem cannot
represent independently, including names that differ only by ASCII case.
Riftri must not discover those aliases after it has created a journal, branch,
or linked-worktree metadata. An in-memory scan compares every complete path and
directory prefix without decoding Unix path bytes. When it finds a potential
alias, an add creates the exact directory and leaf-name structure without file
contents inside a unique temporary directory next to the requested destination.
The destination filesystem itself determines whether those names coexist.
Riftri removes the probe before continuing. Any collision or cleanup failure
stops the add before durable mutation; case-sensitive destinations continue to
accept distinct case variants. Trees with no potential ASCII alias avoid the
per-file probe I/O.

### Cleanup checks survive pointer removal and OverlayFS unmount

Pointer-only worktree cleanup stages the real `.git` pointer at a journal-derived
path and uses a nonrecursive empty-directory removal. Real Git then removes the
registration without force. A concurrent directory recreation goes through
Git's normal dirty-state checks, and failed cleanup restores the pointer without
replacing new files. Repair recognizes interrupted pointer staging.

OverlayFS removal journals also retain a private-layer snapshot captured before
the clean check. After exact unmount, Riftri verifies the snapshot before deleting
the private layers. A change or an unprovable legacy checkpoint preserves the
layers and reports an error. Rollback uses the same before/after snapshot gate.
The snapshot includes inode change times and content so metadata-only copy-up
cannot hide changes. This preserves writes that finish between the original
cleanliness check and unmount; it does not lock out direct tampering with Riftri's
private state by another same-user process.

### Immutable-base integrity markers

New base buckets use a versioned SHA-256 completion marker covering every entry's
native name, kind, file bytes, permissions, and symlink target. Reuse recomputes
the digest under the base lock and refuses a mismatch without deleting evidence
or disturbing existing views. This adds a sequential read on cache hits without
allocating another checkout. An empty marker cannot establish integrity: new
adds use a new cache namespace, while older bases remain available to their
existing views and explicit garbage collection. This detects accidental cache
corruption; it does not make same-user mutable state a security sandbox.

## Open design questions

- Which checkout-profile inputs need first-class names beyond the canonical raw
  input representation?
- Can a future optimization safely seed a base from a separately verified clean
  worktree without weakening D016's correctness guarantee?
- Which Git filter and LFS configurations can be added to a versioned checkout
  profile without making base reuse ambiguous?
- How should operation journals and SQLite state reconcile after either one is
  partially written?
- Should clean-view compaction be manual, idle-time automatic, or policy-based?
- Which Windows fallback provides acceptable performance on ordinary NTFS?
- How should non-ASCII case folding and normalization aliases be detected using
  the destination filesystem's exact comparison rules without adding per-file
  probe I/O to every worktree creation?
- What integration is possible for harnesses that use libgit2 or another
  embedded Git implementation instead of spawning `git`?
