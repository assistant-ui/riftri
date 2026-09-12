# Riftri

**Lightweight Git workspaces for parallel development.**

Riftri creates real Git worktrees without eagerly storing another full physical
copy of every unchanged project file. It is designed for developers and coding
agents working on several tasks at once.

[![CI](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml/badge.svg)](https://github.com/assistant-ui/riftri/actions/workflows/ci.yml)

> [!WARNING]
> Riftri is experimental, pre-release software. Keep important work committed
> or backed up. Optimized worktree operations require a writable APFS volume on
> macOS, Btrfs/reflink-enabled XFS or OverlayFS on Linux, or ReFS on Windows.
> OverlayFS requires either an already mount-capable namespace or the explicit
> system helper described below. Broader Windows filesystem support is still in
> development.

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
the filesystem.

## Installation

Install the experimental Rust-powered CLI through npm:

```console
$ npm install --global riftri
$ riftri doctor
```

Or build it from source:

```console
$ cargo build --release -p riftri-cli
$ ./target/release/riftri doctor
```

The npm package is a small launcher for a prebuilt Rust binary. Builds are
provided for macOS, Linux, and Windows, but optimized worktree creation is
currently available on APFS, supported Linux reflink volumes, caller-visible
OverlayFS mounts, and ReFS.

## Quick start

Check whether the current repository and destination are compatible:

```console
$ riftri doctor --destination ../app-auth
```

Create an optimized worktree explicitly:

```console
$ riftri worktree add ../app-auth -b feature/auth main
$ cd ../app-auth
$ git status
```

The new directory behaves like any other Git worktree. Riftri shares unchanged
data through an immutable native base; files allocate private storage as they
are changed.

## Use normal `git worktree` commands

Activate Riftri's Git shim in the current shell, then enable each repository
that should use optimized worktrees:

```console
$ eval "$(riftri shell hook zsh)"
$ cd app
$ riftri enable
$ git worktree add -b feature/auth ../app-auth main
```

The shell hook may be added to your shell profile if you want it available in
every new shell. This does **not** enable Riftri for every repository:
`riftri enable` is still required in each repository. Commands in repositories
that are not enabled go directly to the real Git executable. Riftri never edits
shell startup files automatically.

For a single agent or command tree, use process-scoped activation instead:

```console
$ riftri enable
$ riftri exec -- claude
```

Use `riftri disable` to opt a repository out. Use `riftri shell status` to
inspect activation, or deactivate the shim in the current shell with:

```console
$ eval "$(riftri shell deactivate zsh)"
```

See [Global shell activation](docs/global-activation.md) for shell setup,
compatibility details, and edge cases involving IDEs, containers, aliases, and
Git wrappers.

## Supported today

On macOS with APFS, Linux with Btrfs or reflink-enabled XFS, and Windows with
ReFS, Riftri supports:

- Optimized creation of real linked worktrees.
- Repository-scoped and process-scoped Git interception.
- Clean worktree removal, move, and prune operations.
- Reusable immutable bases with disk-usage reporting.
- Journaled recovery, repair, and garbage collection.
- Safe compatibility checks before any worktree is created.

Linux OverlayFS is also available experimentally when the add-time probe proves
that the current mount namespace can host a persistent view. On a normal Linux
shell without mount capability, install Riftri's narrow helper once:

```console
$ sudo riftri overlayfs install-helper
```

The helper is available system-wide, but it does not intercept Git or enable
any repository. `riftri enable` remains a separate per-repository choice. The
root-owned helper accepts only validated OverlayFS mount, exact
identity-checked unmount, and disposable work-directory reset requests for
directories owned by the calling user; capability-probe files remain owned and
verified by that user;
Git, agents, editors, builds, and normal file access continue to run without
elevation. Use `--replace` when upgrading an existing helper.

OverlayFS supports the same real-worktree creation, clean removal, isolation,
crash recovery, and explicit repair after a reboot. Mounted-view moves are
rejected before mutation. If neither reflinks, a mount-capable namespace, nor a
valid helper is available, Riftri stops before mutation and never silently
creates a full-copy worktree.

Riftri deliberately stops with a clear explanation when a checkout cannot yet
be reproduced safely—for example, repositories using Git LFS, custom filters,
sparse checkout, submodules, or external attributes. It never silently replaces
an optimized operation with a full worktree copy.

Useful commands:

```console
$ riftri doctor
$ riftri status
$ riftri repair
$ riftri gc
$ riftri gc --apply
$ riftri state forget-missing /absolute/path/to/removed-state
```

If a custom state directory was removed outside Riftri, lifecycle interception
continues to fail closed. Remove that exact stale repository-local registration
explicitly with `riftri state forget-missing`; existing state directories are
never accepted by this command.

## How disk sharing works

Riftri prepares one immutable base for an exact Git tree. It creates native
APFS clones, Linux reflinks, or ReFS block clones from it, or exposes it as an
OverlayFS lower layer with a private writable upper. Unchanged contents are not
materialized again; worktrees are lightweight—not free—and private disk use
grows as they diverge.

`riftri status` reports managed views and filesystem-accounted allocation. For
details on measuring physical sharing, see
[APFS allocation evidence](docs/allocation-evidence.md) and
[Linux reflink verification](docs/linux-reflink.md), or see
[Windows ReFS support](docs/windows-refs.md) for that backend's requirements.
The tested metadata guarantees and platform boundaries are documented in
[Filesystem metadata compatibility](docs/filesystem-compatibility.md).

## Project status

The macOS/APFS, Linux reflink, Linux OverlayFS, and Windows/ReFS implementations
include worktree creation, process-scoped Git interception, lifecycle recovery,
cleanup, and disk accounting. Broader checkout compatibility, ordinary Windows
filesystem alternatives, and managed environments remain roadmap work.

Development plans and design details live in:

- [Project definition](PROJECT.md)
- [Roadmap](ROADMAP.md)
- [Architecture](docs/architecture.md)
- [Design decisions](docs/decisions.md)
- [Release process](RELEASING.md)

## Contributing

Run the complete local quality gate before opening a pull request:

```console
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ npm test
```

Riftri is licensed under the [Apache License 2.0](LICENSE).
