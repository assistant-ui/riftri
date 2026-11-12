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

For large worktrees checked frequently, Git's own [optional FSMonitor](fsmonitor.md)
can reduce repeated status cost. This is a separate, explicit Git setting, not
part of `riftri enable` or process-scoped interception. It can be slower on
smaller trees and does not eliminate initial creation checks. The guide covers
repository-versus-worktree scope and stopping Git-owned watchers.

**Claude Code.** Claude Code creates task worktrees with plain `git worktree`
commands, so either launch it as `riftri exec -- claude` in an enabled
repository, or add the shell hook to your profile and run `riftri enable` once
per repository. Child processes inherit interception; no Riftri-specific
prompt or skill is required. The activation test matrix explicitly covers
`claude`-named child harnesses.

**Claude Code worktree hooks.** When Claude Code creates a worktree itself
(`claude --worktree`, or a subagent with `isolation: "worktree"`), a
`WorktreeCreate` hook replaces its `git worktree add`, and a `WorktreeRemove`
hook replaces the removal of a worktree that hook created. Pointing both at
Riftri gives optimized worktrees without `riftri enable` or a shim, because the
hooks call the explicit interface. In `.claude/settings.json`:

```json
{
  "hooks": {
    "WorktreeCreate": [{ "hooks": [{ "type": "command",
      "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/riftri-worktree-create.sh" }] }],
    "WorktreeRemove": [{ "hooks": [{ "type": "command",
      "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/riftri-worktree-remove.sh" }] }]
  }
}
```

`.claude/hooks/riftri-worktree-create.sh` reads the requested name from the
hook's JSON input (with `jq`) and prints only the new worktree's path:

```sh
#!/bin/sh
set -eu
name=$(jq -r .name)
destination="$CLAUDE_PROJECT_DIR/.claude/worktrees/$name"
# Claude Code reads the worktree path from stdout, so the report goes to stderr.
riftri worktree add "$destination" -b "worktree-$name" HEAD \
  --repository "$CLAUDE_PROJECT_DIR" --json-errors >&2
printf '%s\n' "$destination"
```

`.claude/hooks/riftri-worktree-remove.sh` removes it through Riftri's journal:

```sh
#!/bin/sh
set -eu
riftri worktree remove "$(jq -r .worktree_path)" \
  --repository "$CLAUDE_PROJECT_DIR" --json-errors >&2
```

- Stdout carries the path and nothing else. `riftri worktree add` prints its
  report on stdout, so the hook sends it to stderr; extra stdout breaks the
  path Claude Code reads.
- A refused or failed add exits non-zero, which fails the worktree creation,
  and the `--json-errors` receipt on stderr says why. Nothing is left to clean
  up after a policy refusal.
- The remove hook keeps uncommitted work. `riftri worktree remove` refuses a
  dirty worktree, so the directory stays where it is. Don't add `--force`.
- Change `HEAD` and the branch name to match how your project starts task
  branches.
- Claude Code doesn't call `WorktreeRemove` on every path, and some versions
  remove a hook-created worktree with plain `git worktree remove`. A Riftri
  worktree removed by plain Git leaves its add journal behind and keeps its
  immutable base referenced: `riftri status` reports it and `riftri repair`
  retires it. Launching Claude Code through `riftri exec` in an enabled
  repository sends those Git calls through Riftri as well.

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

## Apps that launch agents

An app that starts agents for the user (a terminal, an editor, an agent
manager) often ships its own Riftri binary and shows the user's worktrees.
These rules keep it in step with the CLI and with any Riftri the user installed
separately:

