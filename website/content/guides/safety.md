# Safety

Riftri is experimental; keep important work committed or backed up.
Copy-on-write isolates file changes; it is **not a security sandbox**.

## What Riftri protects

- Unsupported storage or checkout stops creation — no silent full-copy fallback.
- Normal removal refuses local changes; forced removal is an explicit discard.
- Lifecycle operations are recorded for recovery after an interruption.
- Cleanup preserves changed or ambiguous paths; shared bases are collected
  only when unreferenced.

## What you control

- Use Riftri's lifecycle commands; never delete or edit its internal state
  directories.
- Review `riftri gc` before `riftri gc --apply`.
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
Full safety model and testing details: **Agent .md**.
