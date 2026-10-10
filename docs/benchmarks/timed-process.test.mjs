import assert from 'node:assert/strict';
import test from 'node:test';
import {fork} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {createProcessScope} from './process-scope.mjs';
import {timedProcess} from './timed-process.mjs';

test('cancellation stops a live owned process group before its per-command timeout', {skip: process.platform === 'win32'}, async () => {
  const controller = new AbortController();
  const result = await timedProcess(process.execPath, ['-e', `
    process.on('SIGTERM', () => {});
    process.stderr.write('ready\\n');
    setInterval(() => {}, 1000);
  `], {signal: controller.signal, timeoutMs: 3000, killGraceMs: 100,
    onStderr() { controller.abort('test cancellation'); }});
  assert.equal(result.cancelled, true);
  assert.equal(result.timedOut, false);
  assert.ok(result.milliseconds < 2000, JSON.stringify(result));
  assert.equal(result.signal, 'SIGKILL');
});

const posix = {skip: process.platform === 'win32', timeout: 15000};
const alive = pid => {
  try { process.kill(pid, 0); return true; } catch (error) { if (error.code === 'ESRCH') return false; throw error; }
};
const kill = pid => { try { process.kill(pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; } };

for (const cause of ['SIGTERM', 'SIGINT', 'budget']) test(`owner ${cause} settles four process groups including surviving descendants`, posix, async t => {
  const owner = fork(new URL('./fixtures/process-owner.mjs', import.meta.url), [cause === 'budget' ? '2000' : '10000'],
    {stdio: ['ignore', 'ignore', 'pipe', 'ipc']});
  const ready = [], messages = [];
  let stderr = '';
  owner.stderr.on('data', chunk => { stderr += chunk; });
  t.after(() => { for (const p of ready) { kill(-p.wrapper); } owner.kill('SIGKILL'); });
  owner.on('message', message => {
    messages.push(message);
    if (message.ready) {
      ready.push(message.ready);
      if (ready.length === 4 && cause !== 'budget') owner.kill(cause);
    }
  });
  const outcome = await new Promise((resolve, reject) => {
    owner.on('error', reject);
    owner.on('close', (code, signal) => resolve({code, signal}));
  });
  assert.equal(ready.length, 4, stderr);
  assert.deepEqual(outcome, {code: 1, signal: null}, stderr);
  const receipt = messages.find(m => m.outcomes);
  assert.equal(receipt.reason, cause === 'budget' ? 'benchmark budget exhausted' : cause);
  assert.equal(receipt.refusedNextCommand, true);
  assert.equal(receipt.listenersRestored, true);
  assert.equal(receipt.outcomes.length, 4);
  for (const worker of receipt.outcomes) {
    assert.equal(worker.cancelled, true);
    assert.equal(worker.timedOut, false);
    assert.equal(worker.error, null);
  }
  // Allow the OS reaper to collect exited descendants before checking PIDs.
  for (let n = 0; n < 100 && ready.some(p => alive(p.wrapper) || alive(p.descendant)); n++) await delay(10);
  for (const p of ready) {
    assert.equal(alive(p.wrapper), false, `wrapper ${p.wrapper} survived`);
    assert.equal(alive(p.descendant), false, `descendant ${p.descendant} survived`);
  }
});

test('normal exit, binary input/output, launch failures and output overflow settle', async () => {
  const bytes = Buffer.from([0, 10, 255, 128]);
  const normal = await timedProcess(process.execPath, ['-e', 'process.stdin.pipe(process.stdout)'], {input: bytes, encoding: null});
  assert.deepEqual(normal.stdout, bytes);
  assert.equal(normal.code, 0);
  assert.equal(normal.cancelled, false);
  assert.equal(normal.timedOut, false);
  const missing = await timedProcess('/riftri-test-missing-command', [], {timeoutMs: 1000});
  assert.equal(missing.error.code, 'ENOENT');
  assert.equal(missing.started, false);
  const overflow = await timedProcess(process.execPath, ['-e', 'process.stdout.write("x".repeat(10000)); setInterval(() => {}, 1000)'],
    {maxBuffer: 100, killGraceMs: 10});
  assert.equal(overflow.error.code, 'MAX_BUFFER');
  assert.equal(overflow.stdout, '');
});

test('pre-cancelled work never starts, and a command deadline is not an outer cancellation', posix, async () => {
  const controller = new AbortController(); controller.abort();
  const cancelled = await timedProcess('/riftri-test-missing-command', [], {signal: controller.signal});
  assert.equal(cancelled.started, false);
  assert.equal(cancelled.cancelled, true);
  assert.equal(cancelled.error, null);
  const timeout = await timedProcess(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {timeoutMs: 100, killGraceMs: 10});
  assert.equal(timeout.timedOut, true);
  assert.equal(timeout.cancelled, false);
});

test('closing an owner with unfinished work stops it and refuses later commands', posix, async () => {
  const scope = createProcessScope({budgetMs: 10000, killGraceMs: 10});
  const pending = scope.run(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);
  await scope.close();
  assert.equal((await pending).cancelled, true);
  await assert.rejects(scope.run(process.execPath, ['-e', '']), /scope is closed/);
});

test('invalid timers cannot silently turn a long budget into a one-millisecond timer', async () => {
  for (const budgetMs of [0, -1, NaN, Infinity, 2147483648]) assert.throws(() => createProcessScope({budgetMs}));
  for (const timeoutMs of [0, -1, NaN, Infinity, 2147483648]) {
    await assert.rejects(timedProcess(process.execPath, [], {timeoutMs}), /timer durations/);
  }
});
