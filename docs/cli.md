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
- **`--json`** is accepted by every command that reports or changes state
  (`doctor`, `backends`, `status`, `repair`, `gc`, and all `worktree`
  subcommands). It emits stable machine-readable JSON on success.

Commands that operate on a repository accept it as an optional positional
argument or a `--repository` flag, defaulting to the current directory (`.`).
Commands that touch Riftri state accept `--state-dir <PATH>` to override the
default state directory at `<common-git-dir>/riftri`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Operational failure — Git, storage, journal, or filesystem I/O |
| `2` | Usage error — unknown flag or missing argument, reported by the parser |
| `3` | Policy refusal — Riftri declined a request it will not optimize |

Codes `1` and `3` mirror the `category` field (`operational` / `policy`) of
the `--json-errors` receipt, so a wrapper can branch on the exit status
without parsing JSON. A policy refusal means nothing was changed; consult
[decisions.md](decisions.md) for the forms Riftri refuses and `RIFTRI_BYPASS=1`
to run one such command through ordinary Git instead.

## Destructive commands

`riftri gc --apply` and `riftri worktree remove --force` ask for confirmation
when stdin and stderr are both terminals. Pass `--yes` to skip the prompt.
Non-interactive callers — agents, scripts, CI — are never prompted, so
existing automation keeps working unchanged.

## Enablement and activation

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

### `riftri shell <SUBCOMMAND>`

Configure shell-scoped interception for normal Git commands.

- **`riftri shell hook <SHELL>`** — print initialization code to evaluate in a
  shell. Shells: `sh`, `bash`, `zsh`, `powershell`.
- **`riftri shell deactivate <SHELL>`** — print code to evaluate to deactivate
  Riftri in the current shell. Same shell choices.
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

### `riftri gc [OPTIONS] [REPOSITORY]`

Plan or apply collection of immutable bases with no journaled references.

| Flag | Effect |
| --- | --- |
| `--apply` | Apply the collection plan. Without this flag, nothing is deleted |
| `--yes` | Skip the interactive confirmation. Requires `--apply` |
| `--state-dir <STATE_DIR>` | Explicit Riftri state directory |
| `--json` | Emit stable machine-readable JSON |

### `riftri state forget-missing [OPTIONS] <PATH>`

Forget an explicitly selected registration whose directory is missing.
`PATH` is the missing state directory; `--repository` names the repository
containing the local registration (default `.`).

## Worktrees

All `riftri worktree` subcommands accept `--repository <REPOSITORY>`
(default `.`), `--state-dir <STATE_DIR>`, and `--json`.

### `riftri worktree list`

List active Riftri-managed worktrees and their storage use.

### `riftri worktree add [OPTIONS] <PATH> [REVISION]`

Create a real linked worktree at `PATH` using the platform's native
copy-on-write backend. `REVISION` is the commit-ish to use for the new
worktree.

| Flag | Effect |
| --- | --- |
| `-b, --branch <BRANCH>` | Create and check out a new branch |
| `--detach` | Create a detached worktree instead of a branch |

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
