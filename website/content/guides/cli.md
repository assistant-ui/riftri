# CLI reference

Run these from your repository. New to Riftri? Start with `riftri setup`.

## Create a worktree

```sh
riftri doctor --destination ../app-auth
riftri worktree add ../app-auth -b feature/auth main
```

Replace `main` with any starting branch or commit. The result is a normal
Git worktree; uncommitted changes in the original are not copied.

| Option | Use it to |
| --- | --- |
| `-b <branch>` | Create a new branch |
| `--detach` | Work without creating a branch |
| `--sparse-dir <dir>` | Include selected directories; see [sparse checkout](sparse-checkout.md) |

## Inspect

```sh
riftri worktree list
riftri status
```

`status` adds shared bases, storage use, and anything needing attention.

## Move or remove

```sh
riftri worktree move ../app-auth ../app-login
riftri worktree remove ../app-login
```

- Moves must stay on the same volume; mounted OverlayFS worktrees cannot be moved.
- Removal refuses local changes; `--force` discards them.

## Recover and reclaim space

| Command | Does |
| --- | --- |
| `riftri repair` | Resume or roll back an interrupted operation; `riftri status` shows recovery guidance |
| `riftri gc` | Preview unused-base cleanup; apply with `riftri gc --apply` only after reviewing the plan |
| `riftri worktree compact <path>` | Reclaim private storage in a pristine, full native-clone worktree; refuses untracked and ignored files |

## More options

`--help` works with every command. Complete flag, JSON, exit-code, and
recovery contracts: **View .md**.
