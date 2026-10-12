import assert from 'node:assert/strict';
import test from 'node:test';
import {summarize} from './cold-lfs-summary.mjs';

function fixture() {
  return Object.fromEntries(['control-before', 'candidate', 'control-after'].map(name => [name, {
    schemaVersion: 1, runner: 'supervised-cold-lfs-v1', complete: true,
    count: 32, bytes: 2097152, rounds: 8, git: 'fixture Git', lfs: 'fixture LFS', tree: 'fixture tree', availableParallelism: 4, runnerSha256: {runner: 'fixture'},
    binarySha256: {before: 'baseline', after: name === 'candidate' ? 'candidate' : 'baseline'},
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
    r => { r.refusals.pop(); }, r => { r.binarySha256.after = 'baseline'; },
    r => { r.git = 'different'; }, r => { r.runnerSha256.runner = 'different'; },
  ]) {
    const reports = fixture(); mutate(reports.candidate);
    assert.throws(() => summarize(reports));
  }
});
