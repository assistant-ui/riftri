import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createPlan, inspectBuild, qualifyReports, evaluateHosted} from './cold-lfs-ci.mjs';

const baselineCommit = 'a'.repeat(40), candidateCommit = 'b'.repeat(40);
const profile = {opt_level: '3', debuginfo: 0, debug_assertions: false, overflow_checks: false, test: false};

test('the hosted plan fixes the workload, order and gates before timing', () => {
  const plan = createPlan(baselineCommit, candidateCommit);
  assert.equal(plan.count, 16);
  assert.equal(plan.bytes, 8388608);
  assert.equal(plan.rounds, 20);
  assert.equal(plan.budgetMs, 600000);
  assert.deepEqual(plan.lanes, ['control-before', 'candidate', 'control-after']);
  assert.match(plan.policy, /no exclusions or retries/);
  assert.throws(() => createPlan('main', candidateCommit));
  assert.throws(() => createPlan(baselineCommit, baselineCommit));
});

test('build receipts require successful fresh optimized compiler output for the exact binary', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-lfs-build-proof-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const binary = path.join(root, 'riftri');
  fs.writeFileSync(binary, 'unit-test binary, not performance evidence');
  const artifact = {reason: 'compiler-artifact', target: {name: 'riftri', kind: ['bin']},
    executable: binary, fresh: false, profile};
  const lines = (item, success = true) => [item, {reason: 'build-finished', success}].map(JSON.stringify).join('\n');
  const proof = inspectBuild(lines(artifact), binary, baselineCommit);
  assert.equal(proof.sourceCommit, baselineCommit);
  assert.equal(proof.fresh, false);
  assert.match(proof.sha256, /^[a-f0-9]{64}$/);
  for (const mutate of [
    a => { a.fresh = true; }, a => { a.profile.opt_level = '0'; },
    a => { a.profile.test = true; }, a => { a.profile.debuginfo = 2; },
    a => { a.executable = path.join(root, 'different'); },
    a => { a.target.kind = ['lib']; },
  ]) {
    const changed = structuredClone(artifact); mutate(changed);
    assert.throws(() => inspectBuild(lines(changed), binary, baselineCommit));
  }
  assert.throws(() => inspectBuild(lines(artifact, false), binary, baselineCommit));
  assert.throws(() => inspectBuild(JSON.stringify(artifact), binary, baselineCommit));
  assert.throws(() => inspectBuild(lines(artifact) + '\n' + JSON.stringify(artifact), binary, baselineCommit));
});

function fixture() {
  const plan = createPlan(baselineCommit, candidateCommit);
  const proof = {baseline: {sourceCommit: baselineCommit, sha256: 'c'.repeat(64)},
    candidate: {sourceCommit: candidateCommit, sha256: 'd'.repeat(64)}};
  const reports = Object.fromEntries(plan.lanes.map(name => [name, {
    schemaVersion: 1, runner: 'supervised-cold-lfs-v1', complete: true,
    count: plan.count, bytes: plan.bytes, rounds: plan.rounds, budgetMs: plan.budgetMs,
    git: 'fixture Git', lfs: 'fixture LFS', tree: 'e'.repeat(40), availableParallelism: 4,
    platform: 'darwin', release: 'fixture kernel', cpu: 'fixture CPU',
    runnerSha256: plan.runnerSha256,
    binarySha256: {before: proof.baseline.sha256, after: proof[name === 'candidate' ? 'candidate' : 'baseline'].sha256},
    commands: [{settled: true, code: 0}], refusals: [{failure: 'corrupt', settled: true}, {failure: 'missing', settled: true}],
    // Synthetic timings test the gate; they are never measurement evidence.
    samples: Array.from({length: 40}, (_, i) => ({round: Math.floor(i / 2), label: i % 2 ? 'after' : 'before',
      settled: true, verified: true, reused: false, milliseconds: name === 'candidate' && i % 2 ? 90 : 100})),
  }]));
  return {plan, proof, reports};
}

