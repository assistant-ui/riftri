"use strict";

const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { access, mkdtemp, readFile, rm } = require("node:fs/promises");
const { test } = require("node:test");

const {
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
  assert.equal(packageNameForPlatform("linux", "x64", muslReport), null);
  assert.equal(packageNameForPlatform("freebsd", "x64", glibcReport), null);
});

test("detects GNU libc without treating musl as compatible", () => {
  assert.equal(detectLinuxLibc(glibcReport), "gnu");
  assert.equal(detectLinuxLibc(muslReport), "musl");
  assert.equal(platformKey("linux", "arm64", muslReport), "linux-arm64-musl");
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
  await assert.rejects(access(path.join(destination, "packaging")));
});

test("launcher delegates to the locally built Rust executable", () => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const executable = process.platform === "win32" ? "riftri.exe" : "riftri";
  const binary = path.join(repositoryRoot, "target", "debug", executable);
  const launcher = path.join(repositoryRoot, "packaging", "bin", "riftri.js");
  const result = spawnSync(process.execPath, [launcher, "--version"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: { ...process.env, RIFTRI_BINARY: binary },
  });

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^riftri 0\.1\.0\s*$/);
});

test("launcher preserves process-scoped Rust Git execution", () => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const executable = process.platform === "win32" ? "riftri.exe" : "riftri";
  const binary = path.join(repositoryRoot, "target", "debug", executable);
  const launcher = path.join(repositoryRoot, "packaging", "bin", "riftri.js");
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
  const launcher = path.join(repositoryRoot, "packaging", "bin", "riftri.js");
  const result = spawnSync(process.execPath, [launcher, "not-a-command"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: { ...process.env, RIFTRI_BINARY: binary },
  });

  assert.equal(result.status, 2);
  assert.match(result.stderr, /unrecognized subcommand/);
});
