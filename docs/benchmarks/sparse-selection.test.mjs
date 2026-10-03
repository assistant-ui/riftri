import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('many sparse cones preserve bytes, exclusions, isolation and lifecycle cleanup', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY,
  timeout: 180000,
}, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-sparse-selection-test-'));
  const output = path.join(root, 'results');
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const run = spawnSync(process.execPath, [
    fileURLToPath(new URL('./sparse-selection.mjs', import.meta.url)), binary, binary, output, '512', '32', '1',
  ], { encoding: 'utf8', timeout: 170000, maxBuffer: 20e6 });
  assert.equal(run.status, 0, `${run.error ?? ''}\n${run.stdout}\n${run.stderr}\nFixture retained at ${root}`);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, true);
  assert.equal(report.cases.length, 2);
  // All managed views and bases were removed by the verified lifecycle above.
  fs.rmSync(root, { recursive: true });
});
