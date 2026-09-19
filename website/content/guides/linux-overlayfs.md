# Linux OverlayFS

On Linux without reflinks, Riftri can share a read-only base through OverlayFS.
Each worktree stores its own changes in a private layer.

## Check support

```sh
riftri doctor --destination ../app-auth
```

Riftri prefers reflinks. OverlayFS is selected only if it can create a usable
mount in your environment. Containers and restricted shells may not allow it.

## Optional mount helper

If the diagnostic calls for a helper, an administrator can install it once:

```sh
sudo riftri overlayfs install-helper
```

This installs a privileged, system-wide mount helper. Review the **Agent .md**
reference before installing it. It handles validated mount operations, not
your Git commands or agent. Repository enablement remains separate.

Then create a worktree normally:

```sh
riftri worktree add ../app-auth -b feature/auth main
```

## After a reboot

Mounts disappear at reboot; the stored private changes remain. From the
repository, run:

```sh
riftri repair
```

Keep any custom `--state-dir` argument. Repair can restore the mount without
rebuilding its private changes. It stops if the path or mount identity is ambiguous.

Mounted moves and compaction are not supported. Use Riftri to remove managed
worktrees; do not delete their backing layers manually.
