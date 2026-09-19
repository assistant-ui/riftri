# Sparse checkout

Create a worktree with only the directories you need. Use `--sparse-dir`
once for each repository-relative directory:

```sh
riftri worktree add ../app-core -b feature/core main \
  --sparse-dir src --sparse-dir docs
```

Replace `src` and `docs` with directories in your chosen commit.

## What is included

Git cone mode includes the selected directories, repository-root files, and
files directly inside the selected directories' ancestors. It is still a
real Git worktree with its own branch and index.

## Current limits

- Use literal directory names, not file paths, wildcards, or negations.
- Request sparse views through `riftri worktree add`, not intercepted Git commands.
- Existing repository-level sparse configuration and Git LFS paths are unsupported.
- Sparse worktrees cannot be compacted yet.

You can change the selection later with Git's sparse-checkout commands.
Newly included files are ordinary checkouts, not new Riftri clones.
For a fresh copy-on-write selection, remove the clean view and create another.

Local changes still prevent clean removal. See [CLI reference](cli.md).
