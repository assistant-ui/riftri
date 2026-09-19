# CLI reference

Run these from your repository. New to Riftri? Start with `riftri setup`.

## Create a worktree

Check the destination, then create a branch from `main`:

```sh
riftri doctor --destination ../app-auth
riftri worktree add ../app-auth -b feature/auth main
```

Replace `main` with your starting branch or commit. The new directory is a
normal Git worktree. Uncommitted changes in the original are not copied.

| Option | Use it to |
| --- | --- |
| `-b <branch>` | Create a new branch |
| `--detach` | Work without creating a branch |
| `--sparse-dir <dir>` | Include selected directories; see [sparse checkout](sparse-checkout.md) |

## Inspect your worktrees

```sh
riftri worktree list
riftri status
```

`list` shows managed worktrees. `status` also shows shared bases, storage use,
and anything needing attention.

## Move or remove one

```sh
riftri worktree move ../app-auth ../app-login
```

Moves must stay on the same volume. Mounted OverlayFS worktrees cannot be moved.

Once you have saved your work:

```sh
riftri worktree remove ../app-login
```

Removal refuses local changes. `--force` discards them; use it only when you
intend to lose that work.

## Recover and reclaim space

After an interrupted operation, inspect `riftri status` and follow its recovery
guidance. `riftri repair` can resume or roll back recorded operations.

Preview unused-base cleanup:

```sh
riftri gc
```

Run `riftri gc --apply` only after reviewing the plan. To reclaim private
storage in a pristine, full native-clone worktree, use
`riftri worktree compact <path>`. It refuses untracked and ignored files too.

## More options

```sh
riftri worktree add --help
```

Use `--help` with any command. The **Agent .md** reference has the complete
flag, JSON, exit-code, and recovery contracts.
