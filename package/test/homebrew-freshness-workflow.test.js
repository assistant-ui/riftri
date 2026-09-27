"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const workflowPath = ".github/workflows/homebrew-freshness.yml";

function readWorkflow() {
  return fs.readFileSync(path.join(root, workflowPath), "utf8");
}

/** The text of one top-level job block, from its key to the next job key. */
function jobBlock(workflow, id) {
  const match = new RegExp(`\\n  ${id}:\\n([\\s\\S]*?)(?=\\n  [a-z][\\w-]*:\\n|$)`).exec(
    workflow,
  );
  assert.ok(match, `the ${id} job is missing from ${workflowPath}`);
  return match[1];
}

test("a published release regenerates the formula without waiting for the daily sweep", () => {
  const workflow = readWorkflow();

  // The release is what makes the formula stale, and the generator can only run
  // once that release's SHA256SUMS exists, so this is the earliest safe trigger.
  assert.match(workflow, /\n  release:\n(?:.*\n)*?\s+types:\n\s+- published\n/);
  assert.match(workflow, /\n  schedule:\n(?:.*\n)*?\s+- cron: /);
});

test("only the job that mutates the repository is allowed to write", () => {
  const workflow = readWorkflow();

  // A read-only default at the top, so a new job cannot silently inherit write.
  assert.match(workflow, /\npermissions:\n  contents: read\n/);

  const check = jobBlock(workflow, "check");
  assert.match(check, /permissions:\n\s+contents: read\n/);
  assert.doesNotMatch(check, /contents: write/);
  assert.doesNotMatch(check, /pull-requests: write/);

  const sync = jobBlock(workflow, "sync");
  assert.match(sync, /permissions:\n\s+contents: write\n\s+pull-requests: write\n/);
});

test("the sync job runs only when the check job reported drift", () => {
  const sync = jobBlock(readWorkflow(), "sync");

  assert.match(sync, /needs: check\n/);
  assert.match(sync, /if: needs\.check\.outputs\.drift == 'true'\n/);
});

test("the regenerated formula is re-verified against the release before it is proposed", () => {
  const sync = jobBlock(readWorkflow(), "sync");

  const regenerate = sync.indexOf("sync-homebrew-tap.mjs \"${LATEST_TAG}\"");
  const verify = sync.indexOf("sync-homebrew-tap.mjs --check");
  const propose = sync.indexOf("gh pr create");

  assert.ok(regenerate !== -1, "the sync job never regenerates the formula");
  assert.ok(verify !== -1, "the sync job never re-verifies the regenerated formula");
  assert.ok(propose !== -1, "the sync job never opens a pull request");
  // --check resolves the latest release on its own, so it fails if regenerating
  // did not actually resolve the drift. It has to happen before the proposal.
  assert.ok(regenerate < verify, "the formula is verified before it is regenerated");
  assert.ok(verify < propose, "the pull request is opened before the formula is verified");
});

test("both jobs inspect main rather than whichever ref triggered them", () => {
  const workflow = readWorkflow();

  // A release event checks out the tag by default, whose formula is the
  // pre-release one; the question is always whether main tracks the release.
  const checkouts = workflow.match(/uses: actions\/checkout@[\s\S]*?ref: main\n/g) ?? [];
  assert.equal(checkouts.length, 2, "every checkout should pin ref: main");
});

test("concurrent runs queue instead of cancelling a half-finished sync", () => {
  const workflow = readWorkflow();

  // The sync job force-pushes a shared branch and then opens a pull request.
  // Cancelling between those two steps would leave a branch with no proposal,
  // and a per-ref group would let a release run and the daily run interleave.
  assert.match(workflow, /concurrency:\n(?:.*\n)*?\s+group: homebrew-freshness\n/);
  assert.match(workflow, /cancel-in-progress: false\n/);
  assert.doesNotMatch(workflow, /group: homebrew-freshness-\$\{\{/);
});
