# Riftri documentation

Riftri creates Git worktrees that share storage for unchanged files. Each
worktree keeps its own branch, files, and edits; your editor and Git commands
work as usual.

## Start with one worktree

[Install Riftri](guides/installation.md), then run in an existing repository:

```sh
riftri setup
```

Pick a directory and branch, review the plan, and confirm. Setup starts from
the current commit, not uncommitted changes, and can end by opening an
installed coding agent.

## Choose your workflow

- [CLI reference](guides/cli.md): manage worktrees yourself.
- [Coding agents](guides/agents.md): launch an agent with Riftri enabled.
- [Shell activation](guides/activation.md): keep using `git worktree` in your terminal.

## Before you start

- Requires a [supported filesystem](guides/filesystem-compatibility.md);
  unsupported setups stop, never silently full-copy.
- Dependencies and build outputs stay separate per worktree.
- Experimental: keep important work committed or backed up. Worktree
  isolation is not a security sandbox.
