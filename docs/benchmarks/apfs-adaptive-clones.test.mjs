import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import test from 'node:test';
import {gunzipSync} from 'node:zlib';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const archive = fs.readFileSync(new URL('./apfs-adaptive-clones-2026-10-09.json.gz', import.meta.url));
const plain = gunzipSync(archive);
const record = JSON.parse(plain);

test('rejected adaptive experiment retains its exact sources and raw results', () => {
  assert.equal(hash(archive), 'e8a770a9c97ba5810bb19b041416fd23edc1bccb972d55d89e8b3df12617bf68');
  assert.equal(hash(plain), '8320b65e80887c7abdcdc39aa25833b8fa006978afa0248ab87cc11e541c45dd');
  assert.equal(record.baselineCommit, 'a58006d7c7b4989a055a967e65d5654281b77ea4');
  assert.equal(record.sourceCommit, record.baselineCommit);
  for (const source of Object.values(record.sources)) assert.equal(hash(source.text), source.sha256);
  for (const fixture of record.fixtures) {
    assert.equal(hash(JSON.stringify(fixture.data, null, 2)), fixture.rawResultsSha256);
    assert.equal(fixture.data.binarySha256, '33240072e0f00873ecf588f0917dab1e198a5f9bd246bc3a3d195ad981f1c80a');
    assert.equal(fixture.data.candidateSha256, 'd81c7dbe050163c540627cbf7221ac888d6922e6fca8099a01089b1bf1f938f6');
  }
});

test('partial concurrent evidence and serial regressions cannot be presented as a passing experiment', () => {
  assert.equal(record.overallPerformanceGatePassed, false);
  assert.deepEqual(record.fixtures.map(f => f.name), ['serial-many', 'concurrent-four', 'reference-serial', 'reference-four']);
  assert.deepEqual(record.fixtures.map(f => f.gate.gatePassed), [false, true, false, false]);
  const samples = record.fixtures.flatMap(f => f.data.samples);
  assert.equal(samples.length, 144);
  assert.equal(record.fixtures.flatMap(f => f.data.batches).length, 58);
  assert.equal(samples.filter(s => s.success).length, 140);
  assert.equal(samples.filter(s => s.timedOut && s.label === 'baseline').length, 0);
  const failed = samples.filter(s => !s.success);
  assert.equal(failed.length, 4);
  assert.ok(failed.every(s => s.round === 6 && s.label === 'advice' && s.timedOut));
  assert.deepEqual(failed.map(s => s.worker).sort(), [0, 1, 2, 3]);
  for (const fixture of record.fixtures.slice(0, 3)) {
    assert.equal(fixture.data.complete, true);
    assert.equal(fixture.data.final.operations.active_views, 0);
    assert.deepEqual(fixture.data.final.bases, []);
    assert.deepEqual(fixture.data.final.diagnostic_issues, []);
  }
  const last = record.fixtures.at(-1);
  assert.notEqual(last.data.complete, true);
  assert.equal(last.gate.validity.completeComparison, false);
  assert.equal(last.gate.validity.candidateMedianMilliseconds, null);
  assert.equal(last.data.retained.length, 5);
  assert.equal(last.failedWorkerTraces.length, 4);
  assert.ok(last.failedWorkerTraces.every(t => !t.missing && t.errors.length === 0 && t.events.length > 0));
});

test('same-binary calibration preserves apparent gains and false regressions without claiming an optimization', () => {
  const bytes = fs.readFileSync(new URL('./apfs-adaptive-calibration-2026-10-09.json.gz', import.meta.url));
  assert.equal(hash(bytes), '7c939bf774b20dc2a26a4f556ae10e87b2b2b52697e61c2f7684bc8a71f3f33b');
  const data = JSON.parse(gunzipSync(bytes));
  for (const source of Object.values(data.sources)) assert.equal(hash(source.text), source.sha256);
  assert.equal(data.fixtures.length, 2);
  for (const fixture of data.fixtures) {
    assert.equal(hash(JSON.stringify(fixture.data, null, 2)), fixture.rawResultsSha256);
    assert.equal(fixture.data.binarySha256, fixture.data.candidateSha256);
    assert.equal(fixture.evaluation.validCalibration, true);
    assert.equal(fixture.evaluation.gatePassed, undefined);
    assert.equal(fixture.data.complete, true);
    assert.equal(fixture.data.samples.length, 17);
    assert.equal(fixture.data.batches.length, 16);
    assert.equal(fixture.data.final.operations.active_views, 0);
    assert.deepEqual(fixture.data.final.bases, []);
    assert.deepEqual(fixture.data.final.diagnostic_issues, []);
  }
  assert.equal(data.fixtures[0].evaluation.wouldCrossSerialLimits, true);
  assert.ok(data.fixtures[1].evaluation.apparentReductionPercent > 14);
});
