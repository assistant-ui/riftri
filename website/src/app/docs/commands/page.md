---
title: "CLI commands"
description: "A concise reference for Riftri diagnostics, activation, lifecycle, and recovery."
section: "Reference"
---

# CLI commands

Use `riftri <command> --help` for the complete flags accepted by the installed version.

## Diagnostics

| Command | Purpose |
| --- | --- |
| `riftri doctor` | Inspect Git and the planned storage path without changing anything. |
| `riftri backends <destination>` | Probe storage backends for a concrete destination volume. |
| `riftri status` | Report bases, views, reference counts, journals, and disk use. |

## Activation

| Command | Purpose |
| --- | --- |
| `riftri enable` | Enable optimized operations for the current repository. |
| `riftri disable` | Remove repository-local consent. |
| `riftri exec -- <command>` | Run one process tree with Git worktree interception. |
| `riftri exec --worktree <path> -- <command>` | Start a command at an exact live worktree root. |
| `riftri shell hook <shell>` | Print the sh, bash, or zsh activation script. |
| `riftri shell status` | Explain whether shell and repository activation are effective. |
| `riftri shell deactivate <shell>` | Print the current-shell deactivation script. |

Shell hook and deactivation output must be explicitly evaluated. Riftri does not change the parent
shell or edit profile files by itself.

## Worktree lifecycle

| Command | Purpose |
| --- | --- |
| `riftri worktree add <path> -b <branch> <start>` | Create an optimized worktree and branch. |
| `riftri worktree add <path> --detach <start>` | Create an optimized detached worktree. |
| `riftri worktree move <source> <destination>` | Move a managed worktree with a journal. |
| `riftri worktree remove <path>` | Remove a clean managed worktree. |
| `riftri worktree prune` | Prune eligible Git metadata while protecting managed state. |

## Recovery and collection

| Command | Purpose |
| --- | --- |
| `riftri repair` | Resume or roll back interrupted repository operations conservatively. |
| `riftri gc` | Print the zero-reference base collection plan. |
| `riftri gc --apply` | Revalidate and apply that collection plan. |
| `riftri recover --state-dir <path>` | Recover an explicitly selected state directory. |

Avoid using forced managed lifecycle commands or `RIFTRI_BYPASS=1` unless you intentionally accept
manual repair work.
