# Optional Git FSMonitor

Git can watch a large worktree for changes and avoid checking every unchanged
file on each status request. Riftri already honors this optional Git feature.
It stays off unless you enable it.

## Measure first

On one Mac/APFS fixture with 20,004 files, three-edit status checks fell from
66.7 ms to 46.6 ms. On 4,100 files the watcher was slower. Creation did not get
faster: the initial scan and Riftri's storage checks still happen.

These are synthetic results, not an automatic size threshold. See the
[measurements](https://github.com/assistant-ui/riftri/blob/main/docs/benchmarks/fsmonitor-2026-10-08.md).

## Try without saving a setting

Use current Git in your terminal and IDE. This guide's tested profile is local
macOS/APFS; other Git builds and filesystems need validation. Inspect any existing
watcher setting before overriding it:

```sh
git -C ../task-1 config --show-origin --get-all core.fsmonitor
git -C ../task-1 -c core.fsmonitor=true status --short
git -C ../task-1 -c core.fsmonitor=true fsmonitor--daemon status
```

An unset config key exits with 1. The daemon-status command must succeed before
you persist the option. Compare repeated checks after warm-up. The trial writes
no config file, but starts a Git-owned background watcher.

## Save only if it helps

```sh
git -C ../task-1 config --local core.fsmonitor true
```

This normally affects **all linked worktrees in the repository**, including
future ones. It is separate from `riftri enable`; ending `riftri exec` does not
stop Git's watcher. Do not use `--global` or assume `--worktree` is isolated
without Git's worktree-config extension. The [full guide](https://github.com/assistant-ui/riftri/blob/main/docs/fsmonitor.md)
covers compatibility and narrower configuration.

## Turn it off

For the shared repository setting above:

```sh
git -C ../task-1 config --local core.fsmonitor false
git -C ../task-1 -c core.fsmonitor=true fsmonitor--daemon stop
```

Stop it in every worktree you watched, including the main checkout. Stopping
alone is temporary while the setting remains enabled. For a trial that never
saved configuration, only the stop command is needed. If no watcher is running,
Git may report an error. Restore prior custom settings deliberately.

No new Riftri daemon, global setting, or relaxed dirty-worktree check is involved.
