# Git LFS

Riftri uses Git LFS files whose objects are already present locally; it does
**not** download them during worktree creation.

## Fetch before creating

```sh
git lfs fetch
riftri doctor --destination ../app-auth
```

If the checks pass:

```sh
riftri worktree add ../app-auth -b feature/auth main
```

## What can block it

- Missing or invalid local LFS objects.
- Custom LFS storage, filter commands, or pointer extensions.
- An executable `post-checkout` hook (including Git LFS's own) or a custom
  `core.hooksPath`.
- Sparse worktrees with LFS paths are not supported yet.

Do not remove a required hook to enable Riftri; use ordinary Git outside
interception instead. Full eligibility and object-verification rules:
**Agent .md**.
