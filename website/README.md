# Riftri website

The Riftri product site is a single-page Farm.js application. Product concepts, installation,
transparent Git usage, safety, storage lifecycle, and compatibility all live at `/`.

Development and production builds copy the canonical `../package/install.sh`
to `public/install.sh`, serving it at `/install.sh`. Do not edit that generated
copy. Deploy from the repository with `website` as the app directory so the
staging script can access `package/`. The public site and copyable installation
command use `https://riftri.vercel.app` on the existing `assistant-ui/riftri`
Vercel project. The Windows/manual guide links to a reviewed source commit so
it remains accessible even before the installation PR is merged.

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
