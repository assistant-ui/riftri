#!/usr/bin/env bash
# Standalone macOS/Linux installer. Keep all work inside main so a truncated
# piped download cannot execute a partially received installation procedure.
main() (
  set -euo pipefail
  umask 077
  export LC_ALL=C
  # User tar options must not change what is listed or streamed from the archive.
  unset TAR_OPTIONS

  fail() { printf 'riftri: %s\n' "$*" >&2; exit 1; }
  usage() {
    printf '%s\n' \
      'Usage: bash install.sh [VERSION]' \
      'Install the latest stable release, or pin a version such as v0.1.1.' \
      'RIFTRI_INSTALL_DIR: absolute destination directory (default: ~/.local/bin).' \
      'No sudo, shell profile edits, or Git activation. Requires curl, tar, and SHA-256 tooling.'
  }
  if [[ $# -eq 1 && ( $1 == --help || $1 == -h ) ]]; then usage; exit 0; fi
  [[ $# -le 1 ]] || fail 'Expected at most one version. Use --help for usage.'
  version=${1:-}
  validate_version() {
    [[ $1 =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]] ||
      fail 'Invalid version; expected a release version such as v0.1.1.'
  }
  if [[ $# -eq 1 ]]; then version=${version#v}; validate_version "$version"; fi
  install_dir=${RIFTRI_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}
  [[ $install_dir == /* ]] || fail 'RIFTRI_INSTALL_DIR must be an absolute path.'
  target="$install_dir/riftri"
  validate_target() {
    [[ ! -L $target && ( ! -e $target || -f $target ) ]] ||
      fail 'Install target must be a regular file, not a symlink or directory.'
  }
  validate_target

  for dependency in curl tar mktemp chmod mkdir cp mv rm uname; do
    command -v "$dependency" >/dev/null 2>&1 || fail "Required command not found: $dependency"
  done
  if command -v sha256sum >/dev/null 2>&1; then
    checksum_command=(sha256sum)
  elif command -v shasum >/dev/null 2>&1; then
    checksum_command=(shasum -a 256)
  else
    fail 'Install sha256sum or shasum before continuing.'
  fi
  system=$(uname -s)
  architecture=$(uname -m)
  case "$architecture" in
    arm64|aarch64) architecture=arm64 ;;
    x86_64|amd64) architecture=x64 ;;
    *) fail "Unsupported architecture: $architecture" ;;
  esac
  case "$system" in
    Darwin) platform="darwin-$architecture" ;;
    Linux)
      libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) ||
        fail 'Linux downloads require glibc; musl/Alpine is unsupported. See docs/install.md.'
      [[ $libc == glibc\ * ]] || fail 'Linux downloads require glibc.'
      platform="linux-$architecture-gnu"
      ;;
    *) fail "Unsupported system: $system. Windows users: use the PowerShell direct-download guide."
      ;;
  esac

  release_url=https://github.com/assistant-ui/riftri/releases
  download() {
    curl --disable --fail --silent --show-error --location \
      --proto '=https' --proto-redir '=https' \
      --connect-timeout 20 --max-time 300 --retry 2 "$@"
  }
  if [[ -z $version ]]; then
    latest=$(download --output /dev/null --write-out '%{url_effective}' "$release_url/latest") ||
      fail 'Could not resolve the latest GitHub release.'
    [[ $latest == "$release_url/tag/v"* ]] || fail 'Unexpected latest release URL.'
    version=${latest#"$release_url/tag/v"}
    validate_version "$version"
  fi
  archive="riftri-$platform-v$version.tar.gz"
  download_dir=$(mktemp -d "${TMPDIR:-/tmp}/riftri-install.XXXXXX")
  staged_file=
  cleanup() {
    if [[ -n $staged_file ]]; then rm -f -- "$staged_file"; fi
    # This exact directory was created by mktemp above; never clean the install directory.
    rm -r -- "$download_dir"
  }
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  printf 'Downloading Riftri v%s (%s)…\n' "$version" "$platform"
  download --output "$download_dir/SHA256SUMS" "$release_url/download/v$version/SHA256SUMS" ||
    fail 'Checksum download failed; existing installation is unchanged.'
  download --output "$download_dir/archive.tar.gz" "$release_url/download/v$version/$archive" ||
    fail 'Archive download failed; existing installation is unchanged.'

  expected=
  matches=0
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line =~ ^([0-9a-f]{64})\ \ (.+)$ && ${BASH_REMATCH[2]} == "$archive" ]]; then
      expected=${BASH_REMATCH[1]}
      matches=$((matches + 1))
    fi
  done < "$download_dir/SHA256SUMS"
  [[ $matches -eq 1 ]] || fail 'Expected exactly one matching SHA-256 checksum.'
  actual=$("${checksum_command[@]}" < "$download_dir/archive.tar.gz")
  [[ ${actual%% *} == "$expected" ]] || fail 'Archive checksum mismatch; stopping before extraction.'

  members=$(tar -tzf "$download_dir/archive.tar.gz") || fail 'Could not read archive.'
  [[ $members == riftri ]] || fail 'Unexpected archive contents; expected only riftri.'
  details=$(tar -tvzf "$download_dir/archive.tar.gz") || fail 'Could not inspect archive.'
  [[ $details == -* && $details != *$'\n'* ]] || fail 'Archive must contain one regular file.'
  # Stream the single validated member into our file; never let tar create paths or links.
  tar -xOzf "$download_dir/archive.tar.gz" riftri > "$download_dir/riftri" || fail 'Archive extraction failed.'
  chmod 755 "$download_dir/riftri"
  reported=$("$download_dir/riftri" --version) ||
    fail 'Downloaded binary cannot run. Check OS/runtime support and security policy; installation is unchanged.'
  [[ $reported == "riftri $version" ]] || fail 'Downloaded binary version does not match the release.'

  mkdir -p -- "$install_dir"
  # Stage on the destination filesystem so replacing an existing regular binary
  # is one rename, never a partial in-place copy. Recheck the target before commit.
  staged_file=$(mktemp "$install_dir/.riftri-install.XXXXXX")
  cp -- "$download_dir/riftri" "$staged_file"
  chmod 755 "$staged_file"
  validate_target
  mv -f -- "$staged_file" "$target"
  staged_file=
  printf 'Installed Riftri v%s to %s\n' "$version" "$target"
  printf 'Shell profiles and Git activation were not changed.\n'
  # Always show the explicit command: another Riftri may precede this one in PATH.
  printf 'To use this installation in your current Bash/Zsh shell:\n  export PATH=%q:"$PATH"\n  riftri --version\n' "$install_dir"
)

main "$@"