test('hosted qualification binds all lanes to the predeclared plan and fresh build identity', () => {
  const {plan, proof, reports} = fixture();
  assert.equal(qualifyReports(plan, proof, reports).qualified, true);
  for (const mutate of [
    f => { f.reports.candidate.rounds = 8; f.reports.candidate.samples.splice(16); },
    f => { for (const r of Object.values(f.reports)) r.bytes = 4; },
    f => { f.reports.candidate.binarySha256.after = 'f'.repeat(64); },
    f => { f.proof.candidate.sourceCommit = 'f'.repeat(40); },
    f => { f.plan.bytes = 4; for (const r of Object.values(f.reports)) r.bytes = 4; },
    f => { f.plan.lanes.push('../untrusted'); },
    f => { f.reports.candidate.runnerSha256 = {...f.plan.runnerSha256, 'cold-lfs.mjs': 'f'.repeat(64)}; },
    f => { f.reports.candidate.complete = false; },
  ]) {
    const changed = fixture(); mutate(changed);
    assert.throws(() => qualifyReports(changed.plan, changed.proof, changed.reports));
  }
  const noisy = fixture(); noisy.reports['control-after'].samples.forEach(s => { if (s.label === 'after') s.milliseconds = 110; });
  assert.equal(qualifyReports(noisy.plan, noisy.proof, noisy.reports).qualified, false);
  const flat = fixture(); flat.reports.candidate.samples.forEach(s => { s.milliseconds = 100; });
  assert.equal(qualifyReports(flat.plan, flat.proof, flat.reports).qualified, false);
});

test('hosted workflow is read-only, uncached, bounded, sequential and retains failed evidence', () => {
  const workflow = fs.readFileSync(new URL('../../.github/workflows/cold-lfs.yml', import.meta.url), 'utf8');
  assert.match(workflow, /contents: read/);
  assert.doesNotMatch(workflow, /pull_request_target|secrets\.|continue-on-error|cancel-in-progress: true/);
  assert.equal((workflow.match(/persist-credentials: false/g) ?? []).length, 2);
  assert.equal((workflow.match(/cargo build --release --locked/g) ?? []).length, 2);
  assert.doesNotMatch(workflow, /actions\/cache|rust-cache/);
  assert.match(workflow, /timeout-minutes: 45/);
  assert.match(workflow, /for lane in control-before candidate control-after/);
  assert.match(workflow, /16 8388608 20 600000/);
  assert.match(workflow, /if: always\(\)[\s\S]*uses: actions\/upload-artifact/);
  assert.match(workflow, /git-\*\.jsonl/);
  assert.match(workflow, /command-\*\.stdout/);
});

test('downloaded evidence replays local traces, reports missing evidence and refuses overwrite', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-lfs-hosted-proof-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const {plan, proof, reports} = fixture();
  fs.writeFileSync(path.join(root, 'plan.json'), JSON.stringify(plan));
  fs.writeFileSync(path.join(root, 'build-provenance.json'), JSON.stringify(proof));
  const trace = fs.readFileSync(new URL('./fixtures/lfs-trace-38319750916.jsonl', import.meta.url));
  for (const [lane, report] of Object.entries(reports)) {
    fs.mkdirSync(path.join(root, lane));
    for (const sample of report.samples) {
      sample.trace = '/untrusted-original-host/never-open-this.jsonl';
      fs.writeFileSync(path.join(root, lane, `git-${sample.round}-${sample.label}.jsonl`), trace);
    }
    fs.writeFileSync(path.join(root, lane, 'results.json'), JSON.stringify(report));
  }
  const result = evaluateHosted(root);
  assert.equal(result.qualified, true);
  assert.equal(result.traces.length, 120);
  assert.ok(result.traces.every(t => t.metrics.resetLfsFilterStarts === 1));
  fs.unlinkSync(path.join(root, 'candidate', 'git-0-before.jsonl'));
  assert.throws(() => evaluateHosted(root), {code: 'ENOENT'});
  const cli = fileURLToPath(new URL('./cold-lfs-ci.mjs', import.meta.url));
  const failed = spawnSync(process.execPath, [cli, 'evaluate', root], {encoding: 'utf8', timeout: 10000});
  assert.equal(failed.status, 1);
  const summaryPath = path.join(root, 'summary.json');
  const saved = fs.readFileSync(summaryPath, 'utf8');
  assert.equal(JSON.parse(saved).qualified, false);
  assert.match(JSON.parse(saved).error, /ENOENT/);
  const repeated = spawnSync(process.execPath, [cli, 'evaluate', root], {encoding: 'utf8', timeout: 10000});
  assert.equal(repeated.status, 1);
  assert.match(repeated.stderr, /EEXIST/);
  assert.equal(fs.readFileSync(summaryPath, 'utf8'), saved);
});
