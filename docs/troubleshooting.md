# Troubleshooting and FAQ

Riftri is deliberately fail-closed: when an operation cannot be performed
safely and exactly, it stops with an explanation instead of degrading to a
slower or approximate result. Most "failures" below are that design working
as intended, and most have a specific inspection command that explains the
decision.

## Git repository or index environment overrides

Riftri lifecycle operations refuse a set `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_COMMON_DIR`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, or
`GIT_ALTERNATE_OBJECT_DIRECTORIES`, including an empty value. These overrides
can redirect an internal Git command away from the linked worktree being
created or recovered, risking another worktree's index or reading objects
from another repository's store. Environment-based configuration injection is
refused the same way: `GIT_CONFIG_COUNT` (which activates the
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` family) and `GIT_CONFIG_PARAMETERS`
reach every internal Git command exactly like `-c` options would. Unset them
in the calling process and select the repository with `--repository` where
supported, or run from that repository. Doctor reports the same compatibility
blocker.
Ordinary Git commands inside `riftri exec` still receive the original
environment unchanged; this restriction applies to Riftri lifecycle work.

## Start here

Three read-only commands answer most questions without changing anything:

```console
$ riftri doctor            # Git health and the planned storage path
$ riftri backends <path>   # which storage backends work for a destination
$ riftri status            # retained bases, active views, disk use
```

All three accept `--json`. For automation, add `--json-errors` to any command
to receive failures as one machine-readable receipt on stderr; the receipt
contract is documented in the [agent integration
guide](agent-integration.md).

## Worktree creation

### "Backend unavailable" or creation refuses to run

Riftri needs a copy-on-write-capable filesystem at the destination: APFS on
macOS, Btrfs or reflink-enabled XFS on Linux (or the OverlayFS fallback), and
ReFS with block cloning on Windows. Run `riftri backends <destination>` to see
what was probed and why each backend was accepted or rejected. Riftri never
silently substitutes a full checkout; if no backend qualifies, use plain
`git worktree add` for that destination. Platform requirements are covered in
[linux-reflink.md](linux-reflink.md), [linux-overlayfs.md](linux-overlayfs.md),
[windows-refs.md](windows-refs.md), and
[filesystem-compatibility.md](filesystem-compatibility.md).

### A `post-checkout` hook reported a failure

Riftri runs your `post-checkout` hook after creating a worktree, exactly as
`git worktree add` does, and reports its exit status the same way: the worktree
is created and kept, and the command exits with the hook's code. That is Git's
behavior, not a partial creation, so the worktree is usable and
`riftri worktree list` shows it.

Read the hook's own output to see what failed. Nothing needs repairing on
Riftri's side.

A hook is no longer a reason for Riftri to refuse a repository, so hook
managers that set `core.hooksPath`, such as husky, work normally.

### Creation fails on a repository with sparse checkout, submodules, or custom filters

A repository that already carries its own sparse-checkout profile, submodules,
or custom filters fails closed with an explanation rather than producing an
inexact worktree.

Sparse worktrees themselves are supported, but only when requested explicitly:
pass `--sparse-dir <DIR>` to `riftri worktree add` for cone-mode selection.
Inheriting the repository's sparse configuration, non-cone patterns, sparse
plus Git LFS, and compacting a sparse worktree all still refuse. See
[sparse checkout](sparse-checkout.md). Submodules and broader sparse support
are tracked in the [roadmap](../ROADMAP.md).

### `riftri` fails to start with a GLIBC version error

```
riftri: /lib64/libc.so.6: version `GLIBC_2.39' not found (required by riftri)
```

The GNU build requires glibc 2.34 or newer and this host is older. Nothing is
wrong with the download; the binary cannot be loaded at all.

Use the statically linked musl archive instead, which has no libc requirement:

```sh
curl -fsSL https://riftri.dev/install.sh | bash
```

Current `install.sh` detects the host glibc and picks musl automatically below
the floor. If you downloaded an archive by hand, take the
`riftri-linux-<arch>-musl` one. See
[the glibc requirement](install.md#linux-glibc-requirement).

### Conditional Git configuration is rejected

Riftri accepts conditional includes that contain only `user.name`, `user.email`,
`user.signingKey`, and `user.useConfigOnly`. This includes identity files from
system, global, and repository configuration.

Riftri reads each conditional target even when its condition does not match the
source worktree. Other keys, nested includes, and unreadable targets remain
unsupported. Branch and Git-directory conditions can select different checkout
settings in the destination. A clean Git status does not prove that the checkout
bytes match ordinary Git. Use `RIFTRI_BYPASS=1 git worktree add` for unsupported
conditional configuration.

### Git LFS paths are rejected

Riftri accepts a deliberately narrow, deterministic LFS profile: canonical
pointer blobs, standard `filter.lfs.*` configuration, and objects already
present in the local LFS store. If an object is missing, run `git lfs fetch`
and retry. The exact eligibility checks are listed in
[git-lfs.md](git-lfs.md).

## Activation and interception

### `git worktree add` is not being optimized

Interception is off everywhere by default and requires both an activated
shell or process *and* per-repository consent:

1. Check the exact environment with `riftri shell status` — IDEs, `sudo`,
   containers, and already-running terminals may not have the activated
   environment of a newly opened shell.
2. Confirm the repository is enabled (`riftri enable`, once per repository).
3. Shell aliases or functions named `git` take precedence over `PATH` and
   bypass the shim; use `command git` or remove the alias.
4. Tools that use libgit2/JGit, call Git by absolute path, replace `PATH`, or
   clear the environment bypass the shim and simply behave as before.
5. `RIFTRI_BYPASS=1` in the environment disables optimization intentionally.

For harnesses, `riftri exec <command>` gives process-scoped interception with
no shell activation at all. See
[global-activation.md](global-activation.md) for the full compatibility
matrix and edge cases.

### A custom `git` wrapper loops or misbehaves

A pre-existing wrapper is captured as the real Git command. A wrapper that
resolves `git` through the modified `PATH` again can loop; point it at its
underlying Git by absolute path, or use `riftri exec` instead of shell
activation.

## Recovery

### An operation was interrupted (crash, `kill`, power loss)

Run `riftri repair`. It is always safe: complete journals are resumed,
incomplete ones are rolled back, and a healthy state directory is left
unchanged. Repeated repair is a no-op.

### A managed worktree was deleted by hand

After `rm -rf <worktree>` and `git worktree prune`, Riftri's journal still
claims the path. Run `riftri repair`: it retires the journal, releases the
immutable base for `riftri gc`, and frees the path for a new
`riftri worktree add`. The branch is left alone. Creating a new worktree at the
same path reclaims the stale journal on its own, so the path never ends up
claimed twice.

### A managed worktree was moved with `mv` and `git worktree repair`

Riftri tracks the journaled path, so `riftri status` and `riftri worktree list`
stop showing the worktree and `riftri worktree remove` refuses. The worktree
itself keeps working and its contents are safe. `riftri repair` reports the new
location under `Relocated worktrees Riftri no longer tracks` but deliberately
does not adopt it: Git's registry alone cannot prove the worktree at the new
path is the journal's. To restore tracking, move the directory back to the
journaled path and run `git worktree repair` there, or remove it with Git and
create a new managed worktree at the location you want. Use
`riftri worktree move` for future moves so the journal is updated atomically.

### `riftri gc` collects nothing while `riftri status` reports an unused base

An unfinished journal still claims that base. `riftri gc` names the responsible
operation under `Skipped because a journaled operation still claims them`, and
`riftri status` reports the same base with the same explanation. Run
`riftri repair` to retire the operation, then collect again.

### A Linux OverlayFS worktree is empty or missing after a reboot

Kernel OverlayFS mounts disappear at reboot while the durable layers remain.
`riftri repair` remounts the worktree; tracked, untracked, and dirty changes
in the private upper layer survive. Details are in
[linux-overlayfs.md](linux-overlayfs.md).

### `worktree remove` refuses because of local changes

That is the fail-closed default. `riftri worktree remove --force` proceeds,
but only after recording an exact recovery snapshot of the discarded changes.

### A state registration points at a directory that no longer exists

`riftri state unregister <path>` removes the selected registration only if its
directory is missing. For example, from the repository directory:

```sh
riftri state unregister ../old-state
```

This removes the stale registration, not files. Existing paths (including
dangling symlinks) remain protected. `forget-missing` is still accepted as a
hidden compatibility alias for existing scripts.

## Disk usage

### Cleanup stops because an immutable-base directory is a symbolic link

A symbolic link redirects a path to another location. If an internal directory
such as `bases` or `bases/v1` is replaced by one, Riftri cannot safely assume
the linked data belongs to its storage layout. Collection refuses to follow
the link; recovery also preserves a pending collection with an unsafe parent.
The refusal names the affected path and state directory.

Inspect that state with `riftri status --state-dir <STATE_DIR>`, replacing the
placeholder with the reported state directory and quoting the path for your
shell. Status reports the unsafe path without listing bases through it. Do not
delete or move the linked data manually to make the error disappear; preserve
the layout and diagnostic output when asking for help.

For **new worktrees**, `--state-dir` lets you choose a real storage directory on
the destination volume. It does not migrate existing state or repair a layout
that was already redirected. Cleanup of an affected path remains blocked until
its storage layout can be safely verified.

### How do I see what Riftri is storing?

`riftri status` reports retained bases, active views, reference counts, and
disk use; `riftri worktree list` shows per-worktree storage. Remember that
copy-on-write clones share physical blocks, so naive size tools (like plain
`du` summed per directory) overcount; see
[allocation-evidence.md](allocation-evidence.md) for how to measure real
allocation.

### How do I reclaim space?

- `riftri gc` plans collection of immutable bases with no journaled
  references; nothing is deleted until you re-run with `--apply`.
- `riftri worktree compact <path>` replaces a *clean* managed worktree with a
  fresh native COW view, returning its storage cost to that of a new view.

## Installation

### The Linux binary reports a missing or incompatible runtime

You likely have the wrong libc archive: separate GNU/glibc and musl builds
are published for both architectures. Pick the matching archive from the
table in [install.md](install.md), or build from source. This is a packaging
mismatch, not a filesystem capability failure.

### macOS blocks the executable

If macOS security policy (Gatekeeper) blocks execution, installation stops.
Review the policy yourself; Riftri's instructions never remove quarantine
attributes or bypass a security policy. Signed and notarized builds are
tracked in [#118](https://github.com/assistant-ui/riftri/issues/118).

## Scope

### Is a Riftri worktree a sandbox?

No. Copy-on-write isolation keeps agents from editing the same files, but it
is not a security boundary. Untrusted processes still need a container, VM,
or OS sandbox; see [SECURITY.md](../SECURITY.md).

### Something else?

Open a [discussion or issue](https://github.com/assistant-ui/riftri/issues)
— [SUPPORT.md](../SUPPORT.md) explains where each kind of question belongs.
