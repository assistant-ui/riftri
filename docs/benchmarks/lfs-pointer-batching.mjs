// Unix/native-COW diagnostic. BEFORE AFTER NEW_OUTPUT_DIR [POINTERS=129].
// The deterministic clean stub validates bytes; it is not a real git-lfs speed benchmark.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

const [beforeArg, afterArg, outputArg, countArg = '129'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
const count = Number(countArg);
assert.ok(Number.isInteger(count) && count > 0 && count <= 1024);
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
fs.mkdirSync(path.resolve(outputArg));
const root = fs.realpathSync(path.resolve(outputArg));
const repository = path.join(root, 'repository'), state = path.join(root, 'state'), bin = path.join(root, 'bin');
for (const directory of [repository, bin]) fs.mkdirSync(directory);
const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT']) delete env[key];
function run(command, args, cwd = repository, extraEnv = {}) {
  const result = spawnSync(command, args, { cwd, env: { ...env, ...extraEnv }, encoding: 'utf8', timeout: 120_000, maxBuffer: 20e6 });
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained: ${root}`);
  return result.stdout;
}
run('git', ['init', '--quiet']);
for (const [key, value] of [['user.name', 'LFS Benchmark'], ['user.email', 'test@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value]);
const bytes = Buffer.alloc(64 * 1024, 0x5a);
const oid = createHash('sha256').update(bytes).digest('hex');
const pointer = `version https://git-lfs.github.com/spec/v1\noid sha256:${oid}\nsize ${bytes.length}\n`;
fs.writeFileSync(path.join(repository, '.gitattributes'), '*.bin filter=lfs diff=lfs merge=lfs -text\n');
const names = Array.from({ length: count }, (_, index) => `payload-${index}.bin`);
for (const name of names) fs.writeFileSync(path.join(repository, name), pointer);
run('git', ['add', '--all']);
run('git', ['commit', '--quiet', '-m', 'LFS pointers']);
const object = path.join(repository, '.git/lfs/objects', oid.slice(0, 2), oid.slice(2, 4), oid);
fs.mkdirSync(path.dirname(object), { recursive: true }); fs.writeFileSync(object, bytes);
env.RIFTRI_LFS_TEST_OBJECT = object;
fs.writeFileSync(path.join(bin, 'git-lfs'), `#!/bin/sh\ncase "$1" in\nversion) printf '%s\\n' 'git-lfs/3.7.0 (deterministic benchmark stub)' ;;\nclean) cmp -s - "$RIFTRI_LFS_TEST_OBJECT" || exit 1; printf '%s' '${pointer}' ;;\n*) exit 1 ;;\nesac\n`, { mode: 0o755 });
for (const [key, value] of [['filter.lfs.clean', 'git-lfs clean -- %f'], ['filter.lfs.smudge', 'git-lfs smudge -- %f'], ['filter.lfs.required', 'true']]) run('git', ['config', key, value]);
const riftri = (version, args, extraEnv) => run(binaries[version], [...args, '--state-dir', state, '--no-progress'], repository, extraEnv);
const anchor = path.join(root, 'anchor');
riftri('before', ['worktree', 'add', '--detach', anchor, 'HEAD']);
const result = { count, binaries, filter: 'deterministic validating clean stub; not real git-lfs', cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
for (let round = 0; round < 4; round++) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    const destination = path.join(root, `view-${round}-${version}`);
    const trace = path.join(root, `trace-${round}-${version}.jsonl`);
    const start = performance.now();
    const output = riftri(version, ['worktree', 'add', '--detach', destination, 'HEAD'], { GIT_TRACE2_EVENT: trace });
    const milliseconds = performance.now() - start;
    assert.match(output, /Base: reused/);
    for (const name of names) assert.deepEqual(fs.readFileSync(path.join(destination, name)), bytes);
    assert.equal(run('git', ['status', '--porcelain=v1', '-z'], destination), '');
    const starts = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse).filter(event => event.event === 'start');
    const bodyBatches = starts.filter(event => event.argv.includes('--batch')).length;
    if (version === 'after') assert.equal(bodyBatches, Math.ceil(count / 128));
    fs.writeFileSync(path.join(destination, names[0]), 'private edit');
    assert.deepEqual(fs.readFileSync(path.join(anchor, names[0])), bytes);
    assert.deepEqual(fs.readFileSync(object), bytes);
    fs.writeFileSync(path.join(destination, names[0]), bytes);
    riftri(version, ['worktree', 'remove', destination]);
    result.cases.push({ round, version, milliseconds, gitStarts: starts.length, bodyBatches, verifiedAndRemoved: true }); save();
    console.log(`${round} ${version}: ${milliseconds.toFixed(2)} ms; ${bodyBatches} pointer-body batches`);
  }
}
riftri('after', ['worktree', 'compact', anchor]);
for (const name of names) assert.deepEqual(fs.readFileSync(path.join(anchor, name)), bytes);
riftri('after', ['worktree', 'remove', anchor]);
riftri('after', ['gc', '--apply']);
riftri('after', ['repair']);
const final = JSON.parse(riftri('after', ['status', '--json']));
assert.equal(final.operations.active_views, 0);
assert.equal(final.bases.length, 0);
assert.equal(final.diagnostic_issues.length, 0);
result.complete = true; save();
console.log(`Verified all pointer bytes, clean status, isolation, compaction, and cleanup: ${root}`);
