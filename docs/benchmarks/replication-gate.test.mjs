import assert from 'node:assert/strict';
import { test } from 'node:test';
import { evaluateReplicationFixture } from './replication-gate.mjs';

function fixture(concurrency = 1) {
  const worker = (round, label, worker) => ({ round, label, worker, success:true, code:0, timedOut:false, error:null, reused:round > 0 });
  const data = { complete:true, failure:null, stageDiagnostics:false, rounds:8, concurrency,
    binarySha256:'a'.repeat(64), candidateSha256:'b'.repeat(64),
    final:{ operations:{ active_views:0 }, bases:[], diagnostic_issues:[] },
    samples:[worker(0, 'baseline', 0)], batches:[] };
  for (let round = 1; round <= 8; round++) for (const label of ['baseline', 'advice']) {
    data.batches.push({ round, label, concurrency, milliseconds:label === 'baseline' ? 200 : 100 });
    for (let index = 0; index < concurrency; index++) data.samples.push(worker(round, label, index));
  }
  return data;
}

test('only a complete uninstrumented fixture can pass the declared gate', () => {
  for (const concurrency of [1, 4]) {
    const result = evaluateReplicationFixture(fixture(concurrency), concurrency);
    assert.equal(result.completeComparison, true);
    assert.equal(result.gatePassed, true);
    assert.equal(result.fasterPairs, 8);
    assert.equal(result.baselineMedianMilliseconds, 200);
    assert.equal(result.candidateMedianMilliseconds, 100);
  }
});

test('identical or unidentified binaries and a zero-worker fixture cannot demonstrate improvement', () => {
  for (const candidateSha256 of ['a'.repeat(64), undefined, 'not-a-checksum']) {
    const data = fixture(); data.candidateSha256 = candidateSha256;
    assert.equal(evaluateReplicationFixture(data, 1).gatePassed, false);
  }
  assert.equal(evaluateReplicationFixture(fixture(0), 0).gatePassed, false);
});

test('a lower pooled median cannot hide insufficient paired wins', () => {
  const data = fixture();
  for (const batch of data.batches) if (batch.label === 'advice' && batch.round > 5) batch.milliseconds = 201;
  const result = evaluateReplicationFixture(data, 1);
  assert.equal(result.completeComparison, true);
  assert.equal(result.candidateMedianMilliseconds, 100);
  assert.equal(result.fasterPairs, 5);
  assert.equal(result.gatePassed, false);
  assert.deepEqual(result.reasons, ['fewer than six faster pairs']);
});

test('six paired wins cannot hide a worse median', () => {
  const data = fixture();
  for (const batch of data.batches) {
    batch.milliseconds = batch.round <= 4 ? 1 : 1000;
    if (batch.label === 'advice') batch.milliseconds += batch.round <= 2 ? 1999 : -0.5;
  }
  const result = evaluateReplicationFixture(data, 1);
  assert.equal(result.fasterPairs, 6);
  assert.ok(result.candidateMedianMilliseconds > result.baselineMedianMilliseconds);
  assert.equal(result.gatePassed, false);
  assert.deepEqual(result.reasons, ['candidate median did not improve']);
});

test('failed, missing, duplicated, instrumented or unclean evidence cannot supply a passing comparison', () => {
  const changes = [
    data => { data.complete = false; },
    data => { data.failure = 'retained error'; },
    data => { data.stageDiagnostics = true; },
    data => { data.rounds = 4; },
    data => { data.final.operations.active_views = 1; },
    data => { data.final.bases.push({}); },
    data => { data.final.diagnostic_issues.push({}); },
    data => { data.samples.pop(); },
    data => { data.samples[1] = data.samples[2]; },
    data => { data.samples[1].reused = false; },
    data => { data.samples[1].success = false; },
    data => { data.samples[1].error = 'launch error'; },
    data => { data.samples[1].code = 1; },
    data => { data.batches.pop(); },
    data => { data.batches[1] = data.batches[2]; },
    data => { data.batches[1].milliseconds = 0; },
    data => { data.batches[1].milliseconds = NaN; },
    data => { data.batches[1].concurrency = 4; },
  ];
  for (const change of changes) {
    const data = fixture(); change(data);
    const result = evaluateReplicationFixture(data, 1);
    assert.equal(result.completeComparison, false, change.toString());
    assert.equal(result.gatePassed, false, change.toString());
    assert.equal(result.fasterPairs, null);
    assert.equal(result.candidateMedianMilliseconds, null);
  }
});

test('baseline and candidate timeouts remain distinct and neither produces a passing fixture', () => {
  for (const label of ['baseline', 'advice']) {
    const data = fixture();
    data.samples.find(sample => sample.round === 1 && sample.label === label).timedOut = true;
    const result = evaluateReplicationFixture(data, 1);
    assert.equal(result.gatePassed, false);
    assert.equal(result.completeComparison, false);
    assert.equal(result.candidateTimeouts, label === 'advice' ? 1 : 0);
    assert.equal(result.baselineTimeouts, label === 'baseline' ? 1 : 0);
  }
  assert.equal(evaluateReplicationFixture(undefined, 1).gatePassed, false);
});
