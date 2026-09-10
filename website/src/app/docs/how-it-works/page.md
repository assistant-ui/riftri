---
title: "How Riftri works"
description: "The immutable base and private-view model behind lightweight Git workspaces."
section: "Concepts"
---

# How Riftri works

Riftri changes how worktree files are materialized. It does not replace Git's repository or
worktree model.

## One exact tree, many private views

```text
immutable base at an exact Git tree
├── feature/auth view    shared blocks + private edits
├── feature/billing view shared blocks + private edits
└── fix/tests view        shared blocks + private edits
```

An immutable base is identified by the repository, Git tree, checkout profile, and destination
filesystem volume. Riftri reuses a base only when those inputs match.

## Worktree creation

1. Riftri asks the installed Git executable to resolve the requested commit and tree.
2. Compatibility checks reject checkout behavior that cannot yet be reproduced safely.
3. Git creates real linked-worktree metadata with its normal checkout suppressed.
4. Riftri materializes or reuses the exact-tree immutable base.
5. The APFS backend creates strict native clones for regular files.
6. Riftri synchronizes the Git index and verifies that the new worktree is clean.

If a native clone cannot be created, the operation stops. Riftri does not silently copy the full
checkout.

## What happens after creation?

Nothing special sits between your tools and the files. An editor, compiler, test runner, or agent
reads and writes the worktree through the native filesystem. APFS allocates private blocks when a
file changes.

Git still handles:

- branch and checkout rules;
- status, diff, add, and commit;
- hooks and repository configuration;
- merge, rebase, fetch, push, and pull.

## Recovery model

Worktree lifecycle operations use durable journals. If a process stops midway, `riftri repair`
replays the known operation conservatively. Changed views are preserved for manual attention rather
than deleted on a guess.
