# Releasing Riftri

Riftri publishes one synchronized version across the Cargo workspace, the
`riftri` npm launcher, and eight platform-native npm packages. A tag-driven GitHub
workflow builds native binaries and inspects every tarball, then independently
publishes npm packages and a GitHub release with SHA-256 sums. The npm job
publishes native packages before the launcher. A failure in that job does not
prevent the GitHub direct downloads from being released.

Both standalone native archives and npm packages are public distribution
artifacts. Direct downloads require neither Node.js nor npm; installation and
checksum verification are documented in [docs/install.md](docs/install.md).
The Rust crates are internal implementation units and are marked
`publish = false`.

## Publication channels and retries

The eight-platform build matrix feeds one read-only staging job. It validates
versions, prepares and verifies the native archives, and uploads those exact
archives plus `SHA256SUMS` as the distinct `github-release-assets` workflow
artifact (retained for seven days). After all staging checks pass:

- `publish` publishes npm packages with `contents: read` and `id-token: write`.
- `github-release` downloads the staged assets, verifies the exact eight-archive
  set and every checksum, and creates the GitHub release with `contents: write`.
  It has no npm token or OIDC publishing permission and does not depend on npm.

Both publishing jobs are restricted to tag pushes. `workflow_dispatch` builds,
checks, and stages artifacts as a rehearsal; it never publishes either channel.
An overall workflow failure can therefore coexist with a successful GitHub
release. Inspect the individual jobs and report each channel's status.

The GitHub job uses `--verify-tag`: it never creates or moves a tag. If a release
already exists, creation fails safely rather than replacing any existing asset.
On a retry, inspect the existing release and compare all nine asset names and
checksums with the staged artifact. Do not delete, clobber, or silently replace
published assets to make a rerun pass. If assets differ, investigate and issue a
new version. A partial existing release needs deliberate maintainer review.

The npm publisher skips exact versions already on the registry, publishes the
launcher last, and verifies the complete set of nine exact versions before the
job can succeed. Resume a failed npm job only after resolving its actual
failure; an explicit registry security rejection requires registry review, not
package renaming or blind retries.
GitHub release availability does not imply that `npm install riftri` or
`npx riftri` is available for the same version.

As of September 12, 2026, the initial `v0.1.1` npm publication stopped at
`riftri-win32-arm64` with `E403: Package name triggered spam detection` after
publishing the four macOS/Linux native packages. The Windows x64 package and
main launcher were not attempted. The `v0.1.1` tag predates these independent
jobs; rerunning that old tag uses its original workflow, not the workflow on
`main`. If publishing its direct downloads separately, use binaries from its
successful tag-build jobs and stage them from the exact tagged source, verify
all six archives and checksums, and create the release without changing the
tag or npm versions. Do not substitute binaries from a newer `main` build.

## Package layout

The root `package.json` is the source manifest for the public `riftri` package.
The `package/` source directory groups the Node launcher, platform manifests,
release scripts, and package tests. The published launcher is still named
`riftri`; release staging places `bin/` and `lib/` at the root of its npm
tarball.

Native packages provide a CLI binary for each advertised target. They do not
claim that every filesystem has a mutation backend. Optimized worktrees require
macOS/APFS, Btrfs, reflink-enabled XFS, caller-visible OverlayFS mounts, or
Windows/ReFS, and an active capability probe must succeed before mutation.

## Before making the repository public

Complete these owner-controlled steps after the readiness pull request is
merged and before publishing anything:

1. Review the entire Git history for credentials, private repository contents,
   proprietary fixtures, personal paths, and commit-author email addresses that
   should not become public. Rotate a credential even if it was later removed.
2. Set the repository description, homepage, and topics. Suggested topics are
   `git`, `worktree`, `rust`, `copy-on-write`, `developer-tools`,
   `coding-agents`, and `apfs`.
3. Change `assistant-ui/riftri` visibility to public. Visibility is a deliberate
   owner action and should not be automated by a release script.
4. Protect `main`, require a pull request, and require every `Quality` CI matrix
   job before merge.
5. Keep default GitHub Actions permissions read-only and allow contents-write
   only on the GitHub release job and OIDC only on the npm publishing job.
