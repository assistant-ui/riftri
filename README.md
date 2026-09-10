# Riftri

**Lightweight Git workspaces for parallel development.**

Give every developer or coding agent an isolated workspace without storing
another full copy of every unchanged project file for every task.

[![CI](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml/badge.svg)](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml)

> [!WARNING]
> Riftri is experimental, pre-release software. Keep important changes committed
> or backed up before using its worktree mutation commands. Optimized worktree
> creation and lifecycle mutation currently require macOS on a writable APFS
> volume. Linux and Windows builds provide diagnostics and explicit
> unsupported-backend errors while their native mutation backends remain on the
> roadmap.

Every Riftri workspace is a real Git linked worktree. Unchanged file data is
shared efficiently, while every workspace keeps its own private changes. Git
continues to own cloning, branches, commits, merges, and remotes.

## Why Riftri

One-worktree-per-task is a natural way to run coding agents in parallel. Git
shares repository objects between linked worktrees, but it normally checks out
another fully materialized working directory for each one. On large
repositories, those copies consume disk space and make disposable agent
workspaces slower to create and clean up.

Riftri keeps the workflow developers and agents already understand:

- **Real Git worktrees.** Ordinary paths, files, branches, and Git commands
  continue to work.
- **Shared unchanged data.** Riftri avoids eagerly duplicating the physical
  contents of the whole checkout.
- **Private edits.** A write in one worktree cannot modify another worktree or
  the immutable shared base.
- **No filesystem middleman.** Editors, compilers, and agents use the native
  filesystem directly; Riftri is not a daemon or a file-access proxy.
- **Explicit and recoverable.** Optimization is opt-in per repository, and
  lifecycle mutations use durable journals instead of silent copy fallback.

## Quick start on macOS

Activate Riftri's Git shim in the current shell once:

```console
$ eval "$(riftri shell hook zsh)"
```

You may add that line to your shell profile yourself if you want the Git shim
enabled globally for your user account in every new shell. That global shell
activation does **not** optimize every repository: `riftri enable` remains the
repository-local consent switch, and all other repositories delegate directly
to the real Git executable. Riftri never edits shell startup files
automatically. After shell activation, repository enablement is the
interception switch:

```console
$ git clone git@github.com:acme/app.git
$ cd app
$ riftri enable
$ git worktree add -b feature/auth ../app-auth main
$ git worktree move ../app-auth ../app-auth-renamed
$ git worktree prune
$ git worktree remove ../app-auth-renamed
$ riftri status
```

`riftri enable` records repository-local consent in `riftri.enabled`. With the
shell hook active, supported `git worktree add` commands in that repository are
routed through Riftri. Other Git commands and worktree adds in repositories that
have not been enabled execute the real Git binary directly.

For one agent or command tree without shell setup, process-scoped activation
remains available:

```console
$ riftri enable
$ riftri exec -- claude
```

To start any command in an existing worktree without first changing directory,
bind the process explicitly:

```console
$ riftri exec --worktree ../app-auth -- codex
```

The binding must name the exact root of a live worktree reported by Git. It only
selects the child process's working directory; repository-local enablement still
controls whether that process's supported Git operations are optimized.

The resulting path is a real Git linked worktree. Unchanged data is shared with
an immutable base while writes remain private to that worktree. Riftri does not
modify the parent shell, shell startup files, or system Git. To stop opting in:

```console
$ riftri disable
```

Inspect both activation layers at any time:

```console
$ riftri shell status
```

To remove the shim from the current shell and restore its prior Git resolution:

```console
$ eval "$(riftri shell deactivate zsh)"
```

Deactivation cannot edit the parent shell unless its output is evaluated. It
also cannot remove a hook line that you chose to put in a shell profile; remove
that line yourself to keep Riftri inactive in future shells. Disabling one
repository does not deactivate the shell shim, and deactivating the shim does
not erase any repository's local consent setting.

See [Global shell activation](docs/global-activation.md) for the compatibility
matrix, performance probe, and known boundaries involving aliases, wrappers,
IDEs, containers, and tools that do not spawn Git through `PATH`.

The explicit interface remains available without process activation:

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

## Project status

Riftri has completed the roadmap's capability, explicit APFS, and recoverable
storage-lifecycle milestones on macOS. The repository currently provides:

- A Rust workspace with separate CLI, core, Git, and storage boundaries.
- Destination-volume-specific `doctor` and `backends` diagnostics.
- Git discovery for normal, linked, unborn, detached, and bare repositories.
- Native-path, NUL-delimited Git worktree parsing.
- Exact-tree materialization through an isolated temporary Git index.
- Reusable read-only bases and strict native APFS COW clones with no copy fallback.
- Real `git worktree add --no-checkout` metadata and clean index synchronization.
- Durable, atomic add-operation journals and conservative recovery.
- Journaled clean-worktree removal with resumable recovery.
- Journaled managed-worktree moves and guarded, recoverable Git metadata pruning.
- Retained-base reference counts plus logical and allocated-byte reporting.
- Repository-aware `riftri repair` and actionable lifecycle status explanations.
- Explicit, journaled `riftri gc --apply` for zero-reference immutable bases.
- Read-only orphan and inconsistent-state diagnostics that never guess at cleanup.
- Isolation, Git cleanliness, crash recovery, symlink/mode, and physical-allocation tests.
- Repository-local `riftri enable`/`riftri disable` activation.
- Process-scoped `riftri exec` interception for supported worktree adds.
- Agent-neutral `riftri exec --worktree <path> -- <command>` process binding.
- Explicit sh/bash/zsh activation for normal `git` commands through
  `riftri shell hook`.
