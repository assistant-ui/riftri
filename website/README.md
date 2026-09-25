# Riftri website

The Riftri product site is a single-page Farm.js application. It has a hero,
a storage-model overview, and a three-step get-started section, and links to
the repository documentation for everything else.

Development and production builds copy the canonical `../package/install.sh`
and `../package/install.ps1` to `public/`, serving them at `/install.sh` and
`/install.ps1`. Do not edit those generated
copies. Deploy from the repository with `website` as the app directory so the
staging script can access `package/`. The public site and copyable installation
command use `https://riftri.vercel.app` on the existing `assistant-ui/riftri`
Vercel project. The Windows/manual guide links to `docs/install.md` on `main`.

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
