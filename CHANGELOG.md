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

### Safety

- No worktree mutation, mount, interception, or destructive command is active.
