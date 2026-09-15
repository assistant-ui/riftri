const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

test("website savings measurements match the recorded assistant-ui experiment", () => {
  const data = JSON.parse(read("website/src/data/space-savings.json"));
  assert.equal(data.platform, "macos", "these measurements must remain scoped to macOS");
  const report = read(data.reportPath);
  const bytes = (value) => `${value.toLocaleString("en-US")} B`;
  assert.ok(report.includes(`\`riftri ${data.version}\``));
  assert.ok(report.includes(`| Worktrees | ${data.worktrees} | ${data.worktrees} |`));
  assert.ok(report.includes(`| Tracked files per view | ${data.filesPerView.toLocaleString("en-US")} |`));
  assert.ok(report.includes(`${bytes(data.gitBytes)} (${(data.gitBytes / 2 ** 20).toFixed(2)} MiB)`));
  assert.ok(report.includes(`${bytes(data.riftriBytes)} (${(data.riftriBytes / 2 ** 20).toFixed(2)} MiB)`));
  assert.ok(report.includes(`| Creation time, excluding allocation-sampling pauses | ${data.gitSeconds.toFixed(2)} s | ${data.riftriSeconds.toFixed(2)} s |`));
  assert.equal(((1 - data.riftriBytes / data.gitBytes) * 100).toFixed(1), "87.0");
  assert.equal(((data.gitBytes - data.riftriBytes) / 2 ** 20).toFixed(2), "673.39");
});

test("savings section retains benchmark scope, tradeoff, and a source link", () => {
  const page = read("website/src/app/page.tsx");
  const chart = read("website/src/components/savings-map.tsx");
  assert.match(page, /id="savings"/);
  assert.match(page, /<SavingsMap\s*\/>/);
  assert.match(chart, /<figcaption/);
  assert.match(chart, /<dl/);
  assert.match(chart, /aria-hidden="true"/);
  assert.match(chart, /data\.reportPath/);
  for (const limitation of ["Historical", "adjusted", "linguist-generated", "dependencies", "slower", "not a speed claim"]) {
    assert.ok(chart.includes(limitation), limitation);
  }
});

test("savings platforms cycle without presenting APFS measurements as cross-platform results", () => {
  const chart = read("website/src/components/savings-map.tsx");
  const css = read("website/src/app/globals.css");
  const page = read("website/src/app/page.tsx");
  for (const platform of ["macos", "linux", "windows"]) assert.ok(chart.includes(`id: "${platform}"`));
  assert.match(chart, /platform\.id === data\.platform/);
  assert.match(chart, /Not measured/);
  assert.match(chart, /Pause cycle/);
  assert.match(chart, /aria-pressed/);
  assert.match(chart, /prefers-reduced-motion: reduce/);
  assert.match(chart, /clearInterval/);
  assert.match(chart, /IntersectionObserver/);
  assert.match(page, /Worktree disk usage/);
  assert.doesNotMatch(page, /Ten worktrees\.|A smaller footprint|Same tracked source\. Same number/);
  const savingsCss = css.slice(css.indexOf(".savings-map"), css.indexOf(".start-section"));
  assert.doesNotMatch(savingsCss, /var\(--success\)/);
  assert.match(savingsCss, /var\(--accent\)/);
});
