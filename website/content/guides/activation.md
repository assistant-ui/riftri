# Shell activation

Makes ordinary `git worktree` commands use Riftri in your terminal. For one
agent session, [use `riftri exec`](agents.md) instead.

## Turn it on

In zsh (swap `zsh` for `bash` or `sh`):

```sh
eval "$(riftri shell hook zsh)"
riftri enable
```

In PowerShell:

```powershell
Invoke-Expression ((riftri shell hook powershell) -join [Environment]::NewLine)
riftri enable
```

Run `riftri enable` in the repository to opt it in. The hook covers this
shell and its children; enablement is saved for the repository and its linked
worktrees. **Both are needed.**

```sh
git worktree add ../app-auth -b feature/auth main
```

Other Git commands still go to Git. Riftri never edits your shell profile.

## Check or turn it off

```sh
riftri shell status
riftri disable
```

`disable` removes the repository opt-in. To also deactivate the current
session:

```sh
eval "$(riftri shell deactivate zsh)"
```

```powershell
Invoke-Expression ((riftri shell deactivate powershell) -join [Environment]::NewLine)
```

If you added the hook to your profile yourself, remove that line too.
