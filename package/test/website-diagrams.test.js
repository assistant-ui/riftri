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

test("looping diagrams stop under reduced motion without a pause control", () => {
  // The diagrams are decorative loops with no pause control, so the only
  // motion boundary that has to exist is the reduced-motion one. The backend
  // name stops in CSS; the drawn figures read the same preference and hold a
  // finished pose instead of starting their frame loop.
  const css = read("app/globals.css");
  const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
  assert.ok(reduced.includes(".backend-cycle-item"));
  for (const name of ["components/figure-kit.ts", "components/materialization-track.tsx"]) {
    assert.ok(read(name).includes('matchMedia("(prefers-reduced-motion: reduce)")'), name);
  }
});
