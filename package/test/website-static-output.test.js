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
  const errorIndex = config.routes.findIndex((route) => route.handle === "error");
  assert.ok(errorIndex > filesystemIndex);
  assert.deepEqual(config.routes.at(-1), { src: "/.*", status: 404, dest: "/404" });
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
