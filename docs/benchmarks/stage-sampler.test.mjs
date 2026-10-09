import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { captureStageStack, createStageSampler } from './stage-sampler.mjs';

const tick = ms => new Promise(resolve => setTimeout(resolve, ms));

function processFixture({ listing, parentAfter = '100 1 /fixture/riftri', childAfter = '200 100 /fixture/git', listingCode = 0 }) {
  const calls = [];
  let parentReads = 0;
  const canonicalize = file => file === '/fixture/git-link' ? '/fixture/git' : file;
  const processRunner = async (command, args, options) => {
    calls.push({ command, args, options });
    let stdout = '';
    if (command === '/usr/bin/sample') return { code:0, timedOut:false };
    if (args[0] === '-axo') return { code:listingCode, timedOut:false, stdout:listing };
    assert.equal(args[0], '-p');
    if (args[1] === '100') stdout = ++parentReads === 1 ? '100 1 /fixture/riftri' : parentAfter;
    else if (args[1] === '200') stdout = childAfter;
    else assert.fail(`unexpected process ${args[1]}`);
    return { code:0, timedOut:false, stdout };
  };
  return { calls, dependencies:{ platform:'darwin', canonicalize, processRunner } };
}

const fixtureInput = { pid:100, binary:'/fixture/riftri', gitBinary:'/fixture/git', output:'/fixture', key:'worker', index:0 };

test('diagnostic sampling prefers only an exact, revalidated direct Git child', async () => {
  const fixture = processFixture({ listing:'999 888 /fixture/git\n201 100 /elsewhere/git\n200 100 /fixture/git-link\n' });
  const result = await captureStageStack(fixtureInput, fixture.dependencies);
  assert.equal(result.role, 'git-child');
  assert.equal(result.pid, 200);
  assert.equal(result.parentPid, 100);
  assert.equal(result.binary, '/fixture/git');
  assert.equal(result.code, 0);
  assert.deepEqual(fixture.calls.map(call => call.args.slice(0, 2)), [
    ['-p', '100'], ['-axo', 'pid=,ppid=,comm='], ['-p', '100'], ['-p', '200'], ['200', '1'],
  ]);
  assert.ok(fixture.calls.every(call => call.options.timeoutMs <= 10000));
});

test('diagnostic sampling falls back to the verified parent only when no Git child exists', async () => {
  const fixture = processFixture({ listing:'999 888 /fixture/git\n201 100 /elsewhere/git\n' });
  const result = await captureStageStack(fixtureInput, fixture.dependencies);
  assert.equal(result.role, 'riftri');
  assert.equal(result.pid, 100);
  assert.equal(fixture.calls.filter(call => call.command === '/usr/bin/sample').length, 1);
});

test('a vanished, reparented or replaced child and ambiguous discovery never get sampled', async () => {
  for (const override of [
    { childAfter:'' },
    { childAfter:'200 999 /fixture/git' },
    { childAfter:'200 100 /elsewhere/git' },
    { parentAfter:'100 1 /elsewhere/riftri' },
    { listing:'200 100 /fixture/git\n201 100 /fixture/git' },
    { listingCode:1 },
  ]) {
    const fixture = processFixture({ listing:'200 100 /fixture/git', ...override });
    const result = await captureStageStack(fixtureInput, fixture.dependencies);
    assert.ok(result.skipped, JSON.stringify(override));
    assert.equal(fixture.calls.some(call => call.command === '/usr/bin/sample'), false);
  }
});

test('stage sampling starts only from this build’s stage PID and stops with its worker', async () => {
  const pids = [];
  const sampler = createStageSampler({ binary:'/fixture/riftri', gitBinary:'/fixture/git', output:'/fixture', key:'worker', intervalMs:10,
    capture:async input => { assert.equal(input.gitBinary, '/fixture/git'); pids.push(input.pid); return { code:0 }; } });
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

test('native sampler captures an owned direct Git child waiting for stdin', {
  skip: process.platform !== 'darwin' || process.env.RIFTRI_BENCH_STAGE_SAMPLE_TEST !== '1',
}, async () => {
  const execPath = spawnSync('/usr/bin/git', ['--exec-path'], { encoding:'utf8' });
  assert.equal(execPath.status, 0, execPath.stderr);
  const gitBinary = fs.realpathSync(path.join(execPath.stdout.trim(), 'git'));
  const output = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-git-child-sampler-'));
  // This reads an open pipe; it does not write Git objects or touch a repository.
  const child = spawn(gitBinary, ['hash-object', '--stdin'], { stdio:['pipe', 'ignore', 'ignore'] });
  const exited = once(child, 'exit');
  try {
    await once(child, 'spawn');
    const sampled = await captureStageStack({ pid:process.pid, binary:process.execPath, gitBinary, output, key:'owned-git', index:0 });
    assert.equal(sampled.role, 'git-child', JSON.stringify(sampled));
    assert.equal(sampled.pid, child.pid);
    assert.equal(sampled.parentPid, process.pid);
    assert.equal(sampled.binary, gitBinary);
    assert.equal(sampled.code, 0, JSON.stringify(sampled));
    assert.equal(sampled.timedOut, false);
    assert.match(fs.readFileSync(sampled.file, 'utf8'), /Call graph:/);
  } finally {
    child.kill('SIGTERM');
    await exited;
    fs.rmSync(output, { recursive:true, force:true });
  }
});
