import { access, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { currentRevision } from "./verify-website.mjs";
import { pages } from "./stage-website-docs.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));

const installerRoute = {
  src: "/install\\.(?:sh|ps1)",
  headers: {
    "Cache-Control": "public, max-age=300",
    "Content-Type": "text/plain; charset=utf-8",
  },
  continue: true,
};

const markdownRoute = {
  src: "^/index\\.md$",
  headers: {
    "Cache-Control": "public, max-age=300",
    // Display literal Markdown in browsers that don't recognize text/markdown.
    "Content-Type": "text/plain; charset=utf-8",
    "Content-Disposition": "inline; filename=\"index.md\"",
    "X-Content-Type-Options": "nosniff",
  },
  continue: true,
};

// The docs adapter links this stylesheet from every docs page's <head> and
// serves it from its runtime, which only the narrow docs routes below reach.
// Finalization publishes the runtime's exact response as a static file with
// the runtime's own headers. The adapter marks it immutable at this unhashed
// path; it sits in the lowest cascade layer and the site bundle compiles the
// same theme, so a cached copy from an older adapter cannot override the page.
const docsBrowserCssPath = "/__farm_docs/browser.css";

async function publishDocsBrowserCss(outputDirectory, staticDirectory) {
  const runtime = (await import(pathToFileURL(path.join(outputDirectory, "functions/__nitro.func/index.mjs")).href)).default;
  const response = await runtime?.fetch?.(new Request(`https://riftri.dev${docsBrowserCssPath}`));
  const type = response?.headers.get("content-type") ?? "";
  if (response?.status !== 200 || !type.startsWith("text/css")) {
    throw new Error(`docs runtime did not serve ${docsBrowserCssPath} as CSS (HTTP ${response?.status}, ${type || "no content type"})`);
  }
  const file = path.join(staticDirectory, docsBrowserCssPath);
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, Buffer.from(await response.arrayBuffer()));
  const cacheControl = response.headers.get("cache-control");
  return {
    src: "^/__farm_docs/browser\\.css$",
    headers: { ...(cacheControl && { "Cache-Control": cacheControl }), "Content-Type": type },
    continue: true,
  };
}

// Vercel's error phase runs after the filesystem handle misses, so an unknown
// path serves the prerendered page with a real 404 status instead of the
// platform's generic body.
const notFoundRoute = {
  src: "/.*",
  status: 404,
  dest: "/404",
};

export async function finalizeStaticWebsite(
  outputDirectory = path.join(root, "website/.vercel/output"),
  { docs = false } = {},
) {
  const configPath = path.join(outputDirectory, "config.json");
  const staticDirectory = path.join(outputDirectory, "static");
  const config = JSON.parse(await readFile(configPath, "utf8"));

  if (config.version !== 3) {
    throw new Error(`expected Vercel Build Output API version 3, received ${config.version}`);
  }

  for (const name of [
    "index.html",
    "index.md",
    "install.sh",
    "install.ps1",
    "robots.txt",
    "sitemap.xml",
    "404/index.html",
  ]) {
    await access(path.join(staticDirectory, name));
  }

  await writeFile(path.join(staticDirectory, "build-info.json"), `${JSON.stringify({ revision: currentRevision() })}\n`);

  let docsBrowserCssRoute;
  if (docs) {
    // Fail the build if the official adapter runtime or its content is missing.
    await access(path.join(outputDirectory, "functions/__nitro.func/index.mjs"));
    // Farm.js 0.1.0 bundles content beside Nitro's web-format entry point.
    await access(path.join(outputDirectory, "functions/__nitro.func/farm-docs-content/page.md"));
    // Each page's full reference is served at its own `.md` path.
    for (const page of pages) {
      const reference = page.slug ? path.join(staticDirectory, "docs", `${page.slug}.md`) : path.join(staticDirectory, "docs.md");
      await access(reference);
    }
    docsBrowserCssRoute = await publishDocsBrowserCss(outputDirectory, staticDirectory);
  } else {
    await rm(path.join(outputDirectory, "functions"), { recursive: true, force: true });
    await rm(path.join(outputDirectory, "nitro.json"), { force: true });
  }

  // The rebuilt route table keeps exactly one route from the build output, so
  // a change in how the framework emits it would drop long-term asset caching
  // silently: finalization would still succeed, every test here would still
  // pass, and `verify-website.mjs` checks no asset's Cache-Control. Fail
  // closed instead -- production serves hashed assets `immutable` today.
  const immutableAssetRoute = config.routes?.find(
    (route) => route.headers?.["Cache-Control"]?.includes("immutable"),
  );
  if (!immutableAssetRoute) {
    throw new Error(
      "build output has no immutable asset route; hashed assets would be served without long-term caching",
    );
  }

  const staticConfig = {
    version: 3,
    overrides: {
      "index.html": { path: "" },
      "404/index.html": { path: "404" },
    },
    routes: [
      { src: "^/build-info\\.json$", headers: { "Cache-Control": "no-store", "Content-Type": "application/json" }, continue: true },
      installerRoute,
      markdownRoute,
      docs && {
        src: "^/docs(?:/.*)?\\.md$",
        headers: {
          "Content-Type": "text/plain; charset=utf-8",
          "Content-Disposition": "inline",
          "X-Content-Type-Options": "nosniff",
          "X-Robots-Tag": "noindex",
        },
        continue: true,
      },
      docsBrowserCssRoute,
      immutableAssetRoute,
      { handle: "filesystem" },
      // Only documentation uses SSR. Installers, the landing page, and the raw
      // /index.md guide retain their static responses and existing headers.
      docs && { src: "^/(?:docs(?:/.*|\\.md)?|api/docs)$", dest: "/__nitro", headers: { "Cache-Control": "no-store", "Vary": "x-farm-docs-navigation, Accept" } },
      { handle: "error" },
      notFoundRoute,
    ].filter(Boolean),
  };

  await writeFile(configPath, `${JSON.stringify(staticConfig, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await finalizeStaticWebsite(undefined, { docs: true });
}
