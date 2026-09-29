"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");
const workflowDirectory = path.join(root, ".github/workflows");

/**
 * Every job in one workflow, as `{ id, body }`.
 *
 * The scan starts at the `jobs:` mapping rather than at the top of the file:
 * `on:` also has two-space keys, so `push:` and `schedule:` would otherwise be
 * read as jobs.
 */
function jobs(workflow) {
  const start = workflow.search(/^jobs:$/m);
  assert.notEqual(start, -1, "a workflow must declare a jobs mapping");
  const section = workflow.slice(start);

  const found = [];
  const header = /^ {2}([A-Za-z][\w-]*):$/gm;
  let match;
  while ((match = header.exec(section)) !== null) {
    found.push({
      id: match[1],
      start: match.index,
      from: match.index + match[0].length,
    });
  }
  // Each body ends where the next job begins. Slicing to the end of the file
  // instead would let every job see a later job's keys, so a job that lost its
  // own timeout would still look bounded.
  return found.map((job, index) => ({
    id: job.id,
    body: section.slice(job.from, found[index + 1]?.start ?? section.length),
  }));
}

function workflowFiles() {
  const files = fs
    .readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"));
  assert.ok(files.length > 0, "there should be workflows to check");
  return files;
}

test("the job scanner finds the jobs actually declared, not keys under on:", () => {
  // A fixture rather than a real workflow: the parser has to be tested against
  // known input, or a miscount silently weakens every assertion below.
  const fixture = [
    "name: Example",
    "",
    "on:",
    "  push:",
    "    branches:",
    "      - main",
    "  schedule:",
    "    - cron: \"0 0 * * *\"",
    "",
    "jobs:",
    "  first:",
    "    runs-on: ubuntu-24.04",
    "    timeout-minutes: 30",
    "    steps:",
    "      - run: |",
    "          echo hi",
    "",
    "  second:",
    "    runs-on: ubuntu-24.04",
    "    steps:",
    "      - run: echo bye",
    "",
  ].join("\n");

  const found = jobs(fixture);
  assert.deepEqual(
    found.map((job) => job.id),
    ["first", "second"],
    "push and schedule are not jobs",
  );
  assert.match(found[0].body, /timeout-minutes: 30/);
  assert.doesNotMatch(found[1].body, /timeout-minutes/);
});

test("a job's body stops where the next job starts", () => {
  // The unbounded job comes first and the bounded one second, so a body that
  // ran past its own job would borrow the later timeout and look bounded.
  const fixture = [
    "jobs:",
    "  unbounded:",
    "    runs-on: ubuntu-24.04",
    "    steps:",
    "      - run: echo hi",
    "",
    "  bounded:",
    "    runs-on: ubuntu-24.04",
    "    timeout-minutes: 30",
    "    steps:",
    "      - run: echo bye",
    "",
  ].join("\n");

  const [first, second] = jobs(fixture);
  assert.equal(first.id, "unbounded");
  assert.doesNotMatch(first.body, /timeout-minutes/, "the first job borrowed the second's bound");
  assert.match(second.body, /timeout-minutes: 30/);
});

test("every workflow job bounds its runtime", () => {
  const unbounded = [];

  for (const file of workflowFiles()) {
    const workflow = fs.readFileSync(path.join(workflowDirectory, file), "utf8");
    for (const job of jobs(workflow)) {
      // A job that only calls a reusable workflow inherits that workflow's
      // bounds and cannot set its own.
      if (!/^\s+runs-on:/m.test(job.body)) {
        continue;
      }
      if (!/^\s+timeout-minutes: \d+$/m.test(job.body)) {
        unbounded.push(`${file}:${job.id}`);
      }
    }
  }

  // Without a bound, a hung job sits on GitHub's six-hour default, blocking the
  // pull request and burning runner minutes with no diagnostic (#399, #400).
  assert.deepEqual(
    unbounded,
    [],
    `these jobs have no timeout-minutes: ${unbounded.join(", ")}`,
  );
});

test("no bound is so large that it stops being a backstop", () => {
  const tooLarge = [];

  for (const file of workflowFiles()) {
    const workflow = fs.readFileSync(path.join(workflowDirectory, file), "utf8");
    for (const job of jobs(workflow)) {
      const match = /^\s+timeout-minutes: (\d+)$/m.exec(job.body);
      if (match && Number(match[1]) > 60) {
        tooLarge.push(`${file}:${job.id} (${match[1]}m)`);
      }
    }
  }

  // The point is to convert a six-hour invisible stall into a visible failure.
  // An hour is already generous against the slowest measured job.
  assert.deepEqual(tooLarge, [], `bounds over 60m: ${tooLarge.join(", ")}`);
});

test("the publish job outlasts both npm propagation budgets", async () => {
  // The verification budget and the job bound are set in different files, so
  // raising the budget can silently push the job past its timeout -- which
  // would turn a ridable propagation delay into a hard kill mid-release,
  // exactly the failure the budget exists to prevent.
  const { VERIFICATION_ATTEMPTS, verificationWaits } = await import(
    "../scripts/publish-packages.mjs"
  );
  const phaseMinutes =
    verificationWaits(VERIFICATION_ATTEMPTS).reduce(
      (total, milliseconds) => total + milliseconds,
      0,
    ) / 60_000;
  // Both phases can time out in one run: once before the launcher and once
  // after it. Publishing nine packages adds a couple of minutes on top.
  const worstCaseMinutes = phaseMinutes * 2 + 5;

  const workflow = fs.readFileSync(
    path.join(workflowDirectory, "release.yml"),
    "utf8",
  );
  const publish = jobs(workflow).find((job) => job.id === "publish");
  assert.ok(publish, "release.yml must declare a publish job");
  const match = /^\s+timeout-minutes: (\d+)$/m.exec(publish.body);
  assert.ok(match, "the publish job must bound itself");

  assert.ok(
    Number(match[1]) > worstCaseMinutes,
    `publish is bounded at ${match[1]}m but can spend ${worstCaseMinutes}m ` +
      `waiting on npm; raise the bound or lower VERIFICATION_ATTEMPTS`,
  );
});
