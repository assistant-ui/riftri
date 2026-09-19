const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");

test("every staged page has one direct Markdown action and a short canonical edit link", async () => {
  const { pages, renderWebsiteDoc } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) {
    const original = fs.readFileSync(path.join(root, page.source), "utf8");
    const result = renderWebsiteDoc(page, original);
    const markdownUrl = `/docs${page.slug ? `/${page.slug}` : ""}.md`;
    assert.equal(result.split(`[View .md](${markdownUrl} `).length - 1, 1);
    assert.match(result, /^---\ntitle: [^\n]+\n---\n\n# [^\n]+\n\n\[View \.md\]/);
    assert.ok(result.includes(`[Edit on GitHub](https://github.com/assistant-ui/riftri/blob/main/${page.source} `));
    assert.doesNotMatch(result, /\[Edit this page on GitHub\]/);
  }
});

test("every docs page has a generated SVG icon and the intro omits the closing promotion", () => {
  const groups = JSON.parse(fs.readFileSync(path.join(root, "website/content/docs.json")));
  const icons = JSON.parse(fs.readFileSync(path.join(root, "website/content/docs-icons.json")));
  for (const page of groups.flatMap((group) => group.pages)) {
    assert.match(icons[page.icon], /^<svg\b[^>]*>[\s\S]+<\/svg>$/);
  }
  const intro = fs.readFileSync(path.join(root, "website/content/introduction.md"), "utf8");
  assert.doesNotMatch(intro, /Built in the open|Documentation powered by|Source and issues|Release downloads/);
});

test("docs navigation uses unique routes backed by canonical Markdown", async () => {
  const { pages } = await import("../scripts/stage-website-docs.mjs");
  assert.equal(new Set(pages.map((page) => page.slug)).size, pages.length);
  assert.equal(pages.filter((page) => page.slug === "").length, 1);
  for (const page of pages) {
    assert.match(page.source, /^(docs|website\/content)\/[\w/.-]+\.md$/);
    assert.match(fs.readFileSync(path.join(root, page.source), "utf8"), /^#|\n#/);
  }
  const repositoryOnly = new Set(["benchmarks.md", "allocation-evidence.md", "decisions.md"]);
  for (const name of fs.readdirSync(path.join(root, "docs"))) {
    if (!name.endsWith(".md") || name === "README.md") continue;
    if (repositoryOnly.has(name)) {
      assert.ok(!pages.some((page) => page.source === `docs/${name}`), `keep docs/${name} on GitHub`);
      continue;
    }
    assert.ok(pages.some((page) => page.source === `docs/${name}`), `missing docs/${name}`);
  }
  assert.ok(!pages.some((page) => page.source.startsWith("docs/benchmarks/")));
});

test("docs links stay local where possible without changing command examples", async () => {
  const { rewriteDocLinks } = await import("../scripts/stage-website-docs.mjs");
  const input = "[CLI](cli.md#worktree-add) [source](../crates/riftri-core/src/lib.rs)\n[remote](https://example.com) [anchor](#intro)\n```sh\nprintf '[CLI](cli.md)'\n```\n[ref]: install.md";
  const actual = rewriteDocLinks(input, "docs/README.md");
  assert.match(actual, /\[CLI\]\(\/docs\/cli#worktree-add\)/);
  assert.match(actual, /https:\/\/github.com\/assistant-ui\/riftri\/blob\/main\/crates\/riftri-core\/src\/lib.rs/);
  assert.match(actual, /printf '\[CLI\]\(cli.md\)'/);
  assert.match(actual, /\[ref\]: \/docs\/installation/);
  assert.match(actual, /\[anchor\]\(#intro\)/);
  assert.equal(rewriteDocLinks("[CLI](cli.md?view=full#setup)", "docs/README.md"), "[CLI](/docs/cli?view=full#setup)");
  for (const target of ["benchmarks.md", "allocation-evidence.md", "decisions.md", "benchmarks/assistant-ui-ten-agents-2026-09-12.md"]) {
    assert.equal(rewriteDocLinks(`[Details](${target})`, "docs/README.md"), `[Details](https://github.com/assistant-ui/riftri/blob/main/docs/${target})`);
  }
});

test("homepage links to the docs site and builds stage canonical content", () => {
  const { scripts } = JSON.parse(fs.readFileSync(path.join(root, "website/package.json")));
  assert.match(scripts.stage, /stage-website-docs\.mjs/);
  for (const name of ["dev", "build"]) assert.match(scripts[name], /^pnpm stage && farm/);
  assert.match(fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8"), /href="\/docs">Docs/);
});
