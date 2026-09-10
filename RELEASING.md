# Releasing Riftri

Riftri publishes one synchronized version across the Cargo workspace, the
`riftri` npm launcher, and six platform-native npm packages. A tag-driven GitHub
workflow builds native binaries, inspects every tarball, publishes the native
packages before the launcher, and creates a GitHub release with SHA-256 sums.

Only the npm packages are public distribution artifacts. The Rust crates are
internal implementation units and are marked `publish = false`.

## Package layout

The root `package.json` is the source manifest for the public `riftri` package.
The `package/` source directory groups the Node launcher, platform manifests,
release scripts, and package tests. The published launcher is still named
`riftri`; release staging places `bin/` and `lib/` at the root of its npm
tarball.

Native packages provide a CLI binary for each advertised target. They do not
claim that every platform has a mutation backend. Until the roadmap says
otherwise, worktree mutation remains macOS/APFS-only.

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
5. Keep default GitHub Actions permissions read-only and allow write or OIDC
   permissions only on the release job that needs them.
6. Enable private vulnerability reporting and confirm the link in
   [SECURITY.md](SECURITY.md) opens the private report form.
7. Confirm `@assistant-ui/engineering` resolves as the repository code owner.
8. Confirm all seven npm names are still available immediately before the first
   release: `riftri`, `riftri-darwin-arm64`, `riftri-darwin-x64`,
   `riftri-linux-arm64-gnu`, `riftri-linux-x64-gnu`, `riftri-win32-arm64`, and
   `riftri-win32-x64`.

Public visibility is required for npm provenance from this repository.

## npm authentication

The first publication needs a short-lived, least-privilege npm token because a
trusted publisher can only be attached after each package exists. Add it as the
`NPM_TOKEN` GitHub Actions secret immediately before the bootstrap release.

After all seven packages exist, configure the same GitHub Actions trusted
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

After the workflow succeeds:

1. Confirm the GitHub release contains all six native archives and
   `SHA256SUMS`.
2. Confirm npm shows provenance for the launcher and all native packages.
3. Test `npx --yes riftri@<version> doctor` on at least one supported target.
4. On APFS, create an enabled disposable repository, add and remove a managed
   worktree, run `riftri status`, and verify Git reports clean state.
5. If a published release is defective, deprecate it and issue a fixed version;
   do not move or reuse an existing tag or npm version.
