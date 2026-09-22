const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const { Riftri, RiftriError, EXIT_POLICY } = require("../lib/client.js");

/** A stand-in riftri that echoes a fixed report or receipt. */
function fakeBinary(directory, { stdout = "", stderr = "", code = 0 }) {
  const file = path.join(directory, "fake-riftri");
  fs.writeFileSync(
    file,
    `#!/bin/sh\nprintf '%s' ${JSON.stringify(stdout)}\nprintf '%s' ${JSON.stringify(stderr)} >&2\nexit ${code}\n`,
  );
  fs.chmodSync(file, 0o755);
  return file;
}

test("the package exposes the client as its main entry point", () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  assert.equal(manifest.main, "package/lib/client.js");
  assert.equal(manifest.types, "package/lib/client.d.ts");
  assert.equal(manifest.exports["."].require, "./package/lib/client.js");
  assert.ok(fs.existsSync(path.join(root, manifest.types)), "types file must exist");
});

test("release staging rewrites entry points to the tarball layout", async () => {
  // The tarball puts lib/ at its root, so a published `package/` prefix would
  // make require("riftri") unresolvable.
  const { stageRootPackage } = await import("../scripts/stage-root-package.mjs");
  const destination = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-stage-"));
  try {
    await stageRootPackage(destination);
    const staged = JSON.parse(fs.readFileSync(path.join(destination, "package.json"), "utf8"));
    assert.equal(staged.main, "lib/client.js");
    assert.equal(staged.types, "lib/client.d.ts");
    assert.equal(staged.exports["."].require, "./lib/client.js");
    assert.equal(staged.exports["."].types, "./lib/client.d.ts");
    assert.ok(fs.existsSync(path.join(destination, "lib/client.js")));
    assert.ok(fs.existsSync(path.join(destination, "lib/client.d.ts")));
  } finally {
    fs.rmSync(destination, { recursive: true, force: true });
  }
});

test("reports are parsed and returned", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, {
    stdout: JSON.stringify({ schema_version: 1, cow_backend_active: true }),
  });
  const riftri = new Riftri({ repository: directory, binary });
  const report = await riftri.doctor({ destination: "../task" });
  assert.equal(report.cow_backend_active, true);
  assert.equal(await riftri.isOptimizable("../task"), true);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("a policy refusal becomes a typed error carrying its receipt", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const receipt = {
    schemaVersion: 1,
    outcome: "failed",
    operation: "worktree-add",
    code: "invalid-request",
    category: "policy",
    message: "destination already exists",
    phase: null,
    cleanup: "not-needed",
    recovery: "not-required",
    nextCommand: null,
  };
  const binary = fakeBinary(directory, { stderr: JSON.stringify(receipt), code: EXIT_POLICY });
  const riftri = new Riftri({ repository: directory, binary });

  await assert.rejects(
    () => riftri.worktree.add("../task", { branch: "x" }),
    (error) => {
      assert.ok(error instanceof RiftriError);
      assert.equal(error.exitCode, EXIT_POLICY);
      assert.equal(error.isPolicyRefusal, true);
      assert.equal(error.isBusy, false);
      assert.equal(error.needsRepair, false);
      assert.equal(error.receipt.code, "invalid-request");
      assert.equal(error.message, "destination already exists");
      return true;
    },
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("a busy worktree is distinguished from a refusal", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, {
    stderr: JSON.stringify({ code: "worktree-busy", recovery: "retry", message: "busy" }),
    code: 1,
  });
  const riftri = new Riftri({ repository: directory, binary });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.equal(error.isBusy, true, "waiting helps here, unlike a refusal");
      assert.equal(error.isPolicyRefusal, false);
      return true;
    },
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("a usage error carries no receipt and is never retryable", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, { stderr: "error: unexpected argument", code: 2 });
  const riftri = new Riftri({ repository: directory, binary });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.equal(error.isUsageError, true);
      assert.equal(error.receipt, null, "the parser emits no JSON receipt");
      assert.match(error.message, /unexpected argument/);
      return true;
    },
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("commands without --json are not parsed as JSON", async () => {
  // `enable` prints human text; parsing it would throw on success.
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, { stdout: "Enabled Riftri for /somewhere\n" });
  const riftri = new Riftri({ repository: directory, binary });
  assert.equal(await riftri.enable(), null);
  fs.rmSync(directory, { recursive: true, force: true });
});
