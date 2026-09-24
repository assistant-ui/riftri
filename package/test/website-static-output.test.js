const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

test("website finalization preserves installers and Markdown without a server runtime", async (t) => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-static-site-"));
  t.after(() => fs.rmSync(output, { recursive: true, force: true }));

  fs.mkdirSync(path.join(output, "static"), { recursive: true });
  fs.mkdirSync(path.join(output, "functions", "__nitro.func"), { recursive: true });
  const markdown = "# Riftri\n\n## Installation\n\n```sh\ncurl -fsSL https://riftri.dev/install.sh | bash\n```\n";
  for (const name of ["index.html", "index.md", "install.sh", "install.ps1", "robots.txt", "sitemap.xml"]) {
    fs.writeFileSync(path.join(output, "static", name), name === "index.md" ? markdown : name);
  }
  fs.mkdirSync(path.join(output, "static", "404"));
  fs.writeFileSync(path.join(output, "static", "404", "index.html"), "404");
  fs.writeFileSync(path.join(output, "nitro.json"), "{}");
  fs.writeFileSync(
    path.join(output, "config.json"),
    JSON.stringify({
      version: 3,
      routes: [
        {
          src: "^/assets/.+$",
          headers: { "Cache-Control": "public, max-age=31536000, immutable" },
          continue: true,
        },
        { handle: "filesystem" },
        { src: "/(.*)", dest: "/__nitro" },
      ],
    }),
  );

  const { finalizeStaticWebsite } = await import("../scripts/finalize-static-website.mjs");
  await finalizeStaticWebsite(output);

  assert.equal(fs.existsSync(path.join(output, "functions")), false);
  assert.equal(fs.existsSync(path.join(output, "nitro.json")), false);
  const config = JSON.parse(fs.readFileSync(path.join(output, "config.json"), "utf8"));
  assert.equal(config.version, 3);
  const { currentRevision } = await import("../scripts/verify-website.mjs");
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, "static/build-info.json"), "utf8")), { revision: currentRevision() });
  assert.equal(config.routes.find((route) => route.src === "^/build-info\\.json$").headers["Cache-Control"], "no-store");
  assert.deepEqual(config.overrides, {
    "index.html": { path: "" },
    "404/index.html": { path: "404" },
  });
  assert.ok(config.routes.some((route) => route.handle === "filesystem"));
  assert.ok(config.routes.some((route) => route.src === "/install\\.(?:sh|ps1)"));
  assert.deepEqual(config.routes.find((route) => route.src === "^/index\\.md$"), {
    src: "^/index\\.md$",
    headers: {
      "Cache-Control": "public, max-age=300",
      "Content-Type": "text/plain; charset=utf-8",
      "Content-Disposition": "inline; filename=\"index.md\"",
      "X-Content-Type-Options": "nosniff",
    },
    continue: true,
  });
  assert.equal(fs.readFileSync(path.join(output, "static/index.md"), "utf8"), markdown);
  assert.ok(config.routes.every((route) => route.dest !== "/__nitro"));

  // The error phase must come after the filesystem handle, or every request
  // would fall through to the 404 page.
  const filesystemIndex = config.routes.findIndex((route) => route.handle === "filesystem");
  const missIndex = config.routes.findIndex((route) => route.handle === "miss");
  const errorIndex = config.routes.findIndex((route) => route.handle === "error");
  assert.ok(missIndex > filesystemIndex, "the miss phase runs after the filesystem check");
  assert.ok(errorIndex > missIndex, "the error phase runs after the miss phase");
  const notFound = { src: "/.*", status: 404, dest: "/404" };
  // A static top-level miss reaches the branded page only through the miss
  // phase; without it Vercel answers unknown paths with its own plain-text
  // 404. The error-phase copy still catches a 404 the docs function returns.
  assert.deepEqual(config.routes[missIndex + 1], notFound, "the miss phase serves the branded 404");
  assert.deepEqual(config.routes.at(-1), notFound, "the error phase still serves the branded 404");
});

test("website finalization rejects a build missing its crawler files", async (t) => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-static-seo-"));
  t.after(() => fs.rmSync(output, { recursive: true, force: true }));
  fs.mkdirSync(path.join(output, "static"));
  fs.mkdirSync(path.join(output, "functions"));
  for (const name of ["index.html", "index.md", "install.sh", "install.ps1"]) {
    fs.writeFileSync(path.join(output, "static", name), name);
  }
  fs.writeFileSync(path.join(output, "config.json"), JSON.stringify({ version: 3 }));
  const { finalizeStaticWebsite } = await import("../scripts/finalize-static-website.mjs");
  await assert.rejects(finalizeStaticWebsite(output), { code: "ENOENT" });
  assert.ok(fs.existsSync(path.join(output, "functions")));
});

