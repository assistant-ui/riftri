# Security policy

Riftri is experimental, pre-release software. It performs opt-in worktree
creation, removal, repair, and garbage-collection mutations on supported
macOS/APFS volumes. Until the first tagged release, only the `main` branch
receives security fixes. After releases begin, the latest tagged release and
`main` will receive fixes; older prereleases may require upgrading.

Do not open a public issue for vulnerabilities or behavior that could delete,
overwrite, expose, or cross-contaminate worktree data. Use GitHub's private
vulnerability reporting at:

<https://github.com/assistant-ui/riftri/security/advisories/new>

Include the Riftri version or commit, operating system, filesystem, Git version,
affected paths with sensitive components redacted, and a minimal reproduction.
Do not attach real credentials or proprietary repository contents.

Riftri's copy-on-write isolation is not a security sandbox. Untrusted processes
must still run inside an appropriate container, VM, microVM, or operating-system
sandbox.
