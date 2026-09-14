const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const url = "https://riftri.vercel.app/install.sh";
const powershellUrl = "https://riftri.vercel.app/install.ps1";

test("website and guides use the public Vercel installer URL", () => {
  for (const file of ["website/src/app/page.tsx", "README.md", "docs/install.md"]) {
    const source = fs.readFileSync(path.join(root, file), "utf8");
    assert.ok(source.includes(`curl -fsSL ${url} | bash`), file);
  }
  for (const file of ["README.md", "docs/install.md"]) {
    const source = fs.readFileSync(path.join(root, file), "utf8");
    assert.ok(source.includes(powershellUrl), file);
  }
  const page = fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8");
  assert.ok(page.includes('href="/install.sh"'));
  assert.ok(page.includes('href="/install.ps1"'));
  assert.ok(page.includes("docs/install.md#windows-powershell"));
  assert.ok(!page.includes("npm install --global riftri"));
});

test("website stages the canonical installer without maintaining another copy", async (t) => {
  const destination = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-site-installer-"));
  t.after(() => fs.rmSync(destination, { recursive: true, force: true }));
  const { stageWebsiteInstaller } = await import("../scripts/stage-website-installer.mjs");
  await stageWebsiteInstaller(destination);
  assert.deepEqual(fs.readFileSync(path.join(destination, "install.sh")), fs.readFileSync(path.join(root, "package/install.sh")));
  assert.deepEqual(fs.readFileSync(path.join(destination, "install.ps1")), fs.readFileSync(path.join(root, "package/install.ps1")));
  const { scripts } = JSON.parse(fs.readFileSync(path.join(root, "website/package.json"), "utf8"));
  for (const name of ["dev", "build"]) assert.match(scripts[name], /stage-website-installer\.mjs && farm/);
});

test("hero and quick start reuse one install command and the same quick-start column styling", () => {
  const page = fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8");
  const css = fs.readFileSync(path.join(root, "website/src/app/globals.css"), "utf8");
  const hero = page.slice(page.indexOf("function Hero()"), page.indexOf("const essentials"));
  const steps = page.slice(page.indexOf("const startSteps"), page.indexOf("function GetStarted()"));
  assert.ok(page.includes(`const installCommand = "curl -fsSL ${url} | bash";`));
  assert.match(hero, /<CopyCommand command=\{installCommand\}/);
  assert.match(steps, /command: installCommand/);
  assert.doesNotMatch(css, /\.start-list li:first-child \.command/);
});
