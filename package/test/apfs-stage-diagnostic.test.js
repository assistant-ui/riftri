const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");
const directory = path.join(root, "docs/benchmarks");
const report = JSON.parse(fs.readFileSync(path.join(directory, "apfs-stage-local-2026-10-09.json")));
const median = values => { const sorted = [...values].sort((a,b) => a-b); return (sorted[(sorted.length-1)>>1] + sorted[sorted.length>>1])/2; };

test("diagnostic patches retain exact bytes on Windows checkouts", () => {
  assert.ok(fs.readFileSync(path.join(root, ".gitattributes"), "utf8").split(/\r?\n/).includes("*.patch text eol=lf"));
});

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

test("hosted stage diagnosis retains complete samples, summaries and native stacks", () => {
  const hosted = JSON.parse(fs.readFileSync(path.join(directory, "apfs-stage-ci-2026-10-09.json")));
  assert.equal(hosted.kind, "diagnostic, not a performance claim");
  assert.equal(hosted.fixtures.length, 2);
  assert.equal(hosted.fixtures.reduce((total, run) => total + run.samples.length, 0), 82);
  assert.equal(hosted.fixtures.reduce((total, run) => total + run.batches.length, 0), 20);
  assert.equal(hosted.patchSha256, report.patchSha256);
  assert.equal(hosted.controlPatchSha256, report.controlPatchSha256);
  const stacks = [];
  for (const run of hosted.fixtures) {
    assert.equal(run.stageDiagnostics, true);
    assert.equal(run.complete, true);
    assert.equal(run.failure, null);
    assert.equal(run.finalActiveViews, 0);
    assert.equal(run.finalBases, 0);
    assert.equal(run.finalDiagnosticIssues, 0);
    assert.equal(run.samples.length, 1 + 2 * run.rounds * run.concurrency);
    assert.equal(run.batches.length, 2 * run.rounds);
    for (const sample of run.samples) {
      assert.equal(sample.success, true);
      assert.equal(sample.code, 0);
      assert.equal(sample.timedOut, false);
      assert.equal(sample.indexScanned, run.trackedEntries);
      assert.equal(sample.indexEntries, run.trackedEntries);
      const pending = new Set();
      for (const phase of sample.phases) {
        const match = /^riftri: (apfs|journal)-stage: (start|finish) (\S+) pid=(\d+)(?: microseconds=(\d+))?$/.exec(phase.line);
        if (!match) continue;
        const key = `${match[1]}:${match[3]}:${match[4]}`;
        if (match[2] === "start") {
          assert.equal(pending.has(key), false);
          pending.add(key);
        } else {
          assert.ok(pending.delete(key), `unmatched finish: ${key}`);
          assert.ok(Number.isFinite(Number(match[5])));
        }
      }
      assert.equal(pending.size, 0);
      for (const stack of sample.stackSamples) {
        assert.equal(sample.label, "baseline");
        assert.equal(sample.round, 2);
        assert.equal(run.name, "synthetic-four");
        assert.equal(stack.code, 0);
        assert.equal(stack.timedOut, false);
        assert.equal(createHash("sha256").update(stack.text).digest("hex"), stack.sha256);
        assert.match(stack.text, /Call graph:/);
        assert.match(stack.text, /riftri-stage-control/);
        stacks.push(stack);
      }
    }
    for (const label of ["baseline", "advice"]) {
      const stages = {};
      for (const sample of run.samples.filter(sample => sample.round > 0 && sample.label === label)) {
        for (const phase of sample.phases) {
          const match = /^riftri: (apfs|journal)-stage: finish (\S+) pid=\d+ microseconds=(\d+)$/.exec(phase.line);
          if (match) (stages[`${match[1]}:${match[2]}`] ??= []).push(Number(match[3])/1000);
        }
      }
      assert.deepEqual(Object.keys(stages).sort(), Object.keys(run.summaries[label]).sort());
      for (const [stage, values] of Object.entries(stages)) {
        assert.equal(values.length, run.rounds * run.concurrency);
        assert.deepEqual(run.summaries[label][stage], { samples:values.length, medianMilliseconds:median(values), maximumMilliseconds:Math.max(...values) });
      }
    }
  }
  assert.equal(stacks.length, 3);
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /does not explain or resolve/);
  assert.match(markdown, /overall CI run failed/);
  assert.match(markdown, /child Git processes were not sampled/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8"), /^  apfs-stage-diagnostics:/m);
});

test("the losing single-issuer experiment stays recorded rather than adopted", () => {
  const experiment = JSON.parse(fs.readFileSync(path.join(directory, "apfs-single-issuer-2026-10-09.json")));
  assert.equal(experiment.decision, "rejected; scheduling change reverted");
  assert.equal(experiment.stageDiagnostics, false);
  assert.match(experiment.baselineDescription, /not main/);
  assert.match(experiment.candidatePatch, /deferred_read_ahead_uses_one_issuer/);
  assert.equal(experiment.complete, true);
  assert.equal(experiment.samples.length, 65);
  assert.equal(experiment.batches.length, 16);
  assert.equal(experiment.finalActiveViews, 0);
  assert.equal(experiment.finalBases, 0);
  assert.equal(experiment.finalDiagnosticIssues, 0);
  for (const sample of experiment.samples) {
    assert.equal(sample.success, true);
    assert.equal(sample.code, 0);
    assert.equal(sample.timedOut, false);
    assert.equal(sample.indexScanned, 5864);
    assert.equal(sample.indexEntries, 5864);
  }
  for (const label of ["baseline", "advice"]) {
    assert.equal(experiment.summary[label].medianMilliseconds,
      median(experiment.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
  }
  assert.equal(experiment.summary.fasterPairs, experiment.batches.filter(batch => batch.label === "advice" &&
    batch.milliseconds < experiment.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
  assert.equal(experiment.summary.wallReductionPercent,
    100 * (1 - experiment.summary.advice.medianMilliseconds / experiment.summary.baseline.medianMilliseconds));
  assert.equal(experiment.summary.fasterPairs, 2);
  assert.ok(experiment.summary.wallReductionPercent < 0);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs.rs"), "utf8"),
    /deferred_read_ahead_uses_one_issuer/);
});
