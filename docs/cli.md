# CLI reference

Every Riftri command in one place. Each entry shows the accepted arguments
and flags; the platform guides linked from the [documentation
index](README.md) explain when to use them. Output shown here reflects
`riftri --help` and can be regenerated at any time with
`riftri <command> --help`.

Two conventions apply everywhere:

- **`--json-errors`** is accepted by every command. On failure, it emits one
  machine-readable JSON receipt on stderr instead of human-readable text. The
  receipt contract is documented in the
  [agent integration guide](agent-integration.md).
- **`--json`** is accepted by the explicit commands that report or change state
  (`doctor`, `backends`, `status`, `repair`, `gc`, and all `worktree`
  subcommands). It emits stable machine-readable JSON on success.

The interactive-only `setup` command rejects `--json-errors` with a single JSON
receipt before prompting or making changes; use the explicit commands for
automation. It has no `--json` or `--yes` mode.

A third convention, [progress reporting](#progress-reporting), applies to the
long-running lifecycle commands and is suppressed with the global
`--no-progress` flag.

Repository-selecting commands consistently accept `--repository <PATH>`,
defaulting to the current directory (`.`). `enable`, `disable`, `doctor`,
`status`, `repair`, `gc`, and `shell status` also keep their older positional
repository argument. Use one form, not both; conflicting selectors are a usage
error, never a silent override. For example, `riftri status --repository ../app
--json` and `riftri status ../app --json` inspect the same repository.
`backends` takes a filesystem destination rather than a repository, and `exec`
uses `--worktree` to select the child's working directory; neither accepts
`--repository`.
Commands that touch Riftri state accept `--state-dir <PATH>` to override the
default state directory at `<common-git-dir>/riftri`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Operational failure — Git, storage, journal, or filesystem I/O |
| `2` | Usage error — unknown flag or missing argument, reported by the parser |
| `3` | Policy refusal — Riftri declined a request it will not optimize |
| `130` | Interactive question interrupted with Ctrl-C (already-created worktrees are retained) |

Codes `1` and `3` mirror the `category` field (`operational` / `policy`) of
the `--json-errors` receipt, so a wrapper can branch on the exit status
without parsing JSON. A policy refusal means nothing was changed; consult
[decisions.md](decisions.md) for the forms Riftri refuses and `RIFTRI_BYPASS=1`
to run one such command through ordinary Git instead.

## Terminal interface

Available starting with v0.3.2. Riftri uses Ratatui for guided setup and
interactive confirmations, with a restrained orange accent across
human-readable reports and help. Reports remain in normal terminal scrollback;
full-screen views exist only while a question is open. This is presentation
around the same core operations, not a new worktree implementation.

- Use arrow keys to select, Enter to confirm, and Esc to cancel. Confirmations
  default to **No** and agent selection defaults to **Not now**. Selecting Yes
  with `y` still requires Enter. PgUp/PgDn review long plans and paths.
- **`--plain`** restores line-oriented prompts and undecorated reports. Use it
  for screen readers, terminal recordings, or a minimal interface.
- **`--no-animation`** (or nonempty `RIFTRI_NO_ANIMATION`) keeps the interface
  but replaces the spinner with bounded phase lines. `--no-progress` suppresses
  progress entirely.
- A nonempty **`NO_COLOR`** disables colors without removing keyboard navigation.
- Redirected stdout or stderr, `CI`, and `TERM=dumb` automatically use plain
  output. On Unix, an unset/empty `TERM` does too. `--json`, `--json-errors`,
  shell hook/deactivation code, completions, the Git shim, and child process
  streams are never decorated.

Questions require at least a 30-column × 16-row terminal; smaller windows ask
you to resize or cancel and do not accept an invisible confirmation. Terminal
mode and the cursor are restored before any storage mutation or agent launch,
on errors, and on Ctrl-C. Unix termination signals received during a question
also restore the terminal and return `128 + signal`. No UI can recover terminal
state after an uncatchable kill or a terminal disappearing.
The inherited signal dispositions are restored when each question closes, so
termination during a later transaction still follows the existing recovery
contract and agent launch keeps its normal signal forwarding.

## Progress reporting

In pipes, plain mode, or reduced-motion mode, long-running lifecycle commands
print one plain line on stderr each time the operation durably reaches a journal
phase, starts waiting on a coordination lock another process holds, or
resumes after acquiring it:

```text
riftri: worktree-add: intent-recorded
riftri: worktree-add: materializing new immutable base
riftri: worktree-add: base-ready
riftri: waiting: read-lock immutable base (held by another process)
riftri: resumed: read-lock immutable base
riftri: worktree-add: active
```

Every phase reflects a state the operation genuinely reached. In a supported
terminal, one indeterminate spinner shows the latest real phase, including lock
waits; it does not invent percentages or delay completion. It clears before the
result, an error, or another question. Keyboard actions themselves do not animate.
Redirected logs contain no terminal control sequences and remain bounded by real
transitions. Phase names match those in journals and `--json-errors` receipts.

Two suppression rules keep machine-readable streams intact:

- The global `--no-progress` flag disables progress lines for quiet scripted
  use.
- `--json-errors` implies `--no-progress`, because a caller expecting one
  JSON failure receipt on stderr must never receive interleaved progress
  text. There is no separate structured progress stream; parse the receipt's
  `phase` field instead.

`--json` is unaffected: its single report goes to stdout while progress uses
stderr, so `riftri worktree add … --json 2>log` still yields exactly one
valid JSON document on stdout.

## Destructive commands

`riftri gc --apply` and `riftri worktree remove --force` ask for confirmation
when stdin and stderr are both terminals. Pass `--yes` to skip the prompt.
Non-interactive callers — agents, scripts, CI — are never prompted, so
existing automation keeps working unchanged.

## Enablement and activation

### `riftri setup [OPTIONS]`

Interactive first-worktree onboarding, followed by an optional coding-agent
launch. Available starting with v0.3.1.

```sh
riftri setup
riftri setup --repository ../app --destination ../app-auth --branch feature/auth
```

Standard input, output, and error must all be terminals. `--repository <PATH>`
defaults to the caller's current directory. The repository must already exist;
setup does not clone repositories or fetch revisions.

Setup asks for a destination and a new branch, defaulting to a sibling directory
named `<repository>.task` and `task/first`. `--destination <PATH>` and
`-b, --branch <BRANCH>` skip their respective questions, not confirmation.
Relative paths resolve from the caller's working directory, including when
`--repository` selects a different checkout. Native paths supplied as arguments
are preserved. The start point is always `HEAD`: uncommitted source changes are
not copied. Use `worktree add` directly for an existing branch, another revision,
detached HEAD, sparse checkout, or custom state placement.

Doctor checks the actual destination's storage and checkout compatibility.
Explicit creation does not need repository enablement. Other blockers stop the
flow with remedies before creation; there is no full-copy fallback. A printed
plan and `Create this worktree? [y/N]` confirmation precede the normal journaled
add transaction. Existing destinations or branches are never overwritten. A
decline or end-of-input before creation leaves no new worktree or enablement.

After creation, setup asks which coding agent to open:

- **Not now** (the default): keep the worktree without launching anything or
  changing repository enablement.
- **Claude Code** (`claude`) or **Codex** (`codex`): resolve the installed CLI
  through PATH. Setup neither installs nor signs in to an agent.
- **Another executable**: accept one executable name or path, without arguments
  or shell evaluation. Missing or non-executable choices return to the menu.
  For custom arguments, finish setup and use `riftri exec` yourself.

The separate `Enable this repository and launch the agent now? [y/N]` prompt
explains that enablement is repository-local and shared by its linked worktrees.
Only explicit approval sets `riftri.enabled=true` and launches through the
existing `exec --worktree` machinery. Paths are resolved before changing the
child's directory, arguments are not interpreted by a shell, and no agent
permission settings or shell profiles are changed. Existing bypass settings are
preserved. Agent exit status and signal handling follow `riftri exec`.

Declining launch, EOF, a launch failure, or an agent exit does not delete the
created worktree. Once approved, repository enablement remains in effect even
if launch fails; use `riftri disable` to reverse it. The agent must still decide
to create future worktrees and call `git` through the inherited PATH. Absolute
Git paths and embedded Git libraries are outside interception.

### `riftri enable [PATH]`

Enable optimized worktree creation for one repository. `PATH` defaults to `.`.

### `riftri disable [PATH]`

Disable optimized worktree creation for one repository. `PATH` defaults to
`.`.

### `riftri exec [OPTIONS] <COMMAND>...`

Run a command with process-scoped Git worktree interception — `git worktree
add` calls made by `COMMAND` (for example, an agent harness) are optimized
without any shell-level activation.

| Flag | Effect |
| --- | --- |
| `--worktree <PATH>` | Start the command from this exact, registered Git worktree root |

Termination follows the platform's conventions. On Unix, without a foreground
controlling terminal (a supervisor or script), the command runs in its own
process group, and SIGTERM, SIGINT, or SIGHUP delivered to `riftri exec` is
forwarded to that whole group, stopping the command's descendants without
touching unrelated processes. With a foreground controlling terminal, the
command stays in `riftri exec`'s process group so terminal job control is
unchanged: the terminal keeps delivering Ctrl-C (SIGINT) and Ctrl-\ (SIGQUIT)
to the whole foreground process group, and `riftri exec` ignores both while it
waits — like a shell waiting on a foreground job, the command alone decides
whether the interrupt is fatal, so a command that catches Ctrl-C (a REPL, an
agent session) keeps running under an intact wrapper. SIGTERM and SIGHUP
delivered to interactive `riftri exec` are still forwarded to the command
itself. In both modes `riftri exec` waits for the command, restores its prior
signal dispositions, removes its temporary Git shim, and exits with the
command's status (`128 + signal` when the command dies from a signal — for
example 130 after a fatal SIGINT). SIGKILL cannot be intercepted and still
orphans the command. On Windows, the console already delivers Ctrl-C and
Ctrl-Break events to the command, and a hard `TerminateProcess` cannot be
intercepted, so no forwarding layer exists.

