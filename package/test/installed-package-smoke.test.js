"use strict";

const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const manifest = require("../../package.json");

test(
  "installs packed artifacts and exercises the packaged Rust CLI",
  { timeout: 120_000 },
  async () => {
    const repositoryRoot = path.resolve(__dirname, "..", "..");
    const { smokeInstalledPackage } = await import(
      "../scripts/smoke-installed-package.mjs"
    );
    const result = await smokeInstalledPackage({ repositoryRoot });

    assert.equal(result.version.trim(), `riftri ${manifest.version}`);
    assert.equal(result.installMode, "isolated-global-prefix");
    assert.equal(result.lifecycleTested, process.platform === "darwin");
  },
);
