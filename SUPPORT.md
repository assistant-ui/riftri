# Support

Riftri is experimental, pre-release software. Optimized mutation backends
currently support macOS on writable APFS volumes, Linux on Btrfs,
reflink-enabled XFS, and caller-visible OverlayFS mounts, and Windows on ReFS
volumes with block cloning. Other filesystems, including ordinary NTFS, are
not supported yet.

- For a reproducible bug, open a [bug report](https://github.com/assistant-ui/riftri/issues/new?template=bug_report.yml).
- For a scoped capability proposal, open a [feature request](https://github.com/assistant-ui/riftri/issues/new?template=feature_request.yml).
- For setup or usage help, open a [support question](https://github.com/assistant-ui/riftri/issues/new?template=question.yml).
- For a vulnerability or behavior that could delete, overwrite, expose, or
  cross-contaminate data, follow the private process in [SECURITY.md](SECURITY.md).

Before filing a public issue, run `riftri doctor --json` when possible and
remove credentials, proprietary repository contents, usernames, and sensitive
path components from the output.
