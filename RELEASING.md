# Releasing Riftri

Riftri publishes one synchronized version across the Cargo workspace, the
`riftri` npm launcher, and six platform-native npm packages. A tag-driven GitHub
workflow builds native binaries, checks all tarballs, publishes the native npm
packages before the launcher, and creates a GitHub release with SHA-256 sums.

## One-time repository setup

1. Make the `assistant-ui/riftri` repository public before publishing to npm.
   Public visibility is required for npm provenance.
2. Protect `main` and require the `Quality` CI jobs before merging.
3. Enable private vulnerability reporting in the repository's Security settings.
4. Confirm all seven npm names are still available before the first release:
   `riftri`, `riftri-darwin-arm64`, `riftri-darwin-x64`,
   `riftri-linux-arm64-gnu`, `riftri-linux-x64-gnu`, `riftri-win32-arm64`, and
   `riftri-win32-x64`.

## npm authentication

For the first release, add an npm publishing token as the `NPM_TOKEN` GitHub
Actions secret because trusted publishers can only be configured after the
packages exist. Use the narrowest token the npm account permits and remove it
after bootstrapping.

After all seven packages exist, configure the same GitHub Actions trusted
publisher on each npm package:

- owner: `assistant-ui`
- repository: `riftri`
- workflow: `release.yml`

The workflow grants only its publish job `id-token: write`. Once trusted
publishing works, delete the `NPM_TOKEN` secret. The release script remains
restartable: package versions already present in npm are detected and skipped.

## Cutting a release

1. Update the version in the root `Cargo.toml`, root `package.json`,
   `package-lock.json`, and every `npm/platforms/*/package.json`. All versions
   and optional-dependency pins must match.
2. Move relevant entries from `Unreleased` in `CHANGELOG.md` to a dated version.
3. Run the full local checks:

   ```console
   $ cargo fmt --all --check
   $ cargo clippy --workspace --all-targets --all-features -- -D warnings
   $ cargo test --workspace
   $ npm ci --ignore-scripts --omit=optional
   $ npm test
   $ npm run pack:check
   ```

4. Merge the release change into `main`, create the matching signed tag, and
   push it:

   ```console
   $ git tag -s v0.1.0 -m "Riftri 0.1.0"
   $ git push origin v0.1.0
   ```

The tag version must exactly match every manifest. The release workflow refuses
mismatches or missing platform artifacts rather than publishing a partial
launcher package. Prerelease versions are published under npm's `next` tag and
marked as prereleases on GitHub; stable versions use npm's `latest` tag.
