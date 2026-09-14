const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

test("website finalization removes the Nitro runtime and preserves static installers", async (t) => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-static-site-"));
  t.after(() => fs.rmSync(output, { recursive: true, force: true }));

  fs.mkdirSync(path.join(output, "static"), { recursive: true });
  fs.mkdirSync(path.join(output, "functions", "__nitro.func"), { recursive: true });
  for (const name of ["index.html", "install.sh", "install.ps1"]) {
    fs.writeFileSync(path.join(output, "static", name), name);
  }
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
  assert.deepEqual(config.overrides, { "index.html": { path: "" } });
  assert.ok(config.routes.some((route) => route.handle === "filesystem"));
  assert.ok(config.routes.some((route) => route.src === "/install\\.(?:sh|ps1)"));
  assert.ok(config.routes.every((route) => route.dest !== "/__nitro"));
});

test("website build always runs static output finalization", () => {
  const root = path.resolve(__dirname, "../..");
  const { scripts } = JSON.parse(
    fs.readFileSync(path.join(root, "website/package.json"), "utf8"),
  );
  assert.match(scripts.build, /farm build && node \.\.\/package\/scripts\/finalize-static-website\.mjs$/);
});