- **Launch each agent through `riftri exec`.** Run
  `<path-to-riftri> exec [--worktree <root>] -- <agent> <args>` with the
  absolute path of the binary you ship. Only that process tree gets the Git
  shim. `riftri exec` forwards termination signals and exits with the agent's
  own status, so your process supervision works unchanged; see
  [`riftri exec`](cli.md#riftri-exec-options-command) for the details.
- **Ask before enabling a repository.** The shim optimizes `git worktree add`
  only in repositories that ran `riftri enable`, which is the user's consent
  for that one repository. Ask before running it; `riftri disable` reverses
  it. Without it, every Git command goes to the real Git, and an explicit
  `riftri worktree add` still works.
- **Put Riftri on the agent's `PATH` if the agent calls it.** `riftri exec`
  adds only a `git` shim. An agent that runs `riftri worktree list`, or follows
  the Riftri skill, needs the bundled binary's directory on its `PATH` too.
- **Keep the bundled release current.** Your copy and one the user installed
  with Homebrew or npm work on the same state directory. Journals carry a
  format version that an older binary refuses rather than misreads, but a
  recovery or collection fix only helps the binary that has it.
- **Read state through the JSON reports, not the state directory.**
  `riftri status --json` and `riftri worktree list --all-states --json` are
  versioned. The files under `<common-git-dir>/riftri` are recovery journals:
  they include rolled-back and pending operations, carry no timestamps, change
  between releases, and `riftri gc --apply` deletes the finished ones
  ([D043](decisions.md#d043-gc-retires-finished-journal-history)). Totals
  counted from them are wrong and reset after every collection. Worktrees
  made with `--state-dir` live in another directory altogether, which
  `--all-states` finds.
- **Refresh cheaply.** `status` and `worktree list` read every file of every
  worktree to measure disk use. For a view that refreshes often, use
  `riftri worktree inspect --json -- <path>...`, which reports ownership and
  backend without reading user files, and measure disk use less often or on
  request.
- **Run actions on the machine-readable path.** Pass `--json --json-errors`
  to `worktree remove`, `gc --apply`, and `repair`, show the receipt's
  message, and offer its `nextCommand`. A `"category": "policy"` refusal
  changed nothing. Before offering `riftri gc --apply`, run `riftri gc --json`
  to show what it would delete. Don't offer collection while `status --json`
  reports `counts_complete: false` or a pending operation; offer
  `riftri repair --json` instead.

## Machine-readable contract

Successful lifecycle commands accept `--json` and print exactly one versioned
report on stdout — `doctor`, `backends`, `status`, `repair`, `gc`, and every
`worktree` subcommand, including `worktree list`:

```console
$ riftri worktree add ../task-1 -b agent/task-1 --json
$ riftri worktree list --json
$ riftri status --json
```

For startup readiness without disk accounting, use
`riftri worktree inspect --json -- <path>...`. It reports ownership and backend
for each requested path, plus the actual OverlayFS mount state. An absent or
not-yet-adopted mount reports `recovery-required`; repair its exact owning state
directory and inspect again before launching a process in the worktree. A busy
operation can be skipped by repair, so repair success alone is insufficient.
Foreign mounts, other live namespaces, invalid registrations, and incomplete
operations must never be treated as unmanaged or ready. The query is read-only
and includes exact native path encodings; it does not scan user files.

Reports carry `schema_version`, display paths beside `*_native_hex` fields
with the exact native encoding named in `native_path_encoding`, and raw Git
ref bytes beside lossy display strings, so non-UTF-8 paths and refs remain
representable.
The `doctor` report includes these path pairs for the destination, Git command,
repository root, common Git directory, and storage probe paths. Its
`destination_readiness.backend` and `destination_readiness.next_command` are
always present, explicitly `null` when no backend is selected or no command
applies, so a blocked destination keeps every key a ready one has.
The `backends` report includes these path pairs for the requested destination
and, per probed capability, the volume's requested and probe paths.
The `worktree list --all-states` report (`schema_version` 2) applies the same
pairing to its `diagnostic_issues`: each entry's `state_directory` display
string has a `state_directory_native_hex` sibling, and a registration-level
issue that no state directory owns carries both keys as explicit `null`.
The hex sibling and the explicit nulls were added after `schema_version` 2
first shipped, without a version bump: additive, null-consistent keys do not
change the schema version.
The `status` report counts only journals it could parse, so it carries a
top-level `counts_complete` and a per-base `reference_count_complete`. Both are
`false` when any journal is unreadable, which means every count in that
document is a lower bound rather than the truth. A base whose count is
incomplete reports `in_use: true`, because an unreadable journal may still
claim it: never treat a base as unreferenced unless `reference_count_complete`
is `true`. These keys are additive and do not change the schema version.
Each base also carries an additive `damaged` flag: `true` once an add found
that base failing its integrity check. A damaged base is never reused, so adds
of its tree are refused until the worktrees using it are removed and
`riftri gc --apply` deletes it.
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

`"code": "storage-full"` is the exception that requires operator action
before `nextCommand`: free space on the affected volume first, then follow the
receipt's unchanged lifecycle recovery guidance. The code is derived from the
typed operating-system error rather than matching human-readable text.

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

The Ratatui interface is human-facing only. JSON, shell code, completions,
Git-shim calls, and `riftri exec` child streams are never decorated, even under
a pseudo-terminal. Redirected output automatically stays plain. A harness that
uses human-readable terminal output can explicitly pass `--plain`; interactive
setup still requires a person and explicit confirmation.

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
