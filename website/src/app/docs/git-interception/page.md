---
title: "Git interception"
description: "Use explicit commands, process-scoped activation, or an opt-in shell hook."
section: "Concepts"
---

# Git interception

The explicit Riftri command is the simplest correctness baseline. Transparent modes add convenience
without changing the underlying transaction.

## Explicit mode

No activation is required:

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

Use this mode for scripts, debugging, and the clearest failure reporting.

## Process-scoped mode

Enable the current repository, then start an agent or command tree through Riftri:

```console
$ riftri enable
$ riftri exec -- claude
```

Supported `git worktree` operations started by the child process are routed through Riftri. Other
Git commands immediately delegate to the real Git executable.

Bind a command to an existing worktree without changing directories first:

```console
$ riftri exec --worktree ../app-auth -- codex
```

The path must be the exact root of a live worktree reported by Git.

## Shell-scoped mode

Evaluate the hook in a sh, bash, or zsh session:

```console
$ eval "$(riftri shell hook zsh)"
$ cd app
$ riftri enable
$ git worktree add -b feature/auth ../app-auth main
```

The hook can be added to your shell profile manually if you want it available in every new shell.
That makes interception globally available for your user; it does **not** optimize every repository.
Each repository still requires `riftri enable`.

Riftri never edits shell startup files automatically.

## Inspect and reverse activation

```console
$ riftri shell status
$ riftri disable
$ eval "$(riftri shell deactivate zsh)"
```

Repository consent and shell activation are independent. Disabling a repository does not remove the
shell hook, and deactivating the hook does not erase repository consent.

## Edge cases

Aliases or functions named `git` can take precedence over `PATH`. Tools that use libgit2 or JGit,
call Git through an absolute path, clear the environment, or run outside the shell do not use the
shim. Their normal behavior is unchanged, but their worktree operations are not optimized.

Use `riftri shell status` in the exact terminal, IDE, container, or agent environment you want to
verify.
