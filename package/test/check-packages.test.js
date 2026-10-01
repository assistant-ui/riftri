"use strict";

const assert = require("node:assert/strict");
const { cp, mkdir, mkdtemp, readFile, rm, writeFile } = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const sourceRoot = path.resolve(__dirname, "..", "..");

// A real copy rather than a hand-built skeleton: the gate's whole job is to
// notice drift in these files, so a fixture that invents their shape would
// test the fixture instead of the repository.
async function manifestFixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), "riftri-check-packages-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, "package"), { recursive: true });
  for (const entry of ["package.json", "package-lock.json", "Cargo.toml"]) {
    await cp(path.join(sourceRoot, entry), path.join(root, entry));
  }
  await cp(path.join(sourceRoot, "crates"), path.join(root, "crates"), {
    recursive: true,
    filter: (source) =>
      !path.basename(source).startsWith("target") &&
      (source.endsWith("Cargo.toml") ||
        !path.extname(source) ||
        source === path.join(sourceRoot, "crates")),
  });
  await cp(
    path.join(sourceRoot, "package", "platforms"),
    path.join(root, "package", "platforms"),
    { recursive: true },
  );
  return root;
}

async function editJson(file, edit) {
  const value = JSON.parse(await readFile(file, "utf8"));
  edit(value);
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}

function platformManifest(root, name) {
  return path.join(root, "package", "platforms", name, "package.json");
}

test("an untouched checkout satisfies the gate", async (t) => {
  const repositoryRoot = await manifestFixture(t);
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  const result = await checkPackages({ repositoryRoot });

  assert.equal(result.platforms.length, 8);
  assert.ok(result.crates.includes("riftri-cli"));
});

test("a crate pinning its own version is rejected", async (t) => {
  // The binary reports its crate's CARGO_PKG_VERSION, not the workspace's, so
  // a pinned crate ships inside an npm package claiming a different version.
  // Nothing downstream catches it: the installed smoke test reads --version
  // without comparing it, and `install.sh` only guards the archive path.
  const repositoryRoot = await manifestFixture(t);
  const manifest = path.join(repositoryRoot, "crates", "riftri-cli", "Cargo.toml");
  const source = await readFile(manifest, "utf8");
  await writeFile(
    manifest,
    source.replace(/^version\.workspace = true$/m, 'version = "0.4.0"'),
  );
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-cli must inherit the workspace version/,
  );
});

test("a platform package aimed at the wrong os or cpu is rejected", async (t) => {
  // npm skips an optional dependency whose constraints exclude the host, so
  // this does not fail an install; it produces a launcher with no binary. Only
  // three of the eight packages are installed anywhere in CI.
  const repositoryRoot = await manifestFixture(t);
  await editJson(platformManifest(repositoryRoot, "riftri-darwin-arm64"), (m) => {
    m.os = ["linux"];
  });
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-darwin-arm64 must declare os darwin/,
  );

  const second = await manifestFixture(t);
  await editJson(platformManifest(second, "riftri-win32-x64"), (m) => {
    m.cpu = ["arm64"];
  });
  await assert.rejects(
    checkPackages({ repositoryRoot: second }),
    /riftri-win32-x64 must declare cpu x64/,
  );
});

test("a platform package that ships no binary is rejected", async (t) => {
  // Such a tarball installs cleanly and only fails when someone runs riftri.
  const repositoryRoot = await manifestFixture(t);
  await editJson(platformManifest(repositoryRoot, "riftri-linux-x64-musl"), (m) => {
    m.files = ["README.md"];
  });
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-linux-x64-musl must ship bin\/riftri/,
  );
});

test("a Windows package must ship the .exe, not the bare name", async (t) => {
  const repositoryRoot = await manifestFixture(t);
  await editJson(platformManifest(repositoryRoot, "riftri-win32-arm64"), (m) => {
    m.files = ["bin/riftri"];
  });
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-win32-arm64 must ship bin\/riftri\.exe/,
  );
});

test("libc is checked in npm's spelling, not the launcher's", async (t) => {
  // PLATFORM_PACKAGES keys say `gnu` (the Rust target); npm's manifest field
  // says `glibc`. Conflating them would make every gnu package unusable.
  const repositoryRoot = await manifestFixture(t);
  await editJson(platformManifest(repositoryRoot, "riftri-linux-arm64-gnu"), (m) => {
    m.libc = ["gnu"];
  });
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-linux-arm64-gnu must declare libc glibc/,
  );
});

test("the checks the gate already made still hold", async (t) => {
  const repositoryRoot = await manifestFixture(t);
  await editJson(platformManifest(repositoryRoot, "riftri-darwin-x64"), (m) => {
    m.version = "0.0.0";
  });
  const { checkPackages } = await import("../scripts/check-packages.mjs");

  await assert.rejects(
    checkPackages({ repositoryRoot }),
    /riftri-darwin-x64 version must match riftri/,
  );
});
