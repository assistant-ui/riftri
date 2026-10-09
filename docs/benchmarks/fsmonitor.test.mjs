import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { distribution } from './latency-summary.mjs';

test('recorded FSMonitor percentiles retain every raw timing sample', () => {
  const receipt = JSON.parse(fs.readFileSync(new URL('./fsmonitor-2026-10-08.json', import.meta.url)));
  assert.deepEqual(receipt.runs.map(run => run.trackedFiles), [4100, 20004]);
  for (const run of receipt.runs) {
    assert.equal(run.measurementsComplete, true);
    assert.equal(run.cleanupComplete, true);
    for (const { group, milliseconds, ...summary } of run.measurements) {
      assert.deepEqual(summary, distribution(milliseconds), group);
      assert.equal(milliseconds.length, group.startsWith('status/') ? run.statusRounds
        : group.includes('/cold/') ? run.coldRounds : run.creationRounds);
    }
  }
});

test('FSMonitor comparison checks all variants and retires its disposable fixture', {
  skip: process.platform !== 'darwin' || !process.env.RIFTRI_BENCH_TEST_BINARY,
  timeout: 360_000,
}, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-fsmonitor-test-'));
  const output = path.join(root, 'results');
  const result = spawnSync(process.execPath, [
    fileURLToPath(new URL('./fsmonitor.mjs', import.meta.url)),
    path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY), output,
  ], {
    encoding: 'utf8', timeout: 330_000, maxBuffer: 10e6,
    env: { ...process.env, BENCH_FILES: '128', BENCH_CREATION_ROUNDS: '1', BENCH_COLD_ROUNDS: '1', BENCH_STATUS_ROUNDS: '2' },
  });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture preserved at ${root}`);
  const receipt = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(receipt.measurementsComplete, true);
  assert.equal(receipt.cleanupComplete, true);
  assert.equal(receipt.fixtureRemoved, true);
  assert.equal(receipt.socketsRemoved, true);
  assert.equal(fs.existsSync(path.join(output, 'fixture')), false);
  assert.deepEqual(receipt.cleanupErrors, []);
  assert.equal(receipt.trackedFiles, 132);
  assert.equal(receipt.creation.length, 5);
  assert.equal(receipt.status.length, 18);
  assert.equal(receipt.correctness.length, 9);
  assert.ok(receipt.correctness.every(check => check.passed));
  for (const variant of ['fsmonitor', 'fsmonitor-untracked']) {
    const trace = receipt.traces.find(trace => trace.variant === variant && trace.scenario === 'three-edits');
    assert.ok(trace.counters.some(counter => counter.key === 'apply_count' && counter.value === '3'));
  }
  for (const [key, value] of Object.entries(receipt.finalState.operations)) {
    if (key.startsWith('pending_') || key === 'active_views') assert.equal(value, 0, key);
  }
  // Only after lifecycle cleanup is verified, remove this exact test-owned output.
  fs.rmSync(root, { recursive: true });
});
