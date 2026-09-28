"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");

function workflow(name) {
  return fs.readFileSync(path.join(root, ".github/workflows", name), "utf8");
}

const TRIPLE = /^[a-z0-9_]+(?:-[a-z0-9_]+)+$/;

/**
 * Every target named by a workflow's matrices, in either spelling YAML allows:
 * `- target: <triple>` for a matrix of objects, and a plain `- <triple>` list
 * under a `target:` key for a matrix of one dimension.
 */
function matrixTargets(text) {
  // `target: <triple>`, whether it opens a list item or continues one.
  const found = [...text.matchAll(/^\s+(?:- )?target: ([a-z0-9_-]+)\s*$/gm)].map(
    (match) => match[1],
  );

  const lines = text.split("\n");
  for (const [index, line] of lines.entries()) {
    if (!/^\s+target:\s*$/.test(line)) {
      continue;
    }
    for (const next of lines.slice(index + 1)) {
      const item = /^\s+- ([a-z0-9_-]+)\s*$/.exec(next);
      if (!item) {
        break;
      }
      found.push(item[1]);
    }
  }

  const triples = found.filter((value) => TRIPLE.test(value));
  assert.ok(triples.length > 0, "no target triples were found; the scanner matched nothing");
  return [...new Set(triples)].sort();
}

/** The `cross-targets` job's own matrix. */
function crossTargetJob() {
  const ci = workflow("ci.yml");
  const match = /\n {2}cross-targets:\n([\s\S]*?)(?=\n {2}[a-z][\w-]*:\n|$)/.exec(ci);
  assert.ok(match, "ci.yml must define a cross-targets job");
  return match[1];
}

test("CI type-checks every target the release ships", () => {
  const released = matrixTargets(workflow("release.yml"));
  const checked = matrixTargets(crossTargetJob());

  assert.ok(released.length >= 8, `only found ${released.length} released targets`);

  // A target that ships but is never compiled outside the release is a target
  // whose first compile is the release itself. Five of eight were in that
  // position before this job existed, which hid two tests that do not build
  // against musl.
  const unchecked = released.filter((target) => !checked.includes(target));
  assert.deepEqual(unchecked, [], `shipped but never cross-checked: ${unchecked.join(", ")}`);

  // The reverse drift is worth catching too: a target checked here but no
  // longer shipped is dead CI time, and usually means a rename was half done.
  const unshipped = checked.filter((target) => !released.includes(target));
  assert.deepEqual(unshipped, [], `cross-checked but not shipped: ${unshipped.join(", ")}`);
});

test("the cross-check covers tests, not just the shipped binary", () => {
  const job = crossTargetJob();

  // The release builds `-p riftri-cli`, so a test that does not compile for a
  // target is invisible to it. `--all-targets` is the whole point of this job.
  assert.match(job, /cargo clippy[\s\S]*?--all-targets/, "the check must pass --all-targets");
  assert.match(job, /--locked/, "the check must respect the lockfile");
  // Lints differ by target: a cast required against glibc is redundant against
  // musl, so a plain `cargo check` here would miss what this job exists for.
  assert.match(job, /-- -D warnings/, "the lint must be denied, not merely reported");
  assert.match(
    job,
    /--target \$\{\{ matrix\.target \}\}/,
    "the check must build for the matrix target",
  );
});

test("a failing target does not mask the others", () => {
  const job = crossTargetJob();

  // Without this, the first musl failure would cancel the remaining seven and
  // report one problem where there might be several.
  assert.match(job, /fail-fast: false/, "the matrix must not fail fast");
});
