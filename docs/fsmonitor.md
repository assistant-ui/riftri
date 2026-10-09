# Optional Git FSMonitor

Git's built-in FSMonitor can speed up repeated `git status` checks in large
Riftri worktrees. It is a Git feature, already honored by Riftri, not a new
storage backend or a Riftri watcher. Riftri does not enable it automatically.

## When it helps

A local macOS/APFS experiment with Riftri 0.6.4 measured three-edit checks at
66.7 ms without FSMonitor and 46.6 ms with it on 20,004 tracked files: about
30% less time. On 4,100 files it was slower (23.5 versus 29.3 ms), and cached
creation did not improve (1.36 versus 1.40 seconds). These are synthetic p50
measurements, not a universal file-count threshold or an application benchmark.
See the [raw samples and method](benchmarks/fsmonitor-2026-10-08.md).

FSMonitor helps Git skip metadata checks on unchanged paths **after** the
initial scan. It does not eliminate that scan, immutable-base verification,
index creation, or journal writes. It adds a background Git process per watched
worktree. The untracked cache showed no additional benefit in this experiment;
this guide leaves it unchanged.

## Try one existing worktree first

The tested profile is local macOS/APFS. Other platforms and Git builds need
their own validation; storage-backend support alone does not prove FSMonitor
support. Use current Git versions in both your terminal and IDE. Old Git can
misinterpret boolean `core.fsmonitor` values; see
[Git's compatibility warning](https://git-scm.com/docs/git-config#Documentation/git-config.txt-corefsmonitor).
Do not bypass Git's network-filesystem restrictions.

Inspect existing settings before overriding them, especially a custom watcher:

```sh
git -C ../task-1 --version
git -C ../task-1 config --show-origin --get-all core.fsmonitor
git -C ../task-1 -c core.fsmonitor=true status --short
git -C ../task-1 -c core.fsmonitor=true fsmonitor--daemon status
```

An unset config key returns exit 1. The last command must succeed: otherwise do
not persist the option. The `-c` trial changes no configuration file, but it can
start Git's daemon and refresh its index. Compare repeated checks, excluding
the first warm-up. If it is not useful, stop the trial watcher:

```sh
git -C ../task-1 -c core.fsmonitor=true fsmonitor--daemon stop
```

## Persist only with explicit consent

To enable it for **this repository, including its linked worktrees**:

```sh
git -C ../task-1 config --local core.fsmonitor true
git -C ../task-1 status --short
```

`--local` is not limited to `../task-1`. It normally writes the shared repository
configuration and affects future worktrees too. No global setting is needed.
This opt-in is independent of `riftri enable`: that controls worktree
interception, not FSMonitor. Ordinary Git commands inside `riftri exec` or a
Riftri shell hook honor the same setting without extra flags.

For one worktree only, `git config --worktree core.fsmonitor true` is appropriate
**only if** `git config --bool --get extensions.worktreeConfig` already reports
`true`. Without that extension, `--worktree` falls back to shared local config.
Do not blindly enable the extension in an existing repository: `core.bare`,
`core.worktree`, and sparse settings may need migration. Follow
[Git's worktree configuration instructions](https://git-scm.com/docs/git-worktree#_configuration_file).

## Turn it off and stop watchers

If you used the shared repository option above:

```sh
git -C ../task-1 config --local core.fsmonitor false
git -C ../task-1 -c core.fsmonitor=true fsmonitor--daemon stop
git -C ../task-1 status --short
```

Stop the daemon in each worktree where you started one, including the main
checkout. Git's `stop` can report an error when none is running; `status`
distinguishes that from a running watcher. Stop only the selected repository's
watchers, not processes found by name. If using worktree-scoped configuration,
set `--worktree core.fsmonitor false` there instead; inspect `--show-origin`
again if a more specific setting overrides your change. Restore any prior
custom configuration deliberately, rather than discarding it.

Stopping alone is temporary: Git starts the daemon again if the setting remains
enabled. `riftri disable` and ending `riftri exec` do not undo this separate Git
opt-in or stop Git-owned watchers. Before retiring a watched worktree, opt it
out and stop its watcher, then use normal Riftri removal. Dirty-worktree
protection remains unchanged.

For a one-command comparison or troubleshooting without FSMonitor and the
untracked cache:

```sh
git -C ../task-1 --no-optional-locks -c core.fsmonitor=false -c core.untrackedCache=false status --porcelain=v1 -z --untracked-files=all
```

This still uses Git's ordinary index stat cache; it does not hash every tracked
file and is not a byte-integrity audit. In particular, some Git builds can miss
a same-size edit made within the same ctime second when the file's older mtime
is deliberately restored, with or without FSMonitor. The same-size edit tests
separate ctime seconds so they exercise watcher invalidation rather than this
unrelated Git stat-cache limitation.

The macOS integration test covers explicit/intercepted creation, edits,
deletions, renames, modes, symlinks, untracked files, watcher restart, isolation,
dirty-removal refusal, and opt-out. It also warms the watcher before explicit
and intercepted managed moves and before native-COW compaction replaces the
root inode, then checks tracked and nested untracked edits with the untracked
cache both off and on. Dirty removal/compaction must refuse, peers and bases
must remain unchanged, and final removal/GC must leave no managed state.
This coverage does not certify every Git version or platform.
[Git's FSMonitor manual](https://git-scm.com/docs/git-fsmonitor--daemon) describes
the daemon and filesystem limitations.