The forwarding and ignoring above describe what `riftri exec` does *while it
waits*; they never change what the command itself starts with. On Unix, every
signal whose disposition `riftri exec` replaces — SIGTERM, SIGHUP, and SIGINT,
plus SIGQUIT in interactive mode — is reset in the command before it execs to
the disposition `riftri exec` itself inherited: `SIG_IGN` stays `SIG_IGN`, and
anything else becomes `SIG_DFL`, because a caught handler cannot survive an
exec while an ignore can. So `nohup riftri exec -- <command>` leaves the
command as immune to a hangup as bare `nohup <command>` would, and
`riftri exec` started asynchronously by a shell without job control passes on
the ignored SIGINT and SIGQUIT that POSIX requires for a background job. A
command launched from an ordinary foreground shell is unaffected: nothing was
ignored there, so it starts at `SIG_DFL` and Ctrl-C reaches it normally.

Forwarding also survives the Git shim. When the scoped command runs `git`, the
`git` it resolves is Riftri's own shim, which delegates to the real Git; the
shim applies this same termination contract to that delegation. A SIGTERM or
SIGHUP forwarded to the shim is therefore forwarded on to the real Git process
instead of killing the shim and leaving a `clone` or `fetch` running, orphaned
and still writing. The shim waits for the real Git, restores its own signal
dispositions, and exits with Git's status under the same `128 + signal` rule,
so a terminated `riftri exec -- git …` still reports 143 for SIGTERM and 130
for a fatal SIGINT.

