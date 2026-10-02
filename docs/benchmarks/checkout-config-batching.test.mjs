import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('full comparison verifies a thousand-file fixture and ten concurrent CLI views', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY,
  timeout: 360_000,
}, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-comparison-test-'));
  const source = path.join(root, 'source');
  fs.mkdirSync(source);
  const run = (command, args, cwd = source, env = process.env) => {
    const result = spawnSync(command, args, { cwd, env, encoding: 'utf8', timeout: 330_000, maxBuffer: 10e6 });
    assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture preserved at ${root}`);
    return result.stdout;
  };
  run('git', ['init', '--quiet']);
  for (const [key, value] of [['user.name', 'Benchmark Test'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value]);
  for (let directory = 0; directory < 16; directory++) {
    const folder = path.join(source, `package-${directory}`);
    fs.mkdirSync(folder);
    for (let file = 0; file < 64; file++) fs.writeFileSync(path.join(folder, `file-${file}`), `${directory}:${file}\n`);
  }
  fs.writeFileSync(path.join(source, 'executable'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  fs.symlinkSync('package-0/file-0', path.join(source, 'link'));
  run('git', ['add', '--all']);
  run('git', ['commit', '--quiet', '-m', 'fixture']);
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const output = path.join(root, 'results');
  run(process.execPath, [fileURLToPath(new URL('./checkout-config-batching.mjs', import.meta.url)), binary, binary, source, 'HEAD', output], source,
    { ...process.env, RIFTRI_BENCH_SINGLE_ROUNDS: '1', RIFTRI_BENCH_BATCH_ROUNDS: '1', RIFTRI_BENCH_WORKERS: '10' });
  const result = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.ok(result.completedAt);
  assert.equal(result.allViewsVerifiedAndRemoved, true);
  assert.equal(result.files, 1026);
  assert.equal(result.batches.length, 5);
  assert.equal(result.summaries.length, 5);
  for (const summary of result.summaries) {
    assert.equal(summary.serialSeconds.samples, 1);
    assert.equal(summary.concurrentViewSeconds.samples, 10);
    assert.equal(summary.concurrentBatchSeconds.samples, 1);
  }
  assert.equal(run('git', ['status', '--porcelain=v1']), '');
  // A fully successful harness has removed all views and collected all bases.
  fs.rmSync(root, { recursive: true });
});
