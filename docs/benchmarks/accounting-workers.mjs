// BEFORE AFTER NEW_OUTPUT_DIR [FILES_PER_HEAVY_VIEW=20000]. Unix/native COW.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { performance } from 'node:perf_hooks';

const [beforeArg, afterArg, outputArg, countArg = '20000'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
const count = Number(countArg);
assert.ok(Number.isInteger(count) && count > 0 && count <= 100000);
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
fs.mkdirSync(path.resolve(outputArg)); // Never reuse an existing fixture.
const root = fs.realpathSync(path.resolve(outputArg));
const repository = path.join(root, 'repository'), state = path.join(root, 'state');
fs.mkdirSync(repository);
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT']) delete env[key];
function run(command, args, cwd = repository) {
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8', timeout: 120000, maxBuffer: 20e6 });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  return result.stdout;
}
run('git', ['init', '--quiet']);
for (const [key, value] of [['user.name', 'Accounting Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value]);
fs.writeFileSync(path.join(repository, 'tracked'), 'tracked\n');
run('git', ['add', '--all']); run('git', ['commit', '--quiet', '-m', 'fixture']);
const riftri = (version, args) => run(binaries[version], [...args, '--state-dir', state, '--no-progress']);
const views = [];
for (let index = 0; index < 8; index++) {
  const view = path.join(root, `view-${index}`);
  riftri('before', ['worktree', 'add', '--detach', view, 'HEAD']);
  assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  views.push(view);
}
// Journal order determines scheduling. Use the first two actual journal
// destinations, not assumptions about IDs or filesystem directory order.
const journalDirectory = path.join(state, 'operations');
const heavyViews = fs.readdirSync(journalDirectory).filter(name => name.endsWith('.json')).sort()
  .map(name => JSON.parse(fs.readFileSync(path.join(journalDirectory, name))))
  .slice(0, 2).map(journal => {
    assert.equal(journal.destination.encoding, 'unix-bytes');
    return Buffer.from(journal.destination.units).toString();
  });
assert.equal(heavyViews.length, 2);
for (const view of heavyViews) {
  assert.ok(views.includes(view));
  const scratch = path.join(view, 'benchmark-untracked'); fs.mkdirSync(scratch);
  for (let index = 0; index < count; index++) fs.writeFileSync(path.join(scratch, `${index}`), 'x');
}
const result = { binaries, count, heavyViews, cpus: os.availableParallelism(), cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
let expected;
for (let round = 0; round < 6; round++) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    const start = performance.now();
    const report = JSON.parse(riftri(version, ['status', '--json']));
    const milliseconds = performance.now() - start;
    assert.equal(report.operations.active_views, 8);
    expected ??= report;
    assert.deepEqual(report, expected, 'Complete accounting and diagnostics must match');
    result.cases.push({ round, version, milliseconds }); save();
    console.log(`${round} ${version}: ${milliseconds.toFixed(2)} ms`);
  }
}
for (const view of heavyViews) fs.rmSync(path.join(view, 'benchmark-untracked'), { recursive: true });
for (const view of views) {
  assert.equal(fs.readFileSync(path.join(view, 'tracked'), 'utf8'), 'tracked\n');
  assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  riftri('after', ['worktree', 'remove', view]);
}
riftri('after', ['gc', '--apply']); riftri('after', ['repair']);
const final = JSON.parse(riftri('after', ['status', '--json']));
assert.equal(final.operations.active_views, 0);
assert.equal(final.bases.length, 0);
assert.equal(final.diagnostic_issues.length, 0);
result.complete = true; save();
console.log(`Verified identical reports, untouched tracked files, clean removal, and empty state: ${root}`);
