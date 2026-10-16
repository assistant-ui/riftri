// BEFORE AFTER NEW_OUTPUT [FILES=32] [BYTES=2097152] [ROUNDS=8] [BUDGET_MS=1800000]
// No fetching/installing/user Git configuration. Requires real git-lfs on PATH.
// Cold means a new immutable base, not an evicted OS page cache. Run separate
// same-binary controls and retain all samples before making a speedup claim.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {createProcessScope} from './process-scope.mjs';
import {distribution} from './latency-summary.mjs';

const [before, after, output, countArg = '32', bytesArg = '2097152', roundsArg = '8', budgetArg = '1800000'] = process.argv.slice(2);
assert.ok(before && after && output);
assert.notEqual(process.platform, 'win32', 'this manual runner requires Unix Git configuration paths');
const count = Number(countArg), bytes = Number(bytesArg), rounds = Number(roundsArg), budgetMs = Number(budgetArg);
assert.ok(Number.isInteger(count) && count >= 4 && count <= 128);
assert.ok(Number.isInteger(bytes) && bytes >= 4 && bytes <= 8 * 1024 ** 2 && bytes * count <= 128 * 1024 ** 2);
assert.ok(Number.isInteger(rounds) && rounds >= 1 && rounds <= 20);
const binaries = {before: fs.realpathSync(before), after: fs.realpathSync(after)};
const hash = value => createHash('sha256').update(value).digest('hex');
fs.mkdirSync(path.resolve(output));
const root = fs.realpathSync(output), repo = path.join(root, 'repository'), state = path.join(root, 'state');
fs.mkdirSync(repo);
const env = {...process.env};
for (const key of Object.keys(env)) if (key.startsWith('GIT_') || key.startsWith('RIFTRI_')) delete env[key];
Object.assign(env, {GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_SYSTEM: '/dev/null', GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0'});
const report = {schemaVersion: 1, runner: 'supervised-cold-lfs-v1', complete: false, speedupClaim: false,
  binaries, binarySha256: Object.fromEntries(Object.entries(binaries).map(([name, file]) => [name, hash(fs.readFileSync(file))])),
  runnerSha256: Object.fromEntries(['cold-lfs.mjs', 'process-scope.mjs', 'timed-process.mjs'].map(name => [name, hash(fs.readFileSync(new URL(name, import.meta.url)))])),
  count, bytes, rounds, budgetMs, platform: process.platform, release: os.release(), cpu: os.cpus()[0].model,
  availableParallelism: os.availableParallelism(),
  startedAt: new Date().toISOString(), commands: [], samples: [], refusals: []};
const save = () => {
  fs.writeFileSync(path.join(root, 'results.json.tmp'), JSON.stringify(report, null, 2));
  fs.renameSync(path.join(root, 'results.json.tmp'), path.join(root, 'results.json'));
};
save();
const scope = createProcessScope({budgetMs, onCancel(reason) { report.cancellation = reason; save(); }});
async function execute(command, args, {cwd = repo, extraEnv = {}, allowFailure = false, onStderr} = {}) {
  scope.check();
  const receipt = {index: report.commands.length, command, args, cwd, settled: false};
  report.commands.push(receipt); save();
  const log = fs.openSync(path.join(root, `command-${receipt.index}.log`), 'wx');
  let result;
  try {
    result = await scope.run(command, args, {cwd, env: {...env, ...extraEnv}, timeoutMs: 120000,
      onStderr(chunk) { fs.writeSync(log, chunk); onStderr?.(chunk); }});
  } catch (error) {
    // scope.run checks its deadline again before spawning. The attempt was
    // already retained above, so record this pre-admission rejection rather
    // than leaving a command that never started looking alive.
    Object.assign(receipt, {settled: true, started: false, cancelled: scope.signal.aborted,
      error: error.message});
    // The outer handler persists this receipt without masking the rejection.
    throw error;
  } finally { fs.closeSync(log); }
  Object.assign(receipt, {settled: true, started: result.started, code: result.code, signal: result.signal, timedOut: result.timedOut,
    cancelled: result.cancelled, error: result.error?.message});
  try {
    fs.writeFileSync(path.join(root, `command-${receipt.index}.stdout`), result.stdout);
    save();
  } catch (saveError) {
    if (!scope.signal.aborted) throw saveError;
    console.error('Cannot retain cancelled command receipt:', saveError);
  }
  // Preserve any available output, but do not replace cancellation with a
  // checkpoint error. The outer handler retains both errors and the fixture.
  scope.signal.throwIfAborted();
  assert.ifError(result.error);
  assert.equal(result.cancelled, false);
  assert.equal(result.timedOut, false);
  assert.notEqual(result.code, null);
  if (!allowFailure) assert.equal(result.code, 0, `${command} ${args.join(' ')}\n${result.stderr}`);
  return result;
}
const git = async (args, cwd = repo) => (await execute('git', args, {cwd})).stdout;
const riftri = (label, args, options) => execute(binaries[label], [...args, '--state-dir', state], options);
const payload = identity => { const value = Buffer.alloc(bytes, identity % 251); value.writeUInt32LE(identity); return value; };
try {
  report.git = (await git(['--version'])).trim();
  report.lfs = (await execute('git-lfs', ['version'])).stdout.trim();
  assert.match(report.lfs, /^git-lfs\/\d/);
  await git(['init', '--quiet', '-b', 'main']);
  for (const [key, value] of Object.entries({
    'user.name': 'Cold LFS fixture', 'user.email': 'fixture@example.invalid', 'commit.gpgSign': 'false',
    'core.autocrlf': 'false', 'core.fsmonitor': 'false', 'core.untrackedCache': 'false',
    'filter.lfs.clean': 'git-lfs clean -- %f', 'filter.lfs.smudge': 'git-lfs smudge -- %f',
    'filter.lfs.process': 'git-lfs filter-process', 'filter.lfs.required': 'true',
  })) await git(['config', '--local', key, value]);
  fs.writeFileSync(path.join(repo, '.gitattributes'), '*.bin filter=lfs diff=lfs merge=lfs -text\n');
  const assets = Array.from({length: count}, (_, index) => {
    scope.check();
    const identity = index % 7 === 0 ? 0 : index, name = `asset-${index}.bin`, data = payload(identity), oid = hash(data);
    fs.writeFileSync(path.join(repo, name), data);
    if (index === 0) fs.chmodSync(path.join(repo, name), 0o755);
    return {name, identity, oid, executable: index === 0,
      object: path.join(repo, '.git/lfs/objects', oid.slice(0, 2), oid.slice(2, 4), oid)};
  });
  await git(['add', '--all']); await git(['commit', '--quiet', '-m', 'test: cold LFS fixture']);
  assert.equal(await git(['remote']), '');
  report.commit = (await git(['rev-parse', 'HEAD'])).trim();
  report.tree = (await git(['rev-parse', 'HEAD^{tree}'])).trim();
  for (const asset of assets) {
    assert.equal(await git(['show', `HEAD:${asset.name}`]), `version https://git-lfs.github.com/spec/v1\noid sha256:${asset.oid}\nsize ${bytes}\n`);
    assert.equal(hash(fs.readFileSync(asset.object)), asset.oid);
  }
  const clean = async view => assert.equal(await git(['status', '--porcelain=v1', '-z', '--untracked-files=all'], view), '');
  const verify = directory => {
    for (const asset of assets) {
      scope.check();
      const file = path.join(directory, asset.name);
      assert.equal(hash(fs.readFileSync(file)), asset.oid);
      assert.equal(fs.statSync(file).mode & 0o111, asset.executable ? 0o111 : 0);
    }
  };
  async function emptyState() {
    await riftri('after', ['repair', '--no-progress']);
    const status = JSON.parse((await riftri('after', ['status', '--json', '--no-progress'])).stdout);
    assert.equal(status.operations.active_views, 0);
    assert.equal(status.bases.length, 0);
    assert.equal(status.diagnostic_issues.length, 0);
  }
  for (let round = 0; round < rounds; round++) {
    for (const label of round % 2 ? ['after', 'before'] : ['before', 'after']) {
      const view = path.join(root, `view-${round}-${label}`), trace = path.join(root, `git-${round}-${label}.jsonl`);
      const sample = {round, label, view, trace, load: os.loadavg(), settled: false, verified: false, phases: []};
      report.samples.push(sample); save();
      let partial = '';
      const start = performance.now();
      const result = await riftri(label, ['worktree', 'add', view, 'HEAD', '--detach', '--json'], {
        extraEnv: {GIT_TRACE2_EVENT: trace}, onStderr(chunk) {
          partial += chunk;
          const lines = partial.split('\n'); partial = lines.pop();
          for (const line of lines) if (line.startsWith('riftri: ')) sample.phases.push({milliseconds: performance.now() - start, line});
        },
      });
      sample.milliseconds = result.milliseconds; sample.settled = true; save();
      const receipt = JSON.parse(result.stdout);
      sample.reused = receipt.reused_base; sample.base = receipt.base_path;
      assert.equal(sample.reused, false, 'every timed sample must build a new immutable base');
      await clean(view); verify(view); verify(sample.base);
      assert.equal((await git(['rev-parse', 'HEAD'], view)).trim(), report.commit);
      for (const asset of assets) assert.equal(hash(fs.readFileSync(asset.object)), asset.oid);
      const first = assets[0];
      fs.writeFileSync(path.join(view, first.name), 'private edit');
      assert.notEqual(await git(['status', '--porcelain=v1', '-z', '--untracked-files=all'], view), '', 'real LFS must detect the first private edit');
      assert.equal(hash(fs.readFileSync(path.join(sample.base, first.name))), first.oid);
      assert.equal(hash(fs.readFileSync(first.object)), first.oid);
      fs.writeFileSync(path.join(view, first.name), payload(first.identity));
      await clean(view);
      await riftri(label, ['worktree', 'remove', view, '--no-progress']);
      assert.equal(fs.existsSync(view), false);
      await riftri(label, ['gc', '--apply', '--no-progress']);
      await emptyState();
      sample.verified = true; save();
      console.log(`${round} ${label}: ${sample.milliseconds.toFixed(3)} ms`);
    }
  }
  for (const failure of ['corrupt', 'missing']) {
    const victim = assets[1], held = `${victim.object}.held`, view = path.join(root, `refused-${failure}`);
    if (failure === 'missing') fs.renameSync(victim.object, held);
    else fs.writeFileSync(victim.object, Buffer.alloc(bytes, 0xff));
    const attempt = {failure, settled: false}; report.refusals.push(attempt); save();
    const result = await riftri('after', ['worktree', 'add', view, 'HEAD', '--detach', '--no-progress'], {allowFailure: true});
    assert.notEqual(result.code, 0);
    assert.match(result.stderr, failure === 'missing' ? /unavailable/ : /failed SHA-256 verification/);
    assert.equal(fs.existsSync(view), false);
    assert.ok(!(await git(['worktree', 'list', '--porcelain', '-z'])).includes(view));
    if (failure === 'missing') fs.renameSync(held, victim.object);
    else {
      assert.deepEqual(fs.readFileSync(victim.object), Buffer.alloc(bytes, 0xff));
      fs.writeFileSync(victim.object, payload(victim.identity));
    }
    await emptyState(); attempt.settled = true; save();
  }
  report.summary = Object.fromEntries(['before', 'after'].map(label => [label,
    distribution(report.samples.filter(sample => sample.label === label).map(sample => sample.milliseconds))]));
  scope.check(); report.complete = true; save();
} catch (error) {
  report.complete = false;
  report.failure = String(error?.stack ?? error);
  try { save(); } catch (saveError) {
    // The previous atomic checkpoint may be stale. Preserve the original
    // failure and recovery location in stderr without forcing any cleanup.
    console.error('Cannot retain failure receipt:', saveError);
    console.error('Unpersisted benchmark failure:', JSON.stringify({
      complete: false, failure: report.failure, cancellation: report.cancellation, root,
    }));
  }
  throw error;
} finally { await scope.close(); }
