# Riftri website

The Riftri product site is a Farm.js application. The page at `/`
has a hero with the copyable installer command, a storage-model overview, a
worktree disk-usage comparison, a three-step get-started section, and a compact
FAQ about manual COW copies, Git behavior, storage, activation, and recovery; deeper
concepts, safety, lifecycle, and compatibility material is available at `/docs`
and in the repository documentation and Markdown overview described below.

## Documentation site

`/docs` uses `@farming-labs/docs`, the official `@farming-labs/farmjs` adapter,
and its pixel-border theme. `docs.config.ts` owns the grouped sidebar, search,
table of contents, copy actions, and dark theme. `src/app/docs-theme.css`
adapts those components to the homepage's Geist typography, orange accents,
square controls, and dashed borders. AI chat, telemetry, and MCP are disabled;
reading and searching docs need no API key.

The canonical technical sources remain in `../docs/`. `content/docs.json`
maps them to titles and routes; only the introduction is authored in
`content/introduction.md`. The site focuses on getting started, day-to-day use,
platform support, and core internals. Benchmark reports, allocation evidence,
and design-decision logs stay in the repository; links to those documents point
to GitHub instead of adding them to the sidebar or search index.
`pnpm stage` regenerates ignored `src/app/docs/`
Markdown before development/build, rewrites internal documentation links,
and adds an edit link to each original GitHub file. Do not edit staged files.
Each sidebar entry also names a Lucide icon in `content/docs.json`.
Staging regenerates `content/docs-icons.json` as SVG strings for the adapter;
it also generates small action-icon masks in `public/docs-icons/`. The icon
library and SVG renderer run at build time, not in the browser.
Use `node scripts/stage-doc-icons.mjs --check` to check the generated registry.
After editing a canonical Markdown file during development, rerun `pnpm stage`.
After changing `docs.config.ts`, restart the dev server.

The build keeps the adapter's Vercel function for `/docs`, Markdown mirrors,
and `/api/docs` search. All other routes retain the existing static handling.
Docs responses are not CDN-cached because HTML and client navigation payloads
share URLs. `pnpm preview:static` now previews both parts of this finalized
output; it imports the built function, not the source dev server.
`RIFTRI_PREVIEW_PORT` overrides the default preview/browser-test port.

`pnpm stage && pnpm generate` refreshes Farm's route types when the page map
changes. Browser tests exercise every generated page, Markdown mirrors,
search, copying, sidebar navigation, and responsive layout.

The visual system keeps Riftri's dark canvas and orange accent, with a shared
page frame, fine stacked section rules, and a plain dark surface behind the storage
example. Self-hosted Geist Sans carries the headings and body; Geist Mono is
reserved for navigation, commands, diagrams, and metadata. The compact header
wraps onto two rows on phones without hiding links behind a menu. Its section
links and the skip link transfer keyboard focus to named destinations; keep
those destinations out of the normal tab order with `tabIndex={-1}`.

The FAQ at `/#faq` uses native `details` disclosures so answers remain usable
without JavaScript. The first answer is open initially; readers can open several
answers to compare them. Keep its capability claims aligned with the roadmap.

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
APFS scope, and dependency exclusions visible. A direct benchmark link keeps
the full timing comparison, fixture details, and methodology accessible.
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
to inspect that same build at `http://127.0.0.1:4318` without a dev server.
The preview honors the finalized static-file overrides and error-phase 404
route, including its status, HTML body, and HEAD behavior. It is a preview of
this site's hybrid output, not a general Vercel routing emulator.

## Automatic deployment

Every push to `main` runs the `Website deploy` workflow, which installs, type
checks, builds, publishes the prebuilt output to production, and then verifies
the public site. It is the normal way the site ships; the manual command below
remains for local recovery.

The workflow needs three repository secrets and skips itself with a warning
until all three exist, so it is inert rather than failing every push:

| Secret | Where it comes from |
| --- | --- |
| `VERCEL_TOKEN` | Vercel account settings → Tokens, scoped to `assistant-ui` |
| `VERCEL_ORG_ID` | `vercel link` writes it to `website/.vercel/project.json` as `orgId` |
| `VERCEL_PROJECT_ID` | the same file, as `projectId` |

`website/.vercel/` is gitignored, so read those two values locally once and copy
them into the repository secrets. Deploys are serialized by a concurrency group
so an older commit cannot finish after a newer one and republish stale content.

If the Vercel Git integration is connected to this repository instead, Vercel
builds each push itself. Use one path or the other; running both deploys the
same commit twice.

To publish the tested site manually from this directory:

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

The Farm.js build already generates Vercel Build Output API artifacts under
`.vercel/output`. Keep the downloaded project settings and environment files
under the ignored `.vercel` directory; never commit them. Verify `/install.sh`
over ordinary unauthenticated HTTPS after deployment, not a protected preview
or a Vercel authentication-bypass URL. Publishing the site does not merge a PR
or publish a new CLI release.
