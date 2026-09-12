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

The repository has completed Milestones 1 through 4: the capability/Git-semantics
foundation, explicit APFS prototype, recoverable storage lifecycle, and
process-scoped transparent Git compatibility. On macOS, supported Linux
volumes, and Windows ReFS it can create real linked worktrees from strict native
APFS clones, Linux reflinks, or ReFS block clones, reuse exact-tree immutable
bases, persist atomic add journals, roll back failures, and recover interrupted
adds without deleting a changed view. Repository-local activation, the
process-scoped Git shim, and an
explicitly evaluated sh/bash/zsh hook can route supported adds through the same
transaction. Shell status and explicitly evaluated deactivation keep global
per-user hook setup visible and reversible without editing shell profiles.
Process-scoped commands can optionally start from a validated Git worktree root.
Clean managed removals now use a separate recoverable journal,
and status reports retained-base references and disk usage with repository-aware
repair for incomplete journals. Explicit garbage collection uses its own
recoverable journal and revalidates references under the immutable-base lock.
Status reports unexplained or inconsistent state paths but never deletes them.
Managed move and prune now use separate recoverable journals. Linux creation
actively verifies `FICLONE` with unnamed temporary files and supports Btrfs and
reflink-enabled XFS without a byte-copy fallback. When reflinks are unsupported,
Linux can select OverlayFS only after an artifact-clean active probe succeeds in
the caller's current mount namespace. OverlayFS add and clean removal use the
same durable transaction, exact private-layer ownership, mount identity, and
token-bound crash-gap recovery; mounted moves remain fail-closed. Windows
creation actively verifies ReFS block cloning and private-write isolation before
mutation and uses the same journaled lifecycle. Riftri still has no forced
move/removal lifecycle path, automatic orphan-state repair, least-privilege
mount helper, ordinary-NTFS backend, managed-environment integration, or daemon.
The native COW checkout path accepts only an allowlisted deterministic subset of
in-tree attributes (`text`, `eol`, and `binary` semantics). External attributes,
Git LFS, custom filters, encodings, ident substitution, legacy attributes, and
unknown attribute names remain fail-closed.

Milestone 4's transparent-Git compatibility matrix lives in
`crates/riftri-cli/tests/global_activation.rs`. Global per-user shell activation
never replaces repository-local consent; preserve that distinction in UX and
tests.

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

Users may explicitly activate normal Git interception in a shell:

```console
$ eval "$(riftri shell hook zsh)"
$ riftri enable
$ git worktree add ../app-auth -b feature/auth main
```

Both Git-shim modes must pass normal Git commands directly to the real Git
executable, and repository-local enablement must control optimized adds. Shell
integration must remain explicitly evaluated; never edit shell startup files or
replace system Git globally by default.

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

The root `package.json` defines the public `riftri` npm package. Its launcher,
platform manifests, release tooling, and tests live under `package/`; that
directory is distribution-only. Product policy, Git semantics, and filesystem
behavior remain in Rust.

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
- Squash-merge every pull request. Do not create merge commits or rebase-merge
  pull requests, and keep the pull request title in Conventional Commit form.
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
