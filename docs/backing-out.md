# Backing out of Riftri

Riftri is pre-release software that manages worktrees, so the fair question
before adopting it is what happens when you stop. This page answers it.

## The short version

**A Riftri-managed worktree is an ordinary Git linked worktree.** Git created
it, Git tracks it, and Git can remove it. Riftri's contribution is how the
files were materialized — copy-on-write clones instead of a full copy — and a
journal describing operations in progress.

Nothing about your work is held hostage by Riftri being installed. Your
commits are in the repository. Your working files are real files on disk.

## What happens if Riftri disappears

On a native copy-on-write backend — APFS, Linux reflinks, ReFS — clones are
independent files that happen to share unchanged blocks. The shared blocks
stay alive as long as any file references them, so deleting Riftri's base
does not reach into the worktree.

Verified on macOS/APFS:

| You do this | Your worktree |
| --- | --- |
| Stop running `riftri` | Unaffected. It is a normal worktree |
| `riftri disable` the repository | Unaffected. Files, history, commits all fine |
| Delete Riftri's immutable bases | Unaffected. All content intact, `git status` clean |
| Delete `.git/riftri` entirely | Unaffected. Commit, branch, and merge still work |
| Uninstall the binary | Unaffected |
| `git worktree remove` with plain Git | Works. Riftri is not required to remove it |

Two details worth knowing:

**Immutable bases are read-only on purpose.** A stray `rm -rf .git/riftri`
fails partway with `Permission denied` rather than quietly succeeding. That
is a guard, not a bug. Use `riftri gc --apply` to reclaim bases properly.

**Removing a worktree with plain Git works but skips the journal.** The
worktree goes away and your files are unaffected, but Riftri's state still
references it. `riftri repair` and `riftri gc` reconcile that, so prefer
`riftri worktree remove` while Riftri is still installed.

### OverlayFS is different

The Linux OverlayFS backend does not clone files. A view is a live mount with
the immutable base as its lower layer, so **the base has to stay while the
view exists.** Unmount and remove OverlayFS-backed views with
`riftri worktree remove` before removing state or uninstalling. See
[linux-overlayfs.md](linux-overlayfs.md) for mount lifecycle and recovery.

The table above was verified on APFS. Reflink backends behave the same way
because they also produce independent files; OverlayFS does not.

## Backing out cleanly

If you want Riftri fully gone, do it in this order. Each step is verified:

```console
$ riftri worktree list                      # find managed worktrees
$ riftri worktree remove ../task-1          # for each; refuses dirty views
$ riftri gc --apply --yes                   # reclaim the immutable bases
$ riftri disable                            # remove riftri.enabled from Git config
$ rm -rf "$(git rev-parse --git-common-dir)/riftri"
```

After `gc` has reclaimed the bases, the state directory contains only journal
records and deletes without permission errors. `riftri disable` removes the
repository's `riftri.enabled` setting, so the repository is left exactly as
Git would have it.

Keeping the worktrees but dropping Riftri is fine too — run `riftri disable`,
skip the removal step, and leave the existing worktrees in place. They keep
working as ordinary worktrees; you simply stop getting shared storage for new
ones.

To remove the executable itself, delete `~/.local/bin/riftri` on macOS and
Linux or `%LOCALAPPDATA%\Programs\Riftri\riftri.exe` on Windows. See
[install.md](install.md#updating-and-uninstalling).

## Deactivating interception

Interception is separate from installation, and both are opt-in.

```console
$ eval "$(riftri shell deactivate zsh)"   # this shell only
$ riftri disable                           # this repository only
$ riftri shell status                      # confirm the combined state
```

Riftri never edits shell startup files, so if you added the hook to a profile
yourself, remove that line. A repository without `riftri enable` uses real Git
even inside a hooked shell.

## What you give up

Backing out is safe, but it is not free. Plain `git worktree` has no
equivalent of the journaled recovery, the machine-readable failure receipts,
the pre-mutation compatibility checks, or the per-worktree allocation
reporting described in
[custom-harness.md](custom-harness.md#beyond-cheaper-worktrees). New
worktrees also return to costing a full checkout each.

That is the trade, and it is reversible in both directions: re-running
`riftri enable` opts a repository back in, and worktrees created afterwards
are optimized again. Worktrees created while Riftri was disabled stay
ordinary ones — Riftri does not adopt existing worktrees.

## Related

- [install.md](install.md) — installing and uninstalling the executable
- [global-activation.md](global-activation.md) — activation and reversal
- [troubleshooting.md](troubleshooting.md) — symptom-first fixes
- [cli.md](cli.md) — every command and flag
