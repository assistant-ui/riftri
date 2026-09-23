"use strict";

// examples/lib/riftri.mjs is copyable code: a reader pastes it into their own
// harness, so a defect here ships into other people's runners. It drifted from
// the package client and carried the same four bugs #349-#352 fixed there.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { pathToFileURL } = require("node:url");

const root = path.resolve(__dirname, "../..");
// Windows rejects a bare absolute path here: the ESM loader reads "D:\\..." as
// a URL scheme. Every dynamic import of a local file needs a file:// URL.
const client = () => import(pathToFileURL(path.join(root, "examples/lib/riftri.mjs")).href);
const onWindows = process.platform === "win32";

function scratch(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-example-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function stub(directory, body, name = "fake-riftri") {
  const file = path.join(directory, name);
  fs.writeFileSync(file, `#!/bin/sh\n${body}\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

test("riftri flags go before the exec payload, never inside it", { skip: onWindows }, async (t) => {
  const { riftri } = await client();
  const directory = scratch(t);
  const log = path.join(directory, "argv.txt");
  const bin = stub(directory, `: > ${log}\nfor a in "$@"; do printf '%s\\n' "$a" >> ${log}; done`);

  await riftri(["exec", "--", "git", "--version"], { cwd: directory, bin, json: false });
  assert.deepEqual(fs.readFileSync(log, "utf8").trim().split("\n"), [
    "exec",
    "--json-errors",
    "--",
    "git",
    "--version",
  ]);
});

test("malformed JSON rejects instead of killing the host process", { skip: onWindows }, async (t) => {
  const { riftri } = await client();
  const directory = scratch(t);
  const bin = stub(directory, "printf '{bad'\nexit 0");
  await assert.rejects(
    () => riftri(["status"], { cwd: directory, bin }),
    (error) => {
      assert.match(error.message, /not JSON/);
      assert.match(error.message, /\{bad/, "includes the output to diagnose from");
      return true;
    },
  );
});

test("a silent nonzero exit still produces a message", { skip: onWindows }, async (t) => {
  // `stderr.trim() ?? fallback` kept the empty string, because "" is not
  // nullish, so the thrown error had no message at all.
  const { riftri } = await client();
  const directory = scratch(t);
  const bin = stub(directory, "exit 7");
  await assert.rejects(
    () => riftri(["status"], { cwd: directory, bin }),
    (error) => {
      assert.equal(error.exitCode, 7);
      assert.equal(error.message, "riftri exited 7");
      return true;
    },
  );
});

test("a signalled riftri reports the signal, not a null status", { skip: onWindows }, async (t) => {
  const { riftri } = await client();
  const directory = scratch(t);
  const bin = stub(directory, "kill -TERM $$");
  await assert.rejects(
    () => riftri(["status"], { cwd: directory, bin }),
    (error) => {
      assert.equal(error.signal, "SIGTERM");
      assert.equal(error.wasSignalled, true);
      assert.equal(error.exitCode, 128 + os.constants.signals.SIGTERM);
      assert.equal(error.message, "riftri terminated by SIGTERM");
      return true;
    },
  );
});

test("run() resolves a number for a signalled child", { skip: onWindows }, async (t) => {
  // It resolved null, though its contract promises an exit code, so a caller
  // checking `code !== 0` silently treated a killed command as success.
  const { run } = await client();
  const code = await run("/bin/sh", ["-c", "kill -TERM $$"], { cwd: scratch(t) });
  assert.equal(code, 128 + os.constants.signals.SIGTERM);
  assert.notEqual(code, 0, "a killed command must not look like success");
});

test("run() still reports ordinary exit codes", { skip: onWindows }, async (t) => {
  const { run } = await client();
  assert.equal(await run("/bin/sh", ["-c", "exit 0"], { cwd: scratch(t) }), 0);
  assert.equal(await run("/bin/sh", ["-c", "exit 9"], { cwd: scratch(t) }), 9);
});

test("isOptimizable answers false only for a refusal", { skip: onWindows }, async (t) => {
  const { isOptimizable, EXIT } = await client();
  const directory = scratch(t);

  const refused = stub(directory, `printf '%s' '{"code":"unsupported"}' >&2\nexit ${3}`, "refuse");
  assert.equal(await isOptimizable(directory, "t", { bin: refused }), false);
  assert.equal(EXIT.POLICY, 3);

  const missing = path.join(directory, "not-here");
  await assert.rejects(
    () => isOptimizable(directory, "t", { bin: missing }),
    (error) => (assert.equal(error.code, "ENOENT"), true),
  );
});

test("the example client matches the package client's error contract", async () => {
  // Same names, so a reader moving between the two is not silently wrong.
  const { RiftriError } = await client();
  const pkg = require(path.join(root, "package/lib/client.js"));
  const error = new RiftriError(3, null, "");
  const packaged = new pkg.RiftriError(3, null, "");
  for (const field of ["exitCode", "signal", "receipt", "wasSignalled", "isPolicyRefusal", "isUsageError", "isBusy", "needsRepair"]) {
    assert.equal(field in error || error[field] !== undefined, true, `example client is missing ${field}`);
    assert.equal(
      typeof error[field],
      typeof packaged[field],
      `${field} differs in type between the two clients`,
    );
  }
  assert.equal(error.exitCode, packaged.exitCode);
  assert.equal(error.isPolicyRefusal, true);
});
