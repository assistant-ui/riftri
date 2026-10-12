import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {timedProcess} from './timed-process.mjs';

test('cold real-LFS comparison verifies new bases, isolation, refusals and cleanup', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY, timeout: 600000,
}, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-cold-lfs-'));
  const output = path.join(root, 'results');
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const result = await timedProcess(process.execPath, [
    fileURLToPath(new URL('./cold-lfs.mjs', import.meta.url)), binary, binary, output, '4', '2097152', '1', '500000',
  ], {env: process.env, timeoutMs: 550000});
  assert.equal(result.code, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, true);
  assert.equal(report.speedupClaim, false);
  assert.equal(report.binarySha256.before, report.binarySha256.after);
  assert.equal(report.samples.length, 2);
  assert.ok(report.samples.every(sample => sample.settled && sample.verified && !sample.reused));
  assert.deepEqual(report.refusals, [{failure: 'corrupt', settled: true}, {failure: 'missing', settled: true}]);
  assert.ok(report.commands.every(command => command.settled));
  // The runner has already removed views and collected bases. Keep failures.
  fs.rmSync(root, {recursive: true});
});

test('cold LFS budget expiry retains an incomplete settled receipt', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY, timeout: 15000,
}, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-cold-lfs-cancel-'));
  const output = path.join(root, 'results');
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const result = await timedProcess(process.execPath, [
    fileURLToPath(new URL('./cold-lfs.mjs', import.meta.url)), binary, binary, output, '4', '4', '1', '1',
  ], {env: process.env, timeoutMs: 10000});
  assert.notEqual(result.code, 0);
  assert.notEqual(result.code, null, `runner did not settle: ${result.stderr}`);
  assert.equal(result.timedOut, false);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(report.complete, false);
  assert.equal(report.speedupClaim, false);
  assert.equal(report.cancellation, 'benchmark budget exhausted');
  assert.ok(report.commands.every(command => command.settled));
  assert.ok(report.samples.every(sample => !sample.verified));
  fs.rmSync(root, {recursive: true});
});
