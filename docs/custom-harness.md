# Building a custom harness on Riftri

[agent-integration.md](agent-integration.md) shows the setup for named
harnesses. This page is for the runner you wrote yourself: the loop that
creates a workspace per task, runs something in it, and cleans up.

Nothing here is specific to any agent. Riftri has no plugin API, no SDK, and
no agent-facing protocol — the integration surface is the CLI, its versioned
JSON, and its exit codes.

## Decide one thing first

**Does your runner create the worktree, or does something downstream create
it?**

*Your runner creates it.* Call Riftri directly and skip interception
entirely:

```console
$ riftri worktree add ../task-1 -b agent/task-1 main --json
```

No shim, no environment variables, no shell hook. This is the predictable
path and the one to choose when you control the code that makes worktrees.

*Something downstream creates it* — an agent that shells out to
`git worktree add`, or a script you do not control. Wrap it:

```console
$ riftri exec -- ./run-agent.sh
```

Inside that process tree, `git worktree add` takes the optimized path and
every other Git command is delegated unchanged. The wrapped program needs no
knowledge of Riftri.

Every named harness in the other guide is just one of these two shapes.

## The lifecycle your runner drives

**At startup, before accepting work.** Your process can die mid-operation, so
recover first:

```console
$ riftri repair --json
```

Repair resumes or rolls back interrupted operations, skips anything a live
process still owns, and exits non-zero when something needs a human. Safe to
run every boot and on a schedule.

**Per task.** Create the workspace:

```console
$ riftri worktree add ../task-1 -b agent/task-1 main --json
```

**When the task ends.** Clean removal refuses a dirty worktree, which is
usually what you want — it stops a crashed agent's work from disappearing:

```console
$ riftri worktree remove ../task-1 --json
```

Use `--force --yes` only when discarding is the explicit intent. Riftri
records an exact content snapshot first and stops if the view changed after
that intent was recorded.

**Periodically.** Collect bases no worktree references any more:

```console
$ riftri gc --apply --yes --json
```

`gc` without `--apply` deletes nothing and reports the plan. Confirmation
prompts only appear on a terminal, so a non-interactive runner is never
blocked — `--yes` is for interactive scripts.

## Reading results

Every command that reports or changes state accepts `--json` and prints one
versioned report on stdout:

```json
{
  "schema_version": 1,
  "backend": "apfs-clone",
  "destination": "../task-1",
  "commit": "…",
  "tree": "…",
  "base_path": "…",
  "reused_base": true,
  "journal_path": "…",
  "native_path_encoding": "…"
}
```

`reused_base` tells you whether this add shared an existing base or
materialized a new one — useful for reporting how much the second and later
workspaces actually cost.

Failures are where a harness earns its reliability. Branch on the exit code
before parsing anything:

| Code | Meaning | What a runner should do |
| --- | --- | --- |
| `0` | Success | Continue |
| `1` | Operational — Git, storage, journal, I/O | Retry may help; check `recovery` |
| `2` | Usage — bad flag or argument | Fix the call; never retry |
| `3` | **Policy refusal** | Riftri will not optimize this. Nothing changed. Fall back or stop |

A killed process has no exit code of its own. Report `128 +` the signal
number, as a shell does, rather than passing the null through: a runner
comparing against zero would otherwise read a killed command as success.
`examples/lib/riftri.mjs` does this in both `riftri()` and `run()`.

Code `3` is the one that matters most. It means Riftri declined *before
touching anything* — the request is intact, no cleanup is pending, and
retrying the identical command will fail identically.

With `--json-errors`, the failure arrives as one receipt on stderr:

```json
{
  "schemaVersion": 1,
  "outcome": "failed",
  "operation": "worktree-add",
  "code": "invalid-request",
  "category": "policy",
  "message": "invalid worktree request: destination already exists: ../t1",
  "phase": null,
  "cleanup": "not-needed",
  "recovery": "not-required",
  "nextCommand": null
}
```

`category` mirrors the exit code (`policy` → 3, `operational` → 1), so you
can branch on either. `recovery` and `nextCommand` tell you whether a
follow-up is required — a `recovery` of `required` with a `nextCommand` of
`riftri repair …` means do that before continuing.

`--json-errors` implies `--no-progress`, so stderr stays exactly one JSON
document.

## Beyond cheaper worktrees

Disk is the headline, but it is not the reason most harnesses stay. Plain
`git worktree` gives a runner no way to answer "what happened?" — Riftri
turns the worktree lifecycle into something a program can supervise.

**Operations are journaled and recoverable.** Every add, remove, move,
prune, and compaction advances through a durable state machine. If your
runner is killed halfway through creating a workspace, `riftri repair`
resumes or rolls it back and reports what it did. Plain Git leaves a
half-created worktree and stale metadata for you to untangle by hand.

