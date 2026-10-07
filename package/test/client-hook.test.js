"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { Riftri, RiftriError } = require("../lib/client.js");

test("a real failed checkout hook keeps its managed worktree and never permits fallback", {
  // macOS CI supplies APFS; the shell hook fixture is Unix-specific.
  skip: process.platform !== "darwin",
}, async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-client-hook-"));
  const repository = path.join(directory, "repo");
  const destination = path.join(directory, "view");
  fs.mkdirSync(repository);
  const binary = path.resolve(__dirname, "../../target/debug/riftri");
  const riftri = new Riftri({ repository, binary });
  const git = (...args) => execFileSync("git", ["-C", repository, ...args], {
    encoding: "utf8",
    env: { ...process.env, RIFTRI_BYPASS: "1", GIT_CONFIG_GLOBAL: os.devNull, GIT_CONFIG_NOSYSTEM: "1" },
  }).trim();
  t.after(async () => {
    if (fs.existsSync(destination)) await riftri.worktree.remove(destination);
    if (fs.existsSync(path.join(repository, ".git", "riftri"))) await riftri.gc({ apply: true });
    fs.rmSync(directory, { recursive: true, force: true });
  });
  git("init", "-b", "main");
  git("config", "user.name", "Riftri test");
  git("config", "user.email", "riftri@example.invalid");
  fs.writeFileSync(path.join(repository, "tracked.txt"), "unchanged\n");
  git("add", "tracked.txt");
  git("-c", "commit.gpgsign=false", "commit", "-m", "fixture");
  const hooks = path.join(directory, "hooks");
  fs.mkdirSync(hooks);
  fs.writeFileSync(path.join(hooks, "post-checkout"), "#!/bin/sh\nexit 3\n", { mode: 0o755 });
  git("config", "core.hooksPath", hooks);

  await assert.rejects(riftri.worktree.add(destination, { branch: "topic", revision: "HEAD" }), (error) => {
    assert.ok(error instanceof RiftriError);
    assert.equal(error.exitCode, 3);
    assert.equal(error.isPolicyRefusal, false);
    assert.equal(error.report.post_checkout.exit_code, 3);
    assert.equal(fs.realpathSync(error.report.destination), fs.realpathSync(destination));
    return true;
  });
  assert.equal(git("-C", destination, "status", "--porcelain"), "");
  assert.equal(git("-C", destination, "rev-parse", "HEAD"), git("rev-parse", "HEAD"));
  assert.equal((await riftri.worktree.list()).worktrees.length, 1);
});
