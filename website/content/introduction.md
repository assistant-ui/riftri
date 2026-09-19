---
title: Introduction
description: Real Git worktrees. Shared storage. Private edits.
---

# Riftri documentation

Riftri is an opt-in copy-on-write storage accelerator for real Git linked worktrees.
Unchanged files share storage. Each worktree keeps its own changes, branch, and Git index.

## Start with one worktree

Install the native CLI on macOS or Linux:

```sh
curl -fsSL https://riftri.dev/install.sh | bash
```

From an existing Git repository, open the guided setup:

```sh
riftri setup
```

Setup checks support, shows a plan, and asks before creating a worktree. Afterward,
you can choose an installed coding agent to open, or continue without one.

Continue with [installation](../../docs/install.md), the [CLI reference](../../docs/cli.md),
or [coding agent setup](../../docs/agent-integration.md).

## What stays the same

Keep using your editor, build tools, and ordinary Git commands. Git owns branches,
commits, merges, and linked worktree registration. Riftri does not sit between your
tools and the filesystem.

## Native storage, explicit support

Riftri uses APFS clones on macOS, reflinks or supported OverlayFS configurations on
Linux, and block clones on Windows ReFS. It refuses unsupported destinations rather
than silently making a full copy. Ordinary NTFS is not supported.
