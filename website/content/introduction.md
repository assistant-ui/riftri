# Riftri documentation

Riftri creates Git worktrees that share storage for unchanged files. Each
worktree has its own branch, files, and edits. Keep using your usual editor
and Git commands.

## Start with one worktree

[Install Riftri](guides/installation.md), then run this in an existing repository:

```sh
riftri setup
```

Choose a directory and branch, review the plan, and confirm. Setup can then
open an installed coding agent, or leave you with a ready worktree.
It starts from the current commit, not your uncommitted changes.

## Choose your workflow

- [CLI reference](guides/cli.md): create, inspect, and remove worktrees yourself.
- [Coding agents](guides/agents.md): launch an agent with Riftri enabled.
- [Shell activation](guides/activation.md): keep using `git worktree` in your terminal.

## Before you start

Riftri needs a [supported filesystem](guides/filesystem-compatibility.md).
It stops if storage or repository features are unsupported; it does not
silently make a full copy. Dependencies and build outputs stay separate.

Riftri is experimental. Keep important changes committed or backed up.
Worktree isolation is not a security sandbox.
