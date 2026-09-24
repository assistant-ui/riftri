const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const {
  Riftri,
  RiftriError,
  EXIT_OPERATIONAL,
  EXIT_POLICY,
} = require("../lib/client.js");

// The stand-in below is a shell script, which Windows cannot execute directly:
// spawn() rejects an extensionless file, and Node refuses a .cmd without a
// shell. Real riftri is always riftri.exe there, so the gap is in the fixture
// rather than in the client, and the parsing these tests cover is identical on
// every platform. `resolves the native executable name per platform` keeps the
// one genuinely Windows-specific path covered.
const onWindows = process.platform === "win32";

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

/**
 * A stand-in riftri that records the exact argument list it was handed, so a
 * test can assert argument order rather than only the command's outcome.
 */
function argvBinary(directory) {
  const file = path.join(directory, "argv-riftri");
  const log = path.join(directory, "argv.txt");
  fs.writeFileSync(
    file,
    `#!/bin/sh\n: > ${JSON.stringify(log)}\nfor a in "$@"; do printf '%s\\n' "$a" >> ${JSON.stringify(log)}; done\n`,
  );
  fs.chmodSync(file, 0o755);
  return { binary: file, argv: () => fs.readFileSync(log, "utf8").split("\n").slice(0, -1) };
}

/** A stand-in riftri that kills itself with `signal`. */
function signallingBinary(directory, signal) {
  const file = path.join(directory, `kill-${signal}`);
  fs.writeFileSync(file, `#!/bin/sh\nkill -${signal} $$\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

function scratch(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

/** Run `args` through the client and return the argv riftri actually saw. */
async function argvFor(t, args, options = {}) {
  const directory = scratch(t);
  const stub = argvBinary(directory);
  const riftri = new Riftri({ repository: directory, binary: stub.binary });
  await riftri.run(args, { json: false, ...options });
  return stub.argv();
}

test("resolves the native executable name per platform", () => {
  const { binaryName } = require("../lib/platform.js");
  assert.equal(binaryName("win32"), "riftri.exe");
  assert.equal(binaryName("darwin"), "riftri");
  assert.equal(binaryName("linux"), "riftri");
});

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

test("reports are parsed and returned", { skip: onWindows }, async () => {
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

test("a policy refusal becomes a typed error carrying its receipt", { skip: onWindows }, async () => {
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

test("a busy worktree is distinguished from a refusal", { skip: onWindows }, async () => {
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

test("a usage error carries no receipt and is never retryable", { skip: onWindows }, async () => {
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

test("commands without --json are not parsed as JSON", { skip: onWindows }, async () => {
  // `enable` prints human text; parsing it would throw on success.
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, { stdout: "Enabled Riftri for /somewhere\n" });
  const riftri = new Riftri({ repository: directory, binary });
  assert.equal(await riftri.enable(), null);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("global flags go before the exec payload, never inside it", { skip: onWindows }, async (t) => {
  // Appending them put --json-errors in the argv riftri hands to the child,
  // so `exec -- git --version` died on a flag that was never meant for Git.
  assert.deepEqual(await argvFor(t, ["exec", "--", "git", "--version"]), [
    "exec",
    "--json-errors",
    "--",
    "git",
    "--version",
  ]);
});

test("a command with no payload still receives its flags last", { skip: onWindows }, async (t) => {
  assert.deepEqual(await argvFor(t, ["status"], { json: true }), [
    "status",
    "--json",
    "--json-errors",
  ]);
});

test("malformed JSON from a successful run rejects instead of crashing", { skip: onWindows }, async (t) => {
  // Thrown from the close handler, a SyntaxError is uncatchable by the caller
  // and takes the host process down with it.
  const directory = scratch(t);
  const binary = fakeBinary(directory, { stdout: "{bad" });
  const riftri = new Riftri({ repository: directory, binary });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.ok(error instanceof RiftriError);
      assert.equal(error.exitCode, EXIT_OPERATIONAL);
      assert.match(error.message, /not JSON/);
      assert.match(error.message, /status/, "names the command that misbehaved");
      assert.match(error.message, /\{bad/, "includes the output to diagnose from");
      return true;
    },
  );
});

test("a signalled process reports the signal, not a null exit code", { skip: onWindows }, async (t) => {
  const directory = scratch(t);
  const riftri = new Riftri({ repository: directory, binary: signallingBinary(directory, "TERM") });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.ok(error instanceof RiftriError);
      assert.equal(error.signal, "SIGTERM");
      assert.equal(error.wasSignalled, true);
      // 128 + SIGTERM, matching what native `riftri exec` reports.
      assert.equal(error.exitCode, 128 + os.constants.signals.SIGTERM);
      assert.equal(typeof error.exitCode, "number", "the d.ts promises a number");
      assert.equal(error.message, "riftri terminated by SIGTERM");
      return true;
    },
  );
});

test("an ordinary nonzero exit is not reported as a signal", { skip: onWindows }, async (t) => {
  const directory = scratch(t);
  const binary = fakeBinary(directory, { stderr: "error: unexpected argument", code: 2 });
  const riftri = new Riftri({ repository: directory, binary });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.equal(error.signal, null);
      assert.equal(error.wasSignalled, false);
      assert.equal(error.isUsageError, true);
      return true;
    },
  );
});

test("isOptimizable answers false only when Riftri answers", { skip: onWindows }, async (t) => {
  const directory = scratch(t);

  // A refusal is Riftri saying no: a legitimate false.
  const refused = fakeBinary(directory, {
    stderr: JSON.stringify({ code: "unsupported-filesystem", message: "no backend" }),
    code: EXIT_POLICY,
  });
  assert.equal(await new Riftri({ repository: directory, binary: refused }).isOptimizable("t"), false);

  // So is a report that says no backend is active.
  const inactive = path.join(directory, "inactive");
  fs.writeFileSync(
    inactive,
    `#!/bin/sh\nprintf '%s' '{"cow_backend_active":false}'\n`,
  );
  fs.chmodSync(inactive, 0o755);
  assert.equal(await new Riftri({ repository: directory, binary: inactive }).isOptimizable("t"), false);
});

