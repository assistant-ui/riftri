# Security policy

Riftri is experimental, pre-release software. It performs opt-in worktree
creation, removal, repair, and garbage-collection mutations on supported
macOS/APFS, Linux reflink and OverlayFS, and Windows/ReFS destinations. The
latest tagged release and `main` receive security fixes; older experimental
releases may require upgrading.

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
