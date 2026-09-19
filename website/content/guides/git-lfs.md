# Git LFS

Riftri can use standard Git LFS files when their objects are already present
locally. It does **not** download them during worktree creation.

## Fetch before creating

```sh
git lfs fetch
riftri doctor --destination ../app-auth
```

If the checks pass, create the worktree normally:

```sh
riftri worktree add ../app-auth -b feature/auth main
```

## What can block it

- Missing or invalid local LFS objects.
- Custom LFS storage, filter commands, or pointer extensions.
- An executable `post-checkout` hook, including one installed by Git LFS,
  or a custom `core.hooksPath`.

Do not remove a required hook just to enable Riftri. Use ordinary Git outside
Riftri interception if your LFS setup needs it. Sparse worktrees with LFS paths
are not supported yet.

The full eligibility and object-verification rules are in **Agent .md**.
