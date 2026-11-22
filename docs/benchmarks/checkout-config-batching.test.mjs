import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

// Fixture setup runs Git with no developer configuration, as the harness
// does for its own Git. A global or XDG ignore file, hook, or injected
// setting could otherwise drop fixture files or fail the seed commit.
function fixtureGit(root, cwd, inherited = process.env) {
  const isolation = path.join(root, 'git-isolation');
  const config = path.join(isolation, 'empty-config');
  fs.mkdirSync(path.join(isolation, 'xdg'), { recursive: true });
  fs.writeFileSync(config, '');
  const env = {
    ...inherited,
    GIT_CONFIG_GLOBAL: config,
    GIT_CONFIG_SYSTEM: config,
    GIT_CONFIG_NOSYSTEM: '1',
    XDG_CONFIG_HOME: path.join(isolation, 'xdg'),
  };
  for (const key of Object.keys(env)) {
    if (/^GIT_CONFIG_(COUNT|PARAMETERS|KEY_\d+|VALUE_\d+)$/.test(key) || ['GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR'].includes(key)) delete env[key];
  }
  return args => {
    const result = spawnSync('git', args, { cwd, env, encoding: 'utf8' });
    assert.equal(result.status, 0, `git ${args.join(' ')}\n${result.error ?? ''}\n${result.stderr}\nFixture preserved at ${root}`);
    return result.stdout;
  };
}

test('fixture Git ignores developer config, ignore files and injected settings', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-fixture-git-test-'));
  const source = path.join(root, 'source');
  const xdg = path.join(root, 'developer-xdg');
  fs.mkdirSync(source);
  fs.mkdirSync(path.join(xdg, 'git'), { recursive: true });
  const ignore = path.join(root, 'developer-ignore');
  fs.writeFileSync(ignore, 'file.txt\n');
  fs.writeFileSync(path.join(xdg, 'git', 'ignore'), 'other.txt\n');
  const global = path.join(root, 'developer-config');
  fs.writeFileSync(global, `[core]\n\texcludesFile = ${ignore.replaceAll('\\', '/')}\n`);
  const git = fixtureGit(root, source, {
    ...process.env,
    GIT_CONFIG_GLOBAL: global,
    XDG_CONFIG_HOME: xdg,
    GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: 'commit.gpgSign',
    GIT_CONFIG_VALUE_0: 'true',
  });
  git(['init', '--quiet']);
  for (const [key, value] of [['user.name', 'Benchmark Test'], ['user.email', 'test@example.invalid']]) git(['config', key, value]);
  for (const name of ['file.txt', 'other.txt']) fs.writeFileSync(path.join(source, name), 'fixture\n');
  git(['add', '--all']);
  git(['commit', '--quiet', '-m', 'fixture']);
  assert.deepEqual(git(['ls-tree', '--name-only', 'HEAD']).trim().split('\n'), ['file.txt', 'other.txt']);
  fs.rmSync(root, { recursive: true });
});

