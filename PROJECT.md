# Riftri: lightweight Git workspaces for parallel development

## One-sentence definition

Riftri gives parallel development tasks real, isolated Git worktrees without
storing another full physical copy of every unchanged project file for each
workspace.

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

An existing worktree can be selected without coupling Riftri to an agent:

```console
$ riftri exec --worktree ../app-auth -- <command>
```

Within that process tree, supported `git worktree add` commands are routed
through Riftri when their repository has been explicitly enabled. All other Git
commands and commands in repositories that are not enabled go directly to the
real Git executable. A worktree binding must resolve to an exact live root in
Git's worktree inventory. An explicit bypass environment variable is available
for ordinary-Git operations.

### Repository-scoped mode

Users may explicitly activate a shell hook, then use normal Git commands:

```console
$ eval "$(riftri shell hook zsh)"
$ riftri enable
$ git worktree add -b feature/auth ../app-auth main
```

The hook is shell-scoped and inherited by child processes. It routes supported
adds through Riftri only in repositories explicitly enabled by the user and
delegates everything else to the exact real Git executable. Riftri prints the
hook but never edits shell startup files automatically. Global interception is
never the default.

Shell status reports whether interception is active and whether the selected
repository has opted in. Deactivation is explicitly evaluated, affects only the
current shell, and never removes a profile line or repository consent.

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

### Hosted and managed environments

A virtual machine or container does not imply that a particular storage
backend is available. Riftri must probe the destination volume and the runtime
capabilities before selecting one:

- Linux hosts may use Btrfs or XFS reflinks, or kernel OverlayFS when the host
  permits the required mounts and namespaces.
- Windows hosts may use ReFS block cloning, followed only by native alternatives
  that pass the same correctness and recovery requirements.

Windows and restricted container environments need additional lifecycle and
capability work because they do not expose the same primitives as local APFS.
Hosted and managed integration is therefore a later roadmap milestone, and it
must never silently fall back to duplicating a full worktree.

### State and recovery

Tracks bases, worktree views, reference counts, mounts, and incomplete
operations. The intended implementation is local SQLite plus small operation
journals. State is coordination metadata, not source control.

### Optional Git shim

Recognizes worktree lifecycle commands inside an explicitly activated process
or shell. Repository-local enablement controls optimized adds. Normal Git
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

The Rust workspace and Milestones 1 through 4 are complete. On writable APFS,
Btrfs, reflink-enabled XFS, and ReFS volumes, `riftri worktree add` builds or
reuses an exact-tree immutable base, creates real linked-worktree metadata with
checkout suppressed, activates a native COW view, synchronizes the index, and
requires a clean Git status before success. Linux uses an active unnamed-file
`FICLONE` probe. Windows actively verifies ReFS block cloning and private-write
isolation before mutation. Neither backend substitutes a full byte copy when
its native operation fails. Add operations are journaled and recoverable.
Repository-local enable/disable state, process-scoped execution, and an
explicitly activated sh/bash/zsh hook route supported normal Git adds through
that same transaction. Process-scoped commands can also be bound to a validated
existing worktree without agent-specific behavior. Clean managed removals use a
separate, recoverable journal, and retained bases expose derived reference
counts plus logical and allocated-byte accounting.
Repository-aware repair resumes incomplete journals, and explicit journaled
garbage collection can remove independently revalidated zero-reference bases.
Status diagnoses unjournaled artifacts, empty base buckets, unsafe markers, and
missing paths referenced by active journals without deleting them. Managed move
and guarded prune use recoverable forward-only journals. The Linux reflink and
OverlayFS slices of Milestone 5 and Windows ReFS slice of Milestone 7 are
exercised on disposable native volumes in CI. When reflinks are unavailable,
Linux selects OverlayFS only after proving that the caller's current namespace
can host a persistent mount, either directly or through an explicitly installed
root-owned mount helper. The add transaction places the real
linked-worktree pointer in a private upper layer, persists exact mount identity,
verifies clean Git state, and recovers the mount-ID persistence crash gap
through a private ownership marker. Clean removal revalidates the exact mount,
unmounts it, restores the pointer for real Git removal, and deletes only the
journal-owned private layers. Explicit repair can remount an active view after
a boot change without losing private edits. Mounted moves remain fail-closed.
The helper elevates only validated OverlayFS mount, exact identity-checked
unmount, and disposable work-directory reset requests for caller-owned paths;
probe setup, Git, and worktree I/O remain unprivileged. Missing, user-owned,
writable, or incorrectly installed helpers are rejected before mutation.
Before any durable add state is created, Riftri checks the exact Git tree for
ASCII case aliases and verifies potential collisions on the destination
filesystem. Colliding path sets fail without creating a journal or linked
worktree, while case-sensitive destinations continue to accept distinct names.
Automatic orphan-state repair,
ordinary-NTFS alternatives,
managed-environment integration, and a daemon are not implemented.
