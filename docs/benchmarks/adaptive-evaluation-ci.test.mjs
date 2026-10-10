import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {gunzipSync} from 'node:zlib';
import {prepareEvaluation, evaluateHosted} from './adaptive-evaluation-ci.mjs';

const readRecord = name => JSON.parse(gunzipSync(fs.readFileSync(new URL(name, import.meta.url))));

test('replication preparation is exact, refuses overwrite, and rejects incomplete or changed evidence', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-adaptive-gate-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const prepared = path.join(root, 'prepared');
  prepareEvaluation(prepared);
  assert.throws(() => prepareEvaluation(prepared), {code: 'EEXIST'});
  const candidate = readRecord('./apfs-adaptive-clones-2026-10-09.json.gz');
  const controls = readRecord('./apfs-adaptive-calibration-2026-10-09.json.gz');
  const identityFile = path.join(root, 'identity.json');
  const identity = {baselineCommit: candidate.baselineCommit,
    prototypeHash: candidate.sources.prototype.sha256,
    baselineSha256: candidate.fixtures[0].data.binarySha256,
    candidateSha256: candidate.fixtures[0].data.candidateSha256};
  fs.writeFileSync(identityFile, JSON.stringify(identity));
  assert.equal((await evaluateHosted(root, prepared, identityFile)).passed, false);
  for (const f of [...candidate.fixtures, ...controls.fixtures]) {
    fs.mkdirSync(path.join(root, f.name));
    fs.writeFileSync(path.join(root, f.name, 'results.json'), JSON.stringify(f.data));
  }
  const result = await evaluateHosted(root, prepared, identityFile);
  assert.equal(result.passed, false);
  assert.deepEqual(result.fixtures.map(f => f.passed), [false, false, false, true, false, false]);
  assert.ok(result.fixtures.every(f => f.identityMatches));
  assert.ok(result.fixtures.every(f => f.fixtureMatches));

  // Fabricated unit-test timings exercise the passing branch; these are not
  // benchmark observations and never replace the retained research records.
  for (const f of [...candidate.fixtures, ...controls.fixtures]) {
    const data = structuredClone(f.name === 'reference-four' ? candidate.fixtures[1].data : f.data);
    if (f.name === 'reference-four') {
      for (const key of ['files', 'logicalBytes', 'sourceCommit', 'sourceTree', 'tree']) {
        data[key] = candidate.fixtures[2].data[key];
      }
    }
    for (const batch of data.batches) batch.milliseconds = data.concurrency === 4 && batch.label === 'advice' ? 900 : 1000;
    for (const sample of data.samples) sample.resources = ['1.00 real 0.50 user 0.50 sys'];
    fs.writeFileSync(path.join(root, f.name, 'results.json'), JSON.stringify(data));
  }
  assert.equal((await evaluateHosted(root, prepared, identityFile)).passed, true);
  const referenceFile = path.join(root, 'reference-serial', 'results.json');
  const wrongReference = JSON.parse(fs.readFileSync(referenceFile));
  wrongReference.sourceCommit = '0'.repeat(40);
  fs.writeFileSync(referenceFile, JSON.stringify(wrongReference));
  assert.equal((await evaluateHosted(root, prepared, identityFile)).fixtures[4].passed, false);
  fs.writeFileSync(path.join(root, 'serial-many', 'results.json'), '{invalid');
  assert.match((await evaluateHosted(root, prepared, identityFile)).fixtures[2].error, /SyntaxError/);
  fs.appendFileSync(path.join(prepared, 'prototype.patch'), '\n');
  await assert.rejects(evaluateHosted(root, prepared, identityFile), {name: 'AssertionError'});
});
