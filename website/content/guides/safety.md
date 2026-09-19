# Safety

Riftri is experimental. Keep important work committed or backed up.
Copy-on-write isolates file changes; it is **not a security sandbox**.

## What Riftri protects

- Unsupported storage or checkout features stop creation; there is no silent full-copy fallback.
- Normal removal refuses local changes. Forced removal is an explicit discard.
- Lifecycle operations are recorded for recovery after an interruption.
- Cleanup preserves changed or ambiguous paths instead of guessing they are disposable.
- Shared bases are collected only when no operation still needs them.

## What you control

Use Riftri's lifecycle commands for managed worktrees. Do not delete or edit
its internal state directories. Review `riftri gc` before applying cleanup
with `riftri gc --apply`.

Git interception is opt-in for both the repository and the shell or process.
Riftri does not edit your shell profile. The optional Linux OverlayFS helper
requires a separate administrator installation.

## If something goes wrong

```sh
riftri status
```

Follow the reported recovery guidance, retaining any custom repository or
state-directory arguments. See [troubleshooting](troubleshooting.md).

Report potential data-loss or security problems privately using the
[security policy](https://github.com/assistant-ui/riftri/blob/main/SECURITY.md).
Testing details and the full safety model remain in **Agent .md**.
