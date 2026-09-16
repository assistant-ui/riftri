import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
export const currentRevision = () => execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();

export async function verifyWebsite({ baseUrl = "https://riftri.dev", revision = currentRevision(), fetchImpl = fetch } = {}) {
  assert.match(revision, /^[0-9a-f]{40,64}$/, "expected a full Git revision");
  const checked = [];
  async function get(route, status, type) {
    const response = await fetchImpl(new URL(route, baseUrl), {
      headers: { "Cache-Control": "no-cache" }, signal: AbortSignal.timeout(15_000),
    });
    assert.equal(response.status, status, `${route}: HTTP ${response.status}, expected ${status}`);
    assert.ok(response.headers.get("content-type")?.includes(type), `${route}: expected ${type}`);
    checked.push(route);
    return response;
  }
  const info = await (await get("/build-info.json", 200, "application/json")).json();
  assert.equal(info.revision, revision, "deployed revision differs from this checkout");
  const home = await (await get("/", 200, "text/html")).text();
  assert.ok(home.includes("WORKTREE EXAMPLE") && home.includes("Windows / PowerShell"), "homepage is missing the current onboarding");
  assert.ok((home.match(/<link\b[^>]*>/g) || []).some((tag) => tag.includes('rel="canonical"') && tag.includes('href="https://riftri.dev/"')), "homepage canonical link is missing");
  assert.ok((home.match(/<meta\b[^>]*>/g) || []).some((tag) => tag.includes('property="og:image"') && tag.includes('content="https://riftri.dev/og.png"')), "homepage sharing metadata is missing");
  for (const [route, source, type] of [
    ["/index.md", "website/public/index.md", "text/plain"],
    ["/install.sh", "package/install.sh", "text/plain"],
    ["/install.ps1", "package/install.ps1", "text/plain"],
    ["/og.png", "website/public/og.png", "image/png"],
    ["/robots.txt", "website/public/robots.txt", "text/plain"],
    ["/sitemap.xml", "website/public/sitemap.xml", "xml"],
  ]) {
    const response = await get(route, 200, type);
    if (route === "/index.md") assert.match(response.headers.get("content-disposition") || "", /^inline\b/i, "Markdown must open inline, not download");
    assert.ok(Buffer.from(await response.arrayBuffer()).equals(await readFile(path.join(root, source))), `${route}: deployed content differs from the source`);
  }
  const missing = await get("/__riftri_deployment_check_missing__", 404, "text/html");
  assert.ok((await missing.text()).includes("THAT PAGE DOES NOT EXIST_"), "expected the branded 404 page");
  return checked;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const checked = await verifyWebsite({ baseUrl: process.argv[2] || "https://riftri.dev" });
  console.log(`Verified ${currentRevision()}: ${checked.join(", ")}`);
}
