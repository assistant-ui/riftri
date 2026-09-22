# Examples

Runnable harness patterns. Each folder is one idea, in plain Node with no
dependencies and no build step — clone the repository and run them directly.

```console
$ node examples/basic-runner/run.mjs /path/to/your/repository
```

Every example takes a repository path as its first argument, creates its
worktrees beside it, and cleans up after itself. They need `riftri` on `PATH`
and a destination on a supported filesystem — except `fallback-detection`,
which is the one that handles not having either.

| Example | The idea |
| --- | --- |
| [basic-runner](basic-runner/) | The core loop: recover, create, work, remove, collect. **Start here.** |
| [fallback-detection](fallback-detection/) | Use Riftri as your default layer, fall back to plain Git when it is unavailable |
| [parallel-tasks](parallel-tasks/) | Many workspaces at once, and what base sharing actually saves |
| [wrapped-agent](wrapped-agent/) | Optimizing an agent you do not control, without changing it |

[`lib/riftri.mjs`](lib/riftri.mjs) is the shared client the examples use. It is
about 80 lines and is the entire integration surface: spawn the CLI, parse one
JSON report from stdout, parse one failure receipt from stderr, branch on the
exit code. Copy it next to your runner and adapt it.

## What each one shows

**basic-runner** — `repair` at startup because your process can die
mid-operation, then add, work, remove per task, then `gc`. Run it twice with
different task names and watch `reused_base` flip to `true` on the second
worktree: that is the base sharing.

**fallback-detection** — `doctor` answers whether this destination can be
optimized without creating anything. If it cannot, or if Riftri is not
installed at all, the runner uses plain `git worktree add` and keeps going.
Both paths produce a real worktree. This is what makes Riftri safe to adopt as
a default rather than a requirement.

**parallel-tasks** — six concurrent adds, one of which materializes the
immutable base while the rest reuse it. Per-base locks handle the
coordination, so the example has no locking of its own. It retries only
`worktree-busy`, which is the one failure where waiting helps, and never
retries a policy refusal, which is deterministic.

**wrapped-agent** — the other integration shape. `agent.sh` is a stand-in for
an agent you do not control: it runs plain `git worktree add` and knows
nothing about Riftri. Wrapping it in `riftri exec` puts a Git shim on `PATH`
for that process tree, and the worktree it creates is managed. The agent needs
no prompt, plugin, or configuration.

## Reading the output

`worktree add --json` reports the backend, whether an existing base was
reused, and the journal path. `worktree list --json` reports
`allocated_bytes` and `logical_bytes` per worktree — the difference between
them is what copy-on-write saved.

## Related

- [docs/custom-harness.md](../docs/custom-harness.md) — the guide these
  implement
- [docs/agent-integration.md](../docs/agent-integration.md) — setup for named
  harnesses
- [docs/cli.md](../docs/cli.md) — every command, flag, and exit code
