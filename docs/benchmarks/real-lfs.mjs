// Unix/native COW. BEFORE AFTER NEW_OUTPUT_DIR [COUNTS=127,128,129] [ROUNDS=3].
// Requires real git-lfs on PATH; never installs it or configures the user's Git.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

const [beforeArg, afterArg, outputArg, countsArg = '127,128,129', roundsArg = '3'] = process.argv.slice(2);
assert.ok(beforeArg && afterArg && outputArg);
const counts = countsArg.split(',').map(Number), rounds = Number(roundsArg);
assert.ok(counts.length > 0 && counts.every(n => Number.isInteger(n) && n >= 2 && n <= 1024));
assert.equal(new Set(counts).size, counts.length);
assert.ok(Number.isInteger(rounds) && rounds >= 1 && rounds <= 20);
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
fs.mkdirSync(path.resolve(outputArg));
const root = fs.realpathSync(path.resolve(outputArg));
const env = { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0' };
for (const key of ['GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT', 'GIT_TRACE2_EVENT', 'GIT_LFS_SKIP_SMUDGE']) delete env[key];
function execute(command, args, cwd, extraEnv = {}) {
  return spawnSync(command, args, { cwd, env: { ...env, ...extraEnv }, encoding: 'utf8', timeout: 120000, maxBuffer: 20e6 });
}
function run(command, args, cwd = root, extraEnv = {}) {
  const result = execute(command, args, cwd, extraEnv);
  assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}\nFixture retained at ${root}`);
  return result.stdout;
}
const lfsVersion = run('git-lfs', ['version']).trim();
assert.match(lfsVersion, /^git-lfs\/\d/);
const result = { binaries, lfsVersion, counts, rounds, cases: [], complete: false };
const save = () => fs.writeFileSync(path.join(root, 'results.json'), JSON.stringify(result, null, 2));
save();
for (const count of counts) {
  const folder = path.join(root, String(count)); fs.mkdirSync(folder);
  const repository = path.join(folder, 'repository'), state = path.join(folder, 'state');
  fs.mkdirSync(repository);
  const git = args => run('git', args, repository);
  git(['init', '--quiet']);
  for (const [key, value] of [
    ['user.name', 'Real LFS Benchmark'], ['user.email', 'test@example.invalid'],
    ['core.autocrlf', 'false'], ['commit.gpgSign', 'false'],
    ['filter.lfs.clean', 'git-lfs clean -- %f'], ['filter.lfs.smudge', 'git-lfs smudge -- %f'],
    ['filter.lfs.process', 'git-lfs filter-process'], ['filter.lfs.required', 'true'],
  ]) git(['config', key, value]);
  fs.writeFileSync(path.join(repository, '.gitattributes'), '*.bin filter=lfs diff=lfs merge=lfs -text\n');
  const payloads = Array.from({ length: count }, (_, index) => {
    // Different contents and sizes, with repeated objects as well. Git LFS
    // keeps empty files as ordinary empty blobs, so use nonempty payloads.
    const identity = index % 7 === 0 ? 0 : index;
    const bytes = Buffer.alloc([17, 4096, 65536, 262144][identity % 4], identity % 251);
    bytes.writeUInt32LE(identity);
    const oid = createHash('sha256').update(bytes).digest('hex');
    const name = `payload-${index}.bin`;
    fs.writeFileSync(path.join(repository, name), bytes);
    return { name, bytes, oid, object: path.join(repository, '.git/lfs/objects', oid.slice(0, 2), oid.slice(2, 4), oid) };
  });
  git(['add', '--all']); git(['commit', '--quiet', '-m', 'real LFS fixture']);
  assert.equal(git(['remote']), '', 'The fixture must have no remote to fetch from');
  for (const payload of payloads) {
    assert.equal(git(['show', `HEAD:${payload.name}`]), `version https://git-lfs.github.com/spec/v1\noid sha256:${payload.oid}\nsize ${payload.bytes.length}\n`);
    assert.deepEqual(fs.readFileSync(payload.object), payload.bytes);
  }
  const riftriArgs = args => [...args, '--state-dir', state, '--no-progress'];
  const riftri = (version, args, extraEnv) => run(binaries[version], riftriArgs(args), repository, extraEnv);
  const verify = view => {
    for (const payload of payloads) assert.deepEqual(fs.readFileSync(path.join(view, payload.name)), payload.bytes);
    assert.equal(run('git', ['status', '--porcelain=v1', '-z'], view), '');
  };
  // Refusal must preserve the local store and leave no managed view. Corrupt
  // bytes have the right length so the full hash check, not size alone, fires.
  const victim = payloads[1];
  for (const failure of ['missing', 'corrupt']) {
    const held = `${victim.object}.held`;
    if (failure === 'missing') fs.renameSync(victim.object, held);
    else fs.writeFileSync(victim.object, Buffer.alloc(victim.bytes.length, 0xff));
    const refused = path.join(folder, `refused-${failure}`);
    const output = execute(binaries.after, riftriArgs(['worktree', 'add', '--detach', refused, 'HEAD']), repository);
    assert.notEqual(output.status, null, `Refusal process did not finish: ${output.error ?? output.signal}`);
    assert.notEqual(output.status, 0, `Accepted ${failure} local LFS data`);
    assert.match(output.stderr, failure === 'missing' ? /unavailable/ : /failed SHA-256 verification/);
    assert.equal(fs.existsSync(refused), false);
    if (failure === 'missing') {
      assert.ok(fs.existsSync(held)); fs.renameSync(held, victim.object);
    } else {
      assert.deepEqual(fs.readFileSync(victim.object), Buffer.alloc(victim.bytes.length, 0xff));
      fs.writeFileSync(victim.object, victim.bytes);
    }
    assert.ok(!git(['worktree', 'list', '--porcelain', '-z']).includes(refused));
    if (fs.existsSync(state)) {
      riftri('after', ['repair']);
      assert.equal(JSON.parse(riftri('after', ['status', '--json'])).operations.active_views, 0);
    }
  }
  const anchor = path.join(folder, 'anchor');
  riftri('before', ['worktree', 'add', '--detach', anchor, 'HEAD']); verify(anchor);
  for (let round = 0; round < rounds; round++) {
    for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
      const destination = path.join(folder, `view-${round}-${version}`);
      const trace = path.join(folder, `trace-${round}-${version}.jsonl`);
      const start = performance.now();
      const output = riftri(version, ['worktree', 'add', '--detach', destination, 'HEAD'], { GIT_TRACE2_EVENT: trace });
      const milliseconds = performance.now() - start;
      assert.match(output, /Base: reused/); verify(destination);
      const starts = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse).filter(event => event.event === 'start');
      const batches = starts.filter(event => event.argv.includes('cat-file') && event.argv.includes('--batch')).length;
      if (version === 'after') assert.equal(batches, 1);
      fs.writeFileSync(path.join(destination, victim.name), 'private edit');
      assert.deepEqual(fs.readFileSync(path.join(anchor, victim.name)), victim.bytes);
      assert.deepEqual(fs.readFileSync(victim.object), victim.bytes);
      fs.writeFileSync(path.join(destination, victim.name), victim.bytes);
      riftri(version, ['worktree', 'remove', destination]);
      result.cases.push({ count, round, version, milliseconds, gitStarts: starts.length, batches }); save();
      console.log(`${count} round ${round} ${version}: ${milliseconds.toFixed(2)} ms; ${batches} batches`);
    }
  }
  riftri('after', ['worktree', 'compact', anchor]); verify(anchor);
  riftri('after', ['worktree', 'remove', anchor]);
  riftri('after', ['gc', '--apply']); riftri('after', ['repair']);
  const final = JSON.parse(riftri('after', ['status', '--json']));
  assert.equal(final.operations.active_views, 0);
  assert.equal(final.bases.length, 0);
  assert.equal(final.diagnostic_issues.length, 0);
}
result.complete = true; save();
console.log(`Real-LFS boundaries, refusal, isolation, compaction, and cleanup verified: ${root}`);
