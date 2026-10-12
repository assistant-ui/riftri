import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {distribution} from './latency-summary.mjs';

export function summarize(reports) {
  const results = {};
  const reference = reports['control-before'];
  for (const name of ['control-before', 'candidate', 'control-after']) {
    const report = reports[name];
    assert.equal(report.schemaVersion, 1);
    assert.equal(report.runner, 'supervised-cold-lfs-v1');
    assert.equal(report.complete, true);
    assert.equal(report.cancellation, undefined);
    assert.equal(report.failure, undefined);
    for (const key of ['count', 'bytes', 'rounds', 'git', 'lfs', 'tree', 'availableParallelism']) assert.equal(report[key], reference[key]);
    assert.deepEqual(report.runnerSha256, reference.runnerSha256);
    assert.equal(report.binarySha256.before, reference.binarySha256.before);
    assert.equal(report.samples.length, report.rounds * 2);
    assert.ok(report.commands.length > 0 && report.commands.every(command => command.settled && !command.cancelled && !command.timedOut && !command.error));
    assert.deepEqual(report.refusals, [{failure: 'corrupt', settled: true}, {failure: 'missing', settled: true}]);
    const seen = new Set();
    for (const sample of report.samples) {
      assert.ok(sample.settled && sample.verified && sample.reused === false);
      assert.ok(['before', 'after'].includes(sample.label));
      assert.ok(Number.isInteger(sample.round) && sample.round >= 0 && sample.round < report.rounds);
      const key = `${sample.round}-${sample.label}`;
      assert.ok(!seen.has(key), 'duplicate sample'); seen.add(key);
    }
    const stats = Object.fromEntries(['before', 'after'].map(label => [label,
      distribution(report.samples.filter(sample => sample.label === label).map(sample => sample.milliseconds))]));
    const ratio = {p50: stats.after.p50 / stats.before.p50, p95: stats.after.p95 / stats.before.p95};
    assert.ok(Object.values(ratio).every(Number.isFinite));
    results[name] = {stats, ratio};
    if (name === 'candidate') assert.notEqual(report.binarySha256.after, reference.binarySha256.before);
    else assert.equal(report.binarySha256.after, reference.binarySha256.before);
  }
  const controlsValid = ['control-before', 'control-after'].every(name => Object.values(results[name].ratio).every(value => Math.abs(value - 1) <= 0.05));
  const qualified = controlsValid && results.candidate.ratio.p50 <= 0.95 && results.candidate.ratio.p95 <= 1.05;
  return {qualified, controlsValid, speedupClaim: qualified,
    policy: 'Both controls within 5% p50/p95; candidate p50 at least 5% faster and p95 no more than 5% slower; no exclusions or retries.', results};
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const root = path.resolve(process.argv[2]);
  const reports = Object.fromEntries(['control-before', 'candidate', 'control-after'].map(name => [name,
    JSON.parse(fs.readFileSync(path.join(root, name, 'results.json')))]));
  if (process.argv[3]) {
    const proof = JSON.parse(fs.readFileSync(process.argv[3]));
    for (const [name, report] of Object.entries(reports)) {
      assert.equal(report.binarySha256.before, proof.baseline.sha256);
      assert.equal(report.binarySha256.after, name === 'candidate' ? proof.candidate.sha256 : proof.baseline.sha256);
    }
  }
  console.log(JSON.stringify(summarize(reports), null, 2));
}
