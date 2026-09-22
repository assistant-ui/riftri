const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const examples = path.join(root, "examples");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

const folders = ["basic-runner", "fallback-detection", "parallel-tasks", "wrapped-agent"];

test("every example folder is runnable and indexed", () => {
  const index = read("examples/README.md");
  for (const folder of folders) {
    const entry = path.join(examples, folder, "run.mjs");
    assert.ok(fs.existsSync(entry), `missing examples/${folder}/run.mjs`);
    assert.match(fs.readFileSync(entry, "utf8"), /^#!\/usr\/bin\/env node/);
    assert.ok(index.includes(`${folder}/`), `examples/README.md must link ${folder}`);
  }
  // Nothing new should appear without an index entry.
  const present = fs
    .readdirSync(examples, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name !== "lib")
    .map((entry) => entry.name)
    .sort();
  assert.deepEqual(present, [...folders].sort());
});

test("examples stay dependency-free so they run from a clone", () => {
  for (const folder of [...folders, "lib"]) {
    assert.ok(
      !fs.existsSync(path.join(examples, folder, "package.json")),
      `examples/${folder} must not need an install step`,
    );
  }
  const sources = [
    "examples/lib/riftri.mjs",
    ...folders.map((folder) => `examples/${folder}/run.mjs`),
  ];
  for (const source of sources) {
    for (const specifier of read(source).matchAll(/from "([^"]+)"/g)) {
      const target = specifier[1];
      const local = target.startsWith(".") || target.startsWith("node:");
      assert.ok(local, `${source} imports ${target}; keep examples dependency-free`);
    }
  }
});

test("the shared client documents the exit codes it branches on", () => {
  const client = read("examples/lib/riftri.mjs");
  // These mirror docs/cli.md. A change there should fail here.
  for (const code of ["SUCCESS: 0", "OPERATIONAL: 1", "USAGE: 2", "POLICY: 3"]) {
    assert.ok(client.includes(code), `examples/lib/riftri.mjs must define ${code}`);
  }
  // `--json` is not accepted by every command, so the client must be able to
  // omit it rather than always appending it.
  assert.match(client, /json = true/);
  assert.match(client, /\["--json-errors"\]/);
});

test("the custom harness guide points at the runnable examples", () => {
  assert.match(read("docs/custom-harness.md"), /\.\.\/examples\/|examples\//);
});
