// Unix/native COW. BEFORE AFTER NEW_OUTPUT_DIR [COLLECTIONS=100] [BASES=100].
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

const [beforeArg, afterArg, outputArg, historyArg = '100', basesArg = '100'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
const history = Number(historyArg), bases = Number(basesArg);
assert.ok([history, bases].every(n => Number.isInteger(n) && n > 0 && n <= 1000));
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
fs.mkdirSync(path.resolve(outputArg));
const root = fs.realpathSync(path.resolve(outputArg));
const repository = path.join(root, 'repository'), state = path.join(root, 'state');
fs.mkdirSync(repository);
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT']) delete env[key];
function run(command, args, cwd = repository) {
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8', timeout: 120000, maxBuffer: 30e6 });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  return result.stdout;
}
const git = args => run('git', args);
git(['init', '--quiet']);
for (const [key, value] of [['user.name', 'Diagnostics Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) git(['config', key, value]);
const riftri = (version, args) => run(binaries[version], [...args, '--state-dir', state, '--no-progress']);
for (let index = 0; index < history + bases; index++) {
  const bytes = `version-${index}\n`, view = path.join(root, `view-${index}`);
  fs.writeFileSync(path.join(repository, 'tracked'), bytes);
  git(['add', '--all']); git(['commit', '--quiet', '-m', `tree-${index}`]);
  riftri('before', ['worktree', 'add', '--detach', view, 'HEAD']);
  assert.equal(fs.readFileSync(path.join(view, 'tracked'), 'utf8'), bytes);
  assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  riftri('before', ['worktree', 'remove', view]);
  // Each GC invocation retires older completed journals. Collect the first
  // group in one batch to retain genuine multi-base collection history.
  if (index + 1 === history) riftri('before', ['gc', '--apply']);
  if ((index + 1) % 20 === 0) console.log(`Prepared ${index + 1}/${history + bases} distinct-tree lifecycles`);
}
assert.equal(fs.readdirSync(path.join(state, 'collections')).filter(name => name.endsWith('.json')).length, history);
const result = { binaries, history, bases, cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
let expected;
for (let round = 0; round < 6; round++) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    const start = performance.now();
    const report = JSON.parse(riftri(version, ['status', '--json']));
    const milliseconds = performance.now() - start;
    assert.equal(report.bases.length, bases);
    assert.equal(report.operations.active_views, 0);
    assert.equal(report.diagnostic_issues.length, 0);
    expected ??= report;
    assert.deepEqual(report, expected);
    result.cases.push({ round, version, milliseconds }); save();
    console.log(`${round} ${version}: ${milliseconds.toFixed(2)} ms`);
  }
}
const planned = JSON.parse(riftri('after', ['gc', '--json']));
const applied = JSON.parse(riftri('after', ['gc', '--apply', '--json']));
assert.equal(applied.retired_journals, planned.retirable_journals);
riftri('after', ['repair']);
const final = JSON.parse(riftri('after', ['status', '--json']));
assert.equal(final.bases.length, 0);
assert.equal(final.operations.active_views, 0);
assert.equal(final.diagnostic_issues.length, 0);
assert.equal(git(['status', '--porcelain=v1', '-z']), '');
result.complete = true; save();
console.log(`Verified real history, identical reports and final cleanup: ${root}`);
