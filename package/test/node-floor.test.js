"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");

function ciWorkflow() {
  return fs.readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8");
}

/** The `node-floor` job's text, from its key to the next job key. */
function nodeFloorJob() {
  const workflow = ciWorkflow();
  const match = /\n {2}node-floor:\n([\s\S]*?)(?=\n {2}[a-z][\w-]*:\n|$)/.exec(workflow);
  assert.ok(match, "ci.yml must define a node-floor job");
  return match[1];
}

test("package.json declares the Node floor as a >= range", () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  const range = manifest.engines?.node;

  // The CI job parses this shape to derive the version it installs. A range
  // like "^20 || >=22" would silently break that derivation.
  assert.match(
    range ?? "",
    /^>=\d+\.\d+(?:\.\d+)?$/,
    `engines.node must be a ">=X.Y" floor, found ${JSON.stringify(range)}`,
  );
});

test("CI verifies the launcher on the floor package.json declares", () => {
  const job = nodeFloorJob();

  // Derived from engines, never repeated: a second copy of the number is the
  // thing that drifts.
  assert.match(
    job,
    /jq -r '\.engines\.node' package\.json/,
    "the job must read the floor from engines.node",
  );
  assert.match(
    job,
    /node-version: \$\{\{ steps\.floor\.outputs\.version \}\}/,
    "setup-node must install the derived floor",
  );
  assert.doesNotMatch(
    job,
    /node-version: *\d/,
    "the job must not hard-code a Node version",
  );
});

test("the floor job proves the floor is what actually ran", () => {
  const job = nodeFloorJob();

  // setup-node resolves a range to its newest match, so without this the job
  // could pass while running a current Node.
  //
  // Matching the assignment, not just the expression: process.versions.node
  // also appears in the client-API check's log line, so a looser pattern stays
  // satisfied even after this check is deleted.
  assert.match(
    job,
    /running="\$\(node -p 'process\.versions\.node'\)"/,
    "the job must capture the running Node version",
  );
  assert.match(job, /::error::Expected Node/, "the job must fail on the wrong version");
});

test("the floor job exercises what ships, not the dev suite", () => {
  const job = nodeFloorJob();

  assert.match(job, /package\/bin\/riftri\.js --version/, "the job must run the shipped bin");
  assert.match(job, /require\("\.\/package\/lib\/client\.js"\)/, "the job must load the shipped client");

  // Comments are stripped first: the job explains in prose why the dev suite
  // cannot run here, and that explanation must not read as an invocation.
  const commands = job
    .split("\n")
    .filter((line) => !/^\s*#/.test(line))
    .join("\n");

  // npm test passes --test-timeout, which Node rejects before 20.
  assert.doesNotMatch(commands, /\bnpm (?:run )?test\b/, "the dev suite cannot run on the floor");
});
