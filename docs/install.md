# Installing Riftri

Riftri's standalone Rust executable does not need Node.js, npm, or a Rust
toolchain. Git must be installed. Download a published version from the
[official GitHub releases](https://github.com/assistant-ui/riftri/releases),
along with its `SHA256SUMS` file. Pin the version in the commands below to the
release you intend to install; do not mix files from different versions.

## Bash installer

On macOS (Apple Silicon or Intel) and Linux with glibc (ARM64 or x64):

```sh
curl -fsSL https://riftri.vercel.app/install.sh | bash
```

This selects the latest stable GitHub release once, downloads the matching
archive and `SHA256SUMS` from that exact tag over HTTPS, verifies its checksum,
and checks the executable's version before installing it to `~/.local/bin`.
Existing regular-file installations are replaced atomically after validation;
symlink and directory targets are refused. Failed downloads or validation leave
the existing binary unchanged. Temporary downloads are removed automatically.
No Node.js, npm, Rust toolchain, sudo, shell-profile edits, or Git activation
are involved. Git is still needed to use Riftri.

To pin a release:

```sh
curl -fsSL https://riftri.vercel.app/install.sh | bash -s -- v0.1.1
```

Piping a script to Bash executes code from that URL. To inspect it first,
download to a new directory, review it, and then run the saved file:

```sh
installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/riftri-installer-review.XXXXXX")
curl --fail --location --proto '=https' --proto-redir '=https' \
  https://riftri.vercel.app/install.sh \
  --output "$installer_dir/install.sh"
less "$installer_dir/install.sh"
# Run only after reviewing the downloaded script:
bash "$installer_dir/install.sh" v0.1.1
```

The website serves the canonical `package/install.sh` from its deployed source;
the script can change independently of the selected binary release. For a
reproducible script, download it from GitHub using a reviewed full Git commit ID:
`https://raw.githubusercontent.com/assistant-ui/riftri/<full-commit-id>/package/install.sh`.
SHA-256 checks detect corruption, not a separate signature or notarization;
trust still rests on the website, repository, and HTTPS downloads.

An absolute `RIFTRI_INSTALL_DIR` overrides the destination. Set it on **Bash**,
not on the `curl` side of the pipe:

```sh
curl -fsSL https://riftri.vercel.app/install.sh | RIFTRI_INSTALL_DIR="$HOME/tools/bin" bash
```

Follow the PATH command printed by the installer, then check `riftri --version`.
It never changes your current or future shells automatically. The Bash installer
does not support Windows shells or musl/Alpine; use the manual instructions below
for Windows. If the executable cannot run because of OS, runtime, or security
policy requirements, installation stops without bypassing that policy.

## Choose a native archive

For `v0.1.1`, the release assets are:

| System | Architecture | Archive |
| --- | --- | --- |
| macOS | Apple Silicon / ARM64 | `riftri-darwin-arm64-v0.1.1.tar.gz` |
| macOS | Intel / x64 | `riftri-darwin-x64-v0.1.1.tar.gz` |
| Linux with glibc | ARM64 / aarch64 | `riftri-linux-arm64-gnu-v0.1.1.tar.gz` |
| Linux with glibc | x64 / x86_64 | `riftri-linux-x64-gnu-v0.1.1.tar.gz` |
| Windows | ARM64 | `riftri-win32-arm64-v0.1.1.tar.gz` |
| Windows | x64 | `riftri-win32-x64-v0.1.1.tar.gz` |

Each archive contains only `riftri` on macOS/Linux or `riftri.exe` on Windows.
Linux archives target GNU/glibc, not musl/Alpine. If the executable reports a
missing runtime or incompatible glibc version, build from source in a supported
environment instead of treating it as a filesystem capability failure.

The checksum detects corrupted or mismatched downloads; it is not a separate
signature or a notarization claim. Obtain both files over HTTPS from the
expected repository. Stop if checksum verification fails.

## macOS and Linux

Choose `platform` from the table above, without the version and `.tar.gz`
suffix. These commands download into a new temporary directory and install or
replace only your per-user `~/.local/bin/riftri` executable. They do not use
`sudo`, edit a shell profile, or enable Riftri in any repository.

```sh
(
  set -eu
  version=0.1.1
  platform=riftri-darwin-arm64 # Change for your OS and architecture.
  archive="${platform}-v${version}.tar.gz"
  release_url="https://github.com/assistant-ui/riftri/releases/download/v${version}"
  download_dir=$(mktemp -d "${TMPDIR:-/tmp}/riftri-download.XXXXXX")
  cd "$download_dir"

  curl --fail --location --proto '=https' --proto-redir '=https' "$release_url/$archive" --output "$archive"
  curl --fail --location --proto '=https' --proto-redir '=https' "$release_url/SHA256SUMS" --output SHA256SUMS
  awk -v archive="$archive" '$2 == archive { print; found++ } END { if (found != 1) exit 1 }' SHA256SUMS > CHECKSUM
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum --check CHECKSUM
  else
    shasum -a 256 --check CHECKSUM
  fi

  test "$(tar -tzf "$archive")" = riftri
  tar -xzf "$archive"
  mkdir -p "$HOME/.local/bin"
  install -m 755 riftri "$HOME/.local/bin/riftri"
  "$HOME/.local/bin/riftri" --version
  printf 'Download files retained at %s\n' "$download_dir"
)
```

Run it by its full path, or explicitly add that directory to the current
shell's path:

```sh
export PATH="$HOME/.local/bin:$PATH"
riftri --version
```

For future shells, make any desired `PATH` change yourself using your usual
shell configuration. If macOS security policy blocks execution, stop and review
that policy; these instructions do not remove quarantine or disable Gatekeeper.

## Windows PowerShell

Use PowerShell with `tar` available. Choose `riftri-win32-x64` or
`riftri-win32-arm64` for `Platform`. These commands install or replace only the
executable under your user's local application-data directory, without
administrator access or persistent `PATH` changes.

```powershell
& {
  $ErrorActionPreference = 'Stop'
  $Version = '0.1.1'
  $Platform = 'riftri-win32-x64' # Use riftri-win32-arm64 for Windows ARM64.
  $Archive = "$Platform-v$Version.tar.gz"
  $ReleaseUrl = "https://github.com/assistant-ui/riftri/releases/download/v$Version"
  $DownloadDir = Join-Path ([IO.Path]::GetTempPath()) ("riftri-download-" + [guid]::NewGuid())
  New-Item -ItemType Directory -Path $DownloadDir | Out-Null
  $ArchivePath = Join-Path $DownloadDir $Archive
  $SumsPath = Join-Path $DownloadDir 'SHA256SUMS'
  Invoke-WebRequest -Uri "$ReleaseUrl/$Archive" -OutFile $ArchivePath
  Invoke-WebRequest -Uri "$ReleaseUrl/SHA256SUMS" -OutFile $SumsPath

  $Pattern = '^[0-9a-f]{64}  ' + [regex]::Escape($Archive) + '$'
  $Lines = @(Get-Content $SumsPath | Where-Object { $_ -cmatch $Pattern })
  if ($Lines.Count -ne 1) { throw 'Expected exactly one matching checksum' }
  $Expected = $Lines[0].Substring(0, 64)
  $Actual = (Get-FileHash -Algorithm SHA256 $ArchivePath).Hash.ToLowerInvariant()
  if ($Actual -cne $Expected) { throw 'Archive checksum mismatch; stopping' }

  $Members = @(& tar -tzf $ArchivePath)
  if ($LASTEXITCODE -ne 0 -or $Members.Count -ne 1 -or $Members[0] -cne 'riftri.exe') {
    throw 'Unexpected archive contents; stopping'
  }
  & tar -xzf $ArchivePath -C $DownloadDir
  if ($LASTEXITCODE -ne 0) { throw 'Archive extraction failed' }
  $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Riftri'
  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  $Executable = Join-Path $InstallDir 'riftri.exe'
  Copy-Item (Join-Path $DownloadDir 'riftri.exe') $Executable -Force
  & $Executable --version
  if ($LASTEXITCODE -ne 0) { throw 'Installed executable did not run successfully' }
  Write-Output "Download files retained at $DownloadDir"
}
```

Run the full path or opt into the current PowerShell session's `PATH`:

```powershell
$env:PATH = (Join-Path $env:LOCALAPPDATA 'Programs\Riftri') + ';' + $env:PATH
riftri --version
```

Installing the CLI on NTFS is fine, but it does **not** provide optimized
worktrees on NTFS. Windows worktree destinations need a supported ReFS volume
and a successful active block-cloning probe. Do not run Git or your agents as
administrator to work around an unsupported destination.

## Verify the repository and destination

Installation does not activate Git interception. From a real Git repository,
check a proposed worktree destination:

```console
$ riftri doctor --destination ../app-task
```

Use the capabilities documented for your selected release. Unsupported
filesystem or checkout features fail before mutation, without a silent
full-copy fallback. Continue with the [quick start](../README.md#quick-start)
only after reviewing the diagnostic. Repository-local `riftri enable` and
process-scoped or explicitly evaluated shell activation remain separate choices.

## npm is a separate channel

GitHub downloads and npm packages contain the same native CLI for a given tag,
but publication status is independent. As of September 12, 2026, the main
`riftri@0.1.1` npm launcher is not published: npm rejected its Windows ARM64
dependency after four macOS/Linux native packages were published. Use the
direct archive while that publication remains incomplete. A GitHub release
does not by itself mean `npx riftri` works.

## Updating and uninstalling

To update, select a new published version and repeat checksum verification and
installation. Review its release notes first; never replace files with an
unverified download. If multiple copies of Riftri are installed, check the
resolved executable (`command -v riftri` on Unix or `Get-Command riftri` on
PowerShell) and `riftri --version` to avoid running an older copy.

Before uninstalling the executable, finish managed worktree cleanup and disable
any activation you explicitly configured. Then remove only the installed
`~/.local/bin/riftri` or `%LOCALAPPDATA%\Programs\Riftri\riftri.exe` file and any
`PATH` entry you added. Do not delete Riftri state directories or private
worktree data as an uninstall shortcut.
