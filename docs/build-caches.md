# Dependencies and build caches across worktrees

Riftri shares the unchanged parts of a *Git tree*. Dependency directories and
build outputs are not in that tree: `node_modules/`, `target/`, `.venv/`,
`dist/`, and `.next/` are untracked or ignored, so a new managed worktree never
inherits them. Copy-on-write does nothing for them, and every worktree starts
without them.

That is the right default — two branches can disagree about their dependency
graph — but it means the cost of N parallel worktrees is N installs and N
builds unless something is shared. This page covers what is safe to share and
what is not.

## The rule

**Share content-addressed stores. Do not share writable working directories.**

A content-addressed store is keyed by the hash of its contents, is append-only
in practice, and is built for concurrent readers and writers. A working
directory is rewritten in place by whichever process runs last. Sharing the
first is what package managers already expect; sharing the second reintroduces
exactly the cross-contamination that separate worktrees exist to prevent.

## Safe to share

These are already global by default, so the usual answer is to leave them
alone rather than to configure anything:

| Tool | Shared store | Notes |
| --- | --- | --- |
| pnpm | `pnpm store path` | Content-addressed; `node_modules` becomes links into it |
| npm | `~/.npm/_cacache` | Content-addressed download cache |
| Yarn (Berry) | `~/.yarn/berry/cache` | Zip-per-package global cache |
| Cargo | `$CARGO_HOME/registry`, `$CARGO_HOME/git` | Cargo locks these itself |
| Go | `$GOMODCACHE` | Read-mostly after download |
| uv / pip | `~/.cache/uv`, `~/.cache/pip` | Wheel and download caches |

With a shared store, a second worktree's install is mostly link or unpack work
rather than a fresh download, which is usually the expensive part.

## Not safe to share

| Directory | Why not |
| --- | --- |
| `node_modules/` | Per-branch dependency graph, rewritten in place by every install |
| `target/` (Cargo) | Cargo takes an exclusive lock, so builds serialize; fingerprints thrash when branches differ |
| `.venv/`, `venv/` | Records absolute paths and is rewritten by installs |
| `dist/`, `build/`, `.next/`, `.turbo/` | Build outputs; last writer wins |

Symlinking one of these into a common directory makes parallel worktrees share
mutable state. Two agents installing different dependency sets, or building
different branches, will interleave writes. The failures are intermittent and
look like compiler or bundler bugs, which makes them expensive to diagnose.

## Making per-worktree work cheap

- **Use a package manager that links from a store.** pnpm is the clearest case:
  its `node_modules` is mostly links into the shared store, so a per-worktree
  install is fast and small.
- **Share compilation, not output directories.** For Rust, keep `target/` per
  worktree and put [`sccache`](https://github.com/mozilla/sccache) in front of
  the compiler; it is designed for concurrent access from independent builds.
  The same applies to `ccache` for C and C++.
- **Let the base do the work for tracked files.** Anything committed — vendored
  dependencies, generated code checked into the tree, fixtures — is shared by
  copy-on-write at no extra cost. Committing a large generated file is not free
  in Git, but it is close to free across worktrees.
- **Reclaim space from long-lived worktrees.** A view that has built and
  reverted retains private blocks. `riftri worktree compact <path>` returns a
  pristine worktree to the cost of a fresh view; see the
  [CLI reference](cli.md).

## Checking what a worktree actually costs

`riftri worktree list` reports filesystem-accounted allocation per managed
worktree, and `riftri status` reports bases, reference counts, and total
storage. If a worktree is far larger than its siblings, the cause is usually an
untracked build directory rather than the checkout itself:

```console
$ riftri worktree list
$ du -sh ../app-auth/node_modules ../app-auth/target 2>/dev/null
```

Nothing on this page changes Riftri's guarantees: private writes stay private,
and no configuration here is required for correctness. It is about not paying
for the same download or the same compile once per worktree.
