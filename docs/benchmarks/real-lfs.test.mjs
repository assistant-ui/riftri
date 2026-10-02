import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('real LFS preserves bytes, isolation and refusals across pointer-batch boundaries', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY,
  timeout: 600000,
}, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-real-lfs-test-'));
  const output = path.join(root, 'results');
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const run = spawnSync(process.execPath, [
    fileURLToPath(new URL('./real-lfs.mjs', import.meta.url)), binary, binary, output, '127,128,129', '1',
  ], { env: process.env, encoding: 'utf8', timeout: 590000, maxBuffer: 20e6 });
  assert.equal(run.status, 0, `${run.error ?? ''}\n${run.stdout}\n${run.stderr}\nFixture retained at ${root}`);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, true);
  assert.equal(report.cases.length, 6);
  assert.deepEqual(report.cases.filter(row => row.version === 'after').map(row => [row.count, row.batches]),
    [[127, 1], [128, 1], [129, 2]]);
  // The fixture has already removed its managed views and collected bases.
  fs.rmSync(root, { recursive: true });
});
