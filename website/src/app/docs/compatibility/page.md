---
title: "Compatibility"
description: "Current platform support, checkout boundaries, and transparent Git behavior."
section: "Reference"
---

# Compatibility

Riftri probes real repository and destination capabilities instead of assuming support from the
operating system name.

## Platforms

| Platform | Current behavior | Planned native backend |
| --- | --- | --- |
| macOS on writable APFS | Optimized add, move, remove, prune, repair, and GC | Native APFS clones |
| Linux | Diagnostics and explicit unsupported-backend errors | Btrfs/XFS reflinks, then OverlayFS |
| Windows | Diagnostics and explicit unsupported-backend errors | ReFS block cloning |

A successfully installed CLI does not imply that a mutation backend is available on that machine.

## Supported checkout behavior

Riftri currently accepts deterministic built-in `text`, `eol`, and `binary` in-tree attribute
semantics. It preserves symlinks and executable modes and supports normal, linked, unborn,
detached, and bare repository discovery.

Optimized transparent adds currently require either `-b <new-branch>` or `--detach`.

## Checkout behavior that stops safely

Riftri reports a blocker before mutation for repositories that currently depend on:

- Git LFS or custom filters;
- working-tree encoding or ident substitution;
- external or unknown attributes;
- sparse checkout;
- submodules;
- checkout-changing non-default configuration.

These features are not considered broken. Riftri stops because it cannot yet guarantee that its
materialized bytes exactly match Git's checkout.

## Transparent Git boundaries

Normal Git commands pass directly to the captured Git executable. Repositories without
`riftri enable` also remain on ordinary Git. Aliases, libgit2/JGit clients, absolute Git paths,
environment-clearing tools, and processes outside the activated shell can bypass the shim.

Riftri never silently falls back to creating a full worktree copy.
