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
    assert.match(job, /pnpm run smoke:installed/);
  }
});

test("quality job test-executes both the tui and default/plain builds", async () => {
  const workflow = await ciWorkflow();
  const quality = jobSource(workflow, "quality");

  // The tui feature deselects crates/riftri-cli/src/ui/plain.rs, so testing only
  // that config leaves the default build (what `cargo install` and
  // `pnpm run build:native` produce) unexecuted. Guard that both configurations
  // are actually run by CI.
  assert.match(quality, /cargo test --workspace --locked --features riftri-cli\/tui/);
  assert.match(quality, /^        run: cargo test --workspace --locked$/m);
});

test("macOS runs every Rust test exactly once across its two test jobs", async () => {
  const workflow = await ciWorkflow();
  const quality = jobSource(workflow, "quality");
  const properties = jobSource(workflow, "macos-properties");

  // Quality's macOS runner skips the property tests and `macos-properties`
  // runs only them. libtest matches both filters as substrings of the test
  // name, so the pair partitions the suite only while they name the same
  // string: a test renamed out of one side still runs on the other.
  const skip = /- os: macos-15\n\s+test-filter: --skip (\S+)\n/.exec(quality);
  assert.ok(skip, "Quality must give its macOS runner a --skip filter");
  assert.match(
    quality,
    /run: cargo test --workspace --locked --features riftri-cli\/tui -- \$\{\{ matrix\.test-filter \}\}\n/,
  );
  assert.match(properties, /runs-on: macos-15\n/);
  assert.ok(
    properties.includes(
      `run: cargo test --workspace --locked --features riftri-cli/tui ${skip[1]}\n`,
    ),
    `macos-properties must run exactly the tests Quality skips (${skip[1]})`,
  );
});

test("Git library benchmarks select the library test binary", async () => {
  const benchmarks = jobSource(await ciWorkflow(), "benchmarks");
  for (const name of [
    "reports_bounded_checkout_worker_latency",
    "reports_attribute_stdin_latency",
    "reports_direct_tree_attribute_latency",
    "reports_add_revision_batch_latency",
  ]) {
    const command = benchmarks.split("\n").find((line) => line.includes(name));
    assert.ok(command, `missing benchmark ${name}`);
    assert.match(command.split(" -- ")[0], / --lib\b/);
  }
});
