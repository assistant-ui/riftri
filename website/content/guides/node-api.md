# Node API

The `riftri` npm package ships a programmatic API alongside the CLI. Install
it once and you get both the `riftri` command and a typed client.

```sh
npm install riftri
```

```js
import { Riftri } from "riftri";

const riftri = new Riftri({ repository: "/path/to/repo" });

if (await riftri.isOptimizable("../task-1")) {
  const created = await riftri.worktree.add("../task-1", { branch: "agent/task-1" });
  console.log(created.backend, created.reused_base);
}
```

The client resolves the native binary the same way the CLI does, so there is
no separate download or path to configure. TypeScript types ship with it.

## Methods

| Method | Returns |
| --- | --- |
| `doctor({ destination })` | Backend, readiness, blockers. Creates nothing |
| `isOptimizable(destination)` | `boolean` — the common `doctor` question |
| `enable()` / `disable()` | Per-repository opt-in |
| `worktree.add(path, opts)` | Backend, base, `reused_base`, journal path |
| `worktree.list()` | Managed worktrees with `allocated_bytes` |
| `worktree.remove(path, { force })` | Refuses a dirty worktree unless forced |
| `worktree.move` / `compact` / `prune` | Lifecycle operations |
| `status()` | Bases, totals, pending operations, diagnostics |
| `repair()` | Resume or roll back after an interrupted run |
| `gc({ apply })` | Plan, or reclaim unreferenced bases |

`run(args)` is the escape hatch for anything not wrapped.

## Errors

Failures throw a `RiftriError` carrying the parsed receipt and the exit code,
with the distinctions a runner needs:

```js
try {
  await riftri.worktree.add("../task-1", { branch: "agent/task-1" });
} catch (error) {
  if (error.isPolicyRefusal) {
    // Exit 3. Riftri declined and changed nothing — fall back to plain Git.
  } else if (error.isBusy) {
    // A live process holds the lock; waiting and retrying is correct.
  } else if (error.needsRepair) {
    await riftri.repair();
  }
}
```

`isPolicyRefusal`, `isUsageError`, `isBusy`, and `needsRepair` cover the
branches that matter; `error.receipt` has `code`, `category`, `phase`,
`cleanup`, `recovery`, and `nextCommand`.

## Availability

`npm install riftri` works on macOS, Linux, and Windows x64. The matching
native binary arrives as an optional dependency, so the client and the CLI
install together.

Windows on ARM64 is the exception: its native package is not on npm yet.
Install with the [PowerShell installer](installation.md) and point the client
at that binary:

```js
const riftri = new Riftri({ repository, binary: "C:\\riftri\\riftri.exe" });
```

See the [custom harness guide](custom-harness.md) for the lifecycle a runner
should drive, and [`examples/`](https://github.com/assistant-ui/riftri/tree/main/examples)
for runnable versions.
