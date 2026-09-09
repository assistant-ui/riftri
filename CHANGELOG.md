# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

### Added

- Rust workspace with CLI, core, Git, and storage crate boundaries.
- Destination-specific, read-only storage capability diagnostics.
- Git repository identity, object resolution, and NUL-delimited worktree parsing.
- Versioned base-key, checkout-profile, and operation-journal models.
- npm launcher with platform-specific native binary packages.
- GitHub CI and tag-driven GitHub/npm release automation.
- Explicit `riftri worktree add` support on writable APFS volumes.
- Exact-tree immutable-base creation and reuse with serialized construction.
- Strict APFS COW cloning for regular files with symlink and executable-mode support.
- Atomic native-path add journals and `riftri recover` for interrupted operations.
- Repository-local activation through `riftri enable` and `riftri disable`.
- Process-scoped `riftri exec` Git interception for supported adds in enabled repositories.
- Optional validated worktree binding for any process-scoped command through
  `riftri exec --worktree <path> -- <command>`.
- Explicit `riftri shell hook` activation so normal Git adds are intercepted in
  enabled repositories without wrapping each command.
- A concurrent APFS integration fixture that proves parallel adds serialize
  immutable-base construction and preserve independent Git commits and files.
- Journaled clean-worktree removal through the explicit CLI and enabled Git
  shim, with conservative interrupted-removal recovery.
- `riftri status` reporting for retained bases, active views, reference counts,
  logical bytes, and filesystem-allocated bytes.
- Repository-aware `riftri repair` for conservative add/removal journal recovery,
  plus actionable status explanations for pending removals and retained bases.
- Explicit `riftri gc` planning and `--apply` collection for zero-reference
  immutable bases, with per-base locking, reference revalidation, and durable
  collection journals.
- Read-only status diagnostics for unjournaled files, empty base buckets,
  structurally unsafe state entries, and missing active journal paths.
- Offline release-staging coverage for all six native npm packages, GitHub
  archives, executable names, and SHA-256 checksums.

### Changed

- npm distribution sources now live under `package/` without changing
  published package names or tarball layout.
- Public contribution, support, security, and release guidance now follows the
  Assistant UI organization ownership model.
- Public documentation and npm metadata identify Riftri as experimental,
  pre-release software.
- CI runs once for pull-request branches and again on `main` after merge instead
  of duplicating every pull-request run with an unrestricted push run.
- Milestone 2 now permits only explicit APFS worktree creation. Unsupported
  checkout configurations fail before mutation, and full-copy fallback remains
  disabled.
- Ordinary Git commands and disabled repositories pass directly to real Git
  inside `riftri exec`; unsupported enabled add forms fail without fallback.

### Fixed

- Concurrent worktree operations in one Riftri process now receive distinct
  journal and scratch identifiers even when the system clock returns the same
  timestamp to multiple threads.
- Enabled Git interception now fails closed instead of allowing forced removal,
  move, or prune to mutate managed Riftri lifecycle state outside its journals.

### Safety

- Failed adds roll back real Git metadata and their newly created branch when it
  has not moved.
- Recovery preserves incomplete views that are dirty or no longer match their
  immutable base.
