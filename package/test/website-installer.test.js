const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const siteUrl = "https://riftri.dev";
const url = `${siteUrl}/install.sh`;
const powershellUrl = `${siteUrl}/install.ps1`;

test("website and guides use the public riftri.dev installer URLs", () => {
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

test("public package metadata and installation sources use the custom domain", () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  assert.equal(manifest.homepage, siteUrl);
  const websiteGuide = fs.readFileSync(path.join(root, "website/README.md"), "utf8");
  assert.ok(websiteGuide.includes(siteUrl));
  for (const file of [
    "README.md",
    "docs/install.md",
    "website/README.md",
    "website/src/app/page.tsx",
    "package.json",
    "package/install.sh",
    "package/install.ps1",
  ]) {
    assert.doesNotMatch(fs.readFileSync(path.join(root, file), "utf8"), /riftri\.vercel\.app/, file);
  }
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
