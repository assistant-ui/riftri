# Riftri

> One tree. Many realities.

Riftri is an opt-in copy-on-write storage accelerator for real Git worktrees.
Git continues to own cloning, branches, commits, merges, and remotes. Riftri
only changes how linked worktree files are materialized and stored.

## Current macOS experience

```console
$ git clone git@github.com:acme/app.git
$ cd app
$ riftri enable
$ riftri worktree add ../app-auth -b feature/auth main
```

`riftri enable` records repository-local consent in `riftri.enabled`. The
process-scoped Git interception that consumes this marker is delivered in the
follow-up transparent-activation change. The explicit command above already
creates the optimized worktree.

The resulting path is a real Git linked worktree. Unchanged data is shared with
an immutable base while writes remain private to that worktree. Riftri does not
modify the parent shell, shell startup files, or system Git. To stop opting in:

```console
$ riftri disable
```

The explicit interface remains available without process activation:

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

## Project status

Riftri has an explicit APFS prototype on macOS. The repository currently
provides:

- A Rust workspace with separate CLI, core, Git, and storage boundaries.
- Destination-volume-specific `doctor` and `backends` diagnostics.
- Git discovery for normal, linked, unborn, detached, and bare repositories.
- Native-path, NUL-delimited Git worktree parsing.
- Exact-tree materialization through an isolated temporary Git index.
- Reusable read-only bases and strict native APFS COW clones with no copy fallback.
- Real `git worktree add --no-checkout` metadata and clean index synchronization.
- Durable, atomic add-operation journals and conservative recovery.
- Isolation, Git cleanliness, crash recovery, symlink/mode, and physical-allocation tests.
- Repository-local `riftri enable`/`riftri disable` activation.

Try the safe diagnostic commands:

```console
$ cargo run -p riftri-cli -- doctor
$ cargo run -p riftri-cli -- doctor --destination ../proposed-worktree
$ cargo run -p riftri-cli -- doctor --json
$ cargo run -p riftri-cli -- backends ../proposed-worktree
$ cargo run -p riftri-cli -- enable
$ cargo run -p riftri-cli -- worktree add ../app-auth -b feature/auth main
```

The add command currently requires macOS and a writable APFS volume. Its first
compatibility envelope deliberately rejects attributes, filters/Git LFS,
sparse checkout, submodules, and checkout-changing non-default configuration.
It never falls back to a full copy. If an add is interrupted, run:

```console
$ cargo run -p riftri-cli -- recover --state-dir "$(git rev-parse --git-common-dir)/riftri"
```

Recovery deletes only an unchanged incomplete view. A changed view is preserved
and reported for manual attention.

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
- [APFS allocation evidence](docs/allocation-evidence.md)
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

To run the opt-in APFS physical-allocation check on a quiet volume:

```console
$ cargo test -p riftri-core --test apfs_worktree \
    cached_view_uses_materially_less_physical_space_than_its_logical_size \
    -- --ignored --nocapture
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for development expectations and
[RELEASING.md](RELEASING.md) for the GitHub/npm release process.
