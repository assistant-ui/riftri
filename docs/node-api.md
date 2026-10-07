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
| `worktree.owner(path)` | Managing `state_directory`, or `null` for an unmanaged path; no disk-usage walk |
| `worktree.remove(path, { force })` | Refuses a dirty worktree unless forced |
| `worktree.move` / `compact` / `prune` | Lifecycle operations |
| `status()` | Bases, totals, pending operations, diagnostics |
| `repair()` | Resume or roll back after an interrupted run |
| `gc({ apply })` | Plan, or reclaim unreferenced bases |

`run(args)` is the escape hatch for anything not wrapped.

`worktree.owner(path)` always consults the default and every repository-registered
state directory, even when the client has a `stateDir` configured. A missing or
invalid registration, ambiguous ownership, or an incomplete operation rejects;
never turn that error into ordinary-Git cleanup. The result is advisory: pass
the returned state directory to the lifecycle command, which revalidates under
its operation lock. The report includes `state_directory_native_hex` so a lossy
display path need not be used as an identity.

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
  } else if (error.isStorageFull) {
    // Free space on the affected volume before running recovery.nextCommand.
  } else if (error.needsRepair) {
    await riftri.repair();
  }
}
```

`isPolicyRefusal`, `isUsageError`, `isBusy`, `isStorageFull`, and
`needsRepair` cover the branches that matter; `error.receipt` has `code`,
`category`, `phase`, `cleanup`, `recovery`, and `nextCommand`.

`error.exitCode` is always a number. A process killed by a signal reports
`128 +` the signal number, the same convention native `riftri exec` uses, and
sets `error.signal` and `error.wasSignalled`.

A failed `post-checkout` hook leaves the newly created worktree in place, as
Git does. The API rejects with the hook's exit code and preserves the parsed
creation report in `error.report` (typed `unknown`; validate it before use).
Do not retry creation or fall back to Git in this case. In particular, a hook
exiting `3` is **not** `isPolicyRefusal`: safe fallback requires a native policy
receipt confirming that no cleanup is needed, not just an exit code. Plain
`exec` child exits and malformed or missing receipts are not safe refusals either.

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

Runners that already supervise child processes can import `resolveBinary()`
from `riftri` to get the verified native executable path without starting it.
It uses the same platform/version checks and `RIFTRI_BINARY` override as the
launcher. Spawn it directly (without a shell) and use the CLI's structured
reports, receipts, and journal recovery contract. Keep the `riftri` package and
its native optional dependency external when bundling: resolution depends on
their installed files. A missing or unusable executable throws before spawning.

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

`npm install riftri` works on macOS, Linux, and Windows (x64 and ARM64). The
package pulls the matching native binary as an optional dependency, so the
client and the CLI arrive together.

See the [custom harness guide](custom-harness.md) for the lifecycle a runner
should drive, and [`examples/`](https://github.com/assistant-ui/riftri/tree/main/examples)
for runnable versions.
