# Riftri agent guide

This file is the durable starting context for coding agents working on Riftri.
Read `PROJECT.md`, `ROADMAP.md`, and `docs/architecture.md` before changing the
product boundary or implementing a new storage backend.

## Mission

Riftri is an opt-in copy-on-write storage accelerator for real Git linked
worktrees.

Git remains the source of truth. Users and agents continue to use ordinary Git
commands. Riftri changes only how worktree files are materialized and stored.

## Current stage

The repository has completed the capability/Git-semantics foundation and the
explicit APFS prototype. On macOS it can create real linked worktrees from
strict native APFS clones, reuse exact-tree immutable bases, persist atomic add
journals, roll back failures, and recover interrupted adds without deleting a
changed view. It still has no transparent Git interception, removal
transaction, filesystem mounts, Linux/Windows mutation backend, or daemon.

Check `ROADMAP.md` before starting implementation. Do not skip milestone safety
or compatibility gates merely to reach a working demo faster.

## Hard product boundaries

Riftri does not own:

- Repository cloning or fetching.
- Branch, checkout, commit, merge, rebase, push, or pull semantics.
- Pull requests or Git hosting behavior.
- An agent-specific filesystem API.
- A replacement workspace abstraction.

Riftri owns:

- Explicit or opt-in interception of Git worktree lifecycle operations.
- Preparation and reuse of immutable bases for exact Git trees.
- Native copy-on-write views and their private changes.
- Backend capability detection, cleanup, recovery, and disk accounting.

## Required user experience

The explicit interface is the correctness baseline:

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

The preferred agent interface is process-scoped activation:

```console
$ riftri exec -- claude
$ git worktree add ../app-auth -b feature/auth main
```

The process-scoped Git shim must pass normal Git commands directly to the real
Git executable. Persistent shell integration may be offered later, but it must
remain explicitly enabled. Never replace system Git globally by default.

## Architecture invariants

- Create real Git linked worktrees; do not simulate Git metadata.
- Invoke the installed Git executable for Git behavior.
- Suppress Git's normal checkout before creating a COW-backed view.
- Key immutable bases by repository, Git tree, checkout profile, and filesystem
  volume.
- Never use a mutable working directory as a shared lower layer.
- Keep Riftri out of ordinary file reads and writes.
- Prefer APFS clones, native reflinks, and kernel OverlayFS over FUSE.
- Never silently fall back to a full worktree copy.
- Preserve Git's dirty-worktree and branch-safety behavior.
- Treat cleanup as a journaled, recoverable transaction.
- Never require an agent prompt or skill for correctness.

## Crate responsibilities

- `riftri-cli`: user-facing command parsing and rendering only.
- `riftri-core`: orchestration and product policy.
- `riftri-git`: communication with the real Git executable.
- `riftri-storage`: storage capability and backend contracts.

The root npm package and `npm/` directory are distribution-only. Product policy,
Git semantics, and filesystem behavior remain in Rust.

Platform implementations should remain behind the storage boundary. Do not put
macOS, Linux, or Windows system calls in the CLI crate.

## Engineering rules

- Start with a failing test or a reproducible fixture for behavioral changes.
- Keep destructive operations opt-in and validate exact paths before cleanup.
- Preserve non-UTF-8 path support in low-level APIs; do not assume every path is
  a Rust `String`.
- Prefer structured Git output such as porcelain formats with NUL delimiters.
- Avoid parsing human-oriented Git output.
- Use atomic rename and operation journals for multi-step mutations.
- Do not introduce an always-on daemon unless a backend requires mount recovery.
- Update `docs/decisions.md` when a foundational choice changes.
- Update `ROADMAP.md` when a milestone's acceptance criteria are completed.

## Quality gates

Run all of these before handing off code:

```console
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
```

Filesystem work also needs platform-specific integration tests that prove:

- A newly created worktree is clean according to Git.
- Writes in one worktree cannot change another worktree or its base.
- Physical allocation is materially lower than a normal full checkout.
- Failed creation and removal operations can be recovered safely.

## Documentation map

- `PROJECT.md`: product and technical outline.
- `ROADMAP.md`: implementation order and acceptance criteria.
- `docs/architecture.md`: component and transaction design.
- `docs/decisions.md`: settled decisions and open questions.
- `README.md`: public introduction and current status.
