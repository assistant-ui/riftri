# Coding agents

Your agent uses ordinary Git worktrees. Riftri changes their storage, not how
the agent reads, edits, commits, or tests files.

## Easiest: guided setup

```sh
riftri setup
```

After creating a worktree, choose Claude Code, Codex, another installed
executable, or **Not now**. Riftri asks separately before enabling the
repository and launching the agent. It does not install or sign in to agents.

## Launch an agent yourself

From your repository, opt in and start your installed agent:

```sh
riftri enable
riftri exec -- claude
```

Replace `claude` with `codex` or another CLI. Inside that process and its
children, supported `git worktree` commands use Riftri. Git outside it is unchanged.

To start in an existing worktree:

```sh
riftri exec --worktree ../app-auth -- codex
```

## What the agent needs to know

No special prompt is required for interception. The agent still needs to
choose to create a worktree and call `git` through its inherited PATH.
Tools using embedded Git libraries or absolute Git paths bypass Riftri.

For scripts and harnesses, use the **Agent .md** reference for explicit
commands, JSON responses, failures, and cleanup. Guided setup is interactive only.

Riftri is not a sandbox for untrusted agents. Use an appropriate OS sandbox,
container, or VM when you need that boundary.
