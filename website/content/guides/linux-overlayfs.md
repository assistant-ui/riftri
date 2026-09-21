# Linux OverlayFS

Without reflinks, Riftri can share a read-only base through OverlayFS; each
worktree stores its changes in a private layer.

## Check support

```sh
riftri doctor --destination ../app-auth
```

Riftri prefers reflinks and selects OverlayFS only if it can create a usable
mount. Containers and restricted shells may not allow it.

## Optional mount helper

If the diagnostic calls for it, an administrator installs the helper once:

```sh
sudo riftri overlayfs install-helper
```

The helper is privileged and system-wide — review **View .md** before
installing. It only performs validated mount operations; repository
enablement stays separate. Then create worktrees normally:

```sh
riftri worktree add ../app-auth -b feature/auth main
```

## After a reboot

Mounts disappear; stored private changes remain. From the repository, keeping
any custom `--state-dir` argument:

```sh
riftri repair
```

Repair restores the mount without rebuilding private changes and stops if the
path or mount identity is ambiguous.

Mounted moves and compaction are unsupported. Remove managed worktrees with
Riftri; never delete their backing layers manually.
