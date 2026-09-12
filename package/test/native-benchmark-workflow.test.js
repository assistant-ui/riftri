"use strict";

const assert = require("node:assert/strict");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { test } = require("node:test");

test("CI records benchmark artifacts for every native backend", async () => {
  const workflow = await readFile(
    path.resolve(__dirname, "..", "..", ".github", "workflows", "ci.yml"),
    "utf8",
  );

  for (const artifact of [
    "native-cow-benchmark-macos-apfs",
    "native-cow-benchmark-linux-${{ matrix.filesystem }}",
    "native-cow-benchmark-linux-overlayfs",
    "native-cow-benchmark-windows-refs",
  ]) {
    assert.ok(workflow.includes(`name: ${artifact}`), `missing ${artifact}`);
  }
  assert.equal(
    workflow.match(/reports_cold_cached_and_private_write_costs/g)?.length,
    4,
  );
});
