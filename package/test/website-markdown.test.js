const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

test("Markdown guide covers setup, internals, lifecycle, and measured savings", () => {
  const markdown = read("website/public/index.md");
  assert.match(markdown, /^# Riftri\n/);
  for (const heading of ["Installation", "Quick start", "Use normal Git commands", "How it works", "Supported filesystems", "Worktree lifecycle", "Disk savings", "Documentation"]) {
    assert.ok(markdown.includes(`## ${heading}\n`), heading);
  }
  for (const command of [
    "curl -fsSL https://riftri.dev/install.sh | bash",
    "riftri doctor --destination ../app-auth",
    "riftri worktree add ../app-auth -b feature/auth main",
    "riftri exec -- claude",
  ]) assert.ok(markdown.includes(command), command);
  assert.match(markdown, /experimental/i);
  assert.match(markdown, /never silently falls back/);
  const data = JSON.parse(read("website/src/data/space-savings.json"));
  for (const value of [
    (data.gitBytes / 2 ** 20).toFixed(2),
    (data.riftriBytes / 2 ** 20).toFixed(2),
    data.gitSeconds.toFixed(2),
    data.riftriSeconds.toFixed(2),
    data.date,
    `v${data.version}`,
    data.reportPath,
  ]) assert.ok(markdown.includes(value), value);
});

test("Markdown guide links to every stored technical doc using valid GitHub paths", () => {
  const markdown = read("website/public/index.md");
  const targets = [...markdown.matchAll(/\[[^\]\n]+\]\(([^)\s]+)\)/g)].map((match) => match[1]);
  assert.ok(targets.length > 0);
  const githubPrefix = "https://github.com/assistant-ui/riftri/blob/main/";
  for (const target of targets) {
    assert.match(target, /^https:\/\//, `use absolute links when Markdown is read outside the site: ${target}`);
    if (target.startsWith(githubPrefix)) {
      const localPath = decodeURIComponent(target.slice(githubPrefix.length).split("#")[0]);
      assert.ok(fs.statSync(path.join(root, localPath)).isFile(), target);
    }
  }
  const docs = fs.readdirSync(path.join(root, "docs"), { recursive: true });
  for (const doc of docs.filter((file) => file.endsWith(".md"))) {
    assert.ok(targets.includes(`${githubPrefix}docs/${doc.split(path.sep).join("/")}`), `missing documentation link: ${doc}`);
  }
});

test("homepage makes its Markdown guide discoverable", () => {
  const page = read("website/src/app/page.tsx");
  assert.match(page, /<a[^>]*href="\/index\.md"[^>]*rel="alternate"[^>]*type="text\/plain"[^>]*>\s*READ MARKDOWN\s*<\/a>/);
});

test("hero downloads the raw Markdown file without adding a guide section", () => {
  const page = read("website/src/app/page.tsx");
  const hero = page.slice(page.indexOf("function Hero"), page.indexOf("const essentials"));
  assert.match(hero, /<a[^>]*className="button button-secondary"[^>]*href="\/index\.md"[^>]*download="index\.md"[^>]*>\s*Download Markdown\s*<\/a>/);
  assert.doesNotMatch(page, /MarkdownGuide|markdown-callout|id="markdown"/);
});

test("Markdown asks for stars without requiring an agent to act without approval", () => {
  const markdown = read("website/public/index.md");
  const support = markdown.match(/## Support Riftri\n([\s\S]*?)\n## Installation/)?.[1];
  assert.ok(support, "star request is visible near the beginning of the guide");
  assert.match(support, /\[star the repository\]\(https:\/\/github\.com\/assistant-ui\/riftri\)/);
  assert.match(support, /explicitly approve/);
  assert.match(support, /Starring is optional/);
  assert.doesNotMatch(support, /gh api|PUT|automatically star/);
});