### `riftri shell <SUBCOMMAND>`

Configure shell-scoped interception for normal Git commands.

- **`riftri shell hook <SHELL>`** — print initialization code to evaluate in a
  shell. Shells: `sh`, `bash`, `zsh`, `powershell`.
- **`riftri shell deactivate <SHELL>`** — print code to evaluate to deactivate
  Riftri in the current shell. Same shell choices. The code removes both the
  durable shell-hook shim and any process-scoped `riftri exec` shim entries
  from `PATH`; evaluated inside a `riftri exec` session it ends Git
  interception for the rest of that session.
- **`riftri shell status [REPOSITORY]`** — show shell interception and
  repository opt-in status. `REPOSITORY` defaults to `.`.

See [global-activation.md](global-activation.md) for the shell compatibility
matrix and safe activation patterns.

### `riftri overlayfs install-helper [--replace]`

Install the root-owned mount helper that lets unprivileged shells use the
Linux OverlayFS backend. `--replace` atomically replaces an existing safe
helper during an upgrade. Requirements and recovery are covered in
[linux-overlayfs.md](linux-overlayfs.md).

### `riftri completions <SHELL>`

Print a shell completion script for riftri commands on stdout. Shells:
`bash`, `elvish`, `fish`, `powershell`, `zsh`.

