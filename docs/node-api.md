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

`error.exitCode` is always a number. A process killed by a signal reports
`128 +` the signal number, the same convention native `riftri exec` uses, and
sets `error.signal` and `error.wasSignalled`.

### When `isOptimizable()` returns false

`false` means Riftri answered the question: no copy-on-write backend is active
here, the report marks this destination `blocked`, or Riftri refused it
outright (exit 3).

The backend check alone is not enough. `cow_backend_active` describes the
volume, so outside a Git repository it stays `true` while `worktree.add` cannot
succeed; the destination's readiness decides. A `needs-activation` destination
is optimizable, since `worktree.add` works without `enable()`.

It does not mean "something went wrong". A missing or non-executable binary,
an unreadable repository, and output that is not JSON all reject, so a broken
installation cannot be mistaken for an unsupported repository:

```js
try {
  if (await riftri.isOptimizable("../task-1")) { /* optimized path */ }
  else { /* supported answer: fall back to plain Git */ }
} catch (error) {
  // The client could not run at all — surface this, do not fall back silently.
}
```

## Constructor options

```js
new Riftri({
  repository: "/path/to/repo", // cwd for every command; defaults to process.cwd()
  binary: "/usr/local/bin/riftri", // explicit executable; defaults to the installed platform binary
  stateDir: "/custom/state", // passed as --state-dir where the command supports it
});
```

## How it relates to the CLI

The binary is the implementation. This client spawns it, adds `--json` where
the command supports it and `--json-errors` everywhere, parses the single
report on stdout, and converts a non-zero exit into a `RiftriError`. It
reimplements no Riftri behaviour, so its guarantees are exactly the CLI's
guarantees.

`--json` is not accepted by every command — `enable`, `disable`, and `exec`
print human text — so the client omits it for those and resolves `null`
rather than parsing their output.

## Exit codes

| Constant | Code | Meaning |
| --- | --- | --- |
| `EXIT_SUCCESS` | `0` | Success |
| `EXIT_OPERATIONAL` | `1` | Git, storage, journal, or I/O failure |
| `EXIT_USAGE` | `2` | Malformed request; never retry unchanged |
| `EXIT_POLICY` | `3` | Riftri declined; nothing changed |

These are exported so a runner can branch on `error.exitCode` directly.

## Availability

`npm install riftri` works on macOS, Linux, and Windows x64. The package
pulls the matching native binary as an optional dependency, so the client
and the CLI arrive together.

Windows on ARM64 is the exception: its native package is not yet on npm, so
install with the [PowerShell installer](install.md#windows-powershell) and
point the client at that binary:

```js
const riftri = new Riftri({ repository, binary: "C:\\path\\to\\riftri.exe" });
```

See the [custom harness guide](custom-harness.md) for the lifecycle a runner
should drive, and [`examples/`](https://github.com/assistant-ui/riftri/tree/main/examples)
for runnable versions.
