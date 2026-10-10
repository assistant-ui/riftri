import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
import test from 'node:test';

const binary = process.env.RIFTRI_BENCH_TEST_BINARY;
const integration = {skip: process.platform !== 'darwin' || !binary, timeout: 120000};
const script = fileURLToPath(new URL('./apfs-startup.mjs', import.meta.url));
const fixture = fileURLToPath(new URL('./fixtures/benchmark-binary.mjs', import.meta.url));
const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;
const exists = pid => { try { process.kill(pid, 0); return true; } catch (e) { if (e.code === 'ESRCH') return false; throw e; } };
const killGroup = pid => { try { process.kill(-pid, 'SIGKILL'); } catch (e) { if (e.code !== 'ESRCH') throw e; } };

function start(t, args, env = {}) {
  const child = spawn(process.execPath, [script, ...args], {env: {...process.env, ...env}, detached: true, stdio: ['ignore', 'pipe', 'pipe']});
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
  const archivedOutput = path.join(root, 'archived-result');
  const archived = await start(t, [binary, archivedOutput, '32', '1', binary, '128', path.join(output, 'repository'), '1', '90000']).done;
  assert.equal(archived.code, 0, archived.log);
  const archivedReport = JSON.parse(fs.readFileSync(path.join(archivedOutput, 'results.json')));
  assert.equal(archivedReport.complete, true);
  assert.equal(archivedReport.sourceTree, report.tree);
  assert.equal(archivedReport.tree, report.tree);
  assert.equal(archivedReport.samples.length, 3);
  assert.ok(archivedReport.samples.every(s => s.success));
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
