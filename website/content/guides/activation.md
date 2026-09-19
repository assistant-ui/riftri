# Shell activation

Use this if you want ordinary `git worktree` commands to use Riftri in your
terminal. For just one agent session, [use `riftri exec`](agents.md) instead.

## Turn it on

In zsh:

```sh
eval "$(riftri shell hook zsh)"
riftri enable
```

Replace `zsh` with `bash` or `sh` for those shells. In PowerShell:

```powershell
Invoke-Expression ((riftri shell hook powershell) -join [Environment]::NewLine)
riftri enable
```

Run `riftri enable` in the repository you want to opt in. The hook affects
this shell and its children; enablement is saved for this repository and its
linked worktrees. **Both are needed.**

```sh
git worktree add ../app-auth -b feature/auth main
```

Other Git commands still go to Git. Riftri does not edit your shell profile.

## Check or turn it off

```sh
riftri shell status
riftri disable
```

`disable` turns off the repository opt-in. To also remove interception from
the current zsh session:

```sh
eval "$(riftri shell deactivate zsh)"
```

In PowerShell:

```powershell
Invoke-Expression ((riftri shell deactivate powershell) -join [Environment]::NewLine)
```

Use the matching shell name for Bash or sh. If you added a hook to your
profile yourself, remove that line to keep it off in future terminals.
