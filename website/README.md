# Riftri website

The Riftri product site and documentation are built with Farm.js. The landing page lives at `/`,
and Farm's default docs runtime serves Markdown from `src/app/docs` at `/docs` with search,
Markdown mirrors, agent-readable output, sitemap, and robots routes.

```console
$ pnpm install
$ pnpm dev
```

Quality checks:

```console
$ pnpm type-check
$ pnpm build
```
