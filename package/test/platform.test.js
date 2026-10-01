"use strict";

const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { pathToFileURL } = require("node:url");
const {
  access,
  cp,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} = require("node:fs/promises");
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

// A copy of the real repository with its manifest edited: the staging script
// transforms that manifest, so a hand-built skeleton would test the skeleton.
async function stagingFixture(t, editManifest) {
  const sourceRoot = path.resolve(__dirname, "..", "..");
  const root = await mkdtemp(path.join(os.tmpdir(), "riftri-staging-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const entry of ["README.md", "LICENSE"]) {
    await cp(path.join(sourceRoot, entry), path.join(root, entry));
  }
  for (const entry of ["bin", "lib"]) {
    await cp(
      path.join(sourceRoot, "package", entry),
      path.join(root, "package", entry),
      { recursive: true },
    );
  }
  const manifest = JSON.parse(
    await readFile(path.join(sourceRoot, "package.json"), "utf8"),
  );
  editManifest(manifest);
  await writeFile(
    path.join(root, "package.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  return { root, destination: path.join(root, "staged") };
}

test("staging keeps every export subpath, not only the root one", async (t) => {
  // Rebuilding `exports` from its "." key drops the siblings, and nothing
  // downstream notices: npm resolves a subpath only when something imports it,
  // so the break surfaces as a consumer's failed require after publication.
  const { root, destination } = await stagingFixture(t, (manifest) => {
    manifest.exports["./client"] = {
      types: "./package/lib/client.d.ts",
      require: "./package/lib/client.js",
    };
  });
  const { stageRootPackage } = await import("../scripts/stage-root-package.mjs");

  await stageRootPackage(destination, root);

  const staged = JSON.parse(
    await readFile(path.join(destination, "package.json"), "utf8"),
  );
  assert.deepEqual(Object.keys(staged.exports).sort(), [".", "./client"]);
  assert.deepEqual(staged.exports["./client"], {
    types: "./lib/client.d.ts",
    require: "./lib/client.js",
  });
});

test("staging rewrites a string export target too", async (t) => {
  const { root, destination } = await stagingFixture(t, (manifest) => {
    manifest.exports["./platform"] = "./package/lib/platform.js";
  });
  const { stageRootPackage } = await import("../scripts/stage-root-package.mjs");

  await stageRootPackage(destination, root);

  const staged = JSON.parse(
    await readFile(path.join(destination, "package.json"), "utf8"),
  );
  assert.equal(staged.exports["./platform"], "./lib/platform.js");
});

test("the staging script can be imported without a script argv", async () => {
  // pathToFileURL(undefined) throws, so an unguarded main check makes the
  // module unimportable under `node -e` and in embedders.
  // A bare absolute path is not an importable specifier on Windows, where it
  // parses as the scheme `d:`; the module URL has to be a file:// one.
  const script = pathToFileURL(
    path.resolve(__dirname, "..", "scripts", "stage-root-package.mjs"),
  ).href;
  const result = spawnSync(
    process.execPath,
    [
      "-e",
      `import(${JSON.stringify(script)}).then(() => {}, (error) => { console.error(error); process.exit(1); })`,
    ],
    { encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr);
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

test("Windows ARM64 resolves like every other platform", () => {
  // riftri-win32-arm64 was held by npm's name filter until 0.5.1, and the
  // launcher sent that platform to the PowerShell installer. It is published
  // now, so a missing copy means the same thing as anywhere else.
  assert.throws(
    () =>
      resolveBinary({
        platform: "win32",
        architecture: "arm64",
        environment: {},
      }),
    /riftri-win32-arm64 is missing; reinstall without --omit=optional/,
  );
});

