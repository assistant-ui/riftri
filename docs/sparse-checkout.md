# Sparse-checkout worktrees

Riftri supports a deliberately narrow, deterministic slice of Git
sparse checkout: **cone mode with an explicit directory list**, requested
per worktree on the explicit interface:

```console
$ riftri worktree add ../app-core -b feature/core main \
    --sparse-dir crates/riftri-core --sparse-dir docs
```

The resulting directory is a real linked Git worktree whose working tree
contains every repository-root file, every file directly inside a listed
directory's ancestors, and everything below the listed directories — exactly
the shape `git sparse-checkout set --cone` produces. Git remains the source of
truth: the immutable base is materialized by Git's own sparse-checkout and
`checkout-index` machinery in an isolated administrative directory, and the new
worktree carries real worktree-scoped sparse configuration
(`core.sparseCheckout` plus the cone directory list), real skip-worktree index
bits, and a verified clean `git status` before Riftri reports success.
Enabling that worktree-scoped configuration turns on Git's standard
`extensions.worktreeConfig` setting for the repository, exactly as running
`git sparse-checkout set` in any linked worktree would; the main worktree and
other worktrees keep their ordinary full-checkout behavior.

## Configuration source

The sparse selection comes from repeated `--sparse-dir <DIR>` flags on
`riftri worktree add`. Each directory is repository-relative with `/`
separators.

When no `--sparse-dir` is given, the selection is inherited from the worktree
the command runs in, because that is what Git does: `git worktree add` copies
the current worktree's cone into the new worktree, so an add issued from
inside a sparse worktree is sparse with no sparse argument anywhere on the
command line. Riftri reproduces that deliberately and keys the immutable base
by the inherited profile, so the base always matches what was materialized.
An explicit `--sparse-dir` is an override and always wins over inheritance. The list is canonicalized — sorted, deduplicated, trailing slashes
removed, and nested selections collapsed into their listed ancestors — and the
canonical list becomes part of the versioned checkout profile that keys the
immutable base. Two different selections at the same commit therefore always
build or reuse different bases, a sparse and a full request never share a
base, and repeating an equivalent selection reuses its cached base. Full
(non-sparse) requests are unaffected and keep their existing base identities.

## Refusal behavior

Anything outside this subset fails closed with a precise diagnostic before
Riftri creates lifecycle state, a branch, or Git worktree metadata — a sparse
request is never silently materialized as a full tree, and an unsupported one
never partially materializes:

- Sparse patterns are not accepted: wildcards (`*`, `?`, `[`, `]`), negations
  (`!dir`), backslashes, control characters, absolute paths, and `.`/`..`
  components are all refused. Only literal directory lists are supported.
- Each listed directory must exist as a directory in the exact requested tree,
  so a misspelled selection cannot silently produce a nearly empty worktree.
- Sparse checkout that is enabled but not in cone mode
  (`core.sparseCheckout=true` with `core.sparseCheckoutCone` unset or false)
  remains an unsupported checkout profile for any optimized add, sparse or
  full. Riftri replays the repository's checkout configuration into the
  materialization and does not model non-cone pattern semantics, so it refuses
  rather than guess — including when an explicit `--sparse-dir` is given.
- Intercepted `git worktree add` commands (process-scoped or shell-hook
  activation) cannot *request* a sparse view yet; sparse-looking options are
  refused with a pointer to the explicit interface, and `RIFTRI_BYPASS=1`
  remains the ordinary-Git escape hatch. An intercepted add from inside a
  cone-mode sparse worktree does work, inheriting that worktree's cone exactly
  as Git would.
- Trees with Git LFS-managed paths cannot be combined with a sparse selection
  yet.
- Compacting a sparse worktree rebuilds it from its creation profile, so it
  works while the view still holds that profile and resolves to the base it
  already had. A view whose selection changed since the add — widened,
  narrowed, or disabled by ordinary Git — is refused, naming both profiles,
  because Riftri never rewrites a worktree's immutable creation base and no
  base describes the view any more. Remove and recreate it to reset its
  storage. A sparse checkout outside cone mode is refused outright.

## Lifecycle

Sparse views use the same journaled add transaction, private-write isolation,
clean-removal lifecycle, and crash recovery as full views. An unchanged
interrupted sparse add rolls back and can be retried. If a later selection
change materializes a different set of files, Riftri preserves that view for
inspection instead of resetting its index or deleting it.

Git still owns later selection changes. Inside an active view you can run
`git sparse-checkout set --cone another/directory` or `git sparse-checkout
disable`. Those commands update only that view: the original immutable base
and its peers keep their original selection. Newly included files are ordinary
Git checkouts, not additional Riftri clones. Clean removal works after expanding
or disabling sparse checkout, and dirty changes still prevent clean removal.
For a new COW-backed selection, remove the clean view and create a new one with
the desired `--sparse-dir` arguments.

Non-cone creation, file-level selection, explicit sparse options through Git
interception, compaction of a view whose selection changed since the add, and a
Riftri-managed profile-change operation remain future work tracked in the
roadmap.
