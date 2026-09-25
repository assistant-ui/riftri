# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

### Added

- Explicitly evaluated PowerShell activation and deactivation provide the same
  repository-gated normal Git interception as sh, bash, and zsh without editing
  PowerShell profiles or persistent `PATH`.

### Changed

- `riftri backends` and `riftri doctor` render backend kinds and capability
  statuses as stable human-readable labels instead of Rust debug formatting.
- The redundant `riftri recover --state-dir <dir>` command is removed.
  `riftri repair --state-dir <dir>` performs the same journal recovery and is
  already the form every error message, status hint, and document recommends.

### Documentation

- README records the project's actual MIT license instead of Apache License 2.0.
- SECURITY.md and SUPPORT.md describe the supported macOS APFS, Linux
  Btrfs/reflink-XFS/OverlayFS, and Windows ReFS backends instead of macOS alone.
- The website README describes the current single-page structure, and the
  Windows install link tracks `docs/install.md` on `main` instead of a pinned
  pre-merge commit.

### Release

- Dependabot covers the root npm launcher package and the pnpm-based website
  alongside the existing Cargo and GitHub Actions ecosystems.

## 0.2.1 - 2026-09-13

### Fixed

- OverlayFS unmounts retry transient busy results with bounded backoff while
  revalidating the exact journaled mount identity before every retry.
- Rollback race coverage now synchronizes directly at Git's removal boundary
  and asserts structured failures while proving concurrent writes are preserved.

### Release

- Partial npm publications safely skip completed exact versions, keep the
  launcher last, and verify all nine packages after bounded registry retries.

### Documentation

- Project, roadmap, architecture, and agent guidance now consistently record
  the completed Linux milestone and the journal-backed lifecycle registry.

## 0.2.0 - 2026-09-13

### Added

- A checksum-verified Bash installer for macOS and Linux, with pinned versions,
  atomic per-user upgrades, and a copyable command on the website.
- Standalone direct downloads and npm packages for eight native targets,
  including ARM64 and x64 Linux builds for both glibc and musl.
- Destination-readiness diagnostics that explain the selected backend and give
  a concrete next command or remedy.
- Stable JSON lifecycle failure receipts for automation, including the failure
  category, transaction phase, cleanup result, recovery state, and next command.
- A documented cross-backend guarantee contract and versioned checkout metadata
  profile for APFS, Linux reflink and OverlayFS, and Windows ReFS.

### Changed

- Native reflink and ReFS tree cloning now processes independent regular files
  concurrently with a bounded worker pool while preserving ordered metadata and
  cleanup behavior.
- Exact immutable-base verification can run concurrently for readers, reducing
  contention between parallel worktree creations.
- Checkout configuration is read in batches instead of starting Git once per
  setting.

### Fixed

- GitHub native-archive releases publish independently of npm after shared
  staging checks. Exact asset and checksum validation rejects incomplete or
  changed downloads; release retries never overwrite published assets.
- Linux filesystem detection now uses a libc-independent representation, so
  both GNU and musl builds compile and select backends consistently.

### Testing

- Installed-package lifecycle tests now exercise real APFS, Btrfs,
  reflink-enabled XFS, helper-backed OverlayFS, and ReFS environments.
- Deterministic race hooks verify that cleanup preserves concurrent files and
  rejects path substitution without modifying data outside managed state.
- Native allocation benchmarks cover normal checkout comparisons, concurrent
  shared-base readers, and an evaluation-only APFS bulk-clone candidate.

## 0.1.1 - 2026-09-12

The first public distribution includes the 0.1.0 foundation and the following
platform, lifecycle, and safety improvements.

### Added

- Native Linux reflink worktrees on Btrfs and reflink-enabled XFS, plus Windows
  ReFS block-clone worktrees with real-filesystem CI and allocation checks.
- Linux OverlayFS worktrees with caller-namespace capability checks, an
  explicitly installed least-privilege helper, crash-gap mount adoption, and
  explicit reboot recovery that preserves private changes.
- Repository compatibility preflight, custom state-directory discovery, and
  conservative repair of stale state registrations.
- Deterministic in-tree text, line-ending, and binary attribute support;
  unsupported filters, encodings, and external attributes remain fail-closed.

### Fixed

- Interrupted-add recovery preserves detached commits and other changed HEADs
  instead of removing a worktree whose files happen to be clean (#90).
- Repair skips live adds under per-operation locks and reloads journals after
  acquiring ownership, preventing concurrent rollback of an active creator (#93).
- Unicode normalization and case collisions are checked on the destination
  filesystem before durable state or Git metadata is created (#91).
- Fork-only OverlayFS probes release unrelated inherited file descriptors,
  preventing leaked mount and lock references (#92).
- Immutable bases are verified before reuse, checkout inputs stay pinned to
  the resolved tree, and cleanup preserves writes racing with Git removal.
- Journal snapshots, temporary files, state paths, Git pointers, and storage
  accounting reject unsafe or inconsistent inputs without deleting user data.

### Compatibility

- Concurrent add and repair processes must all use the lock-aware version.
  An older running binary cannot participate in the new ownership protocol.
- New verified base-cache buckets do not reuse bases from the older checkout
  profile. Important work should still be committed or backed up: Riftri remains
  experimental, and unsupported filesystems never silently fall back to copies.

## 0.1.0 - 2026-09-09

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
