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

async function ciWorkflow() {
  return readFile(
    path.resolve(__dirname, "..", "..", ".github", "workflows", "ci.yml"),
    "utf8",
  );
}

test("native backend jobs exercise the globally installed npm command", async () => {
  const workflow = await ciWorkflow();

  for (const jobName of ["linux-reflink", "linux-overlayfs", "windows-refs"]) {
    const job = jobSource(workflow, jobName);
    assert.match(job, /RIFTRI_REQUIRE_INSTALLED_LIFECYCLE: "1"/);
    assert.match(job, /npm run smoke:installed/);
  }
});

test("quality job test-executes both the tui and default/plain builds", async () => {
  const workflow = await ciWorkflow();
  const quality = jobSource(workflow, "quality");

  // The tui feature deselects crates/riftri-cli/src/ui/plain.rs, so testing only
  // that config leaves the default build (what `cargo install` and
  // `npm run build:native` produce) unexecuted. Guard that both configurations
  // are actually run by CI.
  assert.match(quality, /cargo test --workspace --locked --features riftri-cli\/tui/);
  assert.match(quality, /^        run: cargo test --workspace --locked$/m);
});
