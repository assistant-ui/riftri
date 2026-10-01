# Sparse checkout

Create a worktree with only the directories you need, one `--sparse-dir` per
repository-relative directory:

```sh
riftri worktree add ../app-core -b feature/core main \
  --sparse-dir src --sparse-dir docs
```

Git cone mode includes the selected directories, repository-root files, and
files directly inside the selection's ancestors. The result is a real Git
worktree with its own branch and index.

## Current limits

- Literal directory names only; no file paths, wildcards, or negations.
- Explicit selections use `riftri worktree add --sparse-dir`.
- Adds issued inside a cone-mode sparse worktree inherit its selection, including
  intercepted Git adds. Explicit sparse options through interception remain unsupported.
- Sparse selections combined with Git LFS paths are unsupported.
- Pristine native-COW sparse views can compact at their original selection;
  compaction after a selection change remains unsupported.

You can change the selection later with Git's sparse-checkout commands, but
newly included files are ordinary checkouts, not new Riftri clones; for a
fresh copy-on-write selection, remove the clean view and create another.
Local changes still prevent clean removal; see [CLI reference](cli.md).
