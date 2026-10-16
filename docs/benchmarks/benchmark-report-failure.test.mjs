import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {timedProcess} from './timed-process.mjs';

const fault = fileURLToPath(new URL('./fixtures/report-save-failure.mjs', import.meta.url));
const binary = process.env.RIFTRI_BENCH_TEST_BINARY;
const runners = [
  {name: 'cold-lfs', field: 'failure', args: ['4', '4', '1', '90000']},
  {name: 'lifecycle-scaling', field: 'error', args: ['repair', '2', '4']},
];
const script = runner => fileURLToPath(new URL(`./${runner.name}.mjs`, import.meta.url));
const options = {skip: process.platform === 'win32', timeout: 45000};

async function failSetup(runner, operation) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-benchmark-receipt-'));
  const output = path.join(parent, 'evidence');
  // Fail the first Git command without creating a repository or any views.
  // The cancellation variant stays in the runner-owned group until stopped.
  fs.writeFileSync(path.join(parent, 'git'), operation === 'cancel'
    ? '#!/bin/sh\nkill -TERM "$PPID"\nexec sleep 30\n'
    : '#!/bin/sh\necho "intentional Git setup failure" >&2\nexit 37\n', {mode: 0o755});
  const result = await timedProcess(process.execPath, [
    '--import', fault, script(runner), process.execPath, process.execPath, output, ...runner.args,
  ], {env: {...process.env, PATH: `${parent}${path.delimiter}${process.env.PATH}`,
    BENCH_TEST_OUTPUT: output, BENCH_TEST_REPORT_FAULT: operation}, timeoutMs: 35000});
  assert.equal(result.timedOut, false, result.stderr);
  assert.equal(result.code, 1, result.stderr);
  return {parent, output, result};
}

for (const runner of runners) {
  test(`${runner.name}: setup failures persist an incomplete result`, options, async () => {
    const {parent, output, result} = await failSetup(runner, 'none');
    assert.match(result.stderr, /intentional Git setup failure/);
    const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
    assert.equal(report.complete, false);
    assert.match(report[runner.field], /intentional Git setup failure/);
    if (runner.name === 'lifecycle-scaling') assert.equal(report.qualified, false);
    fs.rmSync(parent, {recursive: true}); // No real repository/views were created.
  });

  for (const operation of ['write', 'rename', 'cancel']) {
    test(`${runner.name}: ${operation} receipt failure preserves the primary error and checkpoint`, options, async () => {
      const {parent, output, result} = await failSetup(runner, operation);
      const primary = operation === 'cancel' ? /SIGTERM|cancelled/ : /intentional Git setup failure/;
      assert.match(result.stderr, primary);
      assert.match(result.stderr, /Cannot retain failure receipt:.*ENOSPC/);
      if (operation === 'cancel') assert.match(result.stderr, /Cannot retain cancellation receipt:.*ENOSPC/);
      const fallback = result.stderr.split('\n').find(line => line.startsWith('Unpersisted benchmark failure: '));
      assert.ok(fallback, result.stderr);
      const failure = JSON.parse(fallback.slice('Unpersisted benchmark failure: '.length));
      assert.equal(failure.complete, false);
      assert.match(failure[runner.field], primary);
      assert.equal(failure.root, fs.realpathSync(output));
      if (runner.name === 'lifecycle-scaling') assert.equal(failure.qualified, false);
      // Partial writes must not corrupt the previous atomic checkpoint.
      const checkpoint = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
      assert.equal(checkpoint.complete, false);
      assert.equal(checkpoint[runner.field], undefined);
      assert.ok(fs.existsSync(path.join(output, 'repository')), 'failed fixtures must be retained');
      fs.rmSync(parent, {recursive: true}); // Only the fake command ran.
    });
  }

  test(`${runner.name}: failed final save cannot mark the run complete`, {
    ...options, skip: process.platform === 'win32' || !binary, timeout: 180000,
  }, async () => {
    const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-benchmark-completion-'));
    const output = path.join(parent, 'evidence');
    const result = await timedProcess(process.execPath, [
      '--import', fault, script(runner), binary, binary, output, ...runner.args,
    ], {env: {...process.env, BENCH_TEST_OUTPUT: output, BENCH_TEST_REPORT_FAULT: 'complete'}, timeoutMs: 170000});
    assert.equal(result.timedOut, false, result.stderr);
    assert.equal(result.code, 1, result.stderr);
    const report = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
    assert.equal(report.complete, false);
    assert.match(report[runner.field], /ENOSPC/);
    if (runner.name === 'lifecycle-scaling') assert.equal(report.qualified, false);
    assert.ok(report.samples.length > 0, 'the final-save fault must occur after measurement');
    assert.ok(!fs.readdirSync(output).some(name => name.startsWith('view-')), 'normal cleanup must have finished');
    fs.rmSync(parent, {recursive: true}); // Normal runner cleanup removed the real views and bases.
  });
}
