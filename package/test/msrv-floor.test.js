"use strict";

const assert = require("node:assert/strict");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");

/**
 * The declared minimum supported Rust version, read from the one place that
 * defines it. Every other mention of the floor is checked against this.
 */
async function declaredRustVersion() {
  const manifest = await readFile(path.join(root, "Cargo.toml"), "utf8");
  const match = /^rust-version = "(\d+\.\d+(?:\.\d+)?)"$/m.exec(manifest);
  assert.ok(match, "Cargo.toml must declare a workspace rust-version");
  return match[1];
}

/** Escape a version for embedding in a RegExp, since `.` is a metacharacter. */
function literal(version) {
  return version.replace(/\./g, "\\.");
}

test("the workspace declares a rust-version at all", async () => {
  const version = await declaredRustVersion();
  assert.match(version, /^\d+\.\d+/);
});

test("CI verifies the declared floor rather than a version frozen in the workflow", async () => {
  const version = await declaredRustVersion();
  const workflow = await readFile(path.join(root, ".github/workflows/ci.yml"), "utf8");
  const pattern = literal(version);

  // The job installs the toolchain and then pins cargo to it. Both carry the
  // number, and a bump to Cargo.toml that misses either one leaves CI quietly
  // proving the old floor still builds.
  assert.match(
    workflow,
    new RegExp(`rustup toolchain install ${pattern} `),
    `ci.yml must install Rust ${version}, the declared rust-version`,
  );
  assert.match(
    workflow,
    new RegExp(`cargo \\+${pattern} check --workspace --all-targets --locked`),
    `ci.yml must check the workspace on Rust ${version}`,
  );

  // The job name is what a reader sees in the checks list, so a stale name
  // misreports which floor was proven even when the commands are right.
  assert.match(
    workflow,
    new RegExp(`name: MSRV \\(Rust ${pattern}\\)`),
    `the MSRV job must be named for Rust ${version}`,
  );

  // Every toolchain reference anywhere in the workflow must name the declared
  // floor: a leftover `cargo +1.85` beside a correct install would still run
  // on the wrong toolchain, and the job would still pass.
  const toolchains = [
    ...workflow.matchAll(/rustup toolchain install (\d+\.\d+(?:\.\d+)?)/g),
    ...workflow.matchAll(/cargo \+(\d+\.\d+(?:\.\d+)?)/g),
  ].map((match) => match[1]);

  assert.ok(toolchains.length >= 2, "ci.yml should pin a toolchain to install and to run");
  for (const toolchain of toolchains) {
    assert.equal(
      toolchain,
      version,
      `ci.yml pins Rust ${toolchain}, but the declared floor is ${version}`,
    );
  }
});

test("the documented floor is the declared floor", async () => {
  const version = await declaredRustVersion();
  const safety = await readFile(path.join(root, "docs/safety.md"), "utf8");

  // docs/safety.md presents the MSRV job as part of the supply-chain surface,
  // so a stale number there misstates what the pipeline actually proves.
  assert.match(
    safety,
    new RegExp(`minimum supported Rust toolchain \\(${literal(version)}\\)`),
    `docs/safety.md must state Rust ${version} as the declared floor`,
  );
});

test("no crate overrides the workspace floor with its own", async () => {
  const version = await declaredRustVersion();
  const { readdir } = require("node:fs/promises");
  const crates = await readdir(path.join(root, "crates"));

  for (const crate of crates) {
    const manifestPath = path.join(root, "crates", crate, "Cargo.toml");
    const manifest = await readFile(manifestPath, "utf8").catch(() => null);
    if (manifest === null) {
      continue;
    }
    const own = /^rust-version = "(\d+\.\d+(?:\.\d+)?)"$/m.exec(manifest);
    assert.equal(
      own?.[1] ?? version,
      version,
      `crates/${crate} pins its own rust-version; the floor is one number`,
    );
  }
});
