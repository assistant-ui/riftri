# Safety

Riftri is experimental; keep important work committed or backed up.
Copy-on-write isolates file changes; it is **not a security sandbox**.

Riftri deletes directories and, on Linux, can install a root-owned mount
helper, so it is built to refuse rather than guess. When an operation cannot
be performed safely and exactly, it stops with an explanation instead of
degrading to something approximate.

## What Riftri protects

- Unsupported storage or checkout stops creation — no silent full-copy
  fallback. Sparse checkout, submodules, custom filters, and non-canonical
  Git LFS setups are refused before any state exists.
- Normal removal refuses local changes; forced removal is an explicit
  discard that records an exact content snapshot first and stops if the
  worktree changed after that intent.
- Lifecycle operations are recorded for recovery after an interruption, so a
  killed process leaves a resumable journal rather than half-made state.
- Cleanup preserves changed or ambiguous paths; shared bases are collected
  only when unreferenced.
- Immutable bases are read-only, so a stray delete fails instead of quietly
  succeeding.

## Verified on real filesystems

Copy-on-write cannot be tested where it does not exist. CI builds a
disposable real volume per backend — an APFS image on macOS, loop-mounted
Btrfs and reflink XFS on Linux, a ReFS VHDX on Windows — and runs the
lifecycle and crash-recovery suites there, rather than on a runner's default
filesystem where only the refusal paths would ever execute.

## What you control

- Use Riftri's lifecycle commands; never delete or edit its internal state
  directories.
- Review `riftri gc` before `riftri gc --apply`. Destructive commands confirm
  on a terminal; `--yes` skips that for scripts.
- Interception is opt-in per repository and per shell or process; your shell
  profile is never edited.
- The Linux OverlayFS helper needs a separate administrator installation.

## If something goes wrong

```sh
riftri status
```

Follow its recovery guidance, keeping any custom repository or
state-directory arguments; see [troubleshooting](troubleshooting.md).
Report potential data-loss or security problems privately via the
[security policy](https://github.com/assistant-ui/riftri/blob/main/SECURITY.md).