### `riftri man <DIRECTORY>`

Write one troff man page per riftri command (`riftri.1`,
`riftri-worktree-add.1`, …) into `DIRECTORY`, creating it if needed.

## Inspection

### `riftri doctor [OPTIONS] [PATH]`

Inspect Git and show the planned storage path without changing anything.
`PATH` defaults to `.`.

| Flag | Effect |
| --- | --- |
| `--destination <DESTINATION>` | Proposed worktree destination whose volume should be probed |
| `--json` | Emit machine-readable JSON |

The suggested command uses POSIX shell quoting on every platform, so it runs as
shown in `sh`, `bash`, and `zsh` — including the Git Bash, WSL, and MSYS
environments common on Windows. If the destination is not valid Unicode or
holds control characters, doctor omits the suggested command rather than
substitute characters in the path; the exact path stays in the machine-readable
native-path fields.

With `--json`, `destination_readiness.backend` and
`destination_readiness.next_command` are always present, explicitly `null`
when no backend is selected or no command applies: a blocked destination
keeps every key a ready one has.

### `riftri backends [OPTIONS] [PATH]`

Probe storage backends for a concrete destination volume. `PATH` is an
existing path or proposed destination, defaulting to `.`. Supports `--json`.

### `riftri status [OPTIONS] [REPOSITORY]`

Report retained bases, active views, reference counts, and disk use for one
repository's Riftri state. Supports `--state-dir` and `--json`.

## Maintenance

### `riftri repair [OPTIONS] [REPOSITORY]`

Safely resume or roll back interrupted journaled operations. Running it is
always safe: complete journals are resumed, incomplete ones are rolled back,
and a healthy state directory is left unchanged. Supports `--state-dir` and
`--json`.

Repair also reconciles active journals against Git's own worktree registry:

- A journal whose worktree Git no longer registers and whose directory is gone
  — for example after `rm -rf` plus `git worktree prune` — is retired by a
  journaled completion, releasing its immutable base. Reported as
  `Retired add operations` (`retired_adds`).
- A journal whose worktree Git registers under a *different* path is reported,
  not adopted, under `Relocated worktrees Riftri no longer tracks`
  (`relocated_worktrees`). The live worktree and its contents are untouched.
- Temporary files left by an interrupted journal write are removed, reported as
  `Reaped interrupted journal writes` (`reaped_artifacts`).
- On Linux, an OverlayFS probe directory abandoned next to a worktree after a
  failed probe unmount (named `.riftri-overlay-probe-*`, also flagged by
  `riftri status`) is removed only when the kernel's mount inventory proves
  nothing is mounted at or below it, reported as `Reaped abandoned OverlayFS
  probes` (`reaped_probe_roots`). A probe root any mount still covers is
  preserved and reported (`preserved_probe_mounts`) instead of being touched.

Nothing is retired while its directory still exists or still has content.

### `riftri gc [OPTIONS] [REPOSITORY]`

Plan or apply collection of immutable bases with no journaled references.

