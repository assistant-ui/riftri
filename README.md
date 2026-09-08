# Riftri

> One tree. Many realities.

Riftri is an opt-in copy-on-write storage accelerator for real Git worktrees.
Git continues to own cloning, branches, commits, merges, and remotes. Riftri
only changes how linked worktree files are materialized and stored.

## Intended experience

```console
$ git clone git@github.com:acme/app.git
$ cd app
$ riftri exec -- claude

# Inside the enabled process, this remains an ordinary Git command:
$ git worktree add ../app-auth -b feature/auth main
```

The resulting path is a real Git linked worktree. Unchanged data is shared with
an immutable base while writes remain private to that worktree.

## Project status

Riftri has completed its read-only capability and Git-semantics foundation. The
repository currently provides:

- A Rust workspace with separate CLI, core, Git, and storage boundaries.
- Destination-volume-specific `doctor` and `backends` diagnostics.
- Git discovery for normal, linked, unborn, detached, and bare repositories.
- Native-path, NUL-delimited Git worktree parsing.
- Versioned immutable-base keys and add-operation journal states.
- No Git interception, mounts, worktree creation, or destructive operations yet.

Try the safe diagnostic commands:

```console
$ cargo run -p riftri-cli -- doctor
$ cargo run -p riftri-cli -- doctor --destination ../proposed-worktree
$ cargo run -p riftri-cli -- doctor --json
$ cargo run -p riftri-cli -- backends ../proposed-worktree
```

## Installation

After the first public release, install the Rust-powered CLI through npm:

```console
$ npm install --global riftri
$ riftri doctor
```

The npm package is a small launcher. It installs the matching prebuilt Rust
binary as an optional platform package and forwards arguments, standard I/O,
signals, and exit status. Supported prebuilt targets are macOS ARM64/x64, glibc
Linux ARM64/x64, and Windows ARM64/x64. Linux musl builds are not published yet;
unsupported systems receive a clear error and can build the Cargo workspace
from source.

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
- [Settled decisions and open questions](docs/decisions.md)
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

See [CONTRIBUTING.md](CONTRIBUTING.md) for development expectations and
[RELEASING.md](RELEASING.md) for the GitHub/npm release process.
