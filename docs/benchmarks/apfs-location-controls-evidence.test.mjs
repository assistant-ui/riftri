import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import test from 'node:test';
import {gunzipSync} from 'node:zlib';
import {caseOrder, evaluateLocationControl} from './apfs-location-controls.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const archive = fs.readFileSync(new URL('./apfs-location-controls-2026-10-10.json.gz', import.meta.url));
const plain = gunzipSync(archive);
const record = JSON.parse(plain);

test('location evidence retains exact terminal identities, sources and failure logs', () => {
  assert.equal(hash(archive), 'd6bc1d803c2a688e391735e3140f4a3ea30b3ef85dca5c0dd0538e94ebb8ee23');
  assert.equal(hash(plain), '7d31ab2c37f0beb33a3acd3aa38f026d1e7bba05d04fbbf3c1dc8732b654899d');
  assert.equal(record.sourceCommit, '8973ea5171c542b27f7dda26784a842cd6433bb1');
  assert.equal(record.baselineCommit, 'a58006d7c7b4989a055a967e65d5654281b77ea4');
  assert.equal(record.optimizationClaim, false);
  assert.equal(hash(record.run.text), record.run.sha256);
  assert.equal(hash(record.runLog.text), record.runLog.sha256);
  const run = JSON.parse(record.run.text);
  assert.equal(run.databaseId, 38021509234);
  assert.equal(run.status, 'completed');
  assert.equal(run.conclusion, 'failure');
  assert.equal(run.jobs.length, 2);
  for (const job of run.jobs) assert.equal(job.status, 'completed');
  const forward = run.jobs.find(j => j.name.endsWith('(forward)'));
  const reverse = run.jobs.find(j => j.name.endsWith('(reverse)'));
  assert.equal(forward.conclusion, 'success');
  assert.equal(reverse.conclusion, 'failure');
  const step = (job, name) => job.steps.find(s => s.name === name).conclusion;
  assert.equal(step(forward, 'Detach exact test image without forcing'), 'success');
  assert.equal(step(reverse, 'Run eight non-overlapping full-verification controls'), 'failure');
  assert.equal(step(reverse, 'Detach exact test image without forcing'), 'failure');
  assert.match(record.runLog.text, /has timed out after 35 minutes/);
  assert.match(record.runLog.text, /Resource busy/);
  assert.match(record.runLog.text, /Terminate orphan process: pid \(\d+\) \(riftri\)/);
  for (const source of record.sources) assert.equal(hash(source.text), source.sha256);
  assert.deepEqual(record.replicas.map(r => r.order), ['forward', 'reverse']);
  for (const [index, replica] of record.replicas.entries()) {
    assert.equal(replica.files.length, [684, 646][index]);
    assert.equal(new Set(replica.files.map(f => f.path)).size, replica.files.length);
    for (const file of replica.files) assert.equal(hash(file.text), file.sha256);
  }
});

test('partial attempts and missing cases remain distinct from 14 complete unstable controls', () => {
  const files = record.replicas.flatMap(r => r.files);
  const fixtures = files.filter(f => f.path.endsWith('/results.json')).map(f => JSON.parse(f.text));
  const complete = fixtures.filter(f => f.complete === true);
  assert.equal(fixtures.length, 15);
  assert.equal(complete.length, 14);
  assert.equal(complete.flatMap(f => f.samples).length, 622);
  assert.equal(fixtures.flatMap(f => f.samples).length, 632);
  assert.ok(fixtures.flatMap(f => f.samples).every(s => s.success && !s.timedOut && s.code === 0));
  assert.equal(fixtures.flatMap(f => f.batches).length, 233);
  assert.equal(files.filter(f => /\/git-.*\.jsonl$/.test(f.path)).length, 633);
  assert.equal(files.filter(f => /\/add-.*\.log$/.test(f.path)).length, 632);
  assert.equal(files.filter(f => f.path.includes('/state/operations/')).length, 11);
  for (const fixture of complete) {
    assert.equal(fixture.batches.length, 16);
    assert.ok(fixture.samples.every(s => s.success && !s.timedOut && s.code === 0));
    assert.equal(fixture.final.operations.active_views, 0);
    assert.deepEqual(fixture.final.bases, []);
    assert.deepEqual(fixture.final.diagnostic_issues, []);
  }
  const partial = fixtures.find(f => !f.complete);
  assert.equal(partial.samples.length, 10);
  assert.equal(partial.batches.length, 9);
  assert.equal(partial.final, undefined);
  const evaluations = record.replicas.flatMap(r => r.evaluations);
  assert.equal(evaluations.length, 16);
  assert.equal(evaluations.filter(e => e.validControl).length, 14);
  assert.equal(evaluations.filter(e => e.stable).length, 0);
  assert.equal(evaluations.filter(e => e.missing).length, 1);
});

test('offline evaluation reproduces every hosted outcome without executing archived sources', () => {
  for (const replica of record.replicas) {
    const files = new Map(replica.files.map(f => [f.path, f]));
    const controls = JSON.parse(files.get('location-report/controls.json').text);
    assert.deepEqual(controls.cases, caseOrder(replica.order));
    assert.equal(controls.optimizationClaim, false);
    assert.equal(controls.complete, replica.order === 'forward');
    assert.equal(controls.allControlsStable, false);
    assert.match(controls.binarySha256, /^[a-f0-9]{64}$/);
    assert.ok(files.get('location-binary-sha256.txt').text.startsWith(controls.binarySha256 + ' '));
    assert.equal(files.get('location-baseline-commit.txt').text.trim(), record.baselineCommit);
    assert.notEqual(controls.volumes.host.device, controls.volumes.image.device);
    assert.ok(controls.volumes.host.availableBytes >= 8 * 1024 ** 3);
    for (const item of caseOrder(replica.order)) {
      const root = item.location === 'host' ? 'location-host' : 'location-volume/fixtures';
      const file = files.get(`${root}/${item.name}/results.json`);
      const retained = replica.evaluations.find(e => e.name === item.name);
      if (!file) {
        assert.deepEqual(retained, {...item, missing: true, validControl: false, stable: false});
        continue;
      }
      const evaluation = evaluateLocationControl(JSON.parse(file.text), controls.binarySha256, item.concurrency);
      const hosted = controls.results.find(r => r.name === item.name);
      assert.equal(file.sha256, retained.rawSha256);
      assert.deepEqual(retained, {...item, rawSha256: file.sha256,
        hostedEvaluationPresent: Boolean(hosted), ...evaluation});
      if (hosted) {
        assert.deepEqual(evaluation, hosted.evaluation);
        assert.equal(hosted.rawSha256, file.sha256);
      } else {
        assert.equal(replica.order, 'reverse');
        assert.equal(item.name, 'image-serial-1');
        assert.equal(evaluation.validControl, false);
      }
    }
  }
});
