import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { captureStageStack, createStageSampler } from './stage-sampler.mjs';

const tick = ms => new Promise(resolve => setTimeout(resolve, ms));

test('stage sampling starts only from this build’s stage PID and stops with its worker', async () => {
  const pids = [];
  const sampler = createStageSampler({ binary:'/fixture/riftri', output:'/fixture', key:'worker', intervalMs:10,
    capture:async input => { pids.push(input.pid); return { code:0 }; } });
  sampler.observe('riftri: unrelated pid=999');
  sampler.observe('riftri: apfs-stage: start clone pid=-1');
  await tick(25);
  assert.deepEqual(pids, []);
  sampler.observe('riftri: apfs-stage: start clone pid=123');
  sampler.observe('riftri: journal-stage: start phase pid=456');
  await tick(35);
  const captures = await sampler.stop();
  assert.ok(captures.length >= 1);
  assert.ok(pids.every(pid => pid === 123), 'a later PID cannot redirect a worker observer');
  const count = pids.length;
  await tick(25);
  assert.equal(pids.length, count);
  assert.deepEqual(await sampler.stop(), captures);
});

test('sampling never overlaps itself and stop awaits the in-flight observation', async () => {
  let active = 0, maximum = 0, release;
  const sampler = createStageSampler({ binary:'/fixture/riftri', output:'/fixture', key:'worker', intervalMs:5,
    capture:async () => {
      maximum = Math.max(maximum, ++active);
      await new Promise(resolve => { release = resolve; });
      active--;
      return { code:0 };
    } });
  sampler.observe('riftri: journal-stage: start BaseReady pid=123');
  for (let i=0; i<100 && !release; i++) await tick(5);
  assert.equal(typeof release, 'function');
  await tick(25);
  let finished = false;
  const stopped = sampler.stop().then(result => { finished = true; return result; });
  await tick(10);
  assert.equal(finished, false);
  release();
  assert.equal((await stopped).length, 1);
  assert.equal(maximum, 1);
  assert.equal(active, 0);
});

test('best-effort capture failures remain visible and do not escape stop', async () => {
  const sampler = createStageSampler({ binary:'/fixture/riftri', output:'/fixture', key:'worker', intervalMs:5,
    capture:async () => { throw new Error('sample unavailable'); } });
  sampler.observe('riftri: apfs-stage: start clone pid=123');
  await tick(30);
  const captures = await sampler.stop();
  assert.ok(captures.length > 0);
  assert.ok(captures.every(result => result.error === 'sample unavailable'));
});

test('native sampler verifies an owned executable and captures a bounded stack', {
  skip: process.platform !== 'darwin' || process.env.RIFTRI_BENCH_STAGE_SAMPLE_TEST !== '1',
}, async () => {
  const output = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-stage-sampler-'));
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio:'ignore' });
  const exited = once(child, 'exit');
  try {
    await once(child, 'spawn');
    const wrong = await captureStageStack({ pid:child.pid, binary:'/bin/sh', output, key:'wrong', index:0 });
    assert.equal(wrong.skipped, 'worker executable changed');
    assert.deepEqual(fs.readdirSync(output), []);
    const sampled = await captureStageStack({ pid:child.pid, binary:process.execPath, output, key:'owned', index:0 });
    assert.equal(sampled.code, 0, JSON.stringify(sampled));
    assert.equal(sampled.timedOut, false);
    assert.match(fs.readFileSync(sampled.file, 'utf8'), /Call graph:/);
  } finally {
    child.kill('SIGTERM');
    await exited;
    fs.rmSync(output, { recursive:true, force:true });
  }
});
