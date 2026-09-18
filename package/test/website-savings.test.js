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

test("savings section keeps a short scope note and expandable benchmark details", () => {
  const page = read("website/src/app/page.tsx");
  const chart = read("website/src/components/savings-map.tsx");
  assert.match(page, /id="savings"/);
  assert.match(page, /<SavingsMap\s*\/>/);
  assert.match(chart, /<figcaption/);
  assert.match(chart, /<dl/);
  assert.match(chart, /aria-hidden="true"/);
  assert.match(chart, /<p id="savings-scope">\s*Source files only on APFS\. Dependencies and builds excluded\.\s*<\/p>/);
  const details = chart.match(/<details className="savings-details">([\s\S]*?)<\/details>/)?.[1];
  assert.ok(details, "benchmark details are collapsed by default");
  assert.match(details, /<summary>Benchmark details<\/summary>/);
  assert.match(details, /data\.reportPath/);
  assert.match(details, /data\.riftriSeconds\.toFixed\(2\)/);
  assert.match(details, /data\.gitSeconds\.toFixed\(2\)/);
  assert.match(details, /<time dateTime=\{data\.date\}/);
  for (const context of ["adjusted", "linguist-generated", "volume level"]) {
    assert.ok(details.includes(context), context);
  }
  assert.doesNotMatch(chart, /not a speed claim|Historical experiment/);
});

test("only backend names animate while the APFS reference figures stay fixed", () => {
  const chart = read("website/src/components/savings-map.tsx");
  const label = read("website/src/components/savings-backend-name.tsx");
  const css = read("website/src/app/globals.css");
  const page = read("website/src/app/page.tsx");
  for (const backend of ["APFS", "Linux reflink", "ReFS"]) assert.ok(label.includes(`"${backend}"`));
  assert.match(chart, /APFS reference measurement/);
  assert.match(chart, /not Linux or Windows measurements/);
  assert.match(chart, /<SavingsBackendName\s*\/>/);
  assert.doesNotMatch(chart, /savings-platforms|savings-panel|useState|useEffect|Not measured/);
  // The cycling names are decorative and carry no pause control, so the
  // accessible text must come from the static description instead.
  assert.doesNotMatch(label, /aria-pressed|<button|useState/);
  assert.match(label, /APFS on macOS, native reflinks on Linux, and ReFS on Windows/);
  const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
  assert.match(reduced, /\.savings-backend-item \{ animation: none/);
  assert.doesNotMatch(css, /\.savings-panel|\.savings-controls/);
  assert.match(page, /Worktree disk usage/);
  assert.doesNotMatch(page, /Ten worktrees\.|A smaller footprint|Same tracked source\. Same number/);
  const savingsCss = css.slice(css.indexOf(".savings-map"), css.indexOf(".start-section"));
  assert.doesNotMatch(savingsCss, /var\(--success\)/);
  assert.match(savingsCss, /var\(--accent\)/);
});
