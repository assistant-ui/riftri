# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

### Changed

- `riftri state unregister <PATH>` replaces the displayed `state forget-missing`
  command. The old name remains a hidden compatibility alias, and existing paths
  still cannot be unregistered. Help, completions, and generated man pages use
  the new name.

### Fixed

- Managed removal, forced removal, and compaction reject worktrees with an
  incomplete move journal until repair completes the move.
- State unregistration accepts relative parent components such as `../old-state`
  and resolves existing parent aliases when matching a missing registered path,
  without deleting files or unregistering existing paths.
- The website storage diagram shows the full backend status when text wraps
  near the mobile layout breakpoint.
- The static website preview serves `robots.txt` as plain text and `sitemap.xml`
  as XML instead of `application/octet-stream`.

## 0.2.3 - 2026-09-16

This release changes documentation, tests, and tooling only. The CLI itself is
unchanged since 0.2.2: no behaviour, output, or on-disk format differs, and
upgrading is optional.

### Added

- `docs/build-caches.md` explains which dependency stores are safe to share
  across parallel worktrees and which working directories never are, since
  untracked dependency and build directories are outside the Git tree and are
  therefore never shared by copy-on-write.
- A Homebrew formula at `Formula/riftri.rb`, generated from a release's
  published checksums by `package/scripts/update-homebrew-formula.mjs`, with a
  scheduled check that fails when it falls behind the latest release or pins a
  checksum the release does not publish.
- Test coverage for the destination path preflight across non-ASCII case
  folding, both Unicode normalization forms, and colliding directory
  components, verified against the behaviour of the destination volume itself.

### Changed

- The README is roughly half its previous length; reference material it
  duplicated now lives in the `docs/` pages that own it.
- `docs/filesystem-compatibility.md` documents the destination path preflight
  and its scope instead of deferring case folding and normalization to a
  future compatibility slice.

### Fixed

- The Markdown link checker no longer deadlocks pull requests that add a
  document, which previously could not link to themselves at `main` until after
  merging.

## 0.2.2 - 2026-09-16

### Added

- Checkout-neutral GitHub linguist metadata attributes (`linguist-generated`,
  `linguist-vendored`, `linguist-documentation`, `linguist-detectable`, and
  `linguist-language`) no longer block optimized worktree creation, so stock
  repositories using lockfile display hints work without edits.
- Optimized explicit and intercepted worktree adds can attach an existing local
  branch while preserving Git's branch-in-use checks and Riftri's exact-commit
  transaction boundary.
- `riftri worktree list` reports active managed worktrees in concise human or
  versioned JSON form with Git identity, backend, base, and allocation details.
- A checksum-verifying PowerShell installer provides profile-free per-user
  installation for Windows x64 and ARM64.
- Canonical Git LFS checkouts accept strict v1 pointers backed by verified
  objects already present in the default local LFS store.
- Snapshot-guarded forced managed removal safely supports intentional discard
  while preserving any changes made after force intent is recorded.
- Explicit compaction recreates pristine native-COW views to release retained
  private blocks without changing their Git worktree registration or HEAD.
- Explicitly evaluated PowerShell activation and deactivation provide the same
  repository-gated normal Git interception as sh, bash, and zsh without editing
  PowerShell profiles or persistent `PATH`.
- Shell completion generation, generated man pages, and a Homebrew formula
  generator backed by checksummed native release archives.
- Stable JSON success reports for lifecycle commands, binary-unit byte counts,
  and a `--branch` alias for explicit adds.
- CLI confirmation prompts, distinct error exit codes, and practical help
  examples; documentation now includes a CLI reference, troubleshooting,
  comparisons, and agent integration guidance.

### Fixed

- Website animation controls now keep their accessible Pause/Resume labels in
  sync, clipboard feedback restarts on every copy, and keyboard navigation
  moves focus to the requested section.
- The local static preview now follows branded 404 routes and file overrides,
  while preserving response headers and rejecting paths outside its output.
- Improved website text contrast, Windows installation instructions, sharing
  metadata, and inline access to the Markdown guide at `riftri.dev/index.md`.

### Compatibility

- The redundant `riftri recover` command was removed; use `riftri repair` for
  repository-aware recovery. Scripts should also account for the documented
  distinct error exit codes. Destructive confirmations apply only in terminals;
  `--yes` explicitly skips those prompts.

### Release

- Native GitHub release assets include signed build-provenance attestations.
  npm publication remains paused pending registry review; standalone downloads
  and the Bash and PowerShell installers remain available.
- CI now checks the minimum Rust version, dependency policy, code coverage,
  Markdown links, browser interactions, and public deployment contents.
- Production website verification checks the deployed revision, installers,
  Markdown response, sharing assets, and branded error page.

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