test("isOptimizable rejects a broken installation instead of answering false", { skip: onWindows }, async (t) => {
  // Returning false here told callers "this repository is unsupported" when
  // the truth was "this client cannot run at all".
  const directory = scratch(t);

  const missing = path.join(directory, "not-here");
  await assert.rejects(
    () => new Riftri({ repository: directory, binary: missing }).isOptimizable("t"),
    (error) => (assert.equal(error.code, "ENOENT"), true),
  );

  const unreadable = path.join(directory, "not-executable");
  fs.writeFileSync(unreadable, "#!/bin/sh\n");
  fs.chmodSync(unreadable, 0o644);
  await assert.rejects(
    () => new Riftri({ repository: directory, binary: unreadable }).isOptimizable("t"),
    (error) => (assert.equal(error.code, "EACCES"), true),
  );

  const garbage = fakeBinary(directory, { stdout: "not json at all" });
  await assert.rejects(
    () => new Riftri({ repository: directory, binary: garbage }).isOptimizable("t"),
    /not JSON/,
  );
});

test("isOptimizable trusts readiness, not just the volume", { skip: onWindows }, async (t) => {
  // cow_backend_active describes the volume. Outside a Git repository it is
  // still true while `worktree add` cannot succeed, so reading it alone told
  // a harness to take the optimized path straight into a failure.
  const directory = scratch(t);
  const report = (status) =>
    JSON.stringify({ cow_backend_active: true, destination_readiness: { status } });

  const blocked = fakeBinary(directory, { stdout: report("blocked") });
  assert.equal(
    await new Riftri({ repository: directory, binary: blocked }).isOptimizable("t"),
    false,
    "a blocked destination is not optimizable",
  );

  // Activation only gates Git interception; the explicit interface works
  // without it, so this destination is optimizable.
  const needsActivation = fakeBinary(directory, { stdout: report("needs-activation") });
  assert.equal(
    await new Riftri({ repository: directory, binary: needsActivation }).isOptimizable("t"),
    true,
    "needs-activation is still optimizable",
  );

  const ready = fakeBinary(directory, { stdout: report("ready") });
  assert.equal(
    await new Riftri({ repository: directory, binary: ready }).isOptimizable("t"),
    true,
  );

  // A report without the field at all must not become silently unusable.
  const legacy = fakeBinary(directory, { stdout: JSON.stringify({ cow_backend_active: true }) });
  assert.equal(
    await new Riftri({ repository: directory, binary: legacy }).isOptimizable("t"),
    true,
    "a report lacking destination_readiness keeps the old answer",
  );
});
