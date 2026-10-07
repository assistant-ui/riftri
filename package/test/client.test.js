const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { spawnSync } = require("node:child_process");

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
    `#!/bin/sh\n: > ${JSON.stringify(log)}\nfor a in "$@"; do printf '%s\\n' "$a" >> ${JSON.stringify(log)}; done\nprintf '{}\\n'\n`,
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

test("a process-owning harness can resolve the same verified native executable", () => {
  const { execFileSync } = require("node:child_process");
  const output = execFileSync(process.execPath, ["-e", `
    const { resolveBinary } = require(${JSON.stringify(path.join(root, "package/lib/client.js"))});
    process.stdout.write(resolveBinary());
  `], { encoding: "utf8", env: { ...process.env, RIFTRI_BINARY: process.execPath } });
  assert.equal(output, process.execPath);
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
      assert.equal(error.isStorageFull, false);
      assert.equal(error.needsRepair, false);
      assert.equal(error.receipt.code, "invalid-request");
      assert.equal(error.message, "destination already exists");
      return true;
    },
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("a checkout hook exit of 3 retains the created report and never permits fallback", { skip: onWindows }, async (t) => {
  const directory = scratch(t);
  const report = {
    schema_version: 1,
    destination: "../created",
    backend: "apfs-clone",
    post_checkout: { hook: "post-checkout", started: true, exit_code: 3 },
  };
  const binary = fakeBinary(directory, { stdout: JSON.stringify(report), code: 3 });
  await assert.rejects(new Riftri({ repository: directory, binary }).worktree.add("../created"), (error) => {
    assert.equal(error.isPolicyRefusal, false);
    assert.equal(error.exitCode, 3);
    assert.equal(error.receipt, null);
    assert.deepEqual(error.report, report);
    return true;
  });
});

test("exit 3 without an intact refusal receipt is not a safe fallback", { skip: onWindows }, async (t) => {
  const directory = scratch(t);
  for (const receipt of [null, {}, { category: "policy" }, {
    schemaVersion: 1, outcome: "failed", code: "failed-add", category: "policy", cleanup: "pending",
  }]) {
    const binary = fakeBinary(directory, { stderr: JSON.stringify(receipt), code: 3 });
    await assert.rejects(new Riftri({ repository: directory, binary }).worktree.add("../task"), (error) => {
      assert.equal(error.isPolicyRefusal, false);
      assert.equal(error.report, null);
      return true;
    });
  }
});

test("a receipt after other stderr output is still parsed", { skip: onWindows }, async () => {
  // The receipt is the last line of stderr; a hook or a Git warning may have
  // written there first, and that must not strip the error of its code.
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const receipt = {
    schemaVersion: 1,
    outcome: "failed",
    code: "invalid-request",
    category: "policy",
    message: "invalid worktree request: destination already exists",
    cleanup: "not-needed",
    recovery: "not-required",
  };
  // Real newlines: fakeBinary's printf '%s' would print "\\n" literally.
  const binary = path.join(directory, "noisy-riftri");
  fs.writeFileSync(
    binary,
    "#!/bin/sh\nprintf '%s\\n' 'warning: something Git printed' 'hook output' " +
      `'${JSON.stringify(receipt)}' >&2\nexit ${EXIT_POLICY}\n`,
  );
  fs.chmodSync(binary, 0o755);
  await assert.rejects(new Riftri({ repository: directory, binary }).status(), (error) => {
    assert.ok(error instanceof RiftriError);
    assert.equal(error.receipt?.code, "invalid-request");
    assert.equal(error.isPolicyRefusal, true);
    return true;
  });
});

test("a full volume is distinguished before recovery", { skip: onWindows }, async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-"));
  const binary = fakeBinary(directory, {
    stderr: JSON.stringify({
      code: "storage-full",
      recovery: "required",
      nextCommand: "riftri repair",
      message: "No space left on device",
    }),
    code: 1,
  });
  const riftri = new Riftri({ repository: directory, binary });
  await assert.rejects(
    () => riftri.status(),
    (error) => {
      assert.equal(error.isStorageFull, true);
      assert.equal(error.needsRepair, true);
      assert.equal(error.isBusy, false);
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

test("plain parser text is still a usage error, with no receipt", { skip: onWindows }, async () => {
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

test("a usage receipt is parsed like any other", { skip: onWindows }, async (t) => {
  // What the real binary emits under --json-errors since #304. The types once
  // claimed usage errors carry no receipt and that `category` is only policy
  // or operational, so typed callers never matched this one.
  const directory = scratch(t);
  const usage = {
    schemaVersion: 1,
    outcome: "failed",
    operation: null,
    code: "usage-error",
    category: "usage",
    message: "error: unexpected argument '--nope' found",
    phase: null,
    cleanup: "not-needed",
    recovery: "not-required",
    nextCommand: null,
    repository: null,
    repositoryNativeHex: null,
    stateDirectory: null,
    stateDirectoryNativeHex: null,
    nativePathEncoding: "unix-bytes-hex",
  };
  const binary = fakeBinary(directory, { stderr: JSON.stringify(usage), code: 2 });
  await assert.rejects(
    () => new Riftri({ repository: directory, binary }).status(),
    (error) => {
      assert.equal(error.isUsageError, true);
      assert.deepEqual(error.receipt, usage);
      return true;
    },
  );
});

test("the receipt types admit every category the CLI documents", () => {
  const cli = fs.readFileSync(path.join(root, "docs/cli.md"), "utf8");
  const documented = cli.match(/`category` field \(([^)]*)\)/);
  assert.ok(documented, "docs/cli.md no longer lists the receipt categories");
  const categories = [...documented[1].matchAll(/`([a-z]+)`/g)].map((m) => m[1]);
  assert.deepEqual([...categories].sort(), ["operational", "policy", "usage"]);
  const types = fs.readFileSync(path.join(root, "package/lib/client.d.ts"), "utf8");
  const union = types.match(/^\s*category: (.*);$/m)[1];
  for (const category of categories) assert.ok(union.includes(`"${category}"`), union);
  assert.match(types, /^\s*operation: string \| null;$/m, "usage receipts have no operation");
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

test("listing every state never also names the configured one", { skip: onWindows }, async (t) => {
  // Riftri rejects --all-states with --state-dir, so a client built with
  // `stateDir` failed every `list({ allStates: true })` as a usage error.
  const directory = scratch(t);
  const stub = argvBinary(directory);
  const riftri = new Riftri({ repository: directory, binary: stub.binary, stateDir: "state" });
  await riftri.worktree.list({ allStates: true });
  assert.deepEqual(stub.argv(), ["worktree", "list", "--all-states", "--json", "--json-errors"]);
  await riftri.worktree.list();
  assert.ok(stub.argv().includes("--state-dir=state"), stub.argv().join(" "));
});

test("ownership looks across registered states and protects option-like paths", { skip: onWindows }, async (t) => {
  const directory = scratch(t);
  const stub = argvBinary(directory);
  const riftri = new Riftri({ repository: directory, binary: stub.binary, stateDir: "state" });
  await riftri.worktree.owner("-view");
  assert.deepEqual(stub.argv(), ["worktree", "owner", "--json", "--json-errors", "--", "-view"]);
});

test("a relative binary is relative to the caller, not the repository", { skip: onWindows }, async (t) => {
  // spawn resolves a relative command against its cwd, so this failed with
  // ENOENT whenever `repository` was not the caller's own directory.
  const directory = scratch(t);
  const repository = path.join(directory, "repository");
  fs.mkdirSync(repository);
  const stub = argvBinary(directory);
  const binary = path.relative(process.cwd(), stub.binary);
  assert.ok(!path.isAbsolute(binary) && binary.includes(path.sep), binary);
  await new Riftri({ repository, binary }).run(["status"], { json: false });
  assert.deepEqual(stub.argv(), ["status", "--json-errors"]);
});

test("a command with no payload still receives its flags last", { skip: onWindows }, async (t) => {
  assert.deepEqual(await argvFor(t, ["status"], { json: true }), [
    "status",
    "--json",
    "--json-errors",
  ]);
});

test("malformed receipt messages reject without crashing the host", () => {
  const producer = `process.stderr.write(JSON.stringify({message:{toString:0,valueOf:0}})); process.exitCode=1;`;
  const host = `
    const {Riftri, RiftriError} = require(${JSON.stringify(require.resolve("../lib/client.js"))});
    new Riftri({binary:process.execPath}).run(
      ["-e", ${JSON.stringify(producer)}, "fixture"], {json:true}
    ).then(() => {process.exitCode=2;}, error => {
      if (!(error instanceof RiftriError) || error.exitCode !== 1) process.exitCode=3;
      else console.log("rejection caught");
    });
  `;
  const result = spawnSync(process.execPath, ["-e", host], { encoding: "utf8", timeout: 10000 });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /rejection caught/);
});

test("non-string receipt messages use diagnostic text without coercion", () => {
  for (const message of [null, false, 42, [], { toString: 0, valueOf: 0 }]) {
    assert.equal(new RiftriError(1, { message }, "diagnostic").message, "diagnostic");
  }
  assert.equal(new RiftriError(1, { message: "receipt" }, "diagnostic").message, "receipt");
});

test("a receipt after a large multiline log fits within a small heap", () => {
  const producer = `
    const {once} = require("node:events");
    (async () => {
      const chunk = "log line\\n".repeat(10000);
      for (let i = 0; i < 300; i++) {
        if (!process.stderr.write(chunk)) await once(process.stderr, "drain");
      }
      process.stderr.write(JSON.stringify({code:"worktree-busy",message:"busy"}) + "\\n");
      process.exitCode = 1;
    })();
  `;
  const host = `
    const {Riftri, RiftriError} = require(${JSON.stringify(require.resolve("../lib/client.js"))});
    new Riftri({binary:process.execPath}).run(
      ["-e", ${JSON.stringify(producer)}, "fixture"], {json:false}
    ).then(() => {process.exitCode=2;}, error => {
      if (!(error instanceof RiftriError) || !error.isBusy || error.message !== "busy") process.exitCode=3;
      else console.log("receipt preserved");
    });
  `;
  const result = spawnSync(process.execPath, ["--max-old-space-size=64", "-e", host], {
    encoding: "utf8", timeout: 60000,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /receipt preserved/);
});

test("non-reporting commands drain large stdout within a small heap", () => {
  const producer = `
    const {once} = require("node:events");
    (async () => {
      const chunk = Buffer.alloc(64 * 1024, 120);
      for (let i = 0; i < 1024; i++) {
        if (!process.stdout.write(chunk)) await once(process.stdout, "drain");
      }
    })().catch(() => {process.exitCode=1;});
  `;
  const host = `
    const {Riftri} = require(${JSON.stringify(require.resolve("../lib/client.js"))});
    new Riftri({binary:process.execPath}).run(
      ["-e", ${JSON.stringify(producer)}, "fixture"], {json:false}
    ).then(value => {
      if (value !== null) process.exitCode=2;
      else console.log("drained 64 MiB");
    }, () => {process.exitCode=3;});
  `;
  const result = spawnSync(process.execPath, ["--max-old-space-size=32", "-e", host], {
    encoding: "utf8", timeout: 60000,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /drained 64 MiB/);
});

test("empty JSON reports reject while non-reporting commands still succeed", async () => {
  const riftri = new Riftri({ binary: process.execPath });
  for (const stdout of ["", " \t\r\n"]) {
    const args = ["-e", `process.stdout.write(${JSON.stringify(stdout)})`, "fixture"];
    await assert.rejects(riftri.run(args, { json: true }), (error) => {
      assert.ok(error instanceof RiftriError);
      assert.equal(error.exitCode, EXIT_OPERATIONAL);
      assert.match(error.message, /not JSON/);
      return true;
    });
    assert.equal(await riftri.run(args, { json: false }), null);
  }
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
    stderr: JSON.stringify({ schemaVersion: 1, outcome: "failed", category: "policy", cleanup: "not-needed", code: "unsupported-filesystem", message: "no backend" }),
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
  const empty = fakeBinary(directory, {});
  await assert.rejects(
    () => new Riftri({ repository: directory, binary: empty }).isOptimizable("t"),
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

test("paths and values that start with a dash are never parsed as flags", { skip: onWindows }, async (t) => {
  // A relative destination such as `-scratch`, or a branch value, went onto
  // the command line where Riftri's parser read it as flags, so the call
  // failed as a usage error.
  const directory = scratch(t);
  const stub = argvBinary(directory);
  const riftri = new Riftri({ repository: directory, binary: stub.binary, stateDir: "-state" });

  await riftri.worktree.add("-scratch", { branch: "-topic", revision: "HEAD" });
  assert.deepEqual(stub.argv(), [
    "worktree",
    "add",
    "--branch=-topic",
    "--state-dir=-state",
    "--json",
    "--json-errors",
    "--",
    "-scratch",
    "HEAD",
  ]);
  await riftri.worktree.remove("-scratch", { force: true });
  assert.deepEqual(stub.argv().slice(-2), ["--", "-scratch"]);
  await riftri.worktree.move("-from", "-to");
  assert.deepEqual(stub.argv().slice(-3), ["--", "-from", "-to"]);
  await riftri.worktree.compact("-view");
  assert.deepEqual(stub.argv().slice(-2), ["--", "-view"]);
});
