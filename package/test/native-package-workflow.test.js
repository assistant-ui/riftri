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

test("Windows ReFS runs every Rust test exactly once across its two parts", async () => {
  const refs = jobSource(await ciWorkflow(), "windows-refs");

  // One part runs the tests whose names contain the filter and the other skips
  // exactly those, so a test renamed out of one part still runs in the other.
  // The filters are quoted because YAML reads a trailing `::` as a mapping.
  const parts = [...refs.matchAll(/- part: (\S+)\n\s+test-filter: "(.+)"\n/g)];
  assert.equal(parts.length, 2, "windows-refs must split its suite into two parts");
  const [[, , selected], [, , skipped]] = parts;
  assert.match(selected, /^\S+$/, "the first part must select by a single filter");
  assert.equal(skipped, `--skip ${selected}`, "the second part must skip exactly what the first runs");
  assert.match(
    refs,
    /run: cargo test --workspace --locked --features riftri-core\/native-cow-integration -- \$\{\{ matrix\.test-filter \}\}\n/,
  );

  // A step limited to one part must name a part that exists, or it never runs.
  const names = parts.map(([, name]) => name);
  for (const [, name] of refs.matchAll(/matrix\.part == '([^']*)'/g)) {
    assert.ok(names.includes(name), `windows-refs has no part named ${name}`);
  }
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
