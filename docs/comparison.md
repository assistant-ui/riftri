# Riftri compared with alternatives

Riftri occupies a narrow slot: real Git linked worktrees whose unchanged files
share native copy-on-write storage. Several other approaches solve nearby
problems. This page states what each one shares, what it costs, and when it is
the better choice.

| Approach | Working-tree storage | Git history | Git UX preserved | Crash-safe lifecycle |
| --- | --- | --- | --- | --- |
| Riftri worktree | Shared COW blocks per exact tree | Shared (one repository) | Yes | Journaled add/remove/move/gc/repair |
| `git worktree add` | Full private checkout | Shared (one repository) | Yes | Git's own metadata only |
| Full `git clone` | Full private checkout | Full private copy | Yes | Independent repositories |
| `git clone --reference` | Full private checkout | Shared object store | Yes, with alternates caveats | Independent repositories |
| Manual reflink copy of a checkout | Shared COW blocks | Confused — copied `.git` state | No | None |
| Container image or volume per agent | Depends on the container storage driver | Depends on the mount | Yes, inside the container | Container runtime's |

## Plain `git worktree add`

Linked worktrees already share the object database and repository metadata.
What they do not share is the materialized working directory: ten worktrees of
a large repository store ten full copies of mostly identical files. Riftri
keeps exactly this model — every Riftri view *is* a real linked worktree — and
replaces only the materialization step with a native COW clone of an immutable
base. If your repository is small, or you keep one or two long-lived
worktrees, plain `git worktree add` is simpler and works on every filesystem.
The measured differences for a real repository are recorded in the
[assistant-ui ten-agent experiment](benchmarks/assistant-ui-ten-agents-2026-09-12.md).

## Full clones and `git clone --reference`

A second clone gives complete isolation, including history, at the price of
duplicating everything and splitting your remotes and branch state across
repositories. `git clone --reference` (or `--shared`) removes the object-store
duplication but still materializes a full working tree, and it introduces the
well-known alternates hazard: pruning the reference repository can corrupt
borrowers. Riftri keeps one repository, one object store, and one branch
namespace, and shares the working tree instead.

## Manual copy-on-write copies (`cp -c`, `cp --reflink=always`)

Cloning a checkout with the filesystem's own COW copy shares blocks, but the
result is not a linked worktree: the copied `.git` file or directory points at
the wrong place, index timestamps go stale, and Git has no record that the
directory exists. Nothing tracks which copies exist, whether they diverged, or
how to clean them up safely. Riftri does use these native primitives
internally — but from an immutable base built for an exact Git tree, wrapped
in real `git worktree` metadata, integrity-verified reuse, and forward-only
journals so interrupted operations can be repaired. The storage model is
documented in [architecture](architecture.md).

## A container or VM per agent

Containers isolate processes, dependencies, and side effects — strictly more
isolation than Riftri offers, and the right call for untrusted code (Riftri's
COW isolation is deliberately not a security sandbox; see
[SECURITY.md](../SECURITY.md)). But container layers do not deduplicate the
repository checkout each container materializes, and bind-mounting one shared
checkout gives up per-task isolation of edits. The approaches compose: run
agents in containers whose worktree destinations sit on a supported volume,
and use Riftri inside for cheap per-task views. See
[agent integration](agent-integration.md).

## Different version-control clients (Sapling, Jujutsu)

Sapling and Jujutsu rethink the version-control layer itself and bring their
own working-copy engines. They are larger bets: new commands, new mental
models, and varying degrees of Git interoperability. Riftri deliberately
changes nothing about Git — same commands, same refs, same tooling — and only
accelerates worktree storage. If you want a different VCS, Riftri is not it.

## When not to use Riftri

- The destination volume has no supported backend (for example ordinary NTFS
  or ext4 without reflink support) — see
  [filesystem compatibility](filesystem-compatibility.md). Riftri stops rather
  than silently making a full copy.
- The checkout profile is outside the supported set today: custom filters,
  sparse checkout, submodules, or non-canonical Git LFS configurations — see
  the [Git LFS profile](git-lfs.md) and the [roadmap](../ROADMAP.md).
- You need cross-machine sharing or a commit/merge workflow — both are
  explicitly out of scope ([ROADMAP.md](../ROADMAP.md), deferred ideas).