A base that an unfinished journal still claims is never collected, and is now
reported rather than dropped silently: `Skipped because a journaled operation
still claims them` (`skipped_protected`) names each base, the operation that
claims it, and why. `riftri repair` retires those operations when it safely
can, after which the base becomes collectible.

| Flag | Effect |
| --- | --- |
| `--apply` | Apply the collection plan. Without this flag, nothing is deleted |
| `--yes` | Skip the interactive confirmation. Requires `--apply` |
| `--state-dir <STATE_DIR>` | Explicit Riftri state directory |
| `--json` | Emit stable machine-readable JSON |

### `riftri state unregister [OPTIONS] <PATH>`

Remove a registration for a missing state directory. No files are deleted.
`PATH` is the missing state directory; `--repository` names the repository
containing the local registration (default `.`).

Relative paths such as `../old-state` are resolved from the current directory
and matched to the registered location. Existing paths, including dangling
symlinks, cannot be unregistered. The former name `forget-missing` remains
accepted as a hidden compatibility alias.

## Worktrees

All `riftri worktree` subcommands accept `--repository <REPOSITORY>`
(default `.`), `--state-dir <STATE_DIR>`, and `--json`.

### `riftri worktree list`

List active Riftri-managed worktrees and their storage use.

By default the inventory reads one state directory: the explicit `--state-dir`,
or `<common-git-dir>/riftri` when none is given. That default scope is
unchanged and its JSON keeps `schema_version` 1.

| Flag | Effect |
| --- | --- |
| `--all-states` | Inspect every state directory the repository registers, including the default location. Conflicts with `--state-dir` |

With `--all-states`, discovery reads the default location plus every
`riftri.stateDirectory` registration recorded by `riftri worktree add
--state-dir`. Equivalent registrations are deduplicated, and worktrees owned by
other repositories that share a state directory are filtered out. Missing,
non-absolute, or symlinked registrations are reported as diagnostic entries
instead of being traversed or silently dropped; `riftri state unregister`
removes a stale missing registration. Discovery is strictly read-only.

The `--all-states --json` report uses `schema_version` 2 with
`"scope": "all-registered-states"`: it adds a `state_directories` array
(each entry's `source` is `default` or `registered`), each worktree carries its
owning `state_directory`, and each diagnostic entry carries the
`state_directory` it was found in beside its `state_directory_native_hex`
twin (both explicitly `null` for registration-level issues that no state
directory owns). The hex twin and the explicit nulls were added later without
a `schema_version` bump: additive, null-consistent keys do not change the
version.

### `riftri worktree add [OPTIONS] <PATH> [REVISION]`

Create a real linked worktree at `PATH` using the platform's native
copy-on-write backend. `REVISION` is the commit-ish to use for the new
worktree.

| Flag | Effect |
| --- | --- |
| `-b, --branch <BRANCH>` | Create and check out a new branch |
| `--detach` | Create a detached worktree instead of a branch |
| `--sparse-dir <DIR>` | Materialize only this directory (plus repository-root files) with Git cone-mode sparse checkout; repeatable, repository-relative with `/` separators. See [sparse-checkout.md](sparse-checkout.md) for the supported subset and refusals |

### `riftri worktree remove [OPTIONS] <PATH>`

Safely remove a Riftri-managed linked worktree. Removal is fail-closed:
worktrees with local changes are refused unless `-f, --force` is given, which
discards current changes only after recording an exact recovery snapshot. A
forced removal asks for confirmation on a terminal; `--yes` skips that prompt
and requires `--force`.

### `riftri worktree move [OPTIONS] <SOURCE> <DESTINATION>`

Move a Riftri-managed linked worktree with recoverable metadata updates.
`DESTINATION` must be on the same filesystem volume.

### `riftri worktree compact [OPTIONS] <PATH>`

Replace a pristine managed worktree with a fresh native COW view, returning
its storage cost to that of a new view. The worktree must be clean.

### `riftri worktree prune`

Prune stale unmanaged Git metadata without risking managed worktrees.
