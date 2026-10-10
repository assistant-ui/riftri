import assert from 'node:assert/strict';
import test from 'node:test';
import {caseOrder, evaluateLocationControl} from './apfs-location-controls.mjs';

// Fabricated unit-test timings only; these are never measurement records.
function fixture(concurrency = 1) {
  const sample = (round, label, worker = 0) => ({round, label, worker, success: true, code: 0,
    timedOut: false, error: null, reused: round !== 0, resources: ['1 real 0.5 user 0.5 sys']});
  const data = {complete: true, stageDiagnostics: false, rounds: 8, concurrency,
    sourceCommit: '038cd9f82b418afe9e6d0080648738f78586fbca',
    sourceTree: 'c4de7922b24126e860cb77652f5d080e04c8c396', tree: 'c4de7922b24126e860cb77652f5d080e04c8c396',
    files: 5864, logicalBytes: 73235115, binarySha256: 'a'.repeat(64), candidateSha256: 'a'.repeat(64),
    final: {operations: {active_views: 0}, bases: [], diagnostic_issues: []},
    samples: [sample(0, 'baseline')], batches: []};
  for (let round = 1; round <= 8; round++) for (const label of ['baseline', 'advice']) {
    data.batches.push({round, label, concurrency, milliseconds: 1000});
    for (let worker = 0; worker < concurrency; worker++) data.samples.push(sample(round, label, worker));
  }
  return data;
}

test('case order balances locations in two fresh repeats for both concurrency levels', () => {
  const forward = caseOrder();
  assert.equal(forward.length, 8);
  assert.equal(new Set(forward.map(c => c.name)).size, 8);
  assert.deepEqual(caseOrder('reverse'), [...forward].reverse());
  assert.throws(() => caseOrder('random'));
  for (const location of ['host', 'image']) for (const concurrency of [1, 4]) {
    assert.deepEqual(forward.filter(c => c.location === location && c.concurrency === concurrency).map(c => c.repeat), [1, 2]);
  }
});

test('same-binary control completeness is independent of timing stability', () => {
  for (const concurrency of [1, 4]) {
    const data = fixture(concurrency);
    const result = evaluateLocationControl(data, data.binarySha256, concurrency);
    assert.equal(result.validControl, true);
    assert.equal(result.stable, true);
    assert.equal(result.gatePassed, undefined);
    for (const multiplier of [0.8, 1.2]) {
      const shifted = structuredClone(data);
      shifted.batches.filter(b => b.label === 'advice').forEach(b => b.milliseconds *= multiplier);
      const changed = evaluateLocationControl(shifted, data.binarySha256, concurrency);
      assert.equal(changed.validControl, true);
      assert.equal(changed.stable, false);
    }
  }
});

test('tails, CPU changes, wrong fixture identities and incomplete results cannot be stable controls', () => {
  for (const change of [
    d => { d.batches[1].milliseconds = 2100; }, d => { d.batches[1].milliseconds = 400; },
    d => { d.samples.filter(s => s.label === 'advice').forEach(s => s.resources = ['1 real 0.6 user 0.6 sys']); },
    d => { d.sourceCommit = 'b'.repeat(40); }, d => { d.tree = 'b'.repeat(40); },
    d => { d.files--; }, d => { d.logicalBytes--; }, d => { d.complete = false; },
    d => { d.samples[1].timedOut = true; }, d => { d.samples.pop(); },
  ]) {
    const data = fixture(4); change(data);
    assert.equal(evaluateLocationControl(data, data.binarySha256, 4).stable, false);
  }
  assert.equal(evaluateLocationControl(undefined, 'a'.repeat(64), 4).validControl, false);
});
