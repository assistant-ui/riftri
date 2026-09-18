---
name: riftri-worktrees
description: Create and use copy-on-write Git worktrees with Riftri for isolated coding tasks, or explain repository opt-in and process-scoped agent integration. Use when the user requests Riftri or COW-backed worktrees, not for unrelated Git operations.
---

# Riftri worktrees

Riftri materializes real Git linked worktrees using native copy-on-write storage.
Git still owns commits, branches, merges, and remotes. Once created, the worktree
contains ordinary files; storage isolation does not depend on this skill or on
the agent remembering special file-access rules. It is not a security sandbox.

## Choose the right workflow

- If the user supplied an existing worktree, work there. Do not create another
  one or enable interception merely because this skill is loaded.
- If the task calls for a new isolated worktree, prefer the explicit CLI below.
  It works without shell hooks or repository enablement, including from an
  already-running agent.
- If the user is setting up an agent that creates worktrees through ordinary
  `git`, explain the opt-in launch flow below. Do not launch another agent
  recursively as a substitute for completing the current task.

## Create an explicit worktree

Check `riftri --version` and the installed command's `--help`. If Riftri is
missing, direct the user to the [installation guide](https://riftri.dev/index.md)
or install it only when authorized; this skill does not install the binary.

Agree on the destination, new or existing branch, and starting revision within
the user's task. `HEAD` means the current commit, not uncommitted working files.
From the source repository, inspect the exact intended destination:

```sh
riftri doctor --destination ../app-auth --json
```

Doctor's repository-activation notice concerns transparent Git interception;
explicit creation does not require enablement. Other compatibility or storage
blockers still apply. Do not infer backend support from the OS alone, ignore
blockers, install a privileged helper automatically, or silently fall back to
a full copy.

For a new branch, once the creation is within the authorized task:

```sh
riftri --json-errors worktree add ../app-auth -b feature/auth HEAD --json
git -C ../app-auth status --porcelain=v1
```

Use the returned destination for all task commands. For an existing branch,
omit `-b`: `riftri worktree add ../app-auth feature/auth --json`. Keep Git's
branch-safety refusals; do not force-reset an existing branch. If creation fails,
read the structured failure receipt and inspect `riftri status --json`. Follow
its recovery guidance only when authorized; do not repeatedly retry policy
refusals or manually delete journals, immutable bases, or unexpected paths.

## Explain process-scoped integration

After the user explicitly opts this repository in, they can start an installed
agent from their terminal:

```sh
riftri enable
riftri exec -- claude
```

Replace `claude` with the user's chosen installed executable, such as `codex`.
To start it in an existing registered worktree, use
`riftri exec --worktree ../app-auth -- claude`. The binding does not create a
worktree or imply repository enablement.

Inside that process tree, supported `git worktree` commands resolve through a
scoped PATH shim. Ordinary Git commands pass through. Absolute Git executables,
embedded Git libraries, and separately launched IDE processes are not covered.
Enabling a repository alone cannot retrofit the wrapper into a running agent.
The agent still needs a task or harness that calls for a separate worktree;
Riftri changes how it is created, not whether it is created.

Do not edit shell profiles, globally replace Git, install agents, or change
agent permission settings as part of this flow. No prompt or skill is required
for COW correctness. `riftri disable` reverses repository opt-in without deleting
worktrees; it does not remove a separately configured shell hook.

## Finish safely

Keep the worktree unless cleanup is requested or already part of the task.
When authorized, use `riftri worktree remove ../app-auth`; a dirty refusal is a
reason to stop and inspect, not to add `--force`. Never discard work or run
forced cleanup without explicit authorization. Bases remain cached after
removal: `riftri gc` previews collection, while `riftri gc --apply` deletes
unreferenced bases and requires a separate authorized cleanup decision.

Do not label summed file allocations as physical savings: shared COW blocks
can be counted more than once. Use measured volume-level evidence for savings
claims, and do not promise that creation is faster than ordinary Git.

References: [CLI](https://github.com/assistant-ui/riftri/blob/main/docs/cli.md),
[agent integration](https://github.com/assistant-ui/riftri/blob/main/docs/agent-integration.md),
[safety](https://github.com/assistant-ui/riftri/blob/main/docs/safety.md).
