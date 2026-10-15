import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
import test from 'node:test';
import {readStartupProfile} from './startup-profile.mjs';

const binary = process.env.RIFTRI_BENCH_TEST_BINARY;
const integration = {skip: process.platform !== 'darwin' || !binary, timeout: 120000};
const script = fileURLToPath(new URL('./apfs-startup.mjs', import.meta.url));
const fixture = fileURLToPath(new URL('./fixtures/benchmark-binary.mjs', import.meta.url));
const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;
const exists = pid => { try { process.kill(pid, 0); return true; } catch (e) { if (e.code === 'ESRCH') return false; throw e; } };
const killGroup = pid => { try { process.kill(-pid, 'SIGKILL'); } catch (e) { if (e.code !== 'ESRCH') throw e; } };

function start(t, args, env = {}, nodeArgs = []) {
  const child = spawn(process.execPath, [...nodeArgs, script, ...args], {env: {...process.env, ...env}, detached: true, stdio: ['ignore', 'pipe', 'pipe']});
  let log = '';
  child.stdout.on('data', bytes => { log += bytes; });
  child.stderr.on('data', bytes => { log += bytes; });
  let closed = false;
  const done = new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', (code, signal) => { closed = true; resolve({code, signal, log}); });
  });
  t.after(() => { if (!closed) killGroup(child.pid); });
  return {child, done, closed: () => closed, log: () => log};
}

const reportFault = fileURLToPath(new URL('./fixtures/report-save-failure.mjs', import.meta.url));
for (const operation of ['write', 'rename']) test(`a failed ${operation} of the failure receipt preserves the original error and retained paths`,
  {skip: process.platform !== 'darwin', timeout: 30000}, async t => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-report-failure-'));
    const output = path.join(root, 'result'), wrapper = path.join(root, 'riftri-wrapper');
    // No Riftri or worktree is created: fail the first add at a known point.
    fs.writeFileSync(wrapper, '#!/bin/sh\nif [ "$1" = --version ]; then echo fixture; exit 0; fi\necho "intentional add failure" >&2\nexit 37\n', {mode: 0o755});
    const outcome = await start(t, [wrapper, output, '32', '1'],
      {BENCH_TEST_OUTPUT: output, BENCH_TEST_REPORT_FAULT: operation}, ['--import', reportFault]).done;
    assert.equal(outcome.code, 1, outcome.log);
    assert.match(outcome.log, /intentional add failure/);
    assert.match(outcome.log, /Cannot retain failure receipt:.*ENOSPC/);
    const fallback = outcome.log.split('\n').find(line => line.startsWith('Unpersisted benchmark failure: '));
    assert.ok(fallback, outcome.log);
    const failure = JSON.parse(fallback.slice('Unpersisted benchmark failure: '.length));
    assert.equal(failure.complete, false);
    assert.match(failure.failure, /worker 0-baseline-0 exited with 37/);
    assert.deepEqual(failure.retained, [path.join(fs.realpathSync(output), 'view-0-baseline-0')]);
    // Atomic persistence must leave the last checkpoint readable, not replace
    // it with a partial failure receipt or a successful-looking result.
    const checkpoint = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
    assert.equal(checkpoint.complete, false);
    assert.equal(checkpoint.failure, undefined);
    assert.equal(checkpoint.samples.length, 1);
    assert.equal(checkpoint.samples[0].code, 37);
    assert.equal(checkpoint.samples[0].settled, true);
    assert.equal(checkpoint.samples[0].success, false);
    fs.rmSync(root, {recursive: true}); // This fake binary never created a view.
  });

test('failure to save completion is recorded as an incomplete run', integration, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-report-completion-'));
  const output = path.join(root, 'result');
  const outcome = await start(t, [binary, output, '32', '1', binary, '128', '-', '1', '90000'],
    {BENCH_TEST_OUTPUT: output, BENCH_TEST_REPORT_FAULT: 'complete'}, ['--import', reportFault]).done;
  assert.equal(outcome.code, 1, outcome.log);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, false);
  assert.match(report.failure, /ENOSPC/);
  assert.deepEqual(report.retained, []);
  assert.equal(report.final.operations.active_views, 0);
  assert.deepEqual(report.final.bases, []);
  assert.deepEqual(report.final.diagnostic_issues, []);
  const profile = readStartupProfile(path.join(output, 'results.json'));
  assert.equal(profile.complete, false);
  assert.ok(profile.groups.every(g => Object.values(g.metrics).every(m => m.distribution === null)));
  fs.rmSync(root, {recursive: true}); // All real views and bases were removed.
});

