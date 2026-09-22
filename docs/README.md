# Riftri documentation

One entry per document. The same material is served as a single Markdown
overview at <https://riftri.dev/index.md>; when you add a document here, also
add it to that overview's documentation index — CI enforces the link.

## Setup and platform guides

- [cli.md](cli.md) — every command, argument, and flag, plus the shared
  `--json`/`--json-errors` conventions.
- [install.md](install.md) — standalone installers, all release targets,
  checksum verification, updates, and uninstalling.
- [global-activation.md](global-activation.md) — opt-in Git interception,
  shell hooks, and the shell compatibility matrix.
- [agent-integration.md](agent-integration.md) — harness setup (Claude Code,
  Codex, containers), the JSON automation contract, and the parallel-agent
  lifecycle.
- [custom-harness.md](custom-harness.md) — building your own runner on the
  CLI: the lifecycle, exit codes, and using Riftri as a default workspace
  layer with a fallback.
- [node-api.md](node-api.md) — the programmatic API shipped with the
  `riftri` npm package, its methods, and its typed errors.
- [linux-reflink.md](linux-reflink.md) — Btrfs and reflink-enabled XFS support
  and verification.
- [linux-overlayfs.md](linux-overlayfs.md) — OverlayFS mount requirements,
  the root-owned helper, and reboot recovery.
- [windows-refs.md](windows-refs.md) — ReFS block-clone requirements and
  testing.
- [git-lfs.md](git-lfs.md) — the exact accepted Git LFS profile and its
  fail-closed boundaries.
- [sparse-checkout.md](sparse-checkout.md) — the supported cone-mode sparse
  worktree subset, its base-key rules, and refusal behavior.
- [troubleshooting.md](troubleshooting.md) — symptom-first answers: refused
  operations, activation gaps, recovery, disk usage, and installation.
- [backing-out.md](backing-out.md) — what survives if you stop using
  Riftri, and how to remove it cleanly.
- [build-caches.md](build-caches.md) — which dependency stores are safe to
  share across parallel worktrees, and which directories never are.

## Concepts and guarantees

- [safety.md](safety.md) — how Riftri stays safe: fail-closed refusals,
  real-filesystem CI, honest benchmarks, and supply-chain measures.
- [architecture.md](architecture.md) — components, immutable bases,
  transactions, and journal state machines.
- [decisions.md](decisions.md) — numbered settled decisions and open
  questions.
- [backend-guarantees.md](backend-guarantees.md) — storage contracts and
  metadata profiles per backend.
- [filesystem-compatibility.md](filesystem-compatibility.md) — permissions,
  symlinks, extended attributes, and platform boundaries.
- [comparison.md](comparison.md) — Riftri versus plain `git worktree`,
  reference clones, manual reflink copies, containers, and other VCS clients.

## Measurements

- [benchmarks.md](benchmarks.md) — running and interpreting the native-COW
  benchmarks.
- [allocation-evidence.md](allocation-evidence.md) — APFS physical-sharing
  measurement methodology and results.
- [benchmarks/](benchmarks/) — dated benchmark reports with raw JSON,
  including the
  [assistant-ui ten-agent experiment](benchmarks/assistant-ui-ten-agents-2026-09-12.md).

Project-level documents live at the repository root: [README](../README.md),
[PROJECT](../PROJECT.md), [ROADMAP](../ROADMAP.md),
[CONTRIBUTING](../CONTRIBUTING.md), [RELEASING](../RELEASING.md),
[SECURITY](../SECURITY.md), and [SUPPORT](../SUPPORT.md).
