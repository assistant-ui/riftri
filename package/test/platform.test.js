"use strict";

const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { once } = require("node:events");
const { access, mkdtemp, readFile, rm } = require("node:fs/promises");
const { test } = require("node:test");
const { version } = require("../../package.json");

const {
  assertCompatiblePackageManifest,
  detectLinuxLibc,
  packageNameForPlatform,
  platformKey,
} = require("../lib/platform.js");

const glibcReport = {
  getReport: () => ({ header: { glibcVersionRuntime: "2.39" } }),
};
const muslReport = { getReport: () => ({ header: {} }) };

test("selects packages from platform, architecture, and libc", () => {
  assert.equal(
    packageNameForPlatform("darwin", "arm64", glibcReport),
    "riftri-darwin-arm64",
  );
  assert.equal(
    packageNameForPlatform("linux", "x64", glibcReport),
    "riftri-linux-x64-gnu",
  );
  assert.equal(
    packageNameForPlatform("linux", "x64", muslReport),
    "riftri-linux-x64-musl",
  );
  assert.equal(packageNameForPlatform("freebsd", "x64", glibcReport), null);
});

test("distinguishes GNU libc and musl", () => {
  assert.equal(detectLinuxLibc(glibcReport), "gnu");
  assert.equal(detectLinuxLibc(muslReport), "musl");
  assert.equal(platformKey("linux", "arm64", muslReport), "linux-arm64-musl");
});

test("rejects mismatched native package manifests", () => {
  assert.doesNotThrow(() =>
    assertCompatiblePackageManifest(
      { name: "riftri-linux-x64-musl", version },
      "riftri-linux-x64-musl",
      version,
    ),
  );
  assert.throws(
    () =>
      assertCompatiblePackageManifest(
        { name: "riftri-linux-x64-gnu", version },
        "riftri-linux-x64-musl",
        version,
      ),
    /does not match/,
  );
  assert.throws(
    () =>
      assertCompatiblePackageManifest(
        { name: "riftri-linux-x64-musl", version: "0.0.0" },
        "riftri-linux-x64-musl",
        version,
      ),
    /does not match/,
  );
});

test("stages the public package with conventional root directories", async (t) => {
  const destination = await mkdtemp(path.join(os.tmpdir(), "riftri-package-"));
  t.after(() => rm(destination, { recursive: true, force: true }));
  const { stageRootPackage } = await import("../scripts/stage-root-package.mjs");

  await stageRootPackage(destination);

  const manifest = JSON.parse(
    await readFile(path.join(destination, "package.json"), "utf8"),
  );
  assert.equal(manifest.name, "riftri");
  assert.equal(manifest.bin.riftri, "bin/riftri.js");
  assert.deepEqual(manifest.files, ["bin", "lib", "README.md", "LICENSE"]);
  await access(path.join(destination, "bin", "riftri.js"));
  await access(path.join(destination, "lib", "platform.js"));
  await assert.rejects(access(path.join(destination, "package")));
});

test("launcher delegates to the locally built Rust executable", () => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const executable = process.platform === "win32" ? "riftri.exe" : "riftri";
  const binary = path.join(repositoryRoot, "target", "debug", executable);
  const launcher = path.join(repositoryRoot, "package", "bin", "riftri.js");
  const result = spawnSync(process.execPath, [launcher, "--version"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: { ...process.env, RIFTRI_BINARY: binary },
  });

  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), `riftri ${version}`);
});

test("launcher preserves process-scoped Rust Git execution", () => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const executable = process.platform === "win32" ? "riftri.exe" : "riftri";
  const binary = path.join(repositoryRoot, "target", "debug", executable);
  const launcher = path.join(repositoryRoot, "package", "bin", "riftri.js");
  const result = spawnSync(
    process.execPath,
    [launcher, "exec", "--", "git", "--version"],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: { ...process.env, RIFTRI_BINARY: binary },
    },
  );

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^git version /);
});

test("launcher preserves Rust CLI failures", () => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const executable = process.platform === "win32" ? "riftri.exe" : "riftri";
  const binary = path.join(repositoryRoot, "target", "debug", executable);
  const launcher = path.join(repositoryRoot, "package", "bin", "riftri.js");
  const result = spawnSync(process.execPath, [launcher, "not-a-command"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: { ...process.env, RIFTRI_BINARY: binary },
  });

  assert.equal(result.status, 2);
  assert.match(result.stderr, /unrecognized subcommand/);
});

for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  test(`launcher forwards PID-directed ${signal} to native Riftri`, {
    skip: process.platform === "win32",
    timeout: 10000,
  }, async (t) => {
    const repositoryRoot = path.resolve(__dirname, "..", "..");
    const directory = await mkdtemp(path.join(os.tmpdir(), "riftri-signal-"));
    t.after(() => rm(directory, { recursive: true, force: true }));
    const child = spawn(process.execPath, [
      path.join(repositoryRoot, "package", "bin", "riftri.js"),
      "exec", "--", process.execPath, "-e",
      "console.log(process.ppid); setTimeout(() => {}, 1500);",
    ], {
      cwd: directory,
      env: {
        ...process.env,
        HOME: directory,
        XDG_STATE_HOME: directory,
        RIFTRI_BINARY: path.join(repositoryRoot, "target", "debug", "riftri"),
      },
    });
    t.after(() => child.kill("SIGKILL"));
    const exited = once(child, "exit", { signal: t.signal });
    const [ready] = await once(child.stdout, "data", { signal: t.signal });
    const nativePid = Number(ready.toString().trim());
    assert.ok(Number.isInteger(nativePid) && nativePid > 0);

    child.kill(signal);
    const [code, exitSignal] = await exited;
    assert.equal(code, null);
    assert.equal(exitSignal, signal);
    assert.throws(() => process.kill(nativePid, 0), { code: "ESRCH" });
  });
}
