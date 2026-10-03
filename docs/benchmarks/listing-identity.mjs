// BEFORE AFTER NEW_OUTPUT_DIR. Read-only paired listings after disposable setup.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { performance } from 'node:perf_hooks';
const [before, after, output] = process.argv.slice(2);
assert.ok(before && after && output);
const binaries = { before: path.resolve(before), after: path.resolve(after) };
fs.mkdirSync(path.resolve(output));
const root = fs.realpathSync(path.resolve(output)), shared = path.join(root, 'shared');
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };
for (const key of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG_COUNT', 'GIT_CONFIG', 'GIT_TRACE2_EVENT', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT']) delete env[key];
function run(cmd, args, cwd, extra = {}) {
  const r = spawnSync(cmd, args, { cwd, env: { ...env, ...extra }, encoding: 'utf8', timeout: 120000, maxBuffer: 20e6 });
  assert.equal(r.status, 0, `${r.error ?? ''}\n${r.stderr}\nFixture retained at ${root}`);
  return r.stdout;
}
const repositories = [];
for (const name of ['first', 'second']) {
  const repository = path.join(root, name), view = path.join(root, `${name}-view`);
  fs.mkdirSync(repository);
  run('git', ['init', '--quiet'], repository);
  for (const [key, value] of [['user.name', 'Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value], repository);
  fs.writeFileSync(path.join(repository, 'tracked'), 'tracked\n');
  run('git', ['add', '--all'], repository); run('git', ['commit', '--quiet', '-m', 'fixture'], repository);
  run(binaries.before, ['worktree', 'add', '--detach', view, 'HEAD', '--state-dir', shared, '--no-progress'], repository);
  assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  repositories.push({ repository, view });
}
const result = { binaries, cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
let expected;
for (let round = 0; round < 6; round++) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    const trace = path.join(root, `trace-${round}-${version}.jsonl`);
    const start = performance.now();
    const report = JSON.parse(run(binaries[version], ['worktree', 'list', '--all-states', '--json'], repositories[0].repository, { GIT_TRACE2_EVENT: trace }));
    const milliseconds = performance.now() - start;
    expected ??= report; assert.deepEqual(report, expected);
    assert.equal(report.worktrees.length, 1); assert.equal(report.diagnostic_issues.length, 0);
    assert.equal(report.worktrees[0].path, repositories[0].view);
    const starts = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse).filter(e => e.event === 'start');
    const inspections = starts.filter(e => e.argv.includes('--git-common-dir')).length;
    if (version === 'after') assert.equal(inspections, 2);
    result.cases.push({ round, version, milliseconds, inspections, gitStarts: starts.length }); save();
    console.log(`${round} ${version}: ${milliseconds.toFixed(2)} ms; ${inspections} inspections; ${starts.length} starts`);
  }
}
// A linked-worktree query must still inspect the distinct recorded main root.
const linked = versions => versions.map(v => JSON.parse(run(binaries[v], ['worktree', 'list', '--all-states', '--json'], repositories[0].view)));
const [oldLinked, newLinked] = linked(['before', 'after']); assert.deepEqual(oldLinked, newLinked);
for (const { repository, view } of repositories) run(binaries.after, ['worktree', 'remove', view, '--state-dir', shared, '--no-progress'], repository);
run(binaries.after, ['gc', '--apply', '--state-dir', shared, '--no-progress'], repositories[0].repository);
run(binaries.after, ['repair', '--state-dir', shared, '--no-progress'], repositories[0].repository);
const final = JSON.parse(run(binaries.after, ['status', '--json', '--state-dir', shared], repositories[0].repository));
assert.equal(final.operations.active_views, 0); assert.equal(final.bases.length, 0); assert.equal(final.diagnostic_issues.length, 0);
result.complete = true; save();
console.log(`Verified full report parity, filtering, linked-worktree query and cleanup: ${root}`);
