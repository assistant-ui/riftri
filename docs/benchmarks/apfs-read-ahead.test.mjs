import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

for (const stageDiagnostics of [false, true]) test(`failed APFS workers retain outcomes with diagnostics=${stageDiagnostics}`, {
  skip: process.platform !== 'darwin' || !process.env.RIFTRI_BENCH_TEST_BINARY,
  timeout: 180000,
}, () => {
  const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-advice-failure-test-'));
  const output = path.join(fixture, 'output');
  const candidate = path.join(fixture, 'failing-candidate');
  fs.writeFileSync(candidate, '#!/bin/sh\necho injected-failure >&2\nexit 17\n', { mode: 0o755 });
  const binary = fs.realpathSync(process.env.RIFTRI_BENCH_TEST_BINARY);
  const result = spawnSync(process.execPath, [
    fileURLToPath(new URL('./apfs-read-ahead.mjs', import.meta.url)),
    binary, output, '32', '1', candidate, '128', '-', '2',
  ], { env:{ ...process.env, RIFTRI_BENCH_STAGE_DIAGNOSTICS:stageDiagnostics ? '1' : '' }, encoding: 'utf8', timeout: 150000, maxBuffer: 10e6 });
  assert.ifError(result.error);
  assert.equal(result.status, 1, `${result.stderr}\nPreserved fixture: ${fixture}`);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.schemaVersion, 2);
  assert.equal(report.stageDiagnostics, stageDiagnostics);
  assert.equal(report.complete, undefined);
  assert.match(report.failure, /injected-failure/);
  assert.equal(report.samples.length, 5); // Anchor, two baseline and two failed candidate workers.
  assert.equal(report.batches.length, 1);
  assert.equal(report.batches[0].label, 'baseline');
  const failures = report.samples.filter((sample) => !sample.success);
  assert.equal(failures.length, 2);
  assert.deepEqual(failures.map((sample) => sample.worker).sort(), [0, 1]);
  for (const failure of failures) {
    assert.equal(failure.label, 'advice');
    assert.equal(failure.code, 17);
    assert.equal(failure.signal, null);
    assert.equal(failure.timedOut, false);
    assert.equal(failure.error, null);
    assert.ok(failure.milliseconds > 0);
    assert.equal(failure.gitCommands, undefined);
    if (stageDiagnostics) assert.deepEqual(failure.stackSamples, []);
    else assert.equal(failure.stackSamples, undefined);
    assert.ok(report.retained.includes(failure.view));
  }
  const env = { ...process.env, PATH: '/usr/bin:/bin:/usr/sbin:/sbin' };
  for (const key of Object.keys(env)) if (key.startsWith('GIT_') || key.startsWith('RIFTRI_')) delete env[key];
  Object.assign(env, { GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_SYSTEM: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' });
  const run = (args) => {
    const cleanup = spawnSync(binary, args, { cwd: path.join(output, 'repository'), env, encoding: 'utf8', timeout: 60000 });
    assert.equal(cleanup.status, 0, `${cleanup.stderr}\nPreserved fixture: ${fixture}`);
  };
  const state = path.join(output, 'state');
  // Only the clean anchor remains. Retire its journaled view through Riftri;
  // do not force-remove anything after a failed benchmark.
  for (const sample of report.samples.filter((sample) => sample.success)) {
    if (fs.existsSync(sample.view)) run(['worktree', 'remove', sample.view, '--state-dir', state, '--no-progress']);
  }
  run(['gc', '--apply', '--yes', '--state-dir', state, '--no-progress']);
  fs.rmSync(fixture, { recursive: true });
});
