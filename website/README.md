# Riftri website

The Riftri product site is a single-page Farm.js application. The page at `/`
has a hero with the copyable installer command, a storage-model overview, a
worktree disk-usage comparison, and a three-step get-started section; deeper
concepts, safety, lifecycle, and compatibility material stays in the
repository documentation and the Markdown overview described below.

`public/index.md` is the plain-Markdown overview served at `https://riftri.dev/index.md`.
It covers setup, internals, supported backends, lifecycle, and benchmark context,
with absolute links to the detailed GitHub documentation. Keep it in sync with
product changes and add newly stored technical docs to its documentation index.
The hero installer has a compact `.md` link beside its copy button, with a
descriptive accessible label. It navigates to `https://riftri.dev/index.md` in the same browser
tab, without a download attribute; the footer also opens the raw file. Neither adds a
separate guide section. Static output finalization requires the file and serves it as
`text/plain; charset=utf-8`, with an inline `index.md` filename and `nosniff`.
The response is the unchanged `.md` file, not HTML. Plain-text handling preserves
literal Markdown in standard browsers. Embedded viewers may flatten whitespace
even when the server returns the correct content type.
`node --test package/test/website-markdown.test.js package/test/website-static-output.test.js`
checks document coverage, local GitHub targets, and output configuration.
The guide's star request is optional; agents must have
explicit user approval before starring on someone's behalf.

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

Sharing metadata points to the canonical `https://riftri.dev/` homepage and a
1200×630 PNG card at `/og.png`. Edit `assets/og.svg` and run
`pnpm generate:sharing-card` to regenerate the committed PNG. No image server is needed.

Quality checks (including browser-test TypeScript):

```console
$ pnpm exec farm generate --check
$ pnpm type-check
$ pnpm build
```

The `/#savings` section visualizes the historical assistant-ui experiment in
`../docs/benchmarks/assistant-ui-ten-agents-2026-09-12.md`. Its raw allocation
measurements live in `src/data/space-savings.json`; the chart derives MiB,
saved bytes, and bar proportions from those values. Keep the benchmark version,
APFS scope, and dependency exclusions visible. The expandable benchmark details
retain the timing comparison, adjusted-fixture caveat, methodology, and source link.
`node --test package/test/website-savings.test.js` from the repository root
checks the displayed dataset against the source report. This is not a live
benchmark or a claim about the latest release's performance.
Only the Riftri backend name cycles: APFS, Linux reflink, and ReFS. The graph
and numbers stay fixed to the recorded APFS reference measurement; the rotating
names describe platform support, not additional measured results. Clicking
the backend name pauses or resumes its animation. Reduced-motion preferences
show a static label, with all supported backends available to screen readers.

Browser regression coverage runs against the finalized production output:

```console
$ pnpm exec playwright install chromium
$ pnpm build
$ pnpm test:browser
```

The suite checks desktop, tablet, and phone layouts, keyboard navigation,
clipboard success/retry/repeated-click timing, diagram motion controls,
Windows setup, sharing metadata, and inline Markdown navigation. Its Markdown
navigation test routes the canonical URL to the local build's exact response,
so CI does not depend on the public deployment. Screenshots and failure traces
are retained in `test-results/` and uploaded by CI. Run `pnpm preview:static`
to inspect that same build at `http://127.0.0.1:4318` without a dev runtime.

To publish the tested site from this directory:

```console
$ vercel link --yes --project riftri --scope assistant-ui
$ vercel pull --yes --environment=production --scope assistant-ui
$ pnpm deploy:production
```

Use a clean, committed checkout. The deployment command type-checks, builds,
runs the existing Chromium suite, publishes the exact tested output, and runs
`pnpm verify:production`. The public `/build-info.json` records the build's Git
revision. Verification compares it with the checkout and checks the homepage,
inline Markdown, byte-identical installers and public assets, and branded 404.
A failed verification exits nonzero; it does not silently roll back or redeploy.
The Website deployment check workflow also verifies successful production
deployment events and can be run manually against the intended deployed ref.

The Farm.js build already generates Vercel Build Output API artifacts under
`.vercel/output`. Keep the downloaded project settings and environment files
under the ignored `.vercel` directory; never commit them. Verify `/install.sh`
over ordinary unauthenticated HTTPS after deployment, not a protected preview
or a Vercel authentication-bypass URL. Publishing the site does not merge a PR
or publish a new CLI release.
