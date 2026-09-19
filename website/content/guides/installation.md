# Installing Riftri

You need Git. Riftri is a native executable; no Node.js or Rust toolchain is required.

## macOS and Linux

```sh
curl -fsSL https://riftri.dev/install.sh | bash
```

This runs the installer from the website. [Read the script](../../../package/install.sh) first
if you want to review it. It checks the download and installs to `~/.local/bin`.
Follow the printed PATH command so your terminal can find `riftri`.

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

Check the installation, then start setup **inside an existing Git repository**:

```sh
riftri --version
riftri setup
```

Setup checks your destination, asks for a branch, and shows a plan before
creating anything. It can open an installed coding agent when finished.

## Update or install manually

Rerun the installer to update. For a specific version, manual downloads, or
uninstall steps, open the **Agent .md** reference at the top of this page.
[GitHub releases](https://github.com/assistant-ui/riftri/releases) also provide
standalone binaries. The npm channel is not currently available.

Neither installer edits your shell profile or enables Git interception.
If something fails, see [troubleshooting](troubleshooting.md).
