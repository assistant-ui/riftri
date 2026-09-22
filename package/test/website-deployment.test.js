const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");
const revision = "a".repeat(40);

async function fixture() {
  const files = new Map();
  for (const [route, source, type] of [
    ["/index.md", "website/public/index.md", "text/plain"],
    ["/install.sh", "package/install.sh", "text/plain"],
    ["/install.ps1", "package/install.ps1", "text/plain"],
    ["/og.png", "website/public/og.png", "image/png"],
    ["/robots.txt", "website/public/robots.txt", "text/plain"],
    ["/sitemap.xml", "website/public/sitemap.xml", "application/xml"],
  ]) files.set(route, { body: await fs.readFile(path.join(root, source)), type });
  files.set("/", { body: '<html><link rel="canonical" href="https://riftri.dev/"/><meta property="og:image" content="https://riftri.dev/og.png"/>WORKTREE EXAMPLE Windows / PowerShell</html>', type: "text/html" });
  files.set("/build-info.json", { body: JSON.stringify({ revision }), type: "application/json" });
  files.set("/__riftri_deployment_check_missing__", { body: "<html>THAT PAGE DOES NOT EXIST_</html>", type: "text/html", status: 404 });
  for (const route of ["/docs", "/docs/installation"]) files.set(route, { body: '<html><meta name="generator" content="@farming-labs/farmjs"/><div id="farm-docs-root">Docs</div></html>', type: "text/html" });
  files.set("/api/docs", { body: JSON.stringify([{ content: "OverlayFS", url: "/docs/linux-overlayfs" }]), type: "application/json" });
  const { pages, agentDocUrl, renderAgentDoc } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) files.set(agentDocUrl(page), {
    body: renderAgentDoc(page, await fs.readFile(path.join(root, page.source), "utf8")), type: "text/plain",
  });
  return files;
}

function fetchFixture(files) {
  return async (url) => {
    const route = new URL(url).pathname;
    const file = files.get(route);
    if (!file) return new Response("Missing", { status: 404 });
    return new Response(file.body, { status: file.status || 200, headers: {
      "content-type": file.type,
      ...(route.endsWith(".md") ? { "content-disposition": file.disposition || 'inline' } : {}),
    } });
  };
}

test("deployment check validates the revision and every public entry point", async () => {
  const { verifyWebsite } = await import("../scripts/verify-website.mjs");
  const checked = await verifyWebsite({ revision, fetchImpl: fetchFixture(await fixture()) });
  assert.equal(checked.length, 31);
});

for (const [name, mutate, error] of [
  ["stale revision", (f) => { f.get("/build-info.json").body = JSON.stringify({ revision: "b".repeat(40) }); }, /revision/],
  ["missing sharing image", (f) => f.delete("/og.png"), /og.png.*404/],
  ["downloaded Markdown", (f) => { f.get("/index.md").disposition = "attachment"; }, /inline/],
  ["stale installer", (f) => { f.get("/install.sh").body = "outdated"; }, /install.sh.*differs/],
  ["generic 404", (f) => { f.get("/__riftri_deployment_check_missing__").body = "Not found"; }, /branded 404/],
  ["missing docs runtime", (f) => f.delete("/docs"), /docs.*404/],
  ["broken docs search", (f) => { f.get("/api/docs").body = "[]"; }, /search results/],
  ["missing agent reference", (f) => f.delete("/docs/installation.md"), /installation\.md.*404/],
  ["abridged agent reference", (f) => { f.get("/docs/installation.md").body = "# Short version"; }, /full agent reference differs/],
  ["downloaded agent reference", (f) => { f.get("/docs/installation.md").disposition = "attachment"; }, /agent Markdown must open inline/],
]) test(`deployment check rejects ${name}`, async () => {
  const { verifyWebsite } = await import("../scripts/verify-website.mjs");
  const files = await fixture();
  mutate(files);
  await assert.rejects(verifyWebsite({ revision, fetchImpl: fetchFixture(files) }), error);
});