**Outcomes are machine-readable.** Failures arrive as one receipt naming the
durable phase reached, whether cleanup is pending, and whether recovery is
required — and exit code `3` separates "we refuse" from "something broke."
A runner can decide what to do without matching on error text.

**Correctness is checked before mutation, not after.** Configurations Riftri
cannot reproduce exactly — sparse checkout, submodules, custom filters,
conditional config includes — stop with an explanation before any state
exists. The alternative is discovering mid-task that a workspace does not
match the commit it claims.

**Destructive actions are guarded.** Clean removal refuses a dirty worktree.
A forced removal records an exact content snapshot first and stops if the
view changed after that intent — so a task that wrote files while you were
tearing it down does not lose them silently.

**Storage is observable.** `worktree list --json` reports `allocated_bytes`
and `logical_bytes` per worktree alongside its backend and shared base, and
`status --json` adds totals, pending operations, and `diagnostic_issues`.
That is what lets a harness show users where disk actually went, and what
`gc` and `worktree compact` act on when reclaiming it.

**Parallel creation is coordinated.** Concurrent adds of the same base are
serialized by per-base locks: one process materializes it, the rest verify
and reuse, so you do not need a lock of your own around `worktree add`.

None of this exists in plain `git worktree`. It is worth knowing exactly
what a fallback path gives up.

## Making Riftri the default workspace layer

A harness that wants Riftri as its normal path — not an opt-in flag — needs
one more thing: **graceful degradation.** Optimized worktrees require APFS,
Btrfs, reflink-enabled XFS, ReFS, or OverlayFS. Your users will not all have
one, so a harness that hard-requires Riftri will fail for some of them.

Detect capability once per repository and destination, before you commit to a
path:

```console
$ riftri doctor --destination ../task-1 --json
```

The report answers the question directly:

```json
{
  "cow_backend_active": true,
  "repository_enabled": false,
  "destination_readiness": {
    "destination": "../task-1",
    "status": "needs-activation",
    "backend": "apfs-clone",
    "copy_on_write": true
  },
  "storage_capabilities": [ … ]
}
```

- `cow_backend_active` — is there a usable copy-on-write backend here
- `repository_enabled` — has this repository opted in with `riftri enable`
- `destination_readiness.status` — the combined verdict, with blockers and
  the exact next command when it is not ready

`doctor` creates nothing, so it is safe to call on every startup.

The resulting policy is simple:

- Ready → use `riftri worktree add`
- `needs-activation` → run `riftri enable` once, then use Riftri
- No supported backend, or exit code `3` from an add → **fall back to plain
  `git worktree add`** and continue

That last branch is what makes Riftri safe to adopt as a default. A policy
refusal changed nothing, so falling back to ordinary Git is always
well-defined. Treat Riftri as an optimization you take when available, not a
dependency you require, and your harness works everywhere while getting
cheap workspaces where the filesystem allows it.

Record which path you took. When users report disk usage, you want to know
whether they got optimized workspaces or the fallback.

## Concurrency

Parallel adds are coordinated for you, as described above. The one case your
runner must handle is a worktree whose operation lock is held by a live
process: that reports `"code": "worktree-busy"` with `"recovery": "retry"`.
Unlike a policy refusal, that one genuinely means wait and try again.

## Paths that are not valid UTF-8

Reports pair every display path with a `*_native_hex` field and name the
encoding in `native_path_encoding`. If your runner handles repositories with
non-UTF-8 paths or refs, read the hex fields; the display strings are lossy
and intended for humans.

## What not to build

**Do not wrap Riftri in an MCP server or expose it as a model tool.** The
worktree lifecycle belongs in your runner's deterministic code, not in the
model's tool list. Riftri's advantage for agents is that the model never
learns anything new — `riftri exec` makes the plain `git worktree add` an
agent already runs take the optimized path.

**Do not parse human output.** Every command that matters has `--json` and
`--json-errors`. The human text is not a stable interface.

**Do not retry a policy refusal.** Exit code `3` is deterministic. Fall back
or surface it; retrying the same command wastes time and hides the reason.

## Runnable examples

[`examples/`](../examples/) implements everything on this page as scripts you
can run against your own repository — no dependencies, no build step:

- [`basic-runner`](../examples/basic-runner/) — the lifecycle above, end to end
- [`fallback-detection`](../examples/fallback-detection/) — Riftri as a default
  layer with a plain-Git fallback
- [`parallel-tasks`](../examples/parallel-tasks/) — many workspaces at once and
  what base sharing saves
- [`wrapped-agent`](../examples/wrapped-agent/) — optimizing an agent you do not
  control

## Related

- [agent-integration.md](agent-integration.md) — setup for Claude Code,
  Codex, containers, and CI
- [cli.md](cli.md) — every command, flag, and exit code
- [build-caches.md](build-caches.md) — why `node_modules` and `target` are
  not shared, and what to do about it
- [troubleshooting.md](troubleshooting.md) — symptom-first fixes
