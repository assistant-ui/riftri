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
  return nextJob === -1 ? workflow.slice(start) : workflow.slice(start, start + 1 + nextJob);
}

test("manual release rehearsals cannot receive publishing permissions", async () => {
  const workflow = await readFile(
    path.resolve(__dirname, "..", "..", ".github", "workflows", "release.yml"),
    "utf8",
  );
  const stage = jobSource(workflow, "stage");
  const publish = jobSource(workflow, "publish");

  assert.match(workflow, /^  workflow_dispatch:\s*$/m);

  assert.match(stage, /Stage npm packages and GitHub assets/);
  assert.match(stage, /Confirm non-publishing rehearsal/);
  assert.doesNotMatch(stage, /contents: write/);
  assert.doesNotMatch(stage, /id-token: write/);
  assert.doesNotMatch(stage, /Publish npm packages/);
  assert.doesNotMatch(stage, /Create or update GitHub release/);

  assert.match(publish, /^    if: github\.event_name == 'push'$/m);
  assert.match(publish, /^      contents: write$/m);
  assert.match(publish, /^      id-token: write$/m);
  assert.match(publish, /Publish npm packages/);
  assert.match(publish, /Create or update GitHub release/);
});
