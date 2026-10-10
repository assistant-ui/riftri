import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {gunzipSync} from 'node:zlib';
import {prepareEvaluation, evaluateHosted} from './adaptive-evaluation-ci.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const archive = fs.readFileSync(new URL('./apfs-adaptive-replication-2026-10-09.json.gz', import.meta.url));
const plain = gunzipSync(archive);
const record = JSON.parse(plain);

test('hosted adaptive evidence retains all three terminal runs and exact file identities', () => {
  assert.equal(hash(archive), 'd695be8d487d8f49cd5dcb1f919096f63ea7354d2091f71347c2b47bfd18769f');
  assert.equal(hash(plain), '1ec2b2864a92a73cf7bf47a6cdb9320dfce7a04a6926caf103c41865587497a1');
  assert.equal(record.sourceCommit, 'f08762a445c2163fab9c16a008c2f02f601e45d8');
  assert.equal(record.baselineCommit, 'a58006d7c7b4989a055a967e65d5654281b77ea4');
  assert.equal(hash(record.run.text), record.run.sha256);
  const run = JSON.parse(record.run.text);
  assert.equal(run.databaseId, 38016437732);
  assert.equal(run.status, 'completed');
  assert.equal(run.conclusion, 'failure');
  assert.equal(run.jobs.length, 3);
  for (const job of run.jobs) {
    assert.equal(job.status, 'completed');
    assert.equal(job.conclusion, 'failure');
    assert.equal(job.steps.find(s => s.name === 'Detach exact test image without forcing').conclusion, 'success');
  }
  assert.deepEqual(record.replicas.map(r => r.replica), [1, 2, 3]);
  for (const source of record.sources) assert.equal(hash(source.text), source.sha256);
  for (const replica of record.replicas) {
    assert.equal(replica.files.length, 404);
    assert.equal(new Set(replica.files.map(f => f.path)).size, replica.files.length);
    for (const file of replica.files) assert.equal(hash(file.text), file.sha256);
    assert.equal(replica.hostedEvaluationPresent, true);
  }
});

test('all successful creates and controls remain visible without turning failed gates into speedup claims', () => {
  assert.equal(record.overallPerformanceGatePassed, false);
  const fixtures = record.replicas.flatMap(r => r.files.filter(f => f.path.endsWith('/results.json')).map(f => JSON.parse(f.text)));
  assert.equal(fixtures.length, 18);
  assert.equal(fixtures.flatMap(f => f.samples).length, 594);
  assert.equal(fixtures.flatMap(f => f.batches).length, 288);
  for (const fixture of fixtures) {
    assert.equal(fixture.complete, true);
    assert.equal(fixture.batches.length, 16);
    assert.ok(fixture.samples.every(s => s.success && !s.timedOut && s.code === 0));
    assert.equal(fixture.final.operations.active_views, 0);
    assert.deepEqual(fixture.final.bases, []);
    assert.deepEqual(fixture.final.diagnostic_issues, []);
  }
  const evaluations = record.replicas.flatMap(r => r.evaluation.fixtures);
  assert.equal(evaluations.filter(f => !f.control && f.passed).length, 3);
  assert.equal(evaluations.filter(f => f.control && f.passed).length, 1);
  assert.ok(evaluations.filter(f => !f.control && f.passed).every(f => f.name === 'serial-many'));
  assert.ok(record.replicas.every(r => !r.evaluation.passed));
});

test('offline reevaluation exactly reproduces every hosted gate without executing result-selected code', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-adaptive-replay-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const prepared = path.join(root, 'prepared');
  prepareEvaluation(prepared);
  const fixtureNames = ['calibration-serial', 'calibration-reference-serial',
    'serial-many', 'concurrent-four', 'reference-serial', 'reference-four'];
  for (const replica of record.replicas) {
    const directory = path.join(root, `replica-${replica.replica}`);
    fs.mkdirSync(directory);
    const byPath = new Map(replica.files.map(f => [f.path, f]));
    // Only explicit known data paths are extracted, never paths from results.
    const identityFile = path.join(directory, 'identity.json');
    fs.writeFileSync(identityFile, byPath.get('adaptive-identity.json').text, {flag: 'wx'});
    for (const name of fixtureNames) {
      fs.mkdirSync(path.join(directory, name));
      fs.writeFileSync(path.join(directory, name, 'results.json'),
        byPath.get(`adaptive-volume/${name}/results.json`).text, {flag: 'wx'});
    }
    assert.deepEqual(await evaluateHosted(directory, prepared, identityFile), replica.evaluation);
    assert.deepEqual(JSON.parse(byPath.get('adaptive-evaluation.json').text), replica.evaluation);
  }
});
