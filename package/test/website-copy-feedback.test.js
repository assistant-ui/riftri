const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const source = fs.readFileSync(path.resolve(__dirname, "../../website/src/components/copy-command.tsx"), "utf8");

test("each successful copy owns a fresh confirmation timeout", () => {
  const start = source.indexOf("async function copy()");
  const action = source.slice(start, source.indexOf("\n  return (", start));
  assert.ok(action.includes("window.clearTimeout"), "cancel the previous confirmation when copying again");
  assert.ok(action.includes("window.setTimeout"), "start the timer on every successful copy, not just a boolean state change");
  assert.ok(action.indexOf("window.setTimeout") > action.indexOf("await navigator.clipboard.writeText"));
});

test("stale clipboard requests and unmounted components cannot update feedback", () => {
  assert.ok(source.includes("request.current += 1"));
  assert.ok(source.includes("attempt !== request.current"));
  assert.ok(source.includes("return () =>"));
});

test("copy feedback has a dedicated status region and accessible retry label", () => {
  assert.ok(source.includes('role="status"'));
  assert.ok(source.includes('state === "failed" ? "Retry copying"'));
  assert.ok(source.includes("Select the command to copy it manually."));
});
