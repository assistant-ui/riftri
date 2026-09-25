# Git LFS compatibility

Riftri supports a deliberately narrow, deterministic Git LFS profile. It does
not run an LFS download as part of worktree creation and it does not treat an
arbitrary Git filter as trusted checkout logic.

An executable `post-checkout` hook, including one installed by Git LFS, runs
after creation exactly as it would under `git worktree add`, so it no longer
blocks optimized creation.

An LFS-managed path is eligible when all of these checks pass:

- Git resolves its in-tree attributes exactly to
  `filter=lfs diff=lfs merge=lfs -text`.
- `filter.lfs.clean` and `filter.lfs.smudge` have the standard Git LFS values,
  `filter.lfs.process` is absent or standard, and `filter.lfs.required=true`.
- `lfs.storage` is not customized.
- `git lfs version` succeeds.
- The committed blob is the canonical, extension-free, three-line v1 pointer
  with a lowercase SHA-256 object ID and canonical decimal size.
- The named object is already a regular file of that size in
  `<common-git-dir>/lfs/objects/<aa>/<bb>/<object-id>`.

During a cold base build, Git first materializes the exact pointer tree with
external filters disabled. Riftri opens each local object without following
links and streams it into the staging tree while verifying both size and
SHA-256. The immutable base is not activated or marked complete unless every
object passes. Normal Git LFS clean/filter behavior then proves the new linked
worktree is clean before Riftri reports success.

If an object is missing, fetch it before retrying:

```console
$ git lfs fetch
$ riftri doctor --destination ../app-auth
$ riftri worktree add ../app-auth -b feature/auth main
```

Custom filters, pointer extensions, custom LFS storage, and nonstandard Git LFS
commands remain unsupported. `riftri doctor` reports the exact blocker before
Riftri creates lifecycle state or Git worktree metadata.