test("website finalization rejects a build missing its Markdown guide before cleanup", async (t) => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-static-markdown-"));
  t.after(() => fs.rmSync(output, { recursive: true, force: true }));
  fs.mkdirSync(path.join(output, "static"));
  fs.mkdirSync(path.join(output, "functions"));
  for (const name of ["index.html", "install.sh", "install.ps1"]) {
    fs.writeFileSync(path.join(output, "static", name), name);
  }
  fs.writeFileSync(path.join(output, "config.json"), JSON.stringify({ version: 3 }));
  const { finalizeStaticWebsite } = await import("../scripts/finalize-static-website.mjs");
  await assert.rejects(finalizeStaticWebsite(output), { code: "ENOENT" });
  assert.ok(fs.existsSync(path.join(output, "functions")));
});

test("website build always runs static output finalization", () => {
  const root = path.resolve(__dirname, "../..");
  const { scripts } = JSON.parse(
    fs.readFileSync(path.join(root, "website/package.json"), "utf8"),
  );
  assert.match(scripts.build, /farm build && node \.\.\/package\/scripts\/finalize-static-website\.mjs$/);
});

test("docs finalization retains only the narrow docs runtime routes", async (t) => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-docs-output-"));
  t.after(() => fs.rmSync(output, { recursive: true, force: true }));
  fs.mkdirSync(path.join(output, "static/404"), { recursive: true });
  for (const name of ["index.html", "index.md", "install.sh", "install.ps1", "robots.txt", "sitemap.xml", "404/index.html"]) {
    fs.writeFileSync(path.join(output, "static", name), name);
  }
  fs.writeFileSync(path.join(output, "config.json"), JSON.stringify({ version: 3 }));
  const { finalizeStaticWebsite } = await import("../scripts/finalize-static-website.mjs");
  await assert.rejects(finalizeStaticWebsite(output, { docs: true }), { code: "ENOENT" });
  const runtime = path.join(output, "functions/__nitro.func");
  fs.mkdirSync(path.join(runtime, "chunks/nitro/farm-docs-content"), { recursive: true });
  fs.writeFileSync(path.join(runtime, "index.mjs"), "export default {};");
  fs.writeFileSync(path.join(runtime, "chunks/nitro/farm-docs-content/page.md"), "# Docs");
  await assert.rejects(finalizeStaticWebsite(output, { docs: true }), { code: "ENOENT" });
  const { pages } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) {
    const reference = page.slug
      ? path.join(output, "static/docs", `${page.slug}.md`)
      : path.join(output, "static/docs.md");
    fs.mkdirSync(path.dirname(reference), { recursive: true });
    fs.writeFileSync(reference, "# Full reference");
  }
  await finalizeStaticWebsite(output, { docs: true });
  assert.ok(fs.existsSync(path.join(runtime, "index.mjs")));
  const config = JSON.parse(fs.readFileSync(path.join(output, "config.json")));
  const markdownRoute = config.routes.find((entry) => entry.headers?.["Content-Disposition"] === "inline");
  assert.equal(markdownRoute.headers["Content-Type"], "text/plain; charset=utf-8");
  assert.equal(markdownRoute.headers["X-Content-Type-Options"], "nosniff");
  assert.equal(markdownRoute.continue, true);
  // The reference `.md` files reuse the general Markdown route, which also
  // carries the noindex header; there is no separate agent.md route.
  assert.equal(markdownRoute.headers["X-Robots-Tag"], "noindex");
  assert.ok(!config.routes.some((entry) => entry.src === "^/docs(?:/.*)?/agent\\.md$"));
  for (const url of ["/docs.md", "/docs/cli.md", "/docs/benchmarks/assistant-ui.md"]) assert.ok(new RegExp(markdownRoute.src).test(url));
  for (const url of ["/docs", "/docs/cli", "/api/docs", "/docs-unrelated.md"]) assert.ok(!new RegExp(markdownRoute.src).test(url));
  const route = config.routes.find((entry) => entry.dest === "/__nitro");
  const pattern = new RegExp(route.src);
  for (const url of ["/docs", "/docs.md", "/docs/cli", "/docs/cli.md", "/api/docs"]) assert.ok(pattern.test(url), url);
  for (const url of ["/", "/index.md", "/install.sh", "/api/unknown", "/api/docs/mcp", "/docs-unrelated"]) assert.ok(!pattern.test(url), url);
  assert.equal(route.headers["Cache-Control"], "no-store");
  assert.match(route.headers.Vary, /x-farm-docs-navigation/);
  assert.ok(config.routes.indexOf(route) > config.routes.findIndex((entry) => entry.handle === "filesystem"));
  assert.ok(config.routes.indexOf(route) < config.routes.findIndex((entry) => entry.handle === "error"));
});
