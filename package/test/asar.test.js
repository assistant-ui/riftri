"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");
const { version } = require("../../package.json");

async function fixture(t, archive = "app.asar", platform = "darwin") {
  const root = await fs.realpath(
    await fs.mkdtemp(path.join(os.tmpdir(), "riftri-asar-")),
  );
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const { stageRootPackage } = await import(
    "../scripts/stage-root-package.mjs"
  );
  const modules = path.join(root, archive, "node_modules");
  const launcher = path.join(modules, "riftri");
  const name = `riftri-${platform}-arm64${platform === "linux" ? "-gnu" : ""}`;
  await stageRootPackage(launcher);
  const native = path.join(modules, name);
  await fs.mkdir(path.join(native, "bin"), { recursive: true });
  await fs.writeFile(
    path.join(native, "package.json"),
    JSON.stringify({ name, version }),
  );
  const binary = path.join(
    native,
    "bin",
    platform === "win32" ? "riftri.exe" : "riftri",
  );
  const unpacked = path.join(
    root,
    `${archive}.unpacked`,
    "node_modules",
    name,
    "bin",
    path.basename(binary),
  );
  await fs.mkdir(path.dirname(unpacked), { recursive: true });
  for (const file of [binary, unpacked]) {
    await fs.writeFile(
      file,
      "#!/bin/sh\nprintf 'native executable reached\\n'\n",
      { mode: 0o755 },
    );
  }
  const resolve = ({ electron = true, override } = {}) =>
    spawnSync(
      process.execPath,
      [
        "-e",
        `
    if (${electron}) Object.defineProperty(process.versions, 'electron', {value: 'test'});
    const {resolveBinary} = require(${JSON.stringify(path.join(launcher, "lib/platform.js"))});
    const binary = resolveBinary({
      environment: ${JSON.stringify(override ? { RIFTRI_BINARY: override } : {})},
      platform: ${JSON.stringify(platform)}, architecture: 'arm64',
      report: {getReport: () => ({header: {glibcVersionRuntime: '2.39'}})},
    });
    process.stdout.write(binary);
  `,
      ],
      { encoding: "utf8", timeout: 10000 },
    );
  return { binary, unpacked, resolve };
}

test("Electron resolves an unpacked native executable, including Windows sidecars", async (t) => {
  for (const [archive, platform] of [
    ["app.asar", "darwin"],
    ["app.asar", "linux"],
    ["server.asar", "win32"],
  ]) {
    const f = await fixture(t, archive, platform);
    const result = f.resolve();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, f.unpacked);
    if (process.platform !== "win32") {
      const native = spawnSync(result.stdout, [], {
        encoding: "utf8",
        timeout: 10000,
      });
      assert.equal(native.status, 0, native.stderr);
      assert.equal(native.stdout, "native executable reached\n");
    }
  }
});

test("Electron refuses a missing unpacked binary even when the archive path is readable", async (t) => {
  const f = await fixture(t);
  await fs.unlink(f.unpacked);
  const result = f.resolve();
  assert.equal(result.status, 1);
  assert.ok(result.stderr.includes(f.unpacked));
  assert.match(result.stderr, /does not contain an executable Riftri binary/);
});

test(
  "Electron validates the physical unpacked binary",
  { skip: process.platform === "win32" },
  async (t) => {
    const f = await fixture(t);
    await fs.chmod(f.unpacked, 0o644);
    const result = f.resolve();
    assert.equal(result.status, 1);
    assert.ok(result.stderr.includes(f.unpacked));
  },
);

test("ordinary Node directories named .asar are not rewritten", async (t) => {
  const f = await fixture(t);
  const result = f.resolve({ electron: false });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, f.binary);
});

test("already unpacked packages are not rewritten again", async (t) => {
  const f = await fixture(t, "app.asar.unpacked");
  const result = f.resolve();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, f.binary);
});

test("explicit binary overrides retain their exact path in Electron", async (t) => {
  const f = await fixture(t);
  const result = f.resolve({ override: f.binary });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, f.binary);
});
