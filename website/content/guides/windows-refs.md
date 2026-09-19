# Windows ReFS

Riftri uses ReFS block cloning. Repository, new worktree, and Riftri state
must share the **same writable ReFS volume**; ordinary NTFS is not supported.

## Create a worktree

After [installing Riftri](installation.md), in PowerShell:

```powershell
riftri doctor --destination ..\app-auth
riftri worktree add ..\app-auth -b feature/auth main
```

Riftri verifies block cloning and private writes first; a failed check stops
creation rather than making a full checkout.

## Use an agent or terminal

[Coding agents](agents.md) work through `riftri exec`; for ordinary Git
commands use [shell activation](activation.md). Neither edits your
PowerShell profile.

## Limits

- Small files and file tails may be copied; ReFS clones whole clusters.
- Your account needs permission to create any symlinks in the checkout.
- Unsupported hooks, filters, or repository features still block creation.

Experimental: keep important work committed, and follow
[recovery guidance](troubleshooting.md) after an interruption.
