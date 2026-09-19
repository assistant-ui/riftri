# Troubleshooting

Start in the repository with the destination you want to use:

```sh
riftri doctor --destination ../app-auth
riftri status
```

Read the reported blocker before retrying. Riftri stops instead of silently
making a full copy.

## Command not found

Follow the PATH instructions printed by the installer, then run
`riftri --version`. On Linux, a runtime error can mean the wrong glibc/musl
binary; rerun the [installer](installation.md).

## Unsupported storage or checkout

Check [filesystem compatibility](filesystem-compatibility.md).
Custom filters, submodules, or checkout hooks can also block creation.
Do not disable a needed hook to make Riftri work. Use ordinary Git outside
Riftri interception when a repository is unsupported.

For missing LFS objects, see [Git LFS](git-lfs.md). For a partial checkout,
use [explicit sparse directories](sparse-checkout.md).

## Git is not using Riftri

```sh
riftri shell status
```

You need both repository enablement and an activated shell or process.
[Check activation](activation.md) in the same terminal or tool where the
command runs. Embedded Git libraries and absolute Git paths bypass the hook.

## An operation was interrupted

Follow the diagnostic's recovery command. With the default state location:

```sh
riftri repair
```

If you used a custom repository or state directory, keep those arguments.
Repair preserves changed or ambiguous paths for inspection. Do not delete
internal state directories to clear an error.

## Removal or cleanup is blocked

Commit or back up local changes before removing a worktree. Use `--force`
only if you intend to discard them. If `gc` reports a base still claimed by an
interrupted operation, inspect and repair that operation first.

Still stuck? [Open an issue](https://github.com/assistant-ui/riftri/issues)
with your OS, `riftri --version`, and the diagnostic output. Review output
for private paths or information before sharing it.
