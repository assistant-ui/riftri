# Troubleshooting

Start in the repository with your destination:

```sh
riftri doctor --destination ../app-auth
riftri status
```

Read the reported blocker before retrying; Riftri stops rather than silently
making a full copy.

## Command not found

Follow the installer's printed PATH instructions, then run `riftri --version`.
On Linux, a runtime error can mean the wrong glibc/musl binary; rerun the
[installer](installation.md).

## Unsupported storage or checkout

Check [filesystem compatibility](filesystem-compatibility.md). Custom
filters, submodules, or checkout hooks also block creation; do not disable a
needed hook — use ordinary Git outside interception. Missing LFS objects:
[Git LFS](git-lfs.md). Partial checkout: [sparse directories](sparse-checkout.md).

## Git is not using Riftri

```sh
riftri shell status
```

You need both repository enablement and an [activated shell or
process](activation.md) in the same terminal or tool. Embedded Git libraries
and absolute Git paths bypass the hook.

## An operation was interrupted

Follow the diagnostic's recovery command. With the default state location:

```sh
riftri repair
```

Keep any custom repository or state-directory arguments. Repair preserves
changed or ambiguous paths. Never delete internal state directories to clear
an error.

## Removal or cleanup is blocked

Commit or back up local changes first; `--force` discards them. If `gc`
reports a base claimed by an interrupted operation, repair that operation
first.

Still stuck? [Open an issue](https://github.com/assistant-ui/riftri/issues)
with your OS, `riftri --version`, and the diagnostic output (review it for
private paths first).