- Shell activation status plus explicitly evaluated sh/bash/zsh deactivation.

Try the safe diagnostic commands:

```console
$ cargo run -p riftri-cli -- doctor
$ cargo run -p riftri-cli -- doctor --destination ../proposed-worktree
$ cargo run -p riftri-cli -- doctor --json
$ cargo run -p riftri-cli -- backends ../proposed-worktree
$ cargo run -p riftri-cli -- enable
$ eval "$(cargo run --quiet -p riftri-cli -- shell hook zsh)"
$ cargo run -p riftri-cli -- shell status
$ eval "$(cargo run --quiet -p riftri-cli -- shell deactivate zsh)"
$ cargo run -p riftri-cli -- exec -- $SHELL
$ cargo run -p riftri-cli -- exec --worktree ../app-auth -- codex
$ cargo run -p riftri-cli -- worktree add ../app-auth -b feature/auth main
$ cargo run -p riftri-cli -- worktree move ../app-auth ../app-auth-renamed
$ cargo run -p riftri-cli -- worktree remove ../app-auth-renamed
$ cargo run -p riftri-cli -- worktree prune
$ cargo run -p riftri-cli -- status
$ cargo run -p riftri-cli -- repair
$ cargo run -p riftri-cli -- gc
$ cargo run -p riftri-cli -- gc --apply
```

The add command currently requires macOS and a writable APFS volume. Its first
compatibility envelope deliberately rejects attributes, filters/Git LFS,
sparse checkout, submodules, and checkout-changing non-default configuration.
Transparent optimized adds currently require either `-b <new-branch>` or
`--detach`. The ordinary no-option `git worktree remove <path>`, `git worktree
move <source> <destination>`, and `git worktree prune` forms are also routed
through Riftri when managed state is involved. Dirty views are preserved, and
moves preserve private changes. Forced or configured lifecycle commands that
could bypass the journal fail closed when they affect managed state. Unmanaged
worktrees continue to use ordinary Git. Zero-reference bases remain cached for
reuse until an explicit `riftri gc --apply`; a plain `riftri gc` only prints the
collection plan. These operations never fall back to a full copy. Set `RIFTRI_BYPASS=1`
only when you intentionally want an enabled command to use ordinary Git; using
it for a managed lifecycle operation can require manual repair. If an operation
is interrupted, run the repository-aware repair command:

```console
$ cargo run -p riftri-cli -- repair
```

The lower-level `recover --state-dir <path>` spelling remains available for an
explicit state directory. Repair deletes only an unchanged incomplete add or a
clean removal target. A changed view is preserved and reported for manual
attention. `riftri status` also lists state paths that no valid journal,
completion marker, or supported layout explains. Riftri preserves those paths;
inspect them before any manual cleanup.

The `status` and `gc` commands label allocation as filesystem-accounted. These
per-path values can count shared APFS blocks more than once, so adding them does
not measure Riftri's exclusive physical disk use. The documented APFS
volume-delta benchmark is the physical-sharing check.

## Installation

After the first public release, install the Rust-powered CLI through npm:

```console
$ npm install --global riftri
$ riftri doctor
```

The npm package is a small launcher. It installs the matching prebuilt Rust
binary as an optional platform package and forwards arguments, standard I/O,
signals, and exit status. Prebuilt CLI targets are macOS ARM64/x64, glibc Linux
ARM64/x64, and Windows ARM64/x64. Shipping a CLI binary does not imply that a
mutation backend exists on that platform: APFS is currently the only mutation
backend. Linux musl builds are not published yet; unsupported systems receive a
clear error and can build the Cargo workspace from source.

## Product boundary

Riftri owns:

- Opt-in interception of Git worktree lifecycle commands.
- Immutable base preparation.
- Native copy-on-write views.
- Mount, recovery, and disk-accounting metadata.

Git owns:

- Clone and fetch.
- Branch and checkout semantics.
- Status, diff, add, and commit.
- Merge, rebase, push, and pull.

Continue with:

- [Project outline](PROJECT.md)
- [Implementation roadmap](ROADMAP.md)
- [Architecture overview](docs/architecture.md)
- [APFS allocation evidence](docs/allocation-evidence.md)
- [Settled decisions and open questions](docs/decisions.md)
- [Changelog](CHANGELOG.md)
- [Contributing guide](CONTRIBUTING.md)
- [Support](SUPPORT.md)
- [Security policy](SECURITY.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Agent instructions](AGENTS.md)

## Development

```console
$ npm ci --ignore-scripts --omit=optional
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ npm test
$ npm run pack:check
```

To run the opt-in APFS physical-allocation check on a quiet volume:

```console
$ cargo test -p riftri-core --test apfs_worktree \
    cached_view_uses_materially_less_physical_space_than_its_logical_size \
    -- --ignored --nocapture
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for development expectations and
[RELEASING.md](RELEASING.md) for the GitHub/npm release process.
