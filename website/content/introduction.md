# Riftri documentation

Riftri creates Git worktrees that share storage for unchanged files. Each
worktree keeps its own branch, files, and edits; your editor and Git commands
work as usual.

The saving comes from the filesystem, not from a layer between you and your
files. On a supported volume Riftri asks the filesystem to clone the
unchanged parts — APFS clones, Btrfs and XFS reflinks, ReFS block cloning —
so ten worktrees of the same commit cost far less than ten checkouts. Files
allocate private space as you change them.

## Start with one worktree

[Install Riftri](guides/installation.md), then run in an existing repository:

```sh
riftri setup
```

Pick a directory and branch, review the plan, and confirm. Setup starts from
the current commit, not uncommitted changes, and can end by opening an
installed coding agent. Add `--plain` for line-oriented output, or
`-b <branch>` and `--destination <path>` to skip the questions.

Prefer explicit commands? `riftri worktree add ../app-auth -b feature/auth`
does the same thing without prompts.

## Choose your workflow

- [CLI reference](guides/cli.md): manage worktrees yourself.
- [Coding agents](guides/agents.md): launch an agent with Riftri enabled.
- [Custom harness](guides/custom-harness.md): drive Riftri from your own runner.
- [Shell activation](guides/activation.md): keep using `git worktree` in your terminal.

## What you get beyond the disk saving

Every lifecycle operation is journaled, so an interrupted create or remove is
resumable with `riftri repair` rather than leaving half-made state. Checkouts
Riftri cannot reproduce exactly stop before anything is written. Removal
refuses a worktree with uncommitted work. `riftri status` and
`riftri worktree list` report what each worktree actually allocates, which is
what `gc` and `worktree compact` reclaim.

## Before you start

- Requires a [supported filesystem](guides/filesystem-compatibility.md);
  unsupported setups stop, never silently full-copy.
- Dependencies and build outputs stay separate per worktree — see
  [build caches](guides/build-caches.md).
- Nothing is activated globally: interception needs both an activated shell or
  process and per-repository consent.
- Experimental: keep important work committed or backed up. Worktree
  isolation is not a security sandbox.
