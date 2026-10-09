const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");
const directory = path.join(root, "docs/benchmarks");
const report = JSON.parse(fs.readFileSync(path.join(directory, "apfs-stage-local-2026-10-09.json")));
const median = values => { const sorted = [...values].sort((a,b) => a-b); return (sorted[(sorted.length-1)>>1] + sorted[sorted.length>>1])/2; };

test("stage diagnostics stay in explicit build patches, not the normal Rust runtime", () => {
  for (const file of ["crates/riftri-storage/src/apfs.rs", "crates/riftri-core/src/worktree.rs"]) {
    assert.doesNotMatch(fs.readFileSync(path.join(root, file), "utf8"), /DiagnosticStage|DIAGNOSTIC_HINTS|riftri: journal-stage:/);
  }
  for (const [file, expected] of [["apfs-stage-diagnostic.patch", report.patchSha256], ["apfs-stage-control.patch", report.controlPatchSha256]]) {
    assert.equal(createHash("sha256").update(fs.readFileSync(path.join(directory, file))).digest("hex"), expected);
  }
  assert.notEqual(report.controlSha256, report.hintsSha256);
});

test("local stage diagnosis retains all observations and labels its limits", () => {
  assert.equal(report.kind, "diagnostic, not a performance claim");
  assert.equal(report.stageDiagnostics, true);
  assert.equal(report.complete, true);
  assert.equal(report.samples.length, 33);
  assert.equal(report.batches.length, 8);
  assert.equal(report.finalActiveViews, 0);
  assert.equal(report.finalBases, 0);
  assert.equal(report.finalDiagnosticIssues, 0);
  for (const sample of report.samples) {
    assert.equal(sample.success, true);
    assert.equal(sample.code, 0);
    assert.equal(sample.timedOut, false);
    assert.equal(sample.indexScanned, report.trackedEntries);
    assert.deepEqual(sample.stackSamples, []);
  }
  for (const label of ["baseline", "advice"]) {
    const stages = {};
    for (const sample of report.samples.filter(sample => sample.round > 0 && sample.label === label)) {
      for (const phase of sample.phases) {
        const match = /^riftri: (apfs|journal)-stage: finish (\S+) pid=\d+ microseconds=(\d+)$/.exec(phase.line);
        if (match) (stages[`${match[1]}:${match[2]}`] ??= []).push(Number(match[3])/1000);
      }
    }
    assert.deepEqual(Object.keys(stages).sort(), Object.keys(report.summaries[label]).sort());
    for (const [stage, values] of Object.entries(stages)) {
      assert.equal(values.length, 16);
      assert.deepEqual(report.summaries[label][stage], { samples:values.length, medianMilliseconds:median(values), maximumMilliseconds:Math.max(...values) });
    }
  }
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /not another speedup claim/);
  assert.match(markdown, /no\s+timeout reproduced locally/);
  assert.match(markdown, /not a causal proof/);
});
