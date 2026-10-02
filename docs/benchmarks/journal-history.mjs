// BEFORE AFTER NEW_OUTPUT_DIR [CYCLES=100]. Uses only its disposable fixture.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { performance } from 'node:perf_hooks';

const [beforeArg, afterArg, outputArg, countArg = '100'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
const cycles = Number(countArg);
assert.ok(Number.isInteger(cycles) && cycles >= 1 && cycles <= 1000);
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
const requested = path.resolve(outputArg);
fs.mkdirSync(requested); // Deliberately refuse any existing output directory.
const root = fs.realpathSync(requested);
const repository = path.join(root, 'repository');
const state = path.join(root, 'state');
fs.mkdirSync(repository);
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT']) delete env[key];
function run(command, args) {
  const result = spawnSync(command, args, { cwd: repository, env, encoding: 'utf8', timeout: 60_000, maxBuffer: 20e6 });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  return result.stdout;
}
run('git', ['init', '--quiet']);
for (const [key, value] of [['user.name', 'Journal Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value]);
fs.writeFileSync(path.join(repository, 'tracked'), 'tracked\n');
run('git', ['add', '--all']);
run('git', ['commit', '--quiet', '-m', 'fixture']);
const riftri = (version, args) => run(binaries[version], [...args, '--state-dir', state, '--no-progress']);
const anchor = path.join(root, 'anchor');
riftri('before', ['worktree', 'add', '--detach', anchor, 'HEAD']);
for (let index = 0; index < cycles; index++) {
  const view = path.join(root, `view-${index}`);
  riftri('before', ['worktree', 'add', '--detach', view, 'HEAD']);
  riftri('before', ['worktree', 'remove', view]);
}
const result = { cycles, binaries, platform: process.platform, architecture: process.arch, load: os.loadavg(), cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
for (const command of ['status', 'gc']) {
  let expected;
  for (let round = 0; round < 4; round++) {
    for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
      const start = performance.now();
      const report = JSON.parse(riftri(version, [command, '--json']));
      const milliseconds = performance.now() - start;
      expected ??= report;
      assert.deepEqual(report, expected, 'Complete reports must remain identical');
      result.cases.push({ command, round, version, milliseconds }); save();
      console.log(`${command} ${round} ${version}: ${milliseconds.toFixed(2)} ms`);
    }
  }
}
const planned = JSON.parse(riftri('after', ['gc', '--json']));
const applied = JSON.parse(riftri('after', ['gc', '--apply', '--json']));
assert.equal(applied.retired_journals, planned.retirable_journals);
assert.equal(fs.readFileSync(path.join(anchor, 'tracked'), 'utf8'), 'tracked\n');
assert.ok(!run('git', ['-C', anchor, 'status', '--porcelain=v1']).trim());
riftri('after', ['worktree', 'remove', anchor]);
riftri('after', ['gc', '--apply']);
riftri('after', ['repair']);
const final = JSON.parse(riftri('after', ['status', '--json']));
assert.equal(final.operations.active_views, 0);
assert.equal(final.bases.length, 0);
assert.equal(final.diagnostic_issues.length, 0);
result.complete = true; save();
console.log(`Verified reports, retirement, live-view preservation, and final cleanup: ${root}`);
