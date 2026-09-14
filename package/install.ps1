[CmdletBinding()]
param(
  [Parameter(Position = 0)]
  [string]$Version,

  [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-RiftriInstaller {
  param([string]$RequestedVersion)

  $releaseUrl = 'https://github.com/assistant-ui/riftri/releases'
  $downloadDirectory = $null
  $stagedFile = $null
  $backupFile = $null

  function Fail([string]$Message) {
    throw $Message
  }

  function Assert-HttpsResponse($Response, [string]$RequestedUri) {
    $finalUri = $null
    if ($null -ne $Response -and $null -ne $Response.BaseResponse) {
      if ($Response.BaseResponse.PSObject.Properties.Name -contains 'ResponseUri') {
        $finalUri = $Response.BaseResponse.ResponseUri
      }
      elseif (
        $Response.BaseResponse.PSObject.Properties.Name -contains 'RequestMessage' -and
        $null -ne $Response.BaseResponse.RequestMessage
      ) {
        $finalUri = $Response.BaseResponse.RequestMessage.RequestUri
      }
    }
    if ($null -ne $finalUri -and $finalUri.Scheme -ne 'https') {
      Fail "Download redirected away from HTTPS: $RequestedUri"
    }
  }

  function Download([string]$Uri, [string]$Destination) {
    $response = Invoke-WebRequest -Uri $Uri -OutFile $Destination -UseBasicParsing
    Assert-HttpsResponse $response $Uri
  }

  function Assert-InstallTarget([string]$Target) {
    $item = Get-Item -LiteralPath $Target -Force -ErrorAction SilentlyContinue
    if ($null -eq $item) {
      return
    }
    if (
      $item.PSIsContainer -or
      -not ($item -is [System.IO.FileInfo]) -or
      (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)
    ) {
      Fail 'Install target must be a regular file, not a symlink, reparse point, or directory.'
    }
  }

  try {
    if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
      [System.Runtime.InteropServices.OSPlatform]::Windows
    )) {
      Fail 'This installer supports Windows only. Use install.sh on macOS or Linux.'
    }

    if ([string]::IsNullOrWhiteSpace($RequestedVersion)) {
      $latestResponse = Invoke-WebRequest -Uri "$releaseUrl/latest" -UseBasicParsing
      Assert-HttpsResponse $latestResponse "$releaseUrl/latest"
      $latestUri = if ($latestResponse.BaseResponse.PSObject.Properties.Name -contains 'ResponseUri') {
        $latestResponse.BaseResponse.ResponseUri.AbsoluteUri
      }
      else {
        $latestResponse.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
      }
      if ($latestUri -notmatch '^https://github\.com/assistant-ui/riftri/releases/tag/v(.+)$') {
        Fail 'Could not resolve a valid latest GitHub release.'
      }
      $RequestedVersion = $Matches[1]
    }

    $Version = $RequestedVersion.TrimStart('v')
    if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') {
      Fail 'Invalid version; expected a release version such as v0.2.1.'
    }

    $Architecture = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
      'X64' { 'x64' }
      'Arm64' { 'arm64' }
      default { Fail "Unsupported Windows architecture: $_" }
    }

    $InstallDir = if ([string]::IsNullOrWhiteSpace($env:RIFTRI_INSTALL_DIR)) {
      if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        Fail 'LOCALAPPDATA is required when RIFTRI_INSTALL_DIR is not set.'
      }
      Join-Path $env:LOCALAPPDATA 'Programs\Riftri'
    }
    else {
      $env:RIFTRI_INSTALL_DIR
    }
    if (-not [System.IO.Path]::IsPathRooted($InstallDir)) {
      Fail 'RIFTRI_INSTALL_DIR must be an absolute path.'
    }
    $InstallDir = [System.IO.Path]::GetFullPath($InstallDir)
    $target = Join-Path $InstallDir 'riftri.exe'
    Assert-InstallTarget $target

    $archive = "riftri-win32-$Architecture-v$Version.tar.gz"
    $assetUrl = "$releaseUrl/download/v$Version"
    $downloadDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("riftri-install-" + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $downloadDirectory)
    $archivePath = Join-Path $downloadDirectory $archive
    $checksumPath = Join-Path $downloadDirectory 'SHA256SUMS'

    Write-Output "Downloading Riftri v$Version (win32-$Architecture)..."
    Download "$assetUrl/SHA256SUMS" $checksumPath
    Download "$assetUrl/$archive" $archivePath

    $escapedArchive = [regex]::Escape($archive)
    $matchingChecksums = @(Get-Content -LiteralPath $checksumPath | Where-Object {
      $_ -cmatch "^[0-9a-f]{64}  $escapedArchive$"
    })
    if ($matchingChecksums.Count -ne 1) {
      Fail 'Expected exactly one matching SHA-256 checksum.'
    }
    $expected = $matchingChecksums[0].Substring(0, 64)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
      $archiveStream = [System.IO.File]::Open(
        $archivePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
      )
      try {
        $actualBytes = $sha256.ComputeHash($archiveStream)
      }
      finally {
        $archiveStream.Dispose()
      }
    }
    finally {
      $sha256.Dispose()
    }
    $actual = ([System.BitConverter]::ToString($actualBytes)).Replace('-', '').ToLowerInvariant()
    if ($actual -cne $expected) {
      Fail 'Archive checksum mismatch; existing installation is unchanged.'
    }

    $members = @(& tar.exe -tzf $archivePath)
    if ($LASTEXITCODE -ne 0 -or $members.Count -ne 1 -or $members[0] -cne 'riftri.exe') {
      Fail 'Unexpected archive contents; expected only riftri.exe.'
    }
    $details = @(& tar.exe -tvzf $archivePath)
    if ($LASTEXITCODE -ne 0 -or $details.Count -ne 1 -or $details[0] -notmatch '^-') {
      Fail 'Archive must contain one regular file.'
    }
    & tar.exe -xzf $archivePath -C $downloadDirectory
    if ($LASTEXITCODE -ne 0) {
      Fail 'Archive extraction failed.'
    }
    $downloadedBinary = Join-Path $downloadDirectory 'riftri.exe'
    $downloadedItem = Get-Item -LiteralPath $downloadedBinary -Force
    if (
      -not ($downloadedItem -is [System.IO.FileInfo]) -or
      (($downloadedItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)
    ) {
      Fail 'Extracted binary is not a regular file.'
    }
    $reported = ((& $downloadedBinary --version) | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $reported -cne "riftri $Version") {
      Fail 'Downloaded binary could not run or reported a different release version.'
    }

    [void](New-Item -ItemType Directory -Path $InstallDir -Force)
    $stagedFile = Join-Path $InstallDir (".riftri-install-" + [guid]::NewGuid().ToString('N') + '.tmp')
    [System.IO.File]::Copy($downloadedBinary, $stagedFile, $false)
    Assert-InstallTarget $target
    if ([System.IO.File]::Exists($target)) {
      $backupFile = Join-Path $InstallDir (".riftri-backup-" + [guid]::NewGuid().ToString('N') + '.tmp')
      [System.IO.File]::Replace($stagedFile, $target, $backupFile, $true)
      $stagedFile = $null
      Remove-Item -LiteralPath $backupFile -Force
      $backupFile = $null
    }
    else {
      [System.IO.File]::Move($stagedFile, $target)
      $stagedFile = $null
    }

    Write-Output "Installed Riftri v$Version to $target"
    Write-Output 'PowerShell profiles, persistent PATH, and Git activation were not changed.'
    $escapedInstallDir = $InstallDir.Replace("'", "''")
    Write-Output "To use this installation in the current PowerShell session:"
    Write-Output "  `$env:PATH = '$escapedInstallDir;' + `$env:PATH"
    Write-Output '  riftri --version'
  }
  finally {
    if ($null -ne $stagedFile -and [System.IO.File]::Exists($stagedFile)) {
      Remove-Item -LiteralPath $stagedFile -Force
    }
    if ($null -ne $backupFile -and [System.IO.File]::Exists($backupFile)) {
      Remove-Item -LiteralPath $backupFile -Force
    }
    if ($null -ne $downloadDirectory -and [System.IO.Directory]::Exists($downloadDirectory)) {
      Remove-Item -LiteralPath $downloadDirectory -Recurse -Force
    }
  }
}

if ($Help) {
  Write-Output 'Usage: powershell -File install.ps1 [-Version VERSION]'
  Write-Output 'Installer: https://riftri.dev/install.ps1'
  Write-Output 'Installs the latest stable release, or a pinned version such as v0.2.1.'
  Write-Output 'RIFTRI_INSTALL_DIR overrides the per-user destination directory.'
  exit 0
}

try {
  Invoke-RiftriInstaller $Version
}
catch {
  [Console]::Error.WriteLine("riftri: $($_.Exception.Message)")
  exit 1
}
