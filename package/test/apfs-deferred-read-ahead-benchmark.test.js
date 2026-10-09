const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const directory = path.resolve(__dirname, "../../docs/benchmarks");
const report = JSON.parse(fs.readFileSync(path.join(directory, "apfs-deferred-read-ahead-2026-10-09.json"), "utf8"));
const markdown = fs.readFileSync(path.join(directory, "apfs-deferred-read-ahead-2026-10-09.md"), "utf8");
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  return (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2;
};

test("deferred APFS evidence retains every sample, full scans and successful cleanup", () => {
  assert.equal(report.schemaVersion, 1);
  assert.match(report.allocationCheck.rawResultsSha256, /^[a-f0-9]{64}$/);
  assert.equal(report.allocationCheck.candidateCommit, report.candidateCommit);
  const allocation = report.allocationCheck.measurement;
  assert.equal(allocation.backend, "apfs-clone");
  assert.ok(allocation.cached_volume_growth_bytes < allocation.logical_payload_bytes / 4);
  assert.ok(allocation.private_write_volume_growth_bytes >= allocation.private_write_bytes);
  assert.equal(report.runs.length, 7);
  assert.equal(report.runs.reduce((sum, run) => sum + run.samples.length, 0), 215);
  assert.equal(report.runs.reduce((sum, run) => sum + run.batches.length, 0), 112);
  for (const run of report.runs) {
    assert.match(run.rawResultsSha256, /^[a-f0-9]{64}$/);
    assert.match(run.binarySha256, /^[a-f0-9]{64}$/);
    assert.match(run.candidateSha256, /^[a-f0-9]{64}$/);
    assert.equal(run.complete, true);
    assert.equal(run.finalActiveViews, 0);
    assert.equal(run.finalBases, 0);
    assert.equal(run.finalDiagnosticIssues, 0);
    assert.equal(run.samples.length, 1 + 2 * run.rounds * run.concurrency);
    assert.equal(run.batches.length, 2 * run.rounds);
    for (const sample of run.samples) {
      assert.equal(sample.success, true);
      assert.equal(sample.code, 0);
      assert.equal(sample.signal, null);
      assert.equal(sample.timedOut, false);
      assert.equal(sample.indexScanned, run.trackedEntries);
      assert.equal(sample.indexEntries, run.trackedEntries);
      assert.equal(sample.cpuSeconds, sample.userSeconds + sample.systemSeconds);
      assert.ok(sample.maximumResidentBytes > 0);
    }
    for (let round = 1; round <= run.rounds; round++) {
      const pair = run.samples.filter(sample => sample.round === round);
      assert.equal(pair[0].label, round % 2 ? "baseline" : "advice");
      for (const label of ["baseline", "advice"]) {
        const samples = pair.filter(sample => sample.label === label);
        assert.equal(samples.length, run.concurrency);
        assert.equal(new Set(samples.map(sample => sample.worker)).size, run.concurrency);
      }
    }
    if (run.sourceCommit) {
      assert.equal(run.sourceCommit, "038cd9f82b418afe9e6d0080648738f78586fbca");
      assert.equal(run.fixtureTree, run.sourceTree);
      assert.equal(run.trackedEntries, 5864);
    }
  }
});

test("deferred summaries are recomputed, not enforced as host-independent speed thresholds", () => {
  for (const run of report.runs) {
    for (const label of ["baseline", "advice"]) {
      const samples = run.samples.filter(sample => sample.round > 0 && sample.label === label);
      const batches = run.batches.filter(batch => batch.label === label);
      assert.equal(run.summary[label].medianMilliseconds, median(batches.map(batch => batch.milliseconds)));
      assert.equal(run.summary[label].medianCloneAndHintMilliseconds, median(samples.map(sample => sample.cloneAndHintMilliseconds)));
      assert.equal(run.summary[label].medianIndexRefreshMilliseconds, median(samples.map(sample => sample.indexRefreshSeconds * 1000)));
      assert.equal(run.summary[label].medianCpuSecondsPerAdd, median(samples.map(sample => sample.cpuSeconds)));
    }
    assert.equal(run.summary.wallReductionPercent, 100 * (1 - run.summary.advice.medianMilliseconds / run.summary.baseline.medianMilliseconds));
    assert.equal(run.summary.fasterPairs, run.batches.filter(batch => batch.label === "advice" && batch.milliseconds < run.batches.find(other => other.label === "baseline" && other.round === batch.round).milliseconds).length);
    if (!run.prototype) {
      const render = value => value.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
      assert.ok(markdown.includes(render(run.summary.baseline.medianMilliseconds)));
      assert.ok(markdown.includes(render(run.summary.advice.medianMilliseconds)));
      assert.ok(markdown.includes(`${run.summary.wallReductionPercent.toFixed(2)}%`));
    }
  }
});

