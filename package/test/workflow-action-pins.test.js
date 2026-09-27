"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");
const workflowDirectory = path.join(root, ".github/workflows");

/** Every `uses:` in every workflow, as `{ file, line, value }`. */
function actionReferences() {
  const files = fs
    .readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"));
  assert.ok(files.length > 0, "there should be workflows to check");

  const references = [];
  for (const file of files) {
    const text = fs.readFileSync(path.join(workflowDirectory, file), "utf8");
    text.split("\n").forEach((line, index) => {
      const match = /^\s*(?:-\s+)?uses:\s*(\S+)(.*)$/.exec(line);
      if (match) {
        references.push({
          file,
          line: index + 1,
          value: match[1],
          trailing: match[2],
          raw: line.trim(),
        });
      }
    });
  }
  return references;
}

/** A reference to a workflow or action inside this repository, not a third party. */
function isLocal(reference) {
  return reference.value.startsWith("./");
}

test("there are actions to check at all", () => {
  // Guards against the scanner silently matching nothing, which would make
  // every assertion below vacuous.
  const references = actionReferences().filter((reference) => !isLocal(reference));
  assert.ok(references.length >= 5, `only found ${references.length} action references`);
});

test("every third-party action is pinned to a full commit SHA", () => {
  const unpinned = [];

  for (const reference of actionReferences()) {
    if (isLocal(reference)) {
      continue;
    }
    // A tag or branch is mutable: whoever controls it can change what runs in
    // a workflow that holds write permissions and repository secrets.
    if (!/@[0-9a-f]{40}$/.test(reference.value)) {
      unpinned.push(`${reference.file}:${reference.line} ${reference.raw}`);
    }
  }

  assert.deepEqual(unpinned, [], `not pinned to a SHA:\n  ${unpinned.join("\n  ")}`);
});

test("every pin records the human-readable version it came from", () => {
  const uncommented = [];

  for (const reference of actionReferences()) {
    if (isLocal(reference)) {
      continue;
    }
    // Without the comment a bare SHA is unreadable, and nobody can tell whether
    // a pin is current without resolving it by hand.
    if (!/^\s*#\s*v\d+(?:\.\d+)*/.test(reference.trailing)) {
      uncommented.push(`${reference.file}:${reference.line} ${reference.raw}`);
    }
  }

  assert.deepEqual(
    uncommented,
    [],
    `pinned without a "# vX.Y.Z" comment:\n  ${uncommented.join("\n  ")}`,
  );
});

test("one action resolves to one SHA across every workflow", () => {
  const shas = new Map();

  for (const reference of actionReferences()) {
    if (isLocal(reference) || !reference.value.includes("@")) {
      continue;
    }
    const [action, sha] = reference.value.split("@");
    if (!shas.has(action)) {
      shas.set(action, new Map());
    }
    const seen = shas.get(action);
    seen.set(sha, [...(seen.get(sha) ?? []), `${reference.file}:${reference.line}`]);
  }

  const drifted = [];
  for (const [action, seen] of shas) {
    if (seen.size > 1) {
      const where = [...seen.entries()]
        .map(([sha, sites]) => `${sha.slice(0, 12)} (${sites.join(", ")})`)
        .join(" vs ");
      drifted.push(`${action}: ${where}`);
    }
  }

  // Two SHAs for one action means an upgrade stopped half way, so some jobs run
  // a version nobody reviewed against the other.
  assert.deepEqual(drifted, [], `pinned to more than one SHA:\n  ${drifted.join("\n  ")}`);
});
