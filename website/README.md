# Riftri website

The Riftri product site is a single-page Farm.js application. Product concepts, installation,
transparent Git usage, safety, storage lifecycle, and compatibility all live at `/`.

Development and production builds copy the canonical `../package/install.sh`
to `public/install.sh`, serving it at `/install.sh`. Do not edit that generated
copy. Deploy from the repository with `website` as the app directory so the
staging script can access `package/`. The copyable installation command uses
the repository's stable GitHub raw URL; no custom website domain is assumed.

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
