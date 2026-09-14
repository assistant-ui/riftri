# Support

Riftri is experimental, pre-release software. The optimized mutation backend
currently supports writable APFS volumes on macOS, Btrfs and reflink-enabled
XFS volumes on Linux, validated Linux OverlayFS mounts, and ReFS volumes on
Windows. Every destination must pass an active or strict backend capability
check; unsupported filesystems stop before mutation without silently creating a
full-copy worktree.

- For a reproducible bug, open a [bug report](https://github.com/assistant-ui/riftri/issues/new?template=bug_report.yml).
- For a scoped capability proposal, open a [feature request](https://github.com/assistant-ui/riftri/issues/new?template=feature_request.yml).
- For setup or usage help, open a [support question](https://github.com/assistant-ui/riftri/issues/new?template=question.yml).
- For a vulnerability or behavior that could delete, overwrite, expose, or
  cross-contaminate data, follow the private process in [SECURITY.md](SECURITY.md).

Before filing a public issue, run `riftri doctor --json` when possible and
remove credentials, proprietary repository contents, usernames, and sensitive
path components from the output.
