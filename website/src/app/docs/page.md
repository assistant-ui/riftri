---
title: "Riftri documentation"
description: "Create, use, inspect, and safely clean up lightweight Git workspaces."
section: "Start"
---

# Riftri documentation

Riftri creates real Git linked worktrees whose unchanged files share physical storage. Developers,
editors, test runners, and coding agents keep using ordinary paths and Git commands.

> **Experimental:** optimized worktree operations currently require macOS and a writable APFS
> volume. Keep important changes committed or backed up while evaluating Riftri.

## Start here

- [Getting started](/docs/getting-started) — check compatibility and create your first worktree.
- [How Riftri works](/docs/how-it-works) — understand immutable bases, native clones, and real Git
  metadata.
- [Git interception](/docs/git-interception) — choose explicit, process-scoped, or shell-scoped use.
- [Storage and cleanup](/docs/storage-and-cleanup) — inspect allocation and recover interrupted
  operations.
- [Compatibility](/docs/compatibility) — review supported platforms and checkout behavior.
- [CLI commands](/docs/commands) — find the command for each worktree lifecycle operation.

## The short version

```console
$ riftri doctor --destination ../app-auth
$ riftri worktree add ../app-auth -b feature/auth main
$ cd ../app-auth
$ git status
```

The resulting directory is a normal Git linked worktree. Riftri stays out of ordinary file reads,
writes, builds, and editor operations.

## Product boundary

Riftri owns optimized worktree materialization, storage capability detection, recovery, cleanup, and
disk accounting. Git continues to own repositories, branches, commits, merges, remotes, hooks, and
credentials.
