"use strict";

const assert = require("node:assert/strict");
const { cp, mkdir, mkdtemp, rm } = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const packageVersion = require("../../package.json").version;

async function releaseFixture(t) {
  const sourceRoot = path.resolve(__dirname, "..", "..");
  const root = await mkdtemp(path.join(os.tmpdir(), "riftri-npm-publish-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await cp(
    path.join(sourceRoot, "package", "platforms"),
    path.join(root, "package", "platforms"),
    { recursive: true },
  );
  await mkdir(path.join(root, "dist", "npm-root"), { recursive: true });
  await cp(
    path.join(sourceRoot, "package.json"),
    path.join(root, "dist", "npm-root", "package.json"),
  );
  return root;
}

function fakeRegistry({ existing = [], reject = null, omitAfterPublish = null } = {}) {
  const published = new Set(existing);
  const commands = [];
  return {
    commands,
    runNpm(arguments_) {
      commands.push(arguments_);
      if (arguments_[0] === "view") {
        const identifier = arguments_[1];
        if (published.has(identifier)) {
          return {
            status: 0,
            stdout: `${JSON.stringify(identifier.split("@").at(-1))}\n`,
            stderr: "",
          };
        }
        return { status: 1, stdout: "", stderr: "npm error code E404\n" };
      }
      assert.equal(arguments_[0], "publish");
      const packageName = path.basename(arguments_[1]);
      const identifier =
        packageName === "npm-root"
          ? `riftri@${packageVersion}`
          : `${packageName}@${packageVersion}`;
      if (identifier === reject) {
        return { status: 1, stdout: "", stderr: "npm error code E403\n" };
      }
      if (identifier !== omitAfterPublish) published.add(identifier);
      return { status: 0, stdout: "", stderr: "" };
    },
  };
}

test("resumes a partial release, publishes the launcher last, and verifies every exact version", async (t) => {
  const repositoryRoot = await releaseFixture(t);
  const registry = fakeRegistry({
    existing: [
      `riftri-darwin-arm64@${packageVersion}`,
      `riftri-linux-x64-gnu@${packageVersion}`,
    ],
  });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");

  const packages = await publishPackages({
    repositoryRoot,
    runNpm: registry.runNpm,
  });

  assert.equal(packages.length, 9);
  const publishes = registry.commands.filter(([command]) => command === "publish");
  assert.equal(publishes.length, 7);
  assert.equal(path.basename(publishes.at(-1)[1]), "npm-root");
  assert.ok(
    registry.commands.filter(([command]) => command === "view").length >= 11,
    "the resume pass and final complete-set verification must both query npm",
  );
});

test("stops on a registry rejection without attempting the launcher", async (t) => {
  const repositoryRoot = await releaseFixture(t);
  const rejected = `riftri-linux-arm64-gnu@${packageVersion}`;
  const registry = fakeRegistry({ reject: rejected });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");

  await assert.rejects(
    publishPackages({ repositoryRoot, runNpm: registry.runNpm }),
    new RegExp(`publishing ${rejected} failed`),
  );
  assert.equal(
    registry.commands.some(
      ([command, directory]) =>
        command === "publish" && path.basename(directory) === "npm-root",
    ),
    false,
  );
});

test("fails if a successful publish is not visible in the final complete-set verification", async (t) => {
  const repositoryRoot = await releaseFixture(t);
  const missing = `riftri-win32-x64@${packageVersion}`;
  const registry = fakeRegistry({ omitAfterPublish: missing });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");

  await assert.rejects(
    publishPackages({
      repositoryRoot,
      runNpm: registry.runNpm,
      wait: async () => {},
      verificationAttempts: 2,
    }),
    new RegExp(`${missing} still missing`),
  );
});
