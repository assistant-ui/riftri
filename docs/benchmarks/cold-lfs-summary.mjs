import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {distribution} from './latency-summary.mjs';

const runnerFiles = ['cold-lfs.mjs', 'process-scope.mjs', 'timed-process.mjs'];
const minimumPairs = 8;
const nonempty = value => typeof value === 'string' && value.trim().length > 0;
const integerIn = (value, minimum, maximum) => Number.isSafeInteger(value) && value >= minimum && value <= maximum;

function validateProvenance(report) {
  for (const key of ['platform', 'release', 'cpu', 'git', 'lfs']) {
    assert.ok(nonempty(report[key]), `missing or invalid ${key}`);
  }
  assert.match(report.tree, /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/, 'invalid tree identity');
  assert.ok(integerIn(report.availableParallelism, 1, Number.MAX_SAFE_INTEGER), 'invalid available parallelism');
  // Match the runner's workload limits. Small smoke runs remain valid reports,
  // but the separate minimum-pair gate below prevents a performance claim.
  assert.ok(integerIn(report.rounds, 1, 20), 'invalid rounds');
  assert.ok(integerIn(report.count, 4, 128), 'invalid file count');
  assert.ok(integerIn(report.bytes, 4, 8 * 1024 ** 2) && report.bytes * report.count <= 128 * 1024 ** 2, 'invalid workload bytes');
  assert.deepEqual(Object.keys(report.runnerSha256 ?? {}).sort(), runnerFiles);
  for (const file of runnerFiles) assert.match(report.runnerSha256[file], /^[a-f0-9]{64}$/, `invalid runner hash: ${file}`);
  for (const label of ['before', 'after']) assert.match(report.binarySha256?.[label], /^[a-f0-9]{64}$/, `invalid ${label} binary hash`);
}

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
    validateProvenance(report);
    for (const key of ['count', 'bytes', 'rounds', 'git', 'lfs', 'tree', 'availableParallelism', 'platform', 'release', 'cpu']) assert.equal(report[key], reference[key], `mismatched ${key}`);
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
  const sampleSizeValid = reference.rounds >= minimumPairs;
  const qualified = sampleSizeValid && controlsValid && results.candidate.ratio.p50 <= 0.95 && results.candidate.ratio.p95 <= 1.05;
  return {qualified, controlsValid, sampleSizeValid, minimumPairs, speedupClaim: qualified,
    policy: 'Matching recorded host, runner, binary and workload provenance; at least eight pairs per lane; both controls within 5% p50/p95; candidate p50 at least 5% faster and p95 no more than 5% slower; no exclusions or retries.', results};
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
