// Native COW: BEFORE AFTER NEW_OUTPUT_DIR [DIRECTORIES=20000] [CONES=1000] [ROUNDS=3].
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

const [before, after, output, totalArg = '20000', conesArg = '1000', roundsArg = '3'] = process.argv.slice(2);
assert.ok(before && after && output);
const total = Number(totalArg), cones = Number(conesArg), rounds = Number(roundsArg);
assert.ok(Number.isInteger(total) && total >= 2 && total <= 100000);
assert.ok(Number.isInteger(cones) && cones >= 1 && cones < total && cones <= 5000);
assert.ok(Number.isInteger(rounds) && rounds >= 1 && rounds <= 20);
const binaries = { before: path.resolve(before), after: path.resolve(after) };
fs.mkdirSync(path.resolve(output));
const root = fs.realpathSync(path.resolve(output));
const repository = path.join(root, 'repository'), state = path.join(root, 'state');
fs.mkdirSync(repository);
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT']) delete env[key];
function run(command, args, cwd = repository) {
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8', timeout: 120000, maxBuffer: 20e6 });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  return result.stdout;
}
const git = args => run('git', args);
git(['init', '--quiet']);
for (const [key, value] of [['user.name', 'Sparse Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) git(['config', key, value]);
const names = Array.from({ length: total }, (_, i) => `pkg-${String(i).padStart(5, '0')}`);
fs.writeFileSync(path.join(repository, 'root.txt'), 'root\n');
for (const name of names) {
  fs.mkdirSync(path.join(repository, name));
  fs.writeFileSync(path.join(repository, name, 'file.txt'), `${name}\n`);
}
git(['add', '--all']); git(['commit', '--quiet', '-m', 'sparse scaling fixture']);
const selected = names.slice(-cones), excluded = names.slice(0, -cones);
const selectionArgs = selected.flatMap(name => ['--sparse-dir', name]);
const riftri = (version, args) => run(binaries[version], [...args, '--state-dir', state, '--no-progress']);
function verify(view) {
  assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  assert.equal(fs.readFileSync(path.join(view, 'root.txt'), 'utf8'), 'root\n');
  for (const name of selected) assert.equal(fs.readFileSync(path.join(view, name, 'file.txt'), 'utf8'), `${name}\n`);
  for (const name of excluded) assert.equal(fs.existsSync(path.join(view, name)), false);
  assert.deepEqual(run('git', ['sparse-checkout', 'list'], view).trim().split('\n'), selected);
}
const result = { total, cones, rounds, binaries, cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
const anchor = path.join(root, 'anchor');
riftri('before', ['worktree', 'add', '--detach', anchor, 'HEAD', ...selectionArgs]); verify(anchor);
for (let round = 0; round < rounds; round++) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    const destination = path.join(root, `view-${round}-${version}`);
    const start = performance.now();
    const receipt = riftri(version, ['worktree', 'add', '--detach', destination, 'HEAD', ...selectionArgs]);
    const milliseconds = performance.now() - start;
    assert.match(receipt, /Base: reused/); verify(destination);
    const privateFile = path.join(destination, selected[0], 'file.txt');
    fs.writeFileSync(privateFile, 'private\n');
    assert.equal(fs.readFileSync(path.join(anchor, selected[0], 'file.txt'), 'utf8'), `${selected[0]}\n`);
    fs.writeFileSync(privateFile, `${selected[0]}\n`);
    riftri(version, ['worktree', 'remove', destination]);
    result.cases.push({ round, version, milliseconds }); save();
    console.log(`${round} ${version}: ${milliseconds.toFixed(2)} ms`);
  }
}
riftri('after', ['worktree', 'compact', anchor]); verify(anchor);
riftri('after', ['worktree', 'remove', anchor]);
riftri('after', ['gc', '--apply']); riftri('after', ['repair']);
const final = JSON.parse(riftri('after', ['status', '--json']));
assert.equal(final.operations.active_views, 0);
assert.equal(final.bases.length, 0);
assert.equal(final.diagnostic_issues.length, 0);
assert.equal(git(['status', '--porcelain=v1', '-z']), '');
result.complete = true; save();
console.log(`Verified sparse contents, base reuse, isolation, compaction and cleanup: ${root}`);
