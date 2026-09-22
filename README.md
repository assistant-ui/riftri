# Riftri

**Lightweight Git workspaces for parallel development.**

Riftri creates real Git worktrees without eagerly storing another full physical
copy of every unchanged project file. It is designed for developers and coding
agents working on several tasks at once.

[![CI](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml/badge.svg)](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml)

> [!WARNING]
> Riftri is experimental, pre-release software. Keep important work committed or
> backed up. Optimized worktrees need APFS on macOS, Btrfs or reflink-enabled
> XFS or OverlayFS on Linux, or ReFS on Windows — see
> [what works today](#what-works-today).

## Why Riftri?

Git linked worktrees share repository history, but each worktree normally gets
its own complete checkout. That can consume significant disk space when several
developers or agents work on a large repository in parallel.

Riftri keeps normal Git behavior while making those checkouts lightweight:

- Every workspace is a real Git linked worktree.
- Unchanged files share native copy-on-write storage blocks.
- Changes remain private to each worktree.
- Editors, build tools, and agents use ordinary files and Git commands.
- Interrupted creation and cleanup operations can be recovered safely.

Riftri does not replace Git, manage branches, or sit between applications and
the filesystem. For how it compares to plain `git worktree`, reference clones,
manual reflink copies, and per-agent containers, see the
[comparison with alternatives](docs/comparison.md).

## Install

macOS and Linux:

```sh
curl -fsSL https://riftri.dev/install.sh | bash
```

Windows PowerShell:

```powershell
$Installer = Join-Path $env:TEMP 'riftri-install.ps1'
Invoke-WebRequest https://riftri.dev/install.ps1 -OutFile $Installer
& $Installer
```

Both installers verify SHA-256 and the binary version, install to a per-user
directory, and never use sudo, edit shell profiles, or activate Git
interception. Node.js is not required. Read
[`install.sh`](package/install.sh) or [`install.ps1`](package/install.ps1)
first if you prefer, and see the [installation guide](docs/install.md) for all
eight targets, manual downloads, and checksum verification.

Or build from source:

```console
$ cargo build --release -p riftri-cli
$ ./target/release/riftri doctor
```

The interactive `riftri setup` terminal UI is behind an off-by-default `tui`
feature; add `--features tui` to build it in (the published binaries do). The
default build keeps `setup` working through plain text prompts.

Or install through npm, which fetches the binary for your platform:

```sh
npm install --global riftri
```

> **Windows on ARM64 is not yet available through npm.** Its platform package
> is held up in registry review, so npm installs there resolve without a
> binary. Use the PowerShell installer above until that clears. Every other
> platform — macOS, Linux, and Windows x64 — installs normally.

## Quick start

For an interactive walkthrough (v0.3.1 and later), run
`riftri setup` inside your existing repository. It checks the destination,
confirms a new worktree at `HEAD`, and then asks whether to open Claude Code,
Codex, another installed executable, or no agent. Repository enablement and
agent launch require a separate confirmation. See the
[setup reference](docs/cli.md#riftri-setup-options).

The explicit commands below work in the current release and in automation.

Check that the repository and destination are compatible — this creates
nothing:

```console
$ riftri doctor --destination ../app-auth
```

Then create a worktree:

```console
$ riftri worktree add ../app-auth -b feature/auth main
$ cd ../app-auth
$ git status
```

To attach an existing branch instead, drop the `-b` flag:

```console
$ riftri worktree add ../app-auth feature/auth
```

The new directory behaves like any other Git worktree.

## Use normal `git worktree` commands

Activate the Git shim in your shell, then opt in each repository:

```console
$ eval "$(riftri shell hook zsh)"
$ cd app
$ riftri enable
$ git worktree add -b feature/auth ../app-auth main
```

For a single agent or command tree, skip the shell hook entirely:

```console
$ riftri enable
$ riftri exec -- claude
```

Activating the shell hook does **not** enable Riftri everywhere: `riftri enable`
is still required per repository, and other repositories go straight to real
Git. Use `riftri disable` to opt out and `riftri shell status` to inspect
activation.

PowerShell setup, IDE and container edge cases, and agent harness integration
are covered in [global activation](docs/global-activation.md) and the
[agent integration guide](docs/agent-integration.md).

## What works today

| Platform | Backend | Status |
| --- | --- | --- |
| macOS | APFS clones | Supported |
| Linux | Btrfs / reflink-enabled XFS | Supported |
| Linux | OverlayFS | Experimental |
| Windows | ReFS block clones | Supported |

On those backends Riftri creates real linked worktrees, intercepts Git per
repository or per process, removes, moves, prunes, and compacts them, reuses
immutable bases with disk accounting, and recovers from interruption through
journaled repair and garbage collection.

OverlayFS additionally needs either a mount-capable namespace or a one-time
helper. The helper is installed system-wide but intercepts nothing on its own,
and `riftri enable` remains a separate per-repository choice:

```console
$ sudo riftri overlayfs install-helper
```

**Riftri fails closed.** When a checkout cannot be reproduced exactly — custom
filters, sparse checkout, submodules, external attributes, checkout hooks, or non-canonical
[Git LFS](docs/git-lfs.md) setups — it stops before changing anything rather
than silently falling back to a full copy. See
[how Riftri stays safe](docs/safety.md).

## Common commands

```console
$ riftri doctor                      # check compatibility
$ riftri worktree list               # managed worktrees and their disk use
$ riftri status                      # bases, references, and accounting
$ riftri worktree compact ../app-auth
$ riftri repair                      # resume or roll back interrupted work
$ riftri gc --apply                  # delete unreferenced bases
```

Explicit lifecycle and diagnostic commands accept `--json` and `--json-errors`
for structured success and failure receipts, so automation does not have to
scrape human text. Interactive `setup` is for terminal users, not automation.
The full reference is in [docs/cli.md](docs/cli.md).

## Optional agent skill

The repository includes a reviewable
[`riftri-worktrees` skill](skills/riftri-worktrees/SKILL.md) for agents that need
to create or use Riftri worktrees. It explains explicit creation, process-scoped
Git interception, and safe cleanup; it is not required for copy-on-write safety
and does not install Riftri or enable a repository.

Install it through the [skills CLI](https://skills.sh/docs/cli):

```sh
npx skills add assistant-ui/riftri --skill riftri-worktrees
```

Review the skill first and choose the agent and installation scope in the CLI.
The skill files are hosted here on GitHub; [skills.sh](https://skills.sh/docs/faq)
discovers and ranks skills through CLI installation telemetry. A directory
listing is not a separate package publication or a guarantee of inclusion.

## Documentation

[docs/README.md](docs/README.md) indexes everything. Start with:

- [Architecture](docs/architecture.md) — immutable bases, transactions, journals
- [How Riftri stays safe](docs/safety.md) — fail-closed rules and real-filesystem CI
- [Troubleshooting](docs/troubleshooting.md) — symptom-first fixes
- [CLI reference](docs/cli.md) · [Installation](docs/install.md) · [Benchmarks](docs/benchmarks.md)
- [Project definition](PROJECT.md) · [Roadmap](ROADMAP.md) · [Releasing](RELEASING.md)

## Contributing

Run the complete local quality gate before opening a pull request:

```console
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ npm test
$ npm run smoke:installed
```

Riftri is licensed under the [MIT License](LICENSE).
