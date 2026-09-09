# Riftri project outline

## One-sentence definition

Riftri transparently makes real Git worktrees copy-on-write so multiple coding
agents can work in isolated directories without duplicating every unchanged
project file.

## Problem

Git linked worktrees share repository objects, but each worktree normally has a
separate materialized working directory. Ten agents operating from the same
large tree can therefore create ten copies of most tracked files, increasing
startup time, disk use, and stale-directory cleanup.

## Solution

Riftri preserves normal Git behavior while replacing worktree file
materialization with a native copy-on-write view:

```text
immutable base at an exact Git tree
├── agent A view = base + A's private changes
├── agent B view = base + B's private changes
└── agent C view = base + C's private changes
```

Every view has a normal filesystem path and real Git linked-worktree metadata.
Editors, compilers, test runners, and coding agents use ordinary filesystem and
Git operations.

## Target users

- Developers running several local coding agents concurrently.
- Agent harnesses that create one Git worktree per task.
- Teams operating short-lived development environments on large repositories.
- A future managed service running agents in isolated VMs or microVMs.

## Primary goals

1. Preserve ordinary Git worktree semantics.
2. Reduce physical disk allocation for unchanged working-tree data.
3. Reduce worktree creation latency when a compatible base is cached.
4. Remain neutral across Claude Code, Codex, Cursor, and other agent harnesses.
5. Keep native filesystems on the normal read/write path.
6. Make interrupted creation and cleanup recoverable.

## Non-goals

- Replacing Git or Git hosting providers.
- Managing commits, branches, merges, pushes, or pull requests.
- Requiring agents to use Riftri-specific file tools.
- Providing a full security sandbox for untrusted code.
- Making FUSE or NFS the default data path.
- Automatically deleting dirty or unpushed work.

## User experience

### Explicit mode

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

This is the universal, debuggable interface and the first mutation interface to
implement.

### Process-scoped transparent mode

```console
$ riftri enable
$ riftri exec -- claude
```

Within that process tree, supported `git worktree add` commands are routed
through Riftri when their repository has been explicitly enabled. All other Git
commands and commands in repositories that are not enabled go directly to the
real Git executable. An explicit bypass environment variable is available for
ordinary-Git operations.

### Repository-scoped mode

A future optional shell integration may optimize worktree operations only in
repositories explicitly enabled by the user. Global interception is never the
default.

## System outline

### Git coordinator

Uses the installed Git executable to resolve refs and trees, create linked
worktree metadata, inspect clean state, and preserve the user's configuration,
filters, hooks, and credentials.

### Immutable base manager

Builds one reusable, read-only filesystem tree for compatible worktrees. The
cache identity includes:

```text
repository identity
+ Git tree object ID
+ checkout profile
+ destination filesystem/volume
```

The checkout profile accounts for behavior that can change checked-out bytes,
including attributes, filters, line endings, and sparse-checkout settings.

### Storage engine

Chooses the fastest safe native backend available at the destination:

| Platform | Preferred direction |
|---|---|
| macOS | APFS native clones |
| Linux | Btrfs/XFS reflinks, then kernel OverlayFS |
| Windows | ReFS block cloning, then evaluated native alternatives |
| Unsupported | Explicit ordinary-Git fallback only |

### State and recovery

Tracks bases, worktree views, reference counts, mounts, and incomplete
operations. The intended implementation is local SQLite plus small operation
journals. State is coordination metadata, not source control.

### Optional Git shim

Recognizes worktree lifecycle commands inside an enabled process. Normal Git
commands should immediately execute real Git without opening Riftri state.

### Distribution

The implementation remains Rust. The npm `riftri` package is a thin launcher
that selects a platform-native optional package and executes the Rust binary
with inherited standard I/O and exit behavior. Native binaries and the npm
launcher share one version and are published from tagged GitHub releases.

## Fast-path design

Riftri runs during worktree creation, removal, recovery, and diagnostics. Once a
view exists, the fast path is:

```text
agent or tool -> native operating-system filesystem -> base/private data
```

There should be no userspace Riftri callback on ordinary file reads and writes.

The fastest creation path is:

1. Resolve the requested Git tree.
2. Reuse an existing compatible base.
3. Create a native COW view.
4. Attach the real linked-worktree metadata.
5. Verify a clean Git state.

Concurrent requests for the same base should share one in-progress base build
rather than materializing it repeatedly.

## Safety model

Filesystem isolation is not a security sandbox. Local mode assumes trusted
developer processes. Untrusted execution belongs in a container, VM, microVM,
or another operating-system sandbox.

Riftri must refuse unsafe or ambiguous cleanup, preserve Git's protection of
dirty worktrees, and make full-copy fallback visible and explicit.

## Measures of success

Riftri should eventually report and benchmark:

- Cold and cached worktree creation time.
- Logical size versus physical allocated size.
- Read, write, checkout, build, and test performance.
- Private-layer growth over the worktree lifetime.
- Recovery success after forced termination at each transaction step.
- Compatibility across symlinks, executable bits, filters, LFS, sparse checkout,
  submodules, unusual paths, and case-sensitivity differences.

## Current state

The Rust workspace, Milestone 1 foundation, and Milestone 2 explicit APFS
prototype exist. On a writable APFS volume, `riftri worktree add` builds or
reuses an exact-tree immutable base, creates real linked-worktree metadata with
checkout suppressed, activates a native COW clone, synchronizes the index, and
requires a clean Git status before success. Add operations are journaled and
recoverable. Repository-local enable/disable state and an initial process-scoped
Git shim now route supported adds through that same transaction. Removal and
full lifecycle interception, Linux/Windows mutation backends, mounts, and a
daemon are not implemented. The next storage-lifecycle work remains Milestone 3
in `ROADMAP.md`.
