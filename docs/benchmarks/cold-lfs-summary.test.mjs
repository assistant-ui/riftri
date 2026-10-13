import assert from 'node:assert/strict';
import test from 'node:test';
import {summarize} from './cold-lfs-summary.mjs';

function fixture() {
  return Object.fromEntries(['control-before', 'candidate', 'control-after'].map(name => [name, {
    schemaVersion: 1, runner: 'supervised-cold-lfs-v1', complete: true,
    count: 32, bytes: 2097152, rounds: 8, git: 'fixture Git', lfs: 'fixture LFS', tree: 'a'.repeat(40), availableParallelism: 4,
    platform: 'darwin', release: 'fixture kernel', cpu: 'fixture CPU',
    runnerSha256: Object.fromEntries(['cold-lfs.mjs', 'process-scope.mjs', 'timed-process.mjs'].map(file => [file, 'c'.repeat(64)])),
    binarySha256: {before: 'a'.repeat(64), after: (name === 'candidate' ? 'b' : 'a').repeat(64)},
    commands: [{settled: true, code: 0}], refusals: [{failure: 'corrupt', settled: true}, {failure: 'missing', settled: true}],
    samples: Array.from({length: 16}, (_, i) => ({round: Math.floor(i / 2), label: i % 2 ? 'after' : 'before',
      settled: true, verified: true, reused: false, milliseconds: name === 'candidate' && i % 2 ? 90 : 100})),
  }]));
}

test('qualification requires a whole-creation gain and valid bracketing controls', () => {
  const reports = fixture();
  assert.equal(summarize(reports).qualified, true);
  reports.candidate.samples[1].milliseconds = 120;
  assert.equal(summarize(reports).qualified, false, 'tail regression must fail');
  const noisy = fixture(); noisy['control-after'].samples[1].milliseconds = 120;
  assert.equal(summarize(noisy).controlsValid, false);
  const flat = fixture(); flat.candidate.samples.forEach(sample => { sample.milliseconds = 100; });
  assert.equal(summarize(flat).qualified, false);
});

test('incomplete, duplicated, unverified or same-binary candidates cannot qualify', () => {
  for (const mutate of [
    r => { r.complete = false; }, r => { r.failure = 'failed'; },
    r => { r.commands[0].settled = false; }, r => { r.commands[0].cancelled = true; },
    r => { r.samples.pop(); }, r => { r.samples[1] = {...r.samples[0]}; },
    r => { r.samples[0].verified = false; }, r => { r.samples[0].reused = true; },
    r => { r.refusals.pop(); }, r => { r.binarySha256.after = 'a'.repeat(64); },
    r => { r.git = 'different'; }, r => { r.runnerSha256['cold-lfs.mjs'] = 'd'.repeat(64); },
  ]) {
    const reports = fixture(); mutate(reports.candidate);
    assert.throws(() => summarize(reports));
  }
});

test('missing or mismatched recorded host information cannot qualify', () => {
  for (const key of ['platform', 'release', 'cpu']) {
    const mixed = fixture(); mixed.candidate[key] = 'different';
    assert.throws(() => summarize(mixed), undefined, `mismatched ${key}`);
    for (const invalid of [undefined, '', '  ', 42]) {
      const reports = fixture();
      for (const report of Object.values(reports)) report[key] = invalid;
      assert.throws(() => summarize(reports), undefined, `invalid ${key}: ${invalid}`);
    }
  }
});

test('runner, binary and workload provenance must be present and valid, not merely equal', () => {
  for (const mutate of [
    r => { delete r.runnerSha256; }, r => { r.runnerSha256 = {}; },
    r => { delete r.runnerSha256['process-scope.mjs']; },
    r => { r.runnerSha256['timed-process.mjs'] = 'not a digest'; },
    r => { r.binarySha256.before = undefined; },
    r => { r.binarySha256.before = 'not a digest'; },
    r => { delete r.git; }, r => { r.lfs = ''; }, r => { r.tree = 'not a tree'; },
    r => { r.availableParallelism = 0; }, r => { r.count = 0; },
    r => { r.bytes = 0; }, r => { r.bytes = 8 * 1024 ** 2; r.count = 128; },
  ]) {
    const reports = fixture(); Object.values(reports).forEach(mutate);
    assert.throws(() => summarize(reports));
  }
});

test('smoke runs remain reportable but never qualify below eight complete pairs per lane', () => {
  for (const rounds of [1, 7, 8]) {
    const reports = fixture();
    for (const report of Object.values(reports)) {
      report.rounds = rounds;
      report.samples = report.samples.filter(sample => sample.round < rounds);
    }
    const summary = summarize(reports);
    assert.equal(summary.sampleSizeValid, rounds >= 8);
    assert.equal(summary.qualified, rounds >= 8);
    assert.equal(summary.speedupClaim, rounds >= 8);
    assert.equal(summary.results.candidate.stats.before.samples, rounds);
  }
});
