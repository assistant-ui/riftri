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

test("both looping diagrams have a named user-controlled motion boundary", () => {
  for (const file of ["storage-map.tsx", "materialization-map.tsx"]) {
    assert.ok(read(`components/${file}`).includes("<MotionFigure"));
  }
  const boundary = read("components/motion-figure.tsx");
  assert.ok(boundary.includes("aria-pressed={paused}"));
  assert.ok(boundary.includes("disabled={reducedMotion}"));
  assert.ok(boundary.includes("data-paused={paused}"));
  const css = read("app/globals.css");
  assert.ok(css.includes('.motion-figure[data-paused="true"]'));
});