test('a failed concurrent worker leaves a receipt after its successful peer finishes', {
  skip: !process.env.RIFTRI_BENCH_TEST_BINARY || process.platform === 'win32',
  timeout: 360_000,
}, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-failed-batch-test-'));
  const source = path.join(root, 'source');
  fs.mkdirSync(source);
  const git = fixtureGit(root, source);
  git(['init', '--quiet']);
  for (const [key, value] of [['user.name', 'Benchmark Test'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) git(['config', key, value]);
  fs.writeFileSync(path.join(source, 'file.txt'), 'fixture\n');
  git(['add', '--all']);
  git(['commit', '--quiet', '-m', 'fixture']);
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const events = path.join(root, 'worker-events');
  const wrapper = path.join(root, 'fault-wrapper');
  fs.writeFileSync(wrapper, `#!/usr/bin/env node
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
const args = process.argv.slice(2);
if (args.some(arg => arg.endsWith('parallel-0-explicit-before-0'))) {
  fs.appendFileSync(${JSON.stringify(events)}, 'failed\\n');
  console.error('injected worker failure');
  process.exit(42);
}
const slow = args.some(arg => arg.endsWith('parallel-0-explicit-before-1'));
const run = () => {
  const result = spawnSync(${JSON.stringify(binary)}, args, { stdio: 'inherit' });
  if (slow) fs.appendFileSync(${JSON.stringify(events)}, 'peer-finished\\n');
  process.exit(result.status ?? 1);
};
if (slow) setTimeout(run, 250); else run();
`, { mode: 0o755 });
  const output = path.join(root, 'results');
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('./checkout-config-batching.mjs', import.meta.url)), wrapper, wrapper, source, 'HEAD', output], {
    cwd: source, encoding: 'utf8', timeout: 330_000, maxBuffer: 10e6,
    env: { ...process.env, RIFTRI_BENCH_SINGLE_ROUNDS: '1', RIFTRI_BENCH_BATCH_ROUNDS: '1', RIFTRI_BENCH_WORKERS: '2' },
  });
  assert.equal(result.status, 1, `${result.stderr}\nFixture preserved at ${root}`);
  const receipt = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.equal(receipt.completedAt, undefined);
  assert.equal(receipt.batches.length, 0, 'failed batches are not successful timing samples');
  assert.equal(receipt.failedBatches.length, 1);
  const outcomes = receipt.failedBatches[0].outcomes;
  assert.deepEqual(outcomes.map(entry => entry.status), ['rejected', 'fulfilled']);
  assert.match(outcomes[0].reason.message, /injected worker failure/);
  assert.equal(outcomes[1].value.label, 'parallel-0-explicit-before-1');
  assert.ok(fs.existsSync(outcomes[1].value.destination));
  assert.deepEqual(fs.readFileSync(events, 'utf8').trim().split('\n').sort(), ['failed', 'peer-finished']);
  assert.match(fs.readFileSync(path.join(output, 'parallel-0-explicit-before-0.log'), 'utf8'), /injected worker failure/);
  // A failed batch deliberately preserves live views and an immutable base.
  // Retire them through Riftri before deleting the disposable test fixture.
  const repository = path.join(output, 'repository');
  const cleanup = args => {
    const removed = spawnSync(binary, args, { cwd: repository, encoding: 'utf8', timeout: 60_000 });
    assert.equal(removed.status, 0, `${removed.stderr}\nFixture preserved at ${root}`);
  };
  for (const record of receipt.cases) {
    if (fs.existsSync(record.destination)) cleanup(['worktree', 'remove', '--repository', repository, record.destination]);
  }
  cleanup(['gc', repository, '--apply']);
  fs.rmSync(root, { recursive: true });
});

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
  const git = fixtureGit(root, source);
  git(['init', '--quiet']);
  for (const [key, value] of [['user.name', 'Benchmark Test'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) git(['config', key, value]);
  for (let directory = 0; directory < 16; directory++) {
    const folder = path.join(source, `package-${directory}`);
    fs.mkdirSync(folder);
    for (let file = 0; file < 64; file++) fs.writeFileSync(path.join(folder, `file-${file}`), `${directory}:${file}\n`);
  }
  fs.writeFileSync(path.join(source, 'executable'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  fs.symlinkSync('package-0/file-0', path.join(source, 'link'));
  git(['add', '--all']);
  git(['commit', '--quiet', '-m', 'fixture']);
  const binary = path.resolve(process.env.RIFTRI_BENCH_TEST_BINARY);
  const output = path.join(root, 'results');
  run(process.execPath, [fileURLToPath(new URL('./checkout-config-batching.mjs', import.meta.url)), binary, binary, source, 'HEAD', output], source,
    { ...process.env, RIFTRI_BENCH_COLD_ROUNDS: '2', RIFTRI_BENCH_SINGLE_ROUNDS: '1', RIFTRI_BENCH_BATCH_ROUNDS: '1', RIFTRI_BENCH_WORKERS: '10' });
  const result = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
  assert.ok(result.completedAt);
  assert.equal(result.allViewsVerifiedAndRemoved, true);
  assert.equal(result.files, 1026);
  assert.equal(result.batches.length, 5);
  assert.equal(result.summaries.length, 5);
  assert.equal(result.coldRounds, 2);
  assert.equal(result.coldSummaries.length, 2);
  for (const summary of result.coldSummaries) assert.equal(summary.seconds.samples, 2);
  assert.deepEqual(result.cases.filter(record => record.label.startsWith('cold-')).map(record => record.version), ['before', 'after', 'after', 'before']);
  for (const summary of result.summaries) {
    assert.equal(summary.serialSeconds.samples, 1);
    assert.equal(summary.concurrentViewSeconds.samples, 10);
    assert.equal(summary.concurrentBatchSeconds.samples, 1);
  }
  assert.equal(git(['status', '--porcelain=v1']), '');
  // A fully successful harness has removed all views and collected all bases.
  fs.rmSync(root, { recursive: true });
});
