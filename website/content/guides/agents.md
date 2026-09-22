# Coding agents

Your agent uses ordinary Git worktrees; Riftri changes their storage, not how
the agent reads, edits, commits, or tests files. There is no plugin to
install, no tool to register, and no prompt to change — the agent keeps
calling `git worktree add` and that call takes the optimized path.

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

Both conditions are required. A repository without `riftri enable` uses real
Git even inside `riftri exec`, and an enabled repository is untouched outside
it, so nothing changes for your other projects.

## Why this suits parallel agents

One agent per task means one worktree per task, created and discarded
constantly. Worktrees of the same commit share a single immutable base, so
the second and later ones are cheap — concurrent creates are coordinated by
per-base locks, with one process materializing the base and the rest reusing
it.

When an agent run is killed mid-operation, `riftri repair` resumes or rolls
back what was interrupted instead of leaving half-made state. Clean removal
refuses a worktree with uncommitted work, so a crashed agent's edits are not
discarded silently.

## What the agent needs to know

- No special prompt; the agent creates worktrees by calling `git` through its
  inherited PATH.
- Embedded Git libraries and absolute Git paths bypass Riftri.
- Guided setup is interactive only. Scripts and harnesses should drive the
  commands directly — see the [custom harness guide](custom-harness.md) for
  the JSON reports, exit codes, and cleanup a runner needs.
- Not a sandbox for untrusted agents; use an OS sandbox, container, or VM for
  that boundary.
