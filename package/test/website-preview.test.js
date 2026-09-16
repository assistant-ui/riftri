const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

test("static preview honors route overrides, branded errors, HEAD, and containment", async (t) => {
  const output = await fs.mkdtemp(path.join(os.tmpdir(), "riftri-preview-test-"));
  t.after(() => fs.rm(output, { recursive: true, force: true }));
  const root = path.join(output, "static");
  await fs.mkdir(path.join(root, "404"), { recursive: true });
  await fs.writeFile(path.join(root, "index.html"), "home");
  await fs.writeFile(path.join(root, "404/index.html"), "branded error");
  await fs.writeFile(path.join(root, "index.md"), "# Riftri");
  await fs.writeFile(path.join(output, "private.txt"), "not public");
  await fs.symlink(output, path.join(root, "escape"), process.platform === "win32" ? "junction" : "dir");
  await fs.writeFile(path.join(output, "config.json"), JSON.stringify({
    version: 3, overrides: { "index.html": { path: "" }, "404/index.html": { path: "404" } },
    routes: [
      { src: "^/index\\.md$", headers: { "Content-Type": "text/plain", "Content-Disposition": "inline" }, continue: true },
      { handle: "filesystem" }, { handle: "error" }, { src: "/.*", dest: "/404", status: 404 },
    ],
  }));
  const { createStaticPreview } = await import("../../website/scripts/serve-static.mjs");
  const server = await createStaticPreview(output);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => { server.close(resolve); server.closeAllConnections(); }));
  const get = (route, options) => fetch(`http://127.0.0.1:${server.address().port}${route}`, options);
  for (const [route, status, body] of [["/", 200, "home"], ["/404", 200, "branded error"], ["/missing", 404, "branded error"]]) {
    const response = await get(route);
    assert.equal(response.status, status);
    assert.equal(await response.text(), body);
    assert.match(response.headers.get("content-type"), /text\/html/);
  }
  const markdown = await get("/index.md");
  assert.equal(markdown.headers.get("content-disposition"), "inline");
  assert.equal(await markdown.text(), "# Riftri");
  const head = await get("/missing", { method: "HEAD" });
  assert.equal(head.status, 404);
  assert.equal(await head.text(), "");
  assert.equal(Number(head.headers.get("content-length")), "branded error".length);
  assert.equal((await get("/escape/private.txt")).status, 403);
  assert.equal((await get("/%zz")).status, 400);
  assert.equal((await get("/", { method: "POST" })).status, 405);
});