6. Enable private vulnerability reporting and confirm the link in
   [SECURITY.md](SECURITY.md) opens the private report form.
7. Confirm `@assistant-ui/engineering` resolves as the repository code owner.
8. Confirm all nine npm names are still available immediately before the first
   release: `riftri`, `riftri-darwin-arm64`, `riftri-darwin-x64`,
   `riftri-linux-arm64-gnu`, `riftri-linux-arm64-musl`,
   `riftri-linux-x64-gnu`, `riftri-linux-x64-musl`, `riftri-win32-arm64`, and
   `riftri-win32-x64`.

Public visibility is required for npm provenance from this repository.

## npm authentication

The first publication needs a short-lived, least-privilege npm token because a
trusted publisher can only be attached after each package exists. Add it as the
`NPM_TOKEN` GitHub Actions secret immediately before the bootstrap release.

After all nine packages exist, configure the same GitHub Actions trusted
publisher on each npm package:

- organization or user: `assistant-ui`
- repository: `riftri`
- workflow: `release.yml`

The workflow grants only its publish job `id-token: write`. Once a trusted
publishing release succeeds, delete `NPM_TOKEN`. The publishing script is
restartable: package versions already present in npm are detected and skipped.

## Cutting a release

1. Update the version in the root `Cargo.toml`, root `package.json`, every
   `package/platforms/*/package.json`, and `package-lock.json`. Source optional
   dependencies remain local `file:` references; release staging converts them
   to exact registry versions in the published manifest.
2. Move relevant entries from `Unreleased` in `CHANGELOG.md` to a dated version.
3. Regenerate and review lockfile metadata when needed, then run the complete
   local checks:

   ```console
   $ npm install --package-lock-only --ignore-scripts
   $ npm ci --ignore-scripts --omit=optional
   $ cargo fmt --all --check
   $ cargo clippy --workspace --all-targets --all-features -- -D warnings
   $ cargo test --workspace
   $ npm test
   $ npm run pack:check
   ```

   `npm test` includes an installed-package smoke check using locally generated
   npm tarballs, an isolated global npm prefix, and no registry access. On
   macOS it also runs the complete managed APFS lifecycle against a disposable
   real Git repository. Native-backend CI repeats that installed lifecycle on
   Btrfs, reflink-enabled XFS, helper-backed OverlayFS, and ReFS. Run it
   separately with `npm run smoke:installed` when diagnosing release packaging.

4. Open and squash-merge a conventional release pull request such as
   `chore: release v0.1.0`. The pull request title becomes the release commit
   subject on `main`.
5. From the updated `main`, create and push the matching signed tag:

   ```console
   $ git switch main
   $ git pull --ff-only
   $ git tag -s v0.1.0 -m "Riftri 0.1.0"
   $ git push origin v0.1.0
   ```

The tag version must exactly match every manifest. The release workflow refuses
version mismatches or missing platform artifacts instead of publishing a
partial launcher package. Prerelease versions use npm's `next` tag and are
marked as prereleases on GitHub; stable versions use npm's `latest` tag.

## Verifying a release

Verify each publication channel separately:

1. Confirm the GitHub release contains all eight native archives and
   `SHA256SUMS`. Download a native archive through its public release URL,
   verify its checksum before extraction or execution, then check `riftri
   --version` and `riftri doctor` without the npm launcher.
2. If the npm job succeeded, confirm npm shows provenance for the launcher and
   all native packages. Test `npx --yes riftri@<version> doctor` on at least one
   supported target. If it failed, report the incomplete npm publication
   explicitly instead of advertising the launcher as available. Update dated
   npm-availability notices in `README.md` and `docs/install.md` once the main
   launcher is actually published and verified.
3. Check the user-facing [installation guide](docs/install.md) against the
   downloaded asset names and installed executable.
4. On at least one supported native COW filesystem, create an enabled disposable
   repository, add and remove a managed worktree, run `riftri status`, and
   verify Git reports clean state.
5. If a published release is defective, deprecate it and issue a fixed version;
   do not move or reuse an existing tag or npm version.
