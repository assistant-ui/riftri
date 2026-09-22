"use strict";

const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { access, mkdtemp, readFile, rm } = require("node:fs/promises");
const { test } = require("node:test");
const { version } = require("../../package.json");

const {
  PLATFORM_PACKAGES,
  assertCompatiblePackageManifest,
  detectLinuxLibc,
  packageNameForPlatform,
  platformKey,
  resolveBinary,
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

test("a platform npm refused explains the route that works", () => {
  // riftri-win32-arm64 is rejected by the registry, so the launcher installs
  // on Windows ARM64 with no binary behind it. Telling that user to reinstall
  // sends them around the same loop; point at the PowerShell installer.
  assert.throws(
    () =>
      resolveBinary({
        platform: "win32",
        architecture: "arm64",
        environment: {},
      }),
    (error) => {
      assert.match(error.message, /riftri-win32-arm64 is not published to npm/);
      assert.match(error.message, /PowerShell installer/);
      assert.doesNotMatch(error.message, /--omit=optional/);
      return true;
    },
  );
});

test("a merely uninstalled platform still suggests reinstalling", () => {
  // The tailored message must not swallow the ordinary case, where the package
  // exists on npm and the user skipped optional dependencies.
  assert.throws(
    () =>
      resolveBinary({
        platform: "win32",
        architecture: "x64",
        environment: {},
      }),
    /riftri-win32-x64 is missing; reinstall without --omit=optional/,
  );
});

test("every unpublished platform is still a known package name", async () => {
  // Guards the cleanup: when a name starts publishing, its entry must go, and
  // a typo here would silently never match.
  const source = await readFile(
    path.join(__dirname, "..", "lib", "platform.js"),
    "utf8",
  );
  const block = source.match(/UNPUBLISHED_PACKAGES = Object\.freeze\(\{([\s\S]*?)\}\);/);
  assert.ok(block, "platform.js must declare UNPUBLISHED_PACKAGES");
  const names = [...block[1].matchAll(/"(riftri-[a-z0-9-]+)":/g)].map((m) => m[1]);
  assert.ok(names.length > 0, "drop UNPUBLISHED_PACKAGES once it is empty");
  const published = Object.values(PLATFORM_PACKAGES);
  for (const name of names) {
    assert.ok(published.includes(name), `${name} is not a platform package`);
  }
});