test("confounded and prototype measurements stay distinct from final candidate evidence", () => {
  const confounded = report.runs.filter(run => run.confounded);
  assert.equal(confounded.length, 1);
  assert.equal(confounded[0].name, "deferred-hints-serial");
  assert.equal(confounded[0].prototype, true);
  assert.match(confounded[0].caveat, /fetch overlapped/);
  assert.match(report.prototypePatch, /diff --git a\/crates\/riftri-storage\/src\/apfs.rs/);
  const finalHashes = new Set(report.runs.filter(run => !run.prototype).map(run => run.candidateSha256));
  assert.equal(finalHashes.size, 1);
  assert.ok(!finalHashes.has(confounded[0].candidateSha256));
  assert.match(markdown, /confounded and excluded from headline evidence/);
  assert.match(markdown, /not reduced total CPU/);
  assert.match(markdown, /not a T3 Code compatibility or performance result/);
});

test("cold, shim and plain-Git comparisons retain every verified case and the slower pair", () => {
  const workflow = JSON.parse(fs.readFileSync(path.join(directory, "apfs-deferred-workflows-2026-10-09.json"), "utf8"));
  assert.equal(workflow.cases.length, 137);
  assert.equal(workflow.batches.length, 20);
  assert.equal(workflow.allViewsVerifiedAndRemoved, true);
  assert.ok(workflow.cases.every(sample => sample.verifiedAndRemoved));
  assert.equal(workflow.sourceTree, workflow.fixtureTree);
  assert.match(workflow.rawResultsSha256, /^[a-f0-9]{64}$/);
  for (const pattern of [/Active views: 0\n/, /Retained bases: 0\n/, /Pending adds: 0\n/, /Pending removals: 0\n/, /State issues: 0\n/]) assert.match(workflow.finalStatus, pattern);
  for (const comparison of workflow.comparisons) {
    const parallel = comparison.kind.startsWith("parallel");
    const mode = comparison.kind.split("-")[1];
    const select = version => parallel
      ? workflow.batches.filter(sample => sample.version === version && sample.mode === mode)
      : workflow.cases.filter(sample => sample.version === version && sample.label.startsWith(comparison.kind === "cold" ? "cold-" : "single-") && (comparison.kind === "cold" || sample.mode === mode));
    const baseline = select("before"), candidate = select("after");
    assert.equal(comparison.pairs, candidate.length);
    assert.equal(baseline.length, candidate.length);
    assert.equal(comparison.baselineMedianSeconds, median(baseline.map(sample => sample.seconds)));
    assert.equal(comparison.candidateMedianSeconds, median(candidate.map(sample => sample.seconds)));
    assert.equal(comparison.reductionPercent, 100 * (1 - comparison.candidateMedianSeconds / comparison.baselineMedianSeconds));
    const faster = candidate.filter(sample => {
      const paired = baseline.find(other => parallel ? other.round === sample.round : other.label === sample.label.replace(/after$/, "before"));
      assert.ok(paired);
      return sample.seconds < paired.seconds;
    }).length;
    assert.equal(comparison.fasterPairs, faster);
    assert.ok(markdown.includes(comparison.baselineMedianSeconds.toFixed(3)));
    assert.ok(markdown.includes(comparison.candidateMedianSeconds.toFixed(3)));
    assert.ok(markdown.includes(`${comparison.reductionPercent.toFixed(2)}%`));
  }
  assert.equal(workflow.plainGit.serialMedianSeconds, median(workflow.cases.filter(sample => sample.version === "git" && sample.label.startsWith("single-")).map(sample => sample.seconds)));
  assert.equal(workflow.plainGit.fourWayMedianSeconds, median(workflow.batches.filter(sample => sample.version === "git").map(sample => sample.seconds)));
  assert.equal(workflow.comparisons.find(comparison => comparison.kind === "parallel-shim").fasterPairs, 3);
  assert.match(markdown, /Plain Git remained faster/);
  assert.match(markdown, /4\.555 s versus 4\.327 s/);
});
