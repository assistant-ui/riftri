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
their storage semantics are not an APFS rename. Before `worktree prune`, Riftri
requires every active managed view to exist and remain in Git's structured
inventory and refuses to proceed while another lifecycle journal is pending.
Prune can then be repeated safely during recovery. Unsupported configured or
forced forms fail closed for managed state.

## Open design questions

- Which checkout-profile inputs need first-class names beyond the canonical raw
  input representation?
- Can a future optimization safely seed a base from a separately verified clean
  worktree without weakening D016's correctness guarantee?
- Which Git filter and LFS configurations can be added to a versioned checkout
  profile without making base reuse ambiguous?
- How should operation journals and SQLite state reconcile after either one is
  partially written?
- What is the safest removal transaction for a mounted OverlayFS worktree?
- Should clean-view compaction be manual, idle-time automatic, or policy-based?
- Which Windows fallback provides acceptable performance on ordinary NTFS?
- What integration is possible for harnesses that use libgit2 or another
  embedded Git implementation instead of spawning `git`?
