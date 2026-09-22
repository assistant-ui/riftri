# Installing Riftri

Requires Git. Riftri is a native executable; no Node.js or Rust toolchain.

## macOS and Linux

```sh
curl -fsSL https://riftri.dev/install.sh | bash
```

Verifies the download and installs to `~/.local/bin`
([read the script](../../../package/install.sh) first if you want).
Follow the printed PATH command.

## Windows

Download and review the PowerShell installer:

```powershell
$Installer = Join-Path $env:TEMP 'riftri-install.ps1'
Invoke-WebRequest https://riftri.dev/install.ps1 -OutFile $Installer
Get-Content $Installer
```

Then run it:

```powershell
& $Installer
```

Follow the printed PATH instructions. Optimized worktrees require
[ReFS](windows-refs.md); ordinary NTFS is not supported.

## Create your first worktree

Inside an existing Git repository:

```sh
riftri --version
riftri setup
```

## Update or install manually

Rerun the installer to update. Pinned versions, manual downloads, and
uninstall steps: **View .md** at the top of this page.
[GitHub releases](https://github.com/assistant-ui/riftri/releases) provides
standalone binaries, and `npm install --global riftri` fetches the one for
your platform. Windows on ARM64 is the exception: its platform package is
still in registry review, so use the PowerShell installer there.

Neither installer edits your shell profile or enables Git interception.
Problems? See [troubleshooting](troubleshooting.md).
