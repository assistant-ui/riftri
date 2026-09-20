const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");

test("every public page has a concise human source separate from the full reference", async () => {
  const { pages } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) {
    assert.match(page.content, /^website\/content\/[\w/.-]+\.md$/);
    assert.notEqual(page.content, page.source);
    const human = fs.readFileSync(path.join(root, page.content), "utf8");
    assert.ok(human.trim().split(/\s+/).length <= 450, `${page.slug}: keep the public guide concise`);
    assert.match(human, /^# /);
  }
});

test("agent companions retain the full canonical document and link to other full references", async () => {
  const { pages, renderAgentDoc, rewriteDocLinks } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) {
    const original = fs.readFileSync(path.join(root, page.source), "utf8");
    const agent = renderAgentDoc(page, original);
    assert.ok(agent.includes(rewriteDocLinks(original, page.source, { audience: "agent" }).trim()), page.source);
    assert.ok(agent.includes(`https://github.com/assistant-ui/riftri/blob/main/${page.source}`));
    assert.ok(agent.includes(`https://riftri.dev/docs${page.slug ? `/${page.slug}` : ""}`));
  }
  assert.equal(rewriteDocLinks("[CLI](cli.md#exit-codes)", "docs/install.md", { audience: "agent" }),
    "[CLI](https://riftri.dev/docs/cli.md#exit-codes)");
});

test("staging separates public search content from complete agent companions", async (t) => {
  const { pages, stageWebsiteDocs, renderAgentDoc } = await import("../scripts/stage-website-docs.mjs");
  const fixture = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-docs-stage-"));
  t.after(() => fs.rmSync(fixture, { recursive: true, force: true }));
  for (const page of pages) for (const file of [page.source, page.content]) {
    fs.mkdirSync(path.dirname(path.join(fixture, file)), { recursive: true });
    fs.copyFileSync(path.join(root, file), path.join(fixture, file));
  }
  const stale = path.join(fixture, "website/public/docs/retired.md");
  fs.mkdirSync(path.dirname(stale), { recursive: true });
  fs.writeFileSync(stale, "retired reference");
  const keep = path.join(fixture, "website/public/docs/keep.txt");
  fs.writeFileSync(keep, "unrelated asset");
  await stageWebsiteDocs(fixture);
  assert.ok(!fs.existsSync(stale), "a retired page's reference is removed");
  assert.ok(fs.existsSync(keep), "unrelated public assets are left in place");
  for (const page of pages) {
    const publicFile = path.join(fixture, "website/src/app/docs", page.slug, "page.md");
    const reference = page.slug
      ? path.join(fixture, "website/public/docs", `${page.slug}.md`)
      : path.join(fixture, "website/public/docs.md");
    const original = fs.readFileSync(path.join(root, page.source), "utf8");
    assert.equal(fs.readFileSync(reference, "utf8"), renderAgentDoc(page, original));
    assert.ok(fs.readFileSync(publicFile, "utf8").includes("[Copy .md]"));
    // The rendered HTML page source stays concise; the full reference lives at
    // the page's own `.md`, never as a separate agent.md route.
    assert.ok(!fs.existsSync(path.join(path.dirname(publicFile), "agent.md")));
  }
  const sitemap = fs.readFileSync(path.join(fixture, "website/public/sitemap.xml"), "utf8");
  assert.doesNotMatch(sitemap, /agent\.md/);
});

test("all root-relative links in public guides resolve to a page, mirror, or public asset", async () => {
  const { pages, agentDocUrl, renderWebsiteDoc } = await import("../scripts/stage-website-docs.mjs");
  const routes = new Set(["/install.sh", "/install.ps1", "/index.md"]);
  for (const page of pages) {
    const route = `/docs${page.slug ? `/${page.slug}` : ""}`;
    for (const url of [route, `${route}.md`, agentDocUrl(page)]) routes.add(url);
  }
  for (const page of pages) {
    const human = renderWebsiteDoc(page, fs.readFileSync(path.join(root, page.content), "utf8"));
    for (const match of human.matchAll(/\]\((\/[^\s)]+)[^)]*\)/g)) {
      assert.ok(routes.has(match[1]), `${page.content}: unknown local link ${match[1]}`);
    }
  }
});

test("every staged page has one direct Markdown action and a short canonical edit link", async () => {
  const { pages, renderWebsiteDoc } = await import("../scripts/stage-website-docs.mjs");
  for (const page of pages) {
    const human = fs.readFileSync(path.join(root, page.content), "utf8");
    const result = renderWebsiteDoc(page, human);
    const markdownUrl = `/docs${page.slug ? `/${page.slug}` : ""}.md`;
    assert.equal(result.split(`[View .md](${markdownUrl} `).length - 1, 1);
    // The action row sits below the title and intro paragraph (description
    // first), and always before the page's first section heading.
    assert.match(
      result,
      /^---\ntitle: [^\n]+\n---\n\n# [^\n]+\n\n(?:(?!\[View \.md\])(?!#)[^\n]+\n)*\n?\[View \.md\]/,
    );
    const actionIndex = result.indexOf("[View .md](");
    const firstSection = result.indexOf("\n## ");
    assert.ok(firstSection === -1 || actionIndex < firstSection, `action must precede the first section: ${page.content}`);
    assert.ok(result.includes(`[View .md](${markdownUrl} "View this page as Markdown") / [Copy .md](${markdownUrl} `));
    assert.ok(result.includes(`[Edit on GitHub](https://github.com/assistant-ui/riftri/blob/main/${page.content} `));
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
    assert.match(page.source, /^(README\.md|docs\/[\w/.-]+\.md)$/);
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