test('supervised startup still verifies real APFS creation, isolation and full cleanup', integration, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-supervision-smoke-'));
  const output = path.join(root, 'result');
  const run = start(t, [binary, output, '32', '1', binary, '128', '-', '4', '90000']);
  const outcome = await run.done;
  assert.equal(outcome.code, 0, outcome.log);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, true);
  assert.equal(Object.keys(report.runnerSha256).length, 3);
  assert.ok(Object.values(report.runnerSha256).every(hash => /^[a-f0-9]{64}$/.test(hash)));
  assert.equal(report.cancellation, undefined);
  assert.equal(report.samples.length, 9);
  assert.equal(report.batches.length, 2);
  assert.ok(report.samples.every(s => s.started && s.settled && s.success && !s.cancelled && !s.timedOut));
  assert.equal(report.final.operations.active_views, 0);
  assert.deepEqual(report.final.bases, []);
  assert.deepEqual(report.final.diagnostic_issues, []);
  const profile = readStartupProfile(path.join(output, 'results.json'));
  assert.equal(profile.complete, true);
  assert.equal(profile.samples.length, 9);
  assert.deepEqual(profile.groups.map(g => [g.label, g.reused, g.samples]), [
    ['baseline', false, 1], ['baseline', true, 4], ['advice', true, 4],
  ]);
  assert.ok(profile.groups.every(g => g.metrics.resetMs.distribution?.samples === g.samples));
  assert.ok(profile.groups.every(g => g.metrics.statusMs.distribution?.samples === g.samples));
  const archivedOutput = path.join(root, 'archived-result');
  const archived = await start(t, [binary, archivedOutput, '32', '1', binary, '128', path.join(output, 'repository'), '1', '90000']).done;
  assert.equal(archived.code, 0, archived.log);
  const archivedReport = JSON.parse(fs.readFileSync(path.join(archivedOutput, 'results.json')));
  assert.equal(archivedReport.complete, true);
  assert.equal(archivedReport.sourceTree, report.tree);
  assert.equal(archivedReport.tree, report.tree);
  assert.equal(archivedReport.samples.length, 3);
  assert.ok(archivedReport.samples.every(s => s.success));
  assert.equal(readStartupProfile(path.join(archivedOutput, 'results.json')).complete, true);
  fs.rmSync(root, {recursive: true}); // Only this successful, fully removed fixture.
});

for (const stage of ['create', 'remove']) test(`SIGTERM during ${stage} retains partial evidence and stops owned workers`, integration, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `riftri-supervision-${stage}-`));
  const output = path.join(root, 'result'), readyFile = path.join(root, 'ready.jsonl');
  const wrapper = path.join(root, 'riftri-wrapper');
  fs.writeFileSync(wrapper, `#!/bin/sh\nexec ${shellQuote(process.execPath)} ${shellQuote(fixture)} "$@"\n`, {mode: 0o755});
  const ready = () => fs.existsSync(readyFile) ? fs.readFileSync(readyFile, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
  // All targets come from this test's own wrappers. Never use a process-name kill.
  t.after(() => { for (const worker of ready()) { if (exists(worker.pid)) killGroup(worker.group); } });
  const run = start(t, [wrapper, output, '32', '1', wrapper, '128', '-', '4', '90000'],
    {BENCH_TEST_REAL_BINARY: binary, BENCH_TEST_BLOCK: stage, BENCH_TEST_READY: readyFile});
  const count = stage === 'create' ? 4 : 1;
  for (let n = 0; n < 3000 && ready().length < count && !run.closed(); n++) await delay(10);
  assert.equal(ready().length, count, run.log());
  // Started samples and logs must exist before workers exit.
  const before = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(before.complete, false);
  if (stage === 'create') {
    assert.equal(before.samples.filter(s => !s.settled).length, 4);
    for (const s of before.samples) assert.ok(fs.existsSync(path.join(output, `add-${s.round}-${s.label}-${s.worker}.log`)));
  }
  run.child.kill('SIGTERM');
  const outcome = await run.done;
  assert.equal(outcome.code, 1, outcome.log);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, false);
  assert.equal(report.cancellation.reason, 'SIGTERM');
  assert.ok(report.failure);
  assert.equal(report.samples.length, 5);
  assert.ok(report.samples.every(s => s.settled));
  assert.equal(report.samples.filter(s => s.cancelled).length, stage === 'create' ? 4 : 0);
  for (const worker of ready()) assert.equal(exists(worker.pid), false, `worker ${worker.pid} survived`);
  assert.ok(report.retained.length > 0);
  const profile = readStartupProfile(path.join(output, 'results.json'));
  assert.equal(profile.complete, false);
  assert.equal(profile.samples.length, report.samples.length);
  assert.ok(profile.groups.every(g => Object.values(g.metrics).every(m => m.distribution === null)));
  assert.ok(fs.existsSync(report.samples[0].base), 'interruption must not force-delete the base');
  if (stage === 'create') {
    for (const s of report.samples.filter(s => s.cancelled)) {
      assert.match(fs.readFileSync(path.join(output, `add-${s.round}-${s.label}-${s.worker}.log`), 'utf8'), /test workload ready/);
    }
  } else {
    assert.ok(report.samples.every(s => fs.existsSync(s.view)), 'cancelled cleanup preserves views');
  }
  console.log(`Retained interrupted ${stage} fixture for inspection: ${root}`);
});
