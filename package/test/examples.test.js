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

const os = require("node:os");
const clientUrl = "file://" + path.join(root, "examples/lib/riftri.mjs");
const loadClient = () => import(clientUrl);

/** Write an executable fake `riftri` that runs `body` (an sh script). */
function fakeBinary(t, body) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-example-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, "riftri");
  fs.writeFileSync(bin, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return bin;
}

test("riftri exec keeps Riftri flags before the -- payload boundary", async (t) => {
  const { riftri } = await loadClient();
  // The fake records its own argv, one per line.
  const record = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "riftri-argv-")), "argv");
  const bin = fakeBinary(t, `printf '%s\\n' "$@" > '${record}'`);
  await riftri(["exec", "--", "agent", "--agent-flag"], { bin, json: false });
  const argv = fs.readFileSync(record, "utf8").trim().split("\n");
  const boundary = argv.indexOf("--");
  assert.equal(argv.indexOf("--json-errors") < boundary, true, argv.join(" "));
  assert.deepEqual(argv.slice(boundary), ["--", "agent", "--agent-flag"]);
});

test("malformed success output rejects instead of crashing the host", async (t) => {
  const { riftri, RiftriError } = await loadClient();
  const bin = fakeBinary(t, "printf '{bad'; exit 0");
  await assert.rejects(riftri(["status"], { bin }), (error) => {
    assert.ok(error instanceof RiftriError);
    assert.match(error.message, /not JSON/);
    return true;
  });
});

test("a silent non-zero exit still carries a useful message", async (t) => {
  const { riftri } = await loadClient();
  const bin = fakeBinary(t, "exit 7");
  await assert.rejects(riftri(["status"], { bin }), (error) => {
    assert.equal(error.code, 7);
    assert.equal(error.message, "riftri exited 7");
    return true;
  });
});

test("a signalled riftri rejects with a translated exit code", async (t) => {
  const { riftri } = await loadClient();
  const bin = fakeBinary(t, "kill -TERM $$");
  await assert.rejects(riftri(["status"], { bin }), (error) => {
    assert.equal(error.signal, "SIGTERM");
    assert.equal(error.wasSignalled, true);
    assert.equal(error.code, 143); // 128 + 15
    return true;
  });
});

test("run resolves a signalled child as 128 + signal, never null", async (t) => {
  const { run } = await loadClient();
  const code = await run("/bin/sh", ["-c", "kill -TERM $$"]);
  assert.equal(code, 143);
});
