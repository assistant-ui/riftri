# Windows ReFS

Riftri uses ReFS block cloning on Windows. Your repository, new worktree,
and Riftri state must be on the **same writable ReFS volume**.
Ordinary NTFS is not supported.

## Create a worktree

After [installing Riftri](installation.md), run in PowerShell:

```powershell
riftri doctor --destination ..\app-auth
riftri worktree add ..\app-auth -b feature/auth main
```

Riftri verifies block cloning and private writes before creating the worktree.
A failed check stops creation rather than making an ordinary full checkout.

## Use an agent or terminal

[Coding agents](agents.md) work through `riftri exec`.
For ordinary Git commands in PowerShell, use [shell activation](activation.md).
Neither option edits your PowerShell profile.

## Limits

- Small files and file tails may need copying because ReFS clones whole clusters.
- Your Windows account needs permission to create any symlinks in the checkout.
- Unsupported hooks, filters, or repository features still block creation.

Riftri is experimental. Keep important work committed or backed up, and
follow [recovery guidance](troubleshooting.md) after an interruption.
