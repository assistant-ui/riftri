const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const url = "https://raw.githubusercontent.com/assistant-ui/riftri/main/package/install.sh";

test("website and guides expose the same canonical Bash installer", () => {
  for (const file of ["website/src/app/page.tsx", "README.md", "docs/install.md"]) {
    const source = fs.readFileSync(path.join(root, file), "utf8");
    assert.ok(source.includes(`curl -fsSL ${url} | bash`), file);
  }
  const page = fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8");
  assert.ok(page.includes('href="/install.sh"'));
  assert.ok(page.includes("docs/install.md#windows-powershell"));
  assert.ok(!page.includes("npm install --global riftri"));
});

test("website stages the canonical installer without maintaining another copy", async (t) => {
  const destination = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-site-installer-"));
  t.after(() => fs.rmSync(destination, { recursive: true, force: true }));
  const { stageWebsiteInstaller } = await import("../scripts/stage-website-installer.mjs");
  await stageWebsiteInstaller(destination);
  assert.deepEqual(fs.readFileSync(path.join(destination, "install.sh")), fs.readFileSync(path.join(root, "package/install.sh")));
  const { scripts } = JSON.parse(fs.readFileSync(path.join(root, "website/package.json"), "utf8"));
  for (const name of ["dev", "build"]) assert.match(scripts[name], /stage-website-installer\.mjs && farm/);
});
