# Dependencies and build caches

Riftri shares committed files only. New worktrees do not inherit
`node_modules`, `target`, virtual environments, or other untracked output;
install dependencies in each worktree as usual.

## Share caches, not working directories

| Share through the tool | Keep per worktree |
| --- | --- |
| pnpm or npm package cache | `node_modules` |
| Cargo registry and Git cache | `target` |
| Python wheel/download cache | `.venv` |
| A supported compiler cache | `dist`, `build`, `.next` |

Do not symlink a writable dependency or build directory across branches:
concurrent installs and builds can overwrite each other's files.

## Check storage

```sh
riftri worktree list
riftri status
```

Large dependencies or build outputs can outweigh source savings; clean them
with the project's usual tools when no process is using them. Riftri does not
manage those caches.
