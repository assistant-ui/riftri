import assert from 'node:assert/strict';
import test from 'node:test';
import {evaluateCalibration} from './apfs-calibration-gate.mjs';

function fixture() {
  const sample = (round, label) => ({round, label, worker: 0, success: true, code: 0,
    timedOut: false, error: null, reused: round !== 0, resources: ['1.00 real 0.50 user 0.50 sys']});
  const checksum = '33240072e0f00873ecf588f0917dab1e198a5f9bd246bc3a3d195ad981f1c80a';
  return {complete: true, stageDiagnostics: false, rounds: 8, concurrency: 1,
    binarySha256: checksum, candidateSha256: checksum,
    final: {operations: {active_views: 0}, bases: [], diagnostic_issues: []},
    samples: [sample(0, 'baseline'), ...Array.from({length: 8}, (_, i) =>
      ['baseline', 'advice'].map(label => sample(i + 1, label))).flat()],
    batches: Array.from({length: 8}, (_, i) => ['baseline', 'advice'].map(label =>
      ({round: i + 1, label, concurrency: 1, milliseconds: 1000}))).flat()};
}

test('equal timings are a valid control, never an optimization gate', () => {
  const result = evaluateCalibration(fixture());
  assert.equal(result.validCalibration, true);
  assert.equal(result.wouldCrossSerialLimits, false);
  assert.equal(result.apparentReductionPercent, 0);
  assert.equal(result.gatePassed, undefined);
});
test('hosted controls must match the separately pinned build identity', () => {
  const data = fixture();
  data.binarySha256 = data.candidateSha256 = 'a'.repeat(64);
  assert.equal(evaluateCalibration(data).validCalibration, false);
  assert.equal(evaluateCalibration(data, 'a'.repeat(64)).validCalibration, true);
  assert.equal(evaluateCalibration(data, '').validCalibration, false);
  data.candidateSha256 = 'b'.repeat(64);
  assert.equal(evaluateCalibration(data, 'a'.repeat(64)).validCalibration, false);
});
test('control retains median, individual tail, and CPU deviations', () => {
  for (const change of [
    d => { for (const b of d.batches) if (b.label === 'advice') b.milliseconds = 1100; },
    d => { d.batches[1].milliseconds = 2100; },
    d => { for (const s of d.samples) if (s.label === 'advice') s.resources = ['1 real 1 user 1 sys']; },
  ]) {
    const data = fixture(); change(data);
    const result = evaluateCalibration(data);
    assert.equal(result.validCalibration, true);
    assert.equal(result.wouldCrossSerialLimits, true);
  }
});
test('partial, failed, duplicated, missing and different-binary evidence is invalid', () => {
  for (const change of [
    d => { d.complete = false; }, d => { d.failure = 'failure'; },
    d => { d.candidateSha256 = 'f'.repeat(64); }, d => { d.stageDiagnostics = true; },
    d => { d.samples[1].timedOut = true; }, d => { d.samples.pop(); },
    d => { d.samples[2] = d.samples[1]; }, d => { d.samples[1].resources = []; },
    d => { d.batches.pop(); }, d => { d.batches[2] = d.batches[1]; },
    d => { d.batches[1].milliseconds = 0; }, d => { d.final.bases.push('retained'); },
  ]) {
    const data = fixture(); change(data);
    assert.equal(evaluateCalibration(data).validCalibration, false);
  }
});
