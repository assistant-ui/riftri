# Dependencies and build caches

Riftri shares committed files. New worktrees do not inherit `node_modules`,
`target`, virtual environments, or other untracked build output.
Install dependencies inside each worktree as usual.

## Share caches, not working directories

Package-manager download stores can be reused across worktrees. Keep mutable
install and build directories separate.

| Share through the tool | Keep per worktree |
| --- | --- |
| pnpm or npm package cache | `node_modules` |
| Cargo registry and Git cache | `target` |
| Python wheel/download cache | `.venv` |
| A supported compiler cache | `dist`, `build`, `.next` |

Do not symlink one writable dependency or build directory across branches.
Concurrent installs and builds can overwrite each other's files.

## Check storage

```sh
riftri worktree list
riftri status
```

Large dependencies or build outputs can outweigh source-file savings.
Clean them with the project's usual tools when no process is using them.
Riftri does not manage those caches for you.
