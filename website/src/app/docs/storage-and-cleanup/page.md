---
title: "Storage and cleanup"
description: "Understand disk sharing, retained bases, repair, removal, and garbage collection."
section: "Concepts"
---

# Storage and cleanup

Riftri treats creation and cleanup as recoverable storage transactions. It never automatically
deletes a changed worktree.

## Inspect storage

```console
$ riftri status
```

Status reports active views, retained immutable bases, reference counts, logical size,
filesystem-accounted allocation, incomplete journals, and unexplained state paths.

Per-path APFS allocation can count shared blocks more than once. Do not add those values together to
estimate Riftri's exclusive use of the volume.

## Remove a clean worktree

With interception active, the ordinary form is routed through Riftri when the worktree is managed:

```console
$ git worktree remove ../app-auth
```

Or use the explicit command:

```console
$ riftri worktree remove ../app-auth
```

Dirty worktrees are preserved. Forced lifecycle operations that could bypass Riftri's journal fail
closed when they affect managed state.

## Repair an interrupted operation

```console
$ riftri repair
```

Repair resumes or rolls back known journal states. It deletes an incomplete add only when the view
is unchanged, and completes removal only when the target is clean. Anything ambiguous is preserved
and explained.

## Collect unused bases

Zero-reference bases remain cached for fast reuse. Preview garbage collection first:

```console
$ riftri gc
```

Apply the exact plan explicitly:

```console
$ riftri gc --apply
```

Garbage collection revalidates references under the immutable-base lock before deleting a base.

## Measuring disk use

The repository includes an opt-in APFS volume-level benchmark. In one recorded development run, a
cached view with a 32 MiB tracked payload grew the volume by 49,152 bytes, or 0.146% of the logical
payload.

This is implementation evidence, not a universal performance promise. Filesystem activity, file
count, metadata, fragmentation, and later private writes affect allocation.
