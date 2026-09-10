---
title: "Getting started"
description: "Check APFS compatibility and create a lightweight Git worktree."
section: "Start"
---

# Getting started

This guide creates one Riftri-managed worktree without changing your shell configuration.

## Requirements

- macOS with the destination on a writable APFS volume.
- Git available on `PATH`.
- A normal Git repository with a commit checked out.

Linux and Windows builds currently provide diagnostics but do not create optimized worktrees.

## Install

Install the public CLI through npm:

```console
$ npm install --global riftri
$ riftri --version
```

If the npm package is not available yet, build the Rust CLI from source:

```console
$ cargo build --release -p riftri-cli
$ ./target/release/riftri --version
```

## Check before changing anything

Run `doctor` from the repository that will own the worktree:

```console
$ riftri doctor --destination ../app-auth
```

`doctor` is read-only. It resolves the repository, inspects the requested destination volume, and
checks whether the current checkout can be reproduced safely. A blocked result lists every reason
before Riftri creates a destination, Git metadata, or managed state.

## Create a worktree

```console
$ riftri worktree add ../app-auth -b feature/auth main
```

Riftri creates ordinary Git linked-worktree metadata, materializes or reuses an immutable base, and
activates a private APFS clone at the destination.

Verify it with Git:

```console
$ cd ../app-auth
$ git status
On branch feature/auth
nothing to commit, working tree clean
```

## Inspect Riftri

From any worktree in the repository:

```console
$ riftri status
```

Status reports managed views, retained bases, reference counts, allocation, and incomplete or
unexplained state. It does not delete anything.

Next, learn how to use [normal `git worktree` commands](/docs/git-interception) through opt-in
interception.
