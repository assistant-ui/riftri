# Riftri website

The Riftri product site is a single-page Farm.js application. Product concepts, installation,
transparent Git usage, safety, storage lifecycle, and compatibility all live at `/`.

Development and production builds copy the canonical `../package/install.sh`
and `../package/install.ps1` to `public/`, serving them at `/install.sh` and
`/install.ps1`. Do not edit those generated
copies. Deploy from the repository with `website` as the app directory so the
staging script can access `package/`. The public site and copyable installation
commands use `https://riftri.dev` on the existing `assistant-ui/riftri`
Vercel project, with DNS managed by Cloudflare. The Windows/manual guide links
to the repository's installation documentation.

```console
$ pnpm install
$ pnpm dev
```

Quality checks:

```console
$ pnpm exec farm generate --check
$ pnpm type-check
$ pnpm build
```

The `/#savings` section visualizes the historical assistant-ui experiment in
`../docs/benchmarks/assistant-ui-ten-agents-2026-09-12.md`. Its raw allocation
measurements live in `src/data/space-savings.json`; the chart derives MiB,
saved bytes, and bar proportions from those values. Keep the benchmark version,
adjusted-fixture caveat, dependency exclusions, and timing tradeoff visible.
`node --test package/test/website-savings.test.js` from the repository root
checks the displayed dataset against the source report. This is not a live
benchmark or a claim about the latest release's performance.

To publish the tested site from this directory:

```console
$ vercel link --yes --project riftri --scope assistant-ui
$ vercel pull --yes --environment=production --scope assistant-ui
$ pnpm build
$ vercel deploy --prebuilt --prod --scope assistant-ui
```

The Farm.js build already generates Vercel Build Output API artifacts under
`.vercel/output`. Keep the downloaded project settings and environment files
under the ignored `.vercel` directory; never commit them. Verify `/install.sh`
over ordinary unauthenticated HTTPS after deployment, not a protected preview
or a Vercel authentication-bypass URL. Publishing the site does not merge a PR
or publish a new CLI release.
