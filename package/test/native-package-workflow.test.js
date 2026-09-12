"use strict";

const assert = require("node:assert/strict");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { test } = require("node:test");

function jobSource(workflow, name) {
  const start = workflow.indexOf(`  ${name}:\n`);
  assert.notEqual(start, -1, `missing ${name} job`);
  const rest = workflow.slice(start + 1);
  const nextJob = rest.search(/^  [a-z][a-z0-9_-]*:\n/m);
  return nextJob === -1
    ? workflow.slice(start)
    : workflow.slice(start, start + 1 + nextJob);
}

test("native backend jobs exercise the globally installed npm command", async () => {
  const workflow = await readFile(
    path.resolve(__dirname, "..", "..", ".github", "workflows", "ci.yml"),
    "utf8",
  );

  for (const jobName of ["linux-reflink", "linux-overlayfs", "windows-refs"]) {
    const job = jobSource(workflow, jobName);
    assert.match(job, /RIFTRI_REQUIRE_INSTALLED_LIFECYCLE: "1"/);
    assert.match(job, /npm run smoke:installed/);
  }
});
