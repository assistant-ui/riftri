import { access, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { currentRevision } from "./verify-website.mjs";

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

  const immutableAssetRoute = config.routes?.find(
    (route) => route.headers?.["Cache-Control"]?.includes("immutable"),
  );

  await writeFile(path.join(staticDirectory, "build-info.json"), `${JSON.stringify({ revision: currentRevision() })}\n`);

  await rm(path.join(outputDirectory, "functions"), { recursive: true, force: true });
  await rm(path.join(outputDirectory, "nitro.json"), { force: true });

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
      immutableAssetRoute,
      { handle: "filesystem" },
      { handle: "error" },
      notFoundRoute,
    ].filter(Boolean),
  };

  await writeFile(configPath, `${JSON.stringify(staticConfig, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await finalizeStaticWebsite();
}
