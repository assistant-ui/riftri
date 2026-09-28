"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");

/** The macOS APFS teardown step's script. */
function detachStep() {
  const workflow = fs.readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8");
  const match = /\n {6}- name: Detach disposable APFS benchmark volume\n([\s\S]*?)(?=\n {6}- name: )/.exec(
    workflow,
  );
  assert.ok(match, "ci.yml must define the APFS detach teardown step");
  return match[1];
}

test("the teardown retries a busy detach instead of failing on the first attempt", () => {
  const step = detachStep();

  // The #431 capture found the volume busy at detach but with no cwd inside it
  // and no open file on it seconds later: the holder is transient, so one
  // attempt turns a settling disk image into a red build.
  assert.match(step, /while :;? do|for .* in /, "the detach must be retried in a loop");
  assert.match(step, /sleep /, "retries must back off rather than spin");
  assert.match(step, /attempt/, "the step should count its attempts");
});

test("the retry is bounded, so a genuinely stuck volume still fails the job", () => {
  const step = detachStep();

  // An unbounded retry would recreate the six-hour stall #400 removed.
  const cap = /attempt.*-ge (\d+)/.exec(step);
  assert.ok(cap, "the retry loop must compare the attempt against a cap");
  const attempts = Number(cap[1]);
  assert.ok(attempts >= 2 && attempts <= 10, `implausible retry cap: ${attempts}`);
  assert.match(step, /\n\s*exit 1\n/, "exhausted retries must fail the job");
});

test("the teardown never forces the detach", () => {
  const step = detachStep();

  // --force would unmount regardless of the holder, which is exactly how a
  // real leak would stop being visible. Retrying a plain detach distinguishes
  // a transient holder from a leak; forcing hides both.
  assert.doesNotMatch(step, /hdiutil detach[^\n]*-force/, "the detach must never be forced");
});

test("the holder diagnostics survive the retry", () => {
  const step = detachStep();

  // The whole point of #431/#432 was that rerunning destroys the evidence. A
  // retry must not quietly replace the capture.
  assert.match(step, /working directories inside the volume/);
  assert.match(step, /open files on the volume/);
  assert.match(step, /ps -axo/);
  assert.match(step, /::error::Could not detach/);

  // The diagnostics have to come after the loop; capturing before the retries
  // would report a holder that the next attempt then succeeds without.
  const loopEnd = step.lastIndexOf("done");
  const group = step.indexOf("::group::Processes holding");
  assert.ok(loopEnd !== -1 && group > loopEnd, "diagnostics must run after the retry loop");
});
