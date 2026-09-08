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

### Changed

- Milestone 2 now permits only explicit APFS worktree creation. Unsupported
  checkout configurations fail before mutation, and full-copy fallback remains
  disabled.

### Safety

- Failed adds roll back real Git metadata and their newly created branch when it
  has not moved.
- Recovery preserves incomplete views that are dirty or no longer match their
  immutable base.
