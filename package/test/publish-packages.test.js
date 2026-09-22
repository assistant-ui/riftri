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

test("does not publish the launcher if a platform publish never becomes visible", async (t) => {
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
  assert.equal(
    registry.commands.some(
      ([command, directory]) =>
        command === "publish" && path.basename(directory) === "npm-root",
    ),
    false,
    "the launcher must remain unpublished until every platform is visible",
  );
});

test("still verifies launcher visibility after publication", async (t) => {
  const repositoryRoot = await releaseFixture(t);
  const missing = `riftri@${packageVersion}`;
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

test("waits for platform visibility before publishing the launcher", async (t) => {
  const repositoryRoot = await releaseFixture(t);
  const delayed = `riftri-win32-x64@${packageVersion}`;
  const registry = fakeRegistry({ omitAfterPublish: delayed });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");
  let visible = false;
  let waits = 0;
  await publishPackages({
    repositoryRoot,
    runNpm(arguments_, options) {
      if (arguments_[0] === "publish" && path.basename(arguments_[1]) === "npm-root") {
        assert.equal(visible, true, "platform packages must propagate first");
      }
      if (visible && arguments_[0] === "view" && arguments_[1] === delayed) {
        return { status: 0, stdout: JSON.stringify(packageVersion), stderr: "" };
      }
      return registry.runNpm(arguments_, options);
    },
    wait: async () => { visible = true; waits += 1; },
    verificationAttempts: 2,
  });
  assert.equal(waits, 1);
});

test("a name the registry already refused does not strand the launcher", async (t) => {
  // v0.2.1 died exactly here: riftri-win32-arm64 sorts before riftri-win32-x64
  // and the launcher, so one refusal left npm with no installable riftri.
  const repositoryRoot = await releaseFixture(t);
  const blocked = `riftri-win32-arm64@${packageVersion}`;
  const registry = fakeRegistry({ reject: blocked });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");

  const packages = await publishPackages({
    repositoryRoot,
    runNpm: registry.runNpm,
  });

  assert.ok(!packages.includes(blocked), "a refused name must not be verified");
  assert.equal(packages.length, 8);
  const publishes = registry.commands.filter(([command]) => command === "publish");
  // Attempted anyway — that attempt is how we learn the name was unblocked.
  assert.ok(publishes.some(([, directory]) => path.basename(directory) === "riftri-win32-arm64"));
  assert.equal(path.basename(publishes.at(-1)[1]), "npm-root");
  assert.ok(
    publishes.some(([, directory]) => path.basename(directory) === "riftri-win32-x64"),
    "packages queued behind the refused name must still publish",
  );
});

test("an unexpected rejection still stops the release", async (t) => {
  // The tolerance is per-name; any other failure must remain fatal.
  const repositoryRoot = await releaseFixture(t);
  const rejected = `riftri-darwin-x64@${packageVersion}`;
  const registry = fakeRegistry({ reject: rejected });
  const { publishPackages } = await import("../scripts/publish-packages.mjs");

  await assert.rejects(
    publishPackages({ repositoryRoot, runNpm: registry.runNpm }),
    new RegExp(`publishing ${rejected} failed`),
  );
});
