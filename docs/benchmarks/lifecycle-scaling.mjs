// BEFORE AFTER NEW_OUTPUT_DIR repair|gc [VIEWS=256] [PAIRS=8]
// Real linked-worktree lifecycle comparison; only owns its new fixture.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {createProcessScope} from './process-scope.mjs';
import {distribution} from './latency-summary.mjs';

const [beforeArg, afterArg, outputArg, command, countArg = '256', pairsArg = '8'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
assert.ok(['repair', 'gc'].includes(command));
const count = Number(countArg), pairs = Number(pairsArg);
assert.ok(Number.isSafeInteger(count) && count >= 2 && count <= 1000);
assert.ok(Number.isSafeInteger(pairs) && pairs >= 4 && pairs <= 30);
const binaries = {before: fs.realpathSync(beforeArg), after: fs.realpathSync(afterArg)};
fs.mkdirSync(path.resolve(outputArg)); // Refuse reuse or overwrite.
const root = fs.realpathSync(outputArg), repository = path.join(root, 'repository');
const state = path.join(root, 'state'), views = [];
fs.mkdirSync(repository);
const env = {...process.env};
for (const key of Object.keys(env)) if (key.startsWith('GIT_') || key.startsWith('RIFTRI_')) delete env[key];
Object.assign(env, {GIT_CONFIG_GLOBAL: os.devNull, GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0'});
const hash = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const result = {
  command, count, pairs, binaries,
  hashes: Object.fromEntries(Object.entries(binaries).map(([key, file]) => [key, hash(file)])),
  platform: process.platform, arch: process.arch, release: os.release(), node: process.version,
  startedAt: new Date().toISOString(), initialLoad: os.loadavg(),
  protocol: {controlMedianDriftMax: 0.05, controlP95DriftMax: 0.05,
    candidateMedianImprovementMin: 0.05, candidateP95RegressionMax: 0.05,
    lanes: ['control-before', 'candidate', 'control-after'],
    cache: 'warm; alternating order; no OS cache eviction; no retries or discarded samples'},
  samples: [], complete: false,
};
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save(); // Fix the protocol before setup and measurement.
const scope = createProcessScope({budgetMs: 25 * 60_000, onCancel: reason => {result.cancelled = reason; save();}});
async function run(executable, args) {
  const receipt = await scope.run(executable, args, {cwd: repository, env, timeoutMs: 60_000});
  assert.ok(receipt.code === 0 && !receipt.error && !receipt.timedOut && !receipt.cancelled,
    `${executable} ${args.join(' ')} failed: ${JSON.stringify(receipt)}; fixture retained at ${root}`);
  return receipt;
}
const git = async args => (await run('git', args)).stdout;
const riftri = (version, args) => run(binaries[version], [...args, '--state-dir', state, '--no-progress']);
try {
  result.git = (await git(['--version'])).trim();
  await git(['init', '--quiet']);
  for (const [key, value] of [['user.name', 'Lifecycle Benchmark'], ['user.email', 'test@example.invalid'],
    ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) await git(['config', key, value]);
  for (let n = 0; n < count; n++) {
    // GC needs distinct exact-tree bases. Repair needs many views of one base.
    if (n === 0 || command === 'gc') {
      fs.writeFileSync(path.join(repository, 'tracked'), `tracked ${n}\n`);
      await git(['add', '--all']);
      await git(['commit', '--quiet', '-m', `fixture ${n}`]);
    }
    const view = path.join(root, `view-${n}`);
    await riftri('before', ['worktree', 'add', '--detach', view, 'HEAD']);
    assert.equal(await git(['-C', view, 'status', '--porcelain=v1']), '');
    views.push(view);
    if ((n + 1) % 32 === 0) console.log(`created ${n + 1}/${count}`);
  }
  fs.writeFileSync(path.join(views[0], 'tracked'), 'private edit\n');
  fs.writeFileSync(path.join(views[0], 'untracked'), 'private untracked\n');
  const dirtyStatus = await git(['-C', views[0], 'status', '--porcelain=v1']);
  const registrations = await git(['worktree', 'list', '--porcelain', '-z']);
  assert.equal(registrations.split('worktree ').length - 1, count + 1);
  const check = report => {
    if (command === 'repair') {
      assert.equal(report.active, count);
      assert.equal(report.retired_adds, 0);
      assert.deepEqual(report.errors, []);
      assert.deepEqual(report.relocated_worktrees, []);
    } else {
      assert.equal(report.candidates.length, 0);
      assert.equal(report.skipped_protected.length, count);
    }
  };
  let expected;
  for (const version of ['before', 'after']) {
    const report = JSON.parse((await riftri(version, [command, '--json'])).stdout);
    check(report); expected ??= report; assert.deepEqual(report, expected);
  }
  for (const lane of result.protocol.lanes) {
    for (let round = 0; round < pairs; round++) {
      for (const label of round % 2 ? ['after', 'before'] : ['before', 'after']) {
        const version = lane === 'candidate' ? label : 'before';
        const receipt = await riftri(version, [command, '--json']);
        const report = JSON.parse(receipt.stdout);
        check(report); assert.deepEqual(report, expected, 'Complete reports must be identical');
        const name = `${lane}-${round}-${label}.json`;
        fs.writeFileSync(path.join(root, name), JSON.stringify(receipt, null, 2));
        result.samples.push({lane, round, label, version, milliseconds: receipt.milliseconds, receipt: name});
        save();
      }
    }
    console.log(`measured ${lane}`);
  }
  result.summary = Object.fromEntries(result.protocol.lanes.map(lane => [lane,
    Object.fromEntries(['before', 'after'].map(label => [label, distribution(result.samples
      .filter(s => s.lane === lane && s.label === label).map(s => s.milliseconds))]))]));
  const drift = (lane, metric) => Math.abs(result.summary[lane].after[metric] / result.summary[lane].before[metric] - 1);
  result.controlsPassed = ['control-before', 'control-after'].every(lane => drift(lane, 'p50') <= 0.05 && drift(lane, 'p95') <= 0.05);
  const candidate = result.summary.candidate;
  result.medianImprovement = 1 - candidate.after.p50 / candidate.before.p50;
  result.qualified = result.controlsPassed && result.medianImprovement >= 0.05 && candidate.after.p95 <= candidate.before.p95 * 1.05;
  assert.equal(await git(['worktree', 'list', '--porcelain', '-z']), registrations);
  assert.equal(await git(['-C', views[0], 'status', '--porcelain=v1']), dirtyStatus);
  assert.equal(fs.readFileSync(path.join(views[0], 'tracked'), 'utf8'), 'private edit\n');
  assert.equal(fs.readFileSync(path.join(views[0], 'untracked'), 'utf8'), 'private untracked\n');
  for (const [n, view] of views.entries()) {
    if (n !== 0) {
      assert.equal(await git(['-C', view, 'status', '--porcelain=v1']), '');
      assert.equal(fs.readFileSync(path.join(view, 'tracked'), 'utf8'), `tracked ${command === 'gc' ? n : 0}\n`);
    }
    // Explicitly force only our deliberately dirtied disposable view.
    await riftri('after', ['worktree', 'remove', ...(n === 0 ? ['--force'] : []), view]);
    if ((n + 1) % 32 === 0) console.log(`removed ${n + 1}/${count}`);
  }
  await riftri('after', ['gc', '--apply']);
  await riftri('after', ['repair']);
  const final = JSON.parse((await riftri('after', ['status', '--json'])).stdout);
  assert.equal(final.operations.active_views, 0);
  assert.deepEqual(final.bases, []);
  assert.deepEqual(final.diagnostic_issues, []);
  scope.check();
  result.complete = true; result.finishedAt = new Date().toISOString(); save();
  console.log(JSON.stringify({root, qualified: result.qualified, summary: result.summary}, null, 2));
} catch (error) {
  result.error = String(error.stack ?? error); save(); throw error;
} finally { await scope.close(); }
