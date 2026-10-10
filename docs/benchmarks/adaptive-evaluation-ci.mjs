import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {gunzipSync} from 'node:zlib';
import {evaluateCalibration} from './apfs-calibration-gate.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const prototypeHash = 'bad2251b5708114f5302bc3c2e1253f2469fb572e59401543ffe99abab192660';
const baselineCommit = 'a58006d7c7b4989a055a967e65d5654281b77ea4';
const names = {prototype: 'prototype.patch', plan: 'plan.md',
  adaptiveGate: 'adaptive-evaluation.mjs', adaptiveGateTests: 'adaptive-evaluation.test.mjs',
  evidenceGate: 'docs/benchmarks/replication-gate.mjs', harness: 'docs/benchmarks/apfs-read-ahead.mjs',
  timedProcess: 'docs/benchmarks/timed-process.mjs', stageSampler: 'docs/benchmarks/stage-sampler.mjs'};

function retainedRecord() {
  const bytes = fs.readFileSync(new URL('./apfs-adaptive-clones-2026-10-09.json.gz', import.meta.url));
  assert.equal(hash(bytes), 'e8a770a9c97ba5810bb19b041416fd23edc1bccb972d55d89e8b3df12617bf68');
  const data = JSON.parse(gunzipSync(bytes));
  assert.equal(data.baselineCommit, baselineCommit);
  assert.equal(data.sources.prototype.sha256, prototypeHash);
  for (const name of Object.keys(names)) assert.equal(hash(data.sources[name].text), data.sources[name].sha256);
  return data;
}

export function prepareEvaluation(output) {
  const data = retainedRecord();
  fs.mkdirSync(output); // A fresh directory only; never overwrite a checkout.
  for (const [name, relative] of Object.entries(names)) {
    const target = path.join(output, relative);
    fs.mkdirSync(path.dirname(target), {recursive: true});
    fs.writeFileSync(target, data.sources[name].text, {flag: 'wx'});
  }
}

export function writeBuildIdentity(output, baseline, candidate) {
  const identity = {baselineCommit, prototypeHash,
    baselineSha256: hash(fs.readFileSync(baseline)), candidateSha256: hash(fs.readFileSync(candidate))};
  assert.notEqual(identity.baselineSha256, identity.candidateSha256);
  fs.writeFileSync(output, JSON.stringify(identity, null, 2) + '\n', {flag: 'wx'});
}

export async function evaluateHosted(root, prepared, identityFile) {
  // Verify the extracted code before importing this fixed, repository-owned
  // evidence gate. Result files cannot select modules or executable paths.
  const retained = retainedRecord();
  for (const [name, relative] of Object.entries(names)) {
    assert.equal(hash(fs.readFileSync(path.join(prepared, relative))), retained.sources[name].sha256);
  }
  const {evaluateAdaptiveFixture} = await import(pathToFileURL(path.join(prepared, names.adaptiveGate)));
  const identity = JSON.parse(fs.readFileSync(identityFile));
  assert.equal(identity.baselineCommit, baselineCommit);
  assert.equal(identity.prototypeHash, prototypeHash);
  assert.match(identity.baselineSha256, /^[a-f0-9]{64}$/);
  assert.match(identity.candidateSha256, /^[a-f0-9]{64}$/);
  assert.notEqual(identity.baselineSha256, identity.candidateSha256);
  const cases = [['calibration-serial', 1, true], ['calibration-reference-serial', 1, true],
    ['serial-many', 1, false], ['concurrent-four', 4, false], ['reference-serial', 1, false], ['reference-four', 4, false]];
  const fixtures = cases.map(([name, concurrency, control]) => {
    const file = path.join(root, name, 'results.json');
    if (!fs.existsSync(file)) return {name, control, passed: false, missing: true};
    try {
      const raw = fs.readFileSync(file), data = JSON.parse(raw);
      const identityMatches = data.binarySha256 === identity.baselineSha256 &&
        data.candidateSha256 === (control ? identity.baselineSha256 : identity.candidateSha256);
      const reference = name.includes('reference');
      const fixtureMatches = reference
        ? data.files === 5864 && data.logicalBytes === 73235115 &&
          data.sourceCommit === '038cd9f82b418afe9e6d0080648738f78586fbca' &&
          data.sourceTree === 'c4de7922b24126e860cb77652f5d080e04c8c396' && data.tree === data.sourceTree
        : data.files === 4097 && data.logicalBytes === 4096 * 8192 && !data.sourceCommit;
      const evaluation = control ? evaluateCalibration(data, identity.baselineSha256) : evaluateAdaptiveFixture(data, concurrency);
      // Symmetric control stability is separate from, and cannot relax, the
      // unchanged candidate gate. This is predeclared before hosted timing.
      const stableControl = control && evaluation.validCalibration &&
        Math.abs(evaluation.secondMedianMs / evaluation.firstMedianMs - 1) <= 0.05 &&
        Math.abs(evaluation.medianPairedRatio - 1) <= 0.05 &&
        Math.min(...evaluation.ratios) >= 0.5 && evaluation.maximumPairedRatio <= 2 &&
        Math.abs(evaluation.secondMedianCpuSeconds / evaluation.firstMedianCpuSeconds - 1) <= 0.05;
      return {name, control, rawSha256: hash(raw), identityMatches, fixtureMatches, evaluation,
        passed: identityMatches && fixtureMatches && (control ? stableControl : evaluation.gatePassed)};
    } catch (error) {
      return {name, control, passed: false, error: String(error)};
    }
  });
  return {schemaVersion: 1, identity, fixtures, passed: fixtures.every(f => f.passed)};
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [mode, ...args] = process.argv.slice(2);
  if (mode === 'prepare' && args.length === 1) prepareEvaluation(args[0]);
  else if (mode === 'identity' && args.length === 3) writeBuildIdentity(...args);
  else if (mode === 'evaluate' && args.length === 4) {
    const result = await evaluateHosted(...args.slice(0, 3));
    fs.writeFileSync(args[3], JSON.stringify(result, null, 2) + '\n', {flag: 'wx'});
    console.log(JSON.stringify(result, null, 2));
    if (!result.passed) process.exitCode = 1;
  } else throw Error('expected prepare OUTPUT, identity OUTPUT BASELINE CANDIDATE, or evaluate ROOT PREPARED IDENTITY OUTPUT');
}
