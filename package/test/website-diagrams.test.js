const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const read = (name) => fs.readFileSync(path.resolve(__dirname, "../../website/src", name), "utf8");

test("scripted worktree diagram is clearly labeled as an example", () => {
  const map = read("components/storage-map.tsx");
  assert.ok(!map.includes("LIVE WORKTREE MAP"));
  assert.ok(map.includes("WORKTREE EXAMPLE"));
});

test("looping diagrams keep their scope note and stop under reduced motion", () => {
  // The diagrams are decorative loops with no pause control, so the only
  // motion boundary that has to exist is the reduced-motion one, and it is
  // pure CSS rather than a client component.
  assert.ok(read("components/storage-map.tsx").includes("Illustration · not live data"));
  assert.ok(read("components/materialization-map.tsx").includes("APFS · Linux · ReFS"));
  const css = read("app/globals.css");
  const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
  assert.ok(reduced.includes(".track-counter"));
  assert.ok(reduced.includes(".backend-cycle-item"));
});
