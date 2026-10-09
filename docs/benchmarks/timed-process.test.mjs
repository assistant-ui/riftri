import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { timedProcess } from './timed-process.mjs';

test('timed process retains ordinary output, exit status and elapsed time', async () => {
  const chunks = [];
  const result = await timedProcess(process.execPath, ['-e',
    'process.stdout.write("out"); process.stderr.write("err"); process.exitCode = 7;',
  ], { timeoutMs: 10000, onStderr: (chunk) => chunks.push(chunk) });
  assert.equal(result.code, 7);
  assert.equal(result.signal, null);
  assert.equal(result.timedOut, false);
  assert.equal(result.stdout, 'out');
  assert.equal(result.stderr, 'err');
  assert.equal(chunks.join(''), 'err');
  assert.ok(result.milliseconds > 0);
});

test('spawn failure is an explicit result, not an unhandled event', async () => {
  const result = await timedProcess(path.join(os.tmpdir(), 'riftri-missing-command', 'missing'), [], { timeoutMs: 1000 });
  assert.equal(result.error.code, 'ENOENT');
  assert.equal(result.timedOut, false);
});

test('timeout also kills a signal-ignoring child after its wrapper closes', {
  skip: process.platform === 'win32' ? 'APFS benchmark uses POSIX process groups' : false,
  timeout: 10000,
}, async () => {
  const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-timeout-test-'));
  const heartbeat = path.join(fixture, 'heartbeat');
  // The child deliberately closes the wrapper's pipes and ignores TERM.
  // Both processes self-expire as a backstop if this regression ever returns.
  const childCode = `
    const fs = require('node:fs');
    process.on('SIGTERM', () => {});
    fs.writeFileSync(${JSON.stringify(heartbeat)}, 'started');
    setInterval(() => fs.appendFileSync(${JSON.stringify(heartbeat)}, '.'), 20);
    setTimeout(() => process.exit(0), 5000);
  `;
  const wrapperCode = `
    const { spawn } = require('node:child_process');
    spawn(process.execPath, ['-e', ${JSON.stringify(childCode)}], { stdio: 'ignore' });
    process.stdout.write('wrapper started');
    setTimeout(() => process.exit(0), 5000);
  `;
  const result = await timedProcess(process.execPath, ['-e', wrapperCode], {
    timeoutMs: 1500, killGraceMs: 100,
  });
  assert.equal(result.timedOut, true);
  assert.equal(result.signal, 'SIGTERM');
  assert.equal(result.stdout, 'wrapper started');
  assert.equal(result.error, null);
  const settled = fs.readFileSync(heartbeat, 'utf8');
  await new Promise((resolve) => setTimeout(resolve, 150));
  assert.equal(fs.readFileSync(heartbeat, 'utf8'), settled,
    'the child must stop before the timed-out result is returned');
  fs.rmSync(fixture, { recursive: true });
});
