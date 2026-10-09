const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const report = JSON.parse(fs.readFileSync(path.join(root, "docs/benchmarks/apfs-read-ahead-2026-10-09.json"), "utf8"));
const markdown = fs.readFileSync(path.join(root, "docs/benchmarks/apfs-read-ahead-2026-10-09.md"), "utf8");
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};

test("APFS read-ahead results retain all pairs and full initial Git scans", () => {
  assert.equal(report.schemaVersion, 1);
  assert.match(report.baselineSha256, /^[0-9a-f]{64}$/);
  assert.match(report.candidateSha256, /^[0-9a-f]{64}$/);
  assert.equal(report.runs.length, 6);
  assert.ok(report.allocationCheck.cached_volume_growth_bytes < report.allocationCheck.logical_payload_bytes / 4);
  for (const run of report.runs) {
    assert.equal(run.complete, true);
    assert.equal(run.finalActiveViews, 0);
    assert.equal(run.finalBases, 0);
    assert.equal(run.finalDiagnosticIssues, 0);
    assert.equal(run.samples.length, 1 + 2 * run.rounds * run.concurrency);
    assert.equal(run.batches.length, 2 * run.rounds);
    assert.equal(run.samples[0].round, 0);
    assert.equal(run.samples[0].label, "baseline");
    for (const sample of run.samples) {
      assert.equal(sample.indexScanned, run.trackedEntries, run.name);
      assert.equal(sample.indexEntries, run.trackedEntries, run.name);
      assert.equal(sample.cpuSeconds, sample.userSeconds + sample.systemSeconds);
      assert.ok(sample.milliseconds > 0);
      assert.ok(sample.indexRefreshSeconds >= 0);
      assert.ok(sample.maximumResidentBytes > 0);
    }
    for (let round = 1; round <= run.rounds; round++) {
      const pair = run.samples.filter((sample) => sample.round === round);
      assert.equal(pair[0].label, round % 2 ? "baseline" : "advice");
      for (const label of ["baseline", "advice"]) {
        const selected = pair.filter((sample) => sample.label === label);
        assert.equal(selected.length, run.concurrency);
        assert.equal(new Set(selected.map((sample) => sample.worker)).size, run.concurrency);
        assert.equal(run.batches.filter((batch) => batch.round === round && batch.label === label).length, 1);
      }
    }
  }
});

test("APFS read-ahead summaries are calculated from recorded samples, including losses", () => {
  for (const run of report.runs) {
    for (const label of ["baseline", "advice"]) {
      const samples = run.samples.filter((sample) => sample.round > 0 && sample.label === label);
      const batches = run.batches.filter((batch) => batch.label === label);
      assert.equal(run.summary[label].medianMilliseconds, median(batches.map((batch) => batch.milliseconds)));
      assert.equal(run.summary[label].medianCpuSecondsPerAdd, median(samples.map((sample) => sample.cpuSeconds)));
      assert.equal(run.summary[label].medianIndexRefreshMilliseconds, median(samples.map((sample) => sample.indexRefreshSeconds * 1000)));
    }
    assert.equal(run.summary.wallReductionPercent, 100 * (1 - run.summary.advice.medianMilliseconds / run.summary.baseline.medianMilliseconds));
    const faster = run.batches.filter((batch) => batch.label === "advice" && batch.milliseconds < run.batches.find((baseline) => baseline.label === "baseline" && baseline.round === batch.round).milliseconds).length;
    assert.equal(run.summary.fasterPairs, faster);
    if (!run.prototype) {
      const render = (value) => value.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
      assert.ok(markdown.includes(render(run.summary.baseline.medianMilliseconds)), run.name);
      assert.ok(markdown.includes(render(run.summary.advice.medianMilliseconds)), run.name);
      assert.ok(markdown.includes(`${run.summary.wallReductionPercent.toFixed(2)}%`), run.name);
    }
  }
  const concurrent = report.runs.find((run) => run.concurrency > 1);
  assert.equal(concurrent.summary.fasterPairs, 2);
  assert.match(markdown, /Do not present the lower concurrent median/);
  assert.match(markdown, /not a bound on kernel page-cache residency/);
});
