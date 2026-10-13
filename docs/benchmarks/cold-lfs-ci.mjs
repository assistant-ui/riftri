import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {summarize} from './cold-lfs-summary.mjs';
import {profileTrace} from './startup-profile.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const read = file => JSON.parse(fs.readFileSync(file));
const write = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + '\n', {flag: 'wx'});
const commit = value => assert.match(value, /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/);
const runnerFiles = ['cold-lfs.mjs', 'process-scope.mjs', 'timed-process.mjs'];

export function createPlan(baselineCommit, candidateCommit) {
  commit(baselineCommit); commit(candidateCommit);
  assert.notEqual(baselineCommit, candidateCommit);
  return {schemaVersion: 1, baselineCommit, candidateCommit, count: 16, bytes: 8388608,
    rounds: 20, budgetMs: 600000, lanes: ['control-before', 'candidate', 'control-after'],
    runnerSha256: Object.fromEntries(runnerFiles.map(name => [name, hash(fs.readFileSync(new URL(name, import.meta.url)))])),
    policy: 'One sequential three-lane study, alternating pairs, no exclusions or retries. Both controls within 5% p50/p95; candidate p50 at least 5% faster, p95 no more than 5% slower. OS caches are not evicted.',
    startedAt: new Date().toISOString()};
}

export function inspectBuild(text, binary, sourceCommit) {
  commit(sourceCommit);
  const messages = text.trim().split('\n').map(line => JSON.parse(line));
  assert.equal(messages.at(-1)?.reason, 'build-finished');
  assert.equal(messages.at(-1)?.success, true);
  const artifacts = messages.filter(m => m.reason === 'compiler-artifact' &&
    m.target?.name === 'riftri' && m.target.kind?.includes('bin') && m.executable);
  assert.equal(artifacts.length, 1, 'one compiler receipt for the measured binary required');
  const artifact = artifacts[0];
  assert.equal(artifact.fresh, false, 'each binary must be freshly compiled in its own target directory');
  assert.deepEqual(artifact.profile, {opt_level: '3', debuginfo: 0,
    debug_assertions: false, overflow_checks: false, test: false});
  assert.equal(fs.realpathSync(artifact.executable), fs.realpathSync(binary));
  return {sourceCommit, sha256: hash(fs.readFileSync(binary)), fresh: artifact.fresh, profile: artifact.profile};
}

export function qualifyReports(plan, proof, reports) {
  const expected = createPlan(plan.baselineCommit, plan.candidateCommit);
  for (const key of ['schemaVersion', 'count', 'bytes', 'rounds', 'budgetMs', 'lanes', 'runnerSha256']) {
    assert.deepEqual(plan[key], expected[key], `changed hosted plan: ${key}`);
  }
  assert.equal(proof.baseline.sourceCommit, plan.baselineCommit);
  assert.equal(proof.candidate.sourceCommit, plan.candidateCommit);
  for (const name of plan.lanes) {
    const report = reports[name];
    for (const key of ['count', 'bytes', 'rounds', 'budgetMs']) assert.equal(report[key], plan[key], `unplanned ${key}`);
    assert.deepEqual(report.runnerSha256, plan.runnerSha256);
    assert.equal(report.binarySha256.before, proof.baseline.sha256);
    assert.equal(report.binarySha256.after, proof[name === 'candidate' ? 'candidate' : 'baseline'].sha256);
  }
  // Reuse the existing gate verbatim, including both bracketing controls.
  return summarize(reports);
}

export function evaluateHosted(root) {
  const plan = read(path.join(root, 'plan.json')), proof = read(path.join(root, 'build-provenance.json'));
  const reports = Object.fromEntries(['control-before', 'candidate', 'control-after']
    .map(name => [name, read(path.join(root, name, 'results.json'))]));
  const result = qualifyReports(plan, proof, reports);
  const traces = [];
  for (const [lane, report] of Object.entries(reports)) {
    for (const sample of report.samples) {
      // Resolve only known fixture filenames, never a recorded executable or
      // absolute path. Downloaded artifacts remain independently verifiable.
      const relative = `${lane}/git-${sample.round}-${sample.label}.jsonl`;
      const raw = fs.readFileSync(path.join(root, relative), 'utf8');
      const profile = profileTrace(raw);
      assert.equal(profile.complete, true, `incomplete trace: ${relative}`);
      traces.push({path: relative, sha256: hash(raw), metrics: profile.metrics});
    }
  }
  return {...result, baselineCommit: plan.baselineCommit, candidateCommit: plan.candidateCommit, traces};
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [mode, root, ...args] = process.argv.slice(2);
  if (mode === 'plan' && args.length === 2) {
    const plan = createPlan(...args);
    fs.mkdirSync(root); // Never overwrite or mix evidence from earlier attempts.
    write(path.join(root, 'plan.json'), {...plan,
      runId: process.env.GITHUB_RUN_ID ?? null, runAttempt: process.env.GITHUB_RUN_ATTEMPT ?? null});
  } else if (mode === 'identity' && args.length === 2) {
    const plan = read(path.join(root, 'plan.json'));
    const proof = Object.fromEntries(['baseline', 'candidate'].map((name, index) => [name,
      inspectBuild(fs.readFileSync(path.join(root, `${name}-build.jsonl`), 'utf8'), args[index], plan[`${name}Commit`])]));
    assert.notEqual(proof.baseline.sha256, proof.candidate.sha256, 'candidate must differ from baseline');
    write(path.join(root, 'build-provenance.json'), proof);
  } else if (mode === 'evaluate' && args.length === 0) {
    let result;
    try { result = evaluateHosted(root); }
    catch (error) { result = {qualified: false, speedupClaim: false, error: error.stack}; }
    write(path.join(root, 'summary.json'), result);
    console.log(JSON.stringify(result, null, 2));
    if (!result.qualified) process.exitCode = 1;
  } else throw Error('expected plan ROOT BASELINE_COMMIT CANDIDATE_COMMIT, identity ROOT BASELINE_BINARY CANDIDATE_BINARY, or evaluate ROOT');
}
