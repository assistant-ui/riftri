# Integrating Riftri with coding agents

Riftri's primary audience includes harnesses that create one Git worktree per
task. This guide describes the three integration models, per-harness setup,
the machine-readable output contract, and the lifecycle an agent runner should
drive. Every command here is ordinary Riftri or Git; nothing is agent-specific
inside Riftri itself.

Before integrating, confirm the repository and destination volume are
compatible:

```console
$ riftri doctor --destination ../task-1 --json
```

`doctor` reports the selected backend, copy-on-write availability, blockers
with remedies, and the exact next command, without creating any state.

## Choose an integration model

**Explicit CLI.** The harness calls Riftri directly and never touches Git
interception:

```console
$ riftri worktree add ../task-1 -b agent/task-1 main
$ riftri worktree remove ../task-1
```

Use this when you control the commands the runner executes. It is the most
predictable model: no environment variables, no shims, and unsupported
configurations fail closed with an explanation before any mutation.

**Process-scoped interception.** The harness starts each agent through
`riftri exec`, and only that process tree sees the Git shim:

```console
$ riftri enable
$ riftri exec -- claude
$ riftri exec --worktree ../task-1 -- codex
```

Inside the tree, plain `git worktree add -b agent/task-1 ../task-1 main`
creates an optimized view; every other Git command is delegated unchanged to
the real Git executable. Git outside the launched process is untouched.
`--worktree` additionally binds the process to one exact registered worktree
root.

**Shell or profile activation.** For interactive use, or harnesses spawned
from a shell you control, evaluate the hook and opt repositories in:

```console
$ eval "$(riftri shell hook zsh)"
$ cd repository && riftri enable
```

Both conditions are required: a repository without `riftri enable` uses real
Git directly even inside a hooked shell. `riftri shell status` reports the
combined effect. See [global activation](global-activation.md) for the full
compatibility matrix and reversal steps.

## Harness recipes

**Claude Code.** Claude Code creates task worktrees with plain `git worktree`
commands, so either launch it as `riftri exec -- claude` in an enabled
repository, or add the shell hook to your profile and run `riftri enable` once
per repository. Child processes inherit interception; no Riftri-specific
prompt or skill is required. The activation test matrix explicitly covers
`claude`-named child harnesses.

**Codex and other CLI harnesses.** Identical pattern:
`riftri exec -- codex`, or shell activation. Harnesses that manage worktrees
themselves can instead call `riftri worktree add`/`remove` explicitly and skip
interception entirely.

**Containers and CI.** Optimized creation depends on the destination volume,
not the OS name: APFS on macOS, Btrfs or reflink-enabled XFS on Linux,
caller-visible OverlayFS mounts, or ReFS on Windows. In containers, mount a
supported volume for the worktree destinations and run
`riftri doctor --destination` in the job before creating views. On ordinary
Linux shells without mount capability, a narrow root-owned helper can be
installed once with `sudo riftri overlayfs install-helper`; unsupported
volumes stop before mutation rather than silently copying.

## Machine-readable contract

Successful lifecycle commands accept `--json` and print exactly one versioned
report on stdout — `doctor`, `backends`, `status`, `repair`, `gc`, and every
`worktree` subcommand, including `worktree list`:

```console
$ riftri worktree add ../task-1 -b agent/task-1 --json
$ riftri worktree list --json
$ riftri status --json
```

Reports carry `schema_version`, display paths beside `*_native_hex` fields
with the exact native encoding named in `native_path_encoding`, and raw Git
ref bytes beside lossy display strings, so non-UTF-8 paths and refs remain
representable.
The `doctor` report includes these path pairs for the destination, Git command,
repository root, common Git directory, and storage probe paths.
The `backends` report includes these path pairs for the requested destination
and, per probed capability, the volume's requested and probe paths.
On Unix, `native_path_encoding` is `unix-bytes-hex`. On Windows, it is
`windows-utf16le-hex`.

Failures become machine-readable with the global `--json-errors` flag: one
JSON receipt on stderr with a stable `code`, a `category` distinguishing
policy refusals from operational failures, the durable `phase` reached, a
`cleanup` disposition, and recovery guidance including the exact
`nextCommand`. A harness should treat a non-zero exit with a
`"category": "policy"` receipt as a configuration to report, not retry.

Receipts also carry the context the failing invocation used: `repository` and
`stateDirectory` display strings, their `repositoryNativeHex` and
`stateDirectoryNativeHex` twins, and `nativePathEncoding`. `nextCommand`
targets that same context rather than the caller's working directory, so
running it inspects or repairs the state that actually failed.

`nextCommand` is a **shell string**, with every path quoted for the platform
shell: run it through a shell rather than splitting it on whitespace. It is
`null` when no command applies and also when a path cannot be written as a
shell argument — a non-Unicode path, or one containing control characters.
Riftri never emits a lossy or unquoted command, because one would silently
address a different directory. Automation that needs the exact path should
read the `*NativeHex` fields, which are faithful in every case.

An explicitly passed `--state-dir` that does not exist is refused with
`"code": "invalid-request"` and exit code 3, rather than scanned as empty: a
directory Riftri never found cannot support an all-clear. A repository that
has simply never created Riftri state still reports an all-clear and exits 0.

`--json-errors` also suppresses the human-readable lifecycle progress lines
that `worktree add`, `repair`, and `gc` otherwise print on stderr, so stderr
stays reserved for that single receipt. Harnesses that parse stdout with
`--json` but leave stderr for logs can keep progress enabled — `--json`
output is never interleaved with it — or pass `--no-progress` explicitly;
the [CLI reference](cli.md#progress-reporting) documents the line format.

`RIFTRI_BYPASS=1` routes one intercepted Git command directly to real Git when
an agent intentionally needs an unmanaged worktree form Riftri refuses.

## Running many agents in parallel

Views of the same commit share one immutable base per volume, so the second
and later worktrees are cheap:

```console
$ for task in auth billing search; do
>   riftri worktree add "../app-$task" -b "agent/$task" main --json
> done
$ riftri worktree list --json
$ riftri status --json
```

Creation of the same base is coordinated with per-base locks, so concurrent
adds from independent runner processes are safe; one process materializes the
base and the others reuse it after integrity verification.

When a task finishes:

```console
$ riftri worktree remove ../app-auth          # refuses dirty views
$ riftri worktree remove --force ../app-auth  # snapshot-guarded discard
$ riftri gc --json                            # plan zero-reference cleanup
$ riftri gc --apply --json
```

Clean removal never deletes uncommitted work. Forced removal records an exact
content snapshot first and stops if the view changes after that intent. `gc`
without `--apply` deletes nothing.

After a crashed or killed runner:

```console
$ riftri repair --json
```

Repair resumes or rolls back interrupted journaled operations conservatively,
skips operations still owned by live processes, and exits non-zero if anything
needs manual attention. It is safe to run repeatedly and on a schedule.

Long-lived views that accumulated private storage from since-reverted edits
can be reset onto a fresh base without losing their Git identity:

```console
$ riftri worktree compact ../app-auth
```

## Safety boundaries

- Riftri's copy-on-write isolation is not a security sandbox. Untrusted agent
  processes still need a container, VM, or OS sandbox; see
  [SECURITY.md](../SECURITY.md).
- Unsupported checkout configurations (custom filters, sparse checkout,
  submodules, non-canonical Git LFS setups) fail closed with an explanation
  instead of degrading to a full copy.
- Riftri never edits shell profiles, and interception is off everywhere by
  default: it requires both an explicitly activated process or shell and
  per-repository consent.
