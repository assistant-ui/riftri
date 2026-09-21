# Custom harness

Building your own runner — the loop that creates a workspace per task, runs
something in it, and cleans up. There is no SDK: the integration surface is
the CLI, its versioned JSON, and its exit codes.

## Pick one of two shapes

**Your runner creates the worktree.** Call Riftri directly; no shim, no
environment variables.

```sh
riftri worktree add ../task-1 -b agent/task-1 main --json
```

**Something downstream creates it** — an agent that shells out to
`git worktree add`. Wrap it, and that program needs no knowledge of Riftri.

```sh
riftri exec -- ./run-agent.sh
```

## The lifecycle

| When | Command |
| --- | --- |
| Runner startup | `riftri repair --json` |
| Per task | `riftri worktree add … --json` |
| Task ends | `riftri worktree remove … --json` |
| Periodically | `riftri gc --apply --yes --json` |

Repair first: your process can die mid-operation. Removal refuses dirty
worktrees by default. Prompts appear only on a terminal, so a
non-interactive runner is never blocked.

## Branch on the exit code

| Code | Meaning | Action |
| --- | --- | --- |
| `0` | Success | Continue |
| `1` | Operational | Retry may help |
| `2` | Usage | Fix the call |
| `3` | Policy refusal | Nothing changed. Fall back or stop |

Code `3` means Riftri declined before touching anything, so retrying the
same command fails identically. With `--json-errors`, the failure arrives as
one receipt on stderr carrying `code`, `category`, `phase`, `cleanup`,
`recovery`, and `nextCommand`.

## As your default workspace layer

Optimized worktrees need APFS, Btrfs, reflink-enabled XFS, ReFS, or
OverlayFS, so a harness that hard-requires Riftri fails for some users.
Detect first — `doctor` creates nothing:

```sh
riftri doctor --destination ../task-1 --json
```

Branch on `cow_backend_active`, `repository_enabled`, and
`destination_readiness.status`:

- Ready → use `riftri worktree add`
- `needs-activation` → run `riftri enable` once, then use Riftri
- No supported backend, or exit `3` → fall back to `git worktree add`

That fallback is always well-defined, because a policy refusal changed
nothing. Treat Riftri as an optimization you take when available, not a
dependency you require.

## Notes

Independent runners can add worktrees concurrently; per-base locks
coordinate them. A worktree whose lock is held by a live process reports
`worktree-busy` with `recovery: retry` — that one does mean wait.

Reports pair display paths with `*_native_hex` fields; read those for
non-UTF-8 paths.

Keep the lifecycle in your runner's deterministic code rather than exposing
it as model tools — the advantage is that the model never learns anything
new.
