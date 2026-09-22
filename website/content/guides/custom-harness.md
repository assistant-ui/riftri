# Custom harness

Building your own runner. There is no SDK — the integration surface is the
CLI, its versioned JSON, and its exit codes.

## Pick one of two shapes

**Your runner creates the worktree.** Call Riftri directly — no shim, no
environment variables.

```sh
riftri worktree add ../task-1 -b agent/task-1 main --json
```

**Something downstream creates it** — an agent shelling out to
`git worktree add`. Wrap it; that program needs no knowledge of Riftri.

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

Repair first — your process can die mid-operation. Removal refuses dirty
worktrees. Prompts appear only on a terminal, so runners are never blocked.

## Branch on the exit code

| Code | Meaning | Action |
| --- | --- | --- |
| `0` | Success | Continue |
| `1` | Operational | Retry may help |
| `2` | Usage | Fix the call |
| `3` | Policy refusal | Nothing changed. Fall back or stop |

Code `3` means Riftri declined before touching anything, so retrying fails
identically. `--json-errors` returns one receipt carrying `code`, `category`,
`phase`, `cleanup`, `recovery`, and `nextCommand`.

## As your default workspace layer

Optimized worktrees need APFS, Btrfs, reflink XFS, ReFS, or OverlayFS, so a
harness that hard-requires Riftri fails for some users. Detect first —
`doctor` creates nothing:

```sh
riftri doctor --destination ../task-1 --json
```

Branch on `cow_backend_active`, `repository_enabled`, and
`destination_readiness.status`:

- Ready → use `riftri worktree add`
- `needs-activation` → run `riftri enable` once, then use Riftri
- No supported backend, or exit `3` → fall back to `git worktree add`

That fallback is well-defined because a policy refusal changed nothing.
Riftri is an optimization you take when available, not a dependency.

## Beyond cheaper worktrees

Disk is the headline; supervision is why harnesses stay. Operations are
journaled, so `repair` resumes or rolls back after a killed runner rather
than leaving half-made state. Unsupported checkouts stop before any state
exists. Forced removal snapshots first and refuses if the view changed.
`worktree list --json` reports `allocated_bytes` and `logical_bytes` per
worktree — what `gc` and `compact` act on.

None of that exists in plain `git worktree` — worth knowing what a fallback
gives up.

## Notes

Parallel adds are coordinated for you; `worktree-busy` with
`recovery: retry` is the one code that does mean wait. Keep the lifecycle in
your runner's code, not in model tools.
