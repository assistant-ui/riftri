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

test("the losing hint time budget retains all successful checks without shipping", () => {
  const experiment = JSON.parse(fs.readFileSync(path.join(directory, "apfs-time-admission-2026-10-09.json")));
  assert.equal(experiment.decision, "rejected; runtime experiment reverted");
  assert.equal(experiment.complete, true);
  assert.equal(experiment.stageDiagnostics, false);
  assert.equal(experiment.samples.length, 65);
  assert.equal(experiment.batches.length, 16);
  assert.equal(experiment.finalActiveViews, 0);
  assert.equal(experiment.finalBases, 0);
  assert.equal(experiment.finalDiagnosticIssues, 0);
  assert.match(experiment.candidatePatch, /MAX_ISSUANCE_TIME: Duration = Duration::from_millis\(50\)/);
  assert.match(experiment.baselineDescription, /not main/);
  for (const sample of experiment.samples) {
    assert.equal(sample.success, true);
    assert.equal(sample.timedOut, false);
    assert.equal(sample.code, 0);
    assert.equal(sample.indexScanned, 5864);
    assert.equal(sample.indexEntries, 5864);
  }
  for (const label of ["baseline", "advice"]) assert.equal(experiment.summary[label].medianMilliseconds,
    median(experiment.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
  assert.equal(experiment.summary.fasterPairs, experiment.batches.filter(batch => batch.label === "advice" &&
    batch.milliseconds < experiment.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
  assert.equal(experiment.summary.wallReductionPercent,
    100 * (1 - experiment.summary.advice.medianMilliseconds / experiment.summary.baseline.medianMilliseconds));
  assert.equal(experiment.summary.fasterPairs, 2);
  assert.ok(experiment.summary.wallReductionPercent < 0);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs/read_ahead.rs"), "utf8"), /MAX_ISSUANCE_TIME/);
});

test("the losing directory-handle prototype retains all observations without shipping", () => {
  const experiment = JSON.parse(fs.readFileSync(path.join(directory, "apfs-directory-handles-2026-10-09.json")));
  assert.equal(experiment.decision, "rejected; runtime experiment reverted");
  assert.equal(experiment.complete, true);
  assert.equal(experiment.stageDiagnostics, false);
  assert.equal(experiment.samples.length, 65);
  assert.equal(experiment.batches.length, 16);
  assert.equal(experiment.finalActiveViews, 0);
  assert.equal(experiment.finalBases, 0);
  assert.equal(experiment.finalDiagnosticIssues, 0);
  assert.match(experiment.candidatePatch, /libc::clonefileat/);
  assert.match(experiment.candidatePatch, /bounded_workers_drop_every_private_state_before_returning_an_error/);
  assert.match(experiment.baselineDescription, /not main/);
  for (const sample of experiment.samples) {
    assert.equal(sample.success, true);
    assert.equal(sample.code, 0);
    assert.equal(sample.timedOut, false);
    assert.equal(sample.indexScanned, 5864);
    assert.equal(sample.indexEntries, 5864);
  }
  for (const label of ["baseline", "advice"]) assert.equal(experiment.summary[label].medianMilliseconds,
    median(experiment.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
  assert.equal(experiment.summary.fasterPairs, experiment.batches.filter(batch => batch.label === "advice" &&
    batch.milliseconds < experiment.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
  assert.equal(experiment.summary.wallReductionPercent,
    100 * (1 - experiment.summary.advice.medianMilliseconds / experiment.summary.baseline.medianMilliseconds));
  assert.equal(experiment.summary.fasterPairs, 2);
  assert.ok(experiment.summary.wallReductionPercent < 0);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs.rs"), "utf8"), /struct CloneWorker/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/parallel.rs"), "utf8"), /try_for_each_bounded_with_state/);
});

test("refresh-handoff evidence retains the optional-locks repeated-scan regression", () => {
  const report = JSON.parse(fs.readFileSync(path.join(directory, "git-refresh-handoff-2026-10-09.json")));
  assert.equal(report.complete, true);
  assert.match(report.scope, /not a Riftri startup benchmark/);
  assert.equal(report.trackedEntries, 257);
  assert.equal(report.cases.length, 4);
  assert.equal(new Set(report.cases.map(c => `${c.optionalLocks}-${c.noRefresh}`)).size, 4);
  const source = fs.readFileSync(path.join(directory, report.sourceScript));
  assert.equal(createHash("sha256").update(source).digest("hex"), report.sourceScriptSha256);
  for (const entry of report.cases) {
    assert.ok(["0", "1"].includes(entry.optionalLocks));
    assert.equal(entry.observations.length, 3);
    const [reset, first, second] = entry.observations;
    assert.deepEqual(reset.refreshScans, entry.noRefresh ? [] : [257]);
    assert.deepEqual(first.refreshScans, entry.noRefresh ? [257] : [0]);
    assert.deepEqual(second.refreshScans, entry.noRefresh && entry.optionalLocks === "0" ? [257] : [0]);
    assert.equal(first.indexSha256 === reset.indexSha256, !(entry.noRefresh && entry.optionalLocks === "1"));
    assert.equal(first.indexSha256, second.indexSha256);
  }
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /not a startup benchmark or a native/);
  assert.match(markdown, /No Git behavior is changed/);
});

test("size-selective hint evidence retains all outcomes without shipping the cutoff", () => {
  const experiment = JSON.parse(fs.readFileSync(path.join(directory, "apfs-large-file-hints-2026-10-09.json")));
  const patch = fs.readFileSync(path.join(directory, "apfs-large-file-hints.patch"));
  assert.equal(createHash("sha256").update(patch).digest("hex"), experiment.candidatePatchSha256);
  assert.match(patch.toString(), /MIN_FILE_BYTES: u64 = 4 \* 1024/);
  assert.match(patch.toString(), /small_files_do_not_consume_prefetch_budget/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs/read_ahead.rs"), "utf8"), /MIN_FILE_BYTES/);
  assert.equal(experiment.decision, "not adopted; cutoff removed from normal source");
  assert.equal(experiment.complete, true);
  assert.equal(experiment.stageDiagnostics, false);
  assert.match(experiment.baselineDescription, /not main/);
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
    const cpu = sample.resources.join("\n").match(/([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys/);
    assert.equal(sample.cpuSeconds, Number(cpu[2]) + Number(cpu[3]));
  }
  for (const label of ["baseline", "advice"]) {
    assert.equal(experiment.summary[label].medianMilliseconds,
      median(experiment.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
    assert.equal(experiment.summary[label].medianCpuSecondsPerView,
      median(experiment.samples.filter(sample => sample.round > 0 && sample.label === label).map(sample => sample.cpuSeconds)));
  }
  assert.equal(experiment.summary.fasterPairs, experiment.batches.filter(batch => batch.label === "advice" &&
    batch.milliseconds < experiment.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
  assert.equal(experiment.summary.wallReductionPercent,
    100 * (1 - experiment.summary.advice.medianMilliseconds / experiment.summary.baseline.medianMilliseconds));
  assert.equal(experiment.summary.fasterPairs, 4);
  assert.ok(experiment.summary.wallReductionPercent < 0);
  assert.doesNotMatch(fs.readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8"), /^  apfs-size-selective-evaluation:/m);
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /not traced syscall counts or a measured speedup/);
  assert.match(markdown, /not a universal slowdown estimate/);
});

test("overlap evidence retains both full comparisons without shipping the scheduling prototype", () => {
  const patch = fs.readFileSync(path.join(directory, "apfs-overlap-hints.patch"));
  const patchSha = createHash("sha256").update(patch).digest("hex");
  assert.match(patch.toString(), /overlaps_the_operation_and_joins_admitted_hints_on_error/);
  assert.match(patch.toString(), /panic_stops_admission_and_joins_workers_before_unwinding/);
  for (const file of ["crates/riftri-storage/src/apfs/read_ahead.rs", "crates/riftri-core/src/worktree.rs"]) {
    assert.doesNotMatch(fs.readFileSync(path.join(root, file), "utf8"), /ReadAheadPlan|clone_tree_for_index_sync|read_ahead\.during/);
  }
  for (const [name, entries, wins] of [["reference", 5864, 6], ["synthetic", 4097, 3]]) {
    const run = JSON.parse(fs.readFileSync(path.join(directory, `apfs-overlap-${name}-2026-10-09.json`)));
    assert.equal(run.candidatePatchSha256, patchSha);
    assert.equal(run.decision, "not adopted; overlap removed from normal source");
    assert.match(run.baselineDescription, /not main/);
    assert.equal(run.complete, true);
    assert.equal(run.failure, null);
    assert.equal(run.stageDiagnostics, false);
    assert.equal(run.samples.length, 65);
    assert.equal(run.batches.length, 16);
    assert.equal(run.finalActiveViews, 0);
    assert.equal(run.finalBases, 0);
    assert.equal(run.finalDiagnosticIssues, 0);
    for (const sample of run.samples) {
      assert.equal(sample.success, true);
      assert.equal(sample.code, 0);
      assert.equal(sample.timedOut, false);
      assert.equal(Number(sample.indexScanned), entries);
      assert.equal(Number(sample.indexEntries), entries);
      const reset = sample.gitCommands.find(command => command.argv[1] === "reset");
      assert.equal(Number(reset.counters.find(counter => counter.key === "refresh/sum_scan").value), entries);
      const cpu = sample.resources.join("\n").match(/([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys/);
      assert.equal(sample.cpuSeconds, Number(cpu[2]) + Number(cpu[3]));
    }
    for (const label of ["baseline", "advice"]) {
      assert.equal(run.summary[label].medianMilliseconds,
        median(run.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
      assert.equal(run.summary[label].medianCpuSecondsPerView,
        median(run.samples.filter(sample => sample.round > 0 && sample.label === label).map(sample => sample.cpuSeconds)));
    }
    assert.equal(run.summary.fasterPairs, run.batches.filter(batch => batch.label === "advice" &&
      batch.milliseconds < run.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
    assert.equal(run.summary.fasterPairs, wins);
    assert.equal(run.summary.wallReductionPercent,
      100 * (1 - run.summary.advice.medianMilliseconds / run.summary.baseline.medianMilliseconds));
    assert.ok(Math.abs(run.summary.wallReductionPercent) < 1);
  }
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /small, mixed changes do not justify/);
  assert.match(markdown, /not proof\s+that overlap can never help/);
});

test("fixed clone-worker cap evidence retains the serial latency tradeoff", () => {
  const patch = fs.readFileSync(path.join(directory, "apfs-two-clone-workers.patch"));
  const patchSha = createHash("sha256").update(patch).digest("hex");
  assert.match(patch.toString(), /worker_limit\.clamp\(1, 2\)/);
  assert.match(patch.toString(), /two_clone_workers_still_allow_four_deferred_hint_workers/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs.rs"), "utf8"), /native_clone_workers/);
  for (const [mode, count, wins] of [["four", 65, 6], ["serial", 17, 4]]) {
    const run = JSON.parse(fs.readFileSync(path.join(directory, `apfs-two-clone-workers-${mode}-2026-10-09.json`)));
    assert.equal(run.candidatePatchSha256, patchSha);
    assert.equal(run.decision, "not adopted; fixed clone-worker cap removed from normal source");
    assert.match(run.baselineDescription, /not main/);
    assert.equal(run.complete, true);
    assert.equal(run.failure, null);
    assert.equal(run.stageDiagnostics, false);
    assert.equal(run.samples.length, count);
    assert.equal(run.batches.length, 16);
    assert.equal(run.finalActiveViews, 0);
    assert.equal(run.finalBases, 0);
    assert.equal(run.finalDiagnosticIssues, 0);
    for (const sample of run.samples) {
      assert.equal(sample.success, true);
      assert.equal(sample.code, 0);
      assert.equal(sample.timedOut, false);
      assert.equal(Number(sample.indexScanned), 5864);
      assert.equal(Number(sample.indexEntries), 5864);
      const cpu = sample.resources.join("\n").match(/([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys/);
      assert.equal(sample.cpuSeconds, Number(cpu[2]) + Number(cpu[3]));
    }
    for (const label of ["baseline", "advice"]) {
      assert.equal(run.summary[label].medianMilliseconds,
        median(run.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
      assert.equal(run.summary[label].medianCpuSecondsPerView,
        median(run.samples.filter(sample => sample.round > 0 && sample.label === label).map(sample => sample.cpuSeconds)));
    }
    assert.equal(run.summary.fasterPairs, run.batches.filter(batch => batch.label === "advice" &&
      batch.milliseconds < run.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
    assert.equal(run.summary.fasterPairs, wins);
    assert.equal(run.summary.wallReductionPercent,
      100 * (1 - run.summary.advice.medianMilliseconds / run.summary.baseline.medianMilliseconds));
    assert.ok(run.summary.advice.medianCpuSecondsPerView < run.summary.baseline.medianCpuSecondsPerView);
    assert.equal(run.summary.wallReductionPercent > 0, mode === "four");
  }
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /resource\/latency tradeoff, not a\s+consistent startup win/);
});

test("per-base hint admission evidence keeps its release failure and slow Git refreshes", () => {
  const patch = fs.readFileSync(path.join(directory, "apfs-hint-admission.patch"));
  const run = JSON.parse(fs.readFileSync(path.join(directory, "apfs-hint-admission-2026-10-09.json")));
  assert.equal(run.candidatePatchSha256, createHash("sha256").update(patch).digest("hex"));
  assert.match(patch.toString(), /libc::LOCK_EX \| libc::LOCK_NB/);
  assert.match(patch.toString(), /impl Drop for HintAdmission/);
  assert.match(patch.toString(), /hint_admission_release_does_not_wait_for_a_duplicated_descriptor/);
  assert.match(patch.toString(), /terminating_another_process_releases_its_hint_admission/);
  for (const file of ["crates/riftri-storage/src/apfs.rs", "crates/riftri-storage/src/apfs/read_ahead.rs"]) {
    assert.doesNotMatch(fs.readFileSync(path.join(root, file), "utf8"), /HintAdmission/);
  }
  assert.equal(run.decision, "not adopted; hint admission guard removed from normal source");
  assert.match(run.baselineDescription, /not main/);
  assert.equal(run.preBenchmarkFailure.reproducedErrorKind, "WouldBlock");
  assert.equal(run.preBenchmarkFailure.reproducedErrorCode, 35);
  assert.match(run.preBenchmarkFailure.attributionLimit, /did not trace/);
  assert.equal(run.preBenchmarkFailure.verificationAfterFix.workspacePassed, 707);
  assert.equal(run.preBenchmarkFailure.verificationAfterFix.topLevelPassed, 700);
  assert.equal(run.preBenchmarkFailure.verificationAfterFix.counting, "passing executions including subprocess helpers");
  assert.equal(run.preBenchmarkFailure.verificationAfterFix.ignoredChildHelperInvokedByActiveParentTest, true);
  assert.equal(run.complete, true);
  assert.equal(run.failure, null);
  assert.equal(run.stageDiagnostics, false);
  assert.equal(run.samples.length, 65);
  assert.equal(run.batches.length, 16);
  assert.equal(run.finalActiveViews, 0);
  assert.equal(run.finalBases, 0);
  assert.equal(run.finalDiagnosticIssues, 0);
  for (const sample of run.samples) {
    assert.equal(sample.success, true);
    assert.equal(sample.code, 0);
    assert.equal(sample.timedOut, false);
    assert.equal(Number(sample.indexScanned), 5864);
    assert.equal(Number(sample.indexEntries), 5864);
    const cpu = sample.resources.join("\n").match(/([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys/);
    assert.equal(sample.cpuSeconds, Number(cpu[2]) + Number(cpu[3]));
  }
  for (const label of ["baseline", "advice"]) {
    assert.equal(run.summary[label].medianMilliseconds,
      median(run.batches.filter(batch => batch.label === label).map(batch => batch.milliseconds)));
    assert.equal(run.summary[label].medianCpuSecondsPerView,
      median(run.samples.filter(sample => sample.round > 0 && sample.label === label).map(sample => sample.cpuSeconds)));
  }
  assert.equal(run.summary.fasterPairs, run.batches.filter(batch => batch.label === "advice" &&
    batch.milliseconds < run.batches.find(other => other.round === batch.round && other.label === "baseline").milliseconds).length);
  assert.equal(run.summary.fasterPairs, 4);
  assert.equal(run.summary.wallReductionPercent,
    100 * (1 - run.summary.advice.medianMilliseconds / run.summary.baseline.medianMilliseconds));
  assert.ok(run.summary.wallReductionPercent < 0);
  const slow = run.samples.filter(sample => sample.round === 6 && sample.label === "advice" && sample.indexRefreshSeconds > 18);
  assert.equal(slow.length, 3);
  for (const sample of slow) {
    const reset = sample.gitCommands.find(command => command.argv[1] === "reset");
    assert.equal(sample.indexRefreshSeconds, reset.regions.find(region => region.category === "index" && region.label === "refresh").seconds);
  }
  const markdown = fs.readFileSync(path.join(directory, "apfs-stage-diagnostics-2026-10-09.md"), "utf8");
  assert.match(markdown, /prototype-only issue/);
  assert.match(markdown, /no per-worker hint-admission trace/);
});

test("writable bulk evidence remains a test-only storage-phase result with open metadata gates", () => {
  const bulk = JSON.parse(fs.readFileSync(path.join(directory, "apfs-writable-bulk-2026-10-09.json")));
  assert.equal(bulk.measurement.production_eligible, false);
  assert.equal(bulk.measurement.samples.length, 16);
  assert.equal(bulk.correctness.directoryXattrParity, false);
  assert.equal(bulk.correctness.aclParity, "not established");
  assert.equal(bulk.correctness.fileFlagParity, "not established");
  assert.equal(bulk.correctness.endToEndGitLifecycle, "not measured");
  assert.equal(bulk.correctness.physicalVolumeAllocation, "not measured");
  assert.equal(bulk.platformContract.directDirectoryClone, "discouraged by Apple");
  assert.equal(bulk.platformContract.recursiveForceClone, "not supported by copyfile");
  assert.equal(bulk.summary.iterativeMedianMicroseconds, median(bulk.measurement.samples.filter(s => !s.candidate).map(s => s.microseconds)));
  assert.equal(bulk.summary.bulkMedianMicroseconds, median(bulk.measurement.samples.filter(s => s.candidate).map(s => s.microseconds)));
  assert.equal(bulk.summary.reductionPercent, 100 * (1 - bulk.summary.bulkMedianMicroseconds / bulk.summary.iterativeMedianMicroseconds));
  assert.equal(bulk.summary.fasterPairs, bulk.measurement.samples.filter(s => s.candidate &&
    s.microseconds < bulk.measurement.samples.find(other => other.round === s.round && !other.candidate).microseconds).length);
  const source = fs.readFileSync(path.join(root, "crates/riftri-storage/src/apfs.rs"), "utf8");
  assert.doesNotMatch(source.split("#[cfg(test)]\nmod tests {")[0], /bulk_writable_for_evaluation/);
  const markdown = fs.readFileSync(path.join(directory, "apfs-bulk-directory-clone-2026-09-13.md"), "utf8");
  assert.match(markdown, /the CLI cannot select it/);
  assert.match(markdown, /not.*measure Git initialization/);
  assert.match(markdown, /35\.99%/);
  assert.match(markdown, /Metadata parity tests alone therefore would not justify/);
});
