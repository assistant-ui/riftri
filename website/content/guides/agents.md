# Coding agents

Your agent uses ordinary Git worktrees; Riftri changes their storage, not how
the agent reads, edits, commits, or tests files.

## Easiest: guided setup

```sh
riftri setup
```

After creating a worktree, pick Claude Code, Codex, another installed
executable, or **Not now**. Riftri asks separately before enabling the
repository and launching; it never installs or signs in to agents.

## Launch an agent yourself

From your repository:

```sh
riftri enable
riftri exec -- claude
```

Replace `claude` with `codex` or another CLI. Inside that process and its
children, supported `git worktree` commands use Riftri; Git outside is
unchanged. To start in an existing worktree:

```sh
riftri exec --worktree ../app-auth -- codex
```

## What the agent needs to know

- No special prompt; the agent creates worktrees by calling `git` through its
  inherited PATH.
- Embedded Git libraries and absolute Git paths bypass Riftri.
- Guided setup is interactive only; scripts and harnesses should open
  **View .md** for explicit commands, JSON responses, failures, and cleanup.
- Not a sandbox for untrusted agents; use an OS sandbox, container, or VM for
  that boundary.
