# Backing out

Riftri is pre-release software that manages worktrees, so the fair question
before adopting it is what happens when you stop.

## Your work is never trapped

A Riftri-managed worktree is an **ordinary Git linked worktree**. Git created
it, Git tracks it, Git can remove it. Riftri only changed how the files were
materialized — copy-on-write clones instead of a full copy.

On APFS, reflink, and ReFS backends, clones are independent files that happen
to share unchanged blocks. Those blocks live as long as any file references
them, so removing Riftri does not reach into your worktree.

Verified on macOS/APFS:

| You do this | Your worktree |
| --- | --- |
| Stop running `riftri` | Unaffected |
| `riftri disable` | Unaffected |
| Delete the immutable bases | All content intact, `git status` clean |
| Delete `.git/riftri` entirely | Commit, branch, merge still work |
| `git worktree remove` with plain Git | Works without Riftri |

Bases are read-only on purpose, so a stray `rm -rf` fails rather than quietly
succeeding. Use `gc` to reclaim them properly.

**OverlayFS differs.** That backend is a live mount with the base as its
lower layer, so the base must stay while the view exists. Remove those views
with `riftri worktree remove` first.

## Removing it cleanly

```sh
riftri worktree list            # find managed worktrees
riftri worktree remove ../task-1  # for each; refuses dirty views
riftri gc --apply --yes         # reclaim the bases
riftri disable                  # drop riftri.enabled from Git config
rm -rf "$(git rev-parse --git-common-dir)/riftri"
```

Once `gc` has reclaimed the bases, the state directory deletes without
permission errors and the repository is left exactly as Git would have it.

Keeping the worktrees but dropping Riftri works too: run `riftri disable` and
leave them in place. They keep working; new ones simply cost a full checkout
again.

## Deactivating interception

```sh
eval "$(riftri shell deactivate zsh)"   # this shell
riftri disable                           # this repository
riftri shell status                      # confirm
```

Riftri never edits shell startup files, so remove the hook line yourself if
you added one.

## What you give up

Journaled recovery, machine-readable failure receipts, pre-mutation
compatibility checks, and per-worktree allocation reporting have no plain
`git worktree` equivalent. Re-running `riftri enable` opts back in, and
worktrees created afterwards are optimized again — but existing ones stay
ordinary, since Riftri does not adopt them.
