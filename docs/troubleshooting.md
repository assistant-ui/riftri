# Troubleshooting and FAQ

Riftri is deliberately fail-closed: when an operation cannot be performed
safely and exactly, it stops with an explanation instead of degrading to a
slower or approximate result. Most "failures" below are that design working
as intended, and most have a specific inspection command that explains the
decision.

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

### Creation fails on a repository with sparse checkout, submodules, or custom filters

These checkout configurations are not yet supported and fail closed with an
explanation rather than producing an inexact worktree. Sparse-checkout
profiles and submodule support are tracked in the
[roadmap](../ROADMAP.md).

### Conditional Git configuration is rejected

Riftri rejects `includeIf` configuration, even when the condition does not match
the source worktree. Branch and Git-directory conditions can select different
checkout settings in the destination. A clean Git status does not prove that
the checkout bytes match ordinary Git. Use `RIFTRI_BYPASS=1 git worktree add`
when conditional configuration is necessary.

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

### A Linux OverlayFS worktree is empty or missing after a reboot

Kernel OverlayFS mounts disappear at reboot while the durable layers remain.
`riftri repair` remounts the worktree; tracked, untracked, and dirty changes
in the private upper layer survive. Details are in
[linux-overlayfs.md](linux-overlayfs.md).

### `worktree remove` refuses because of local changes

That is the fail-closed default. `riftri worktree remove --force` proceeds,
but only after recording an exact recovery snapshot of the discarded changes.

### A state registration points at a directory that no longer exists

`riftri state forget-missing <path>` forgets one explicitly selected
registration whose directory is missing.

## Disk usage

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
