import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { timedProcess } from './timed-process.mjs';
import { createStageSampler } from './stage-sampler.mjs';

// Manual macOS/APFS comparison. Never install the candidate or change the
// source repository; archive its committed HEAD into a new disposable fixture.
// Usage: node apfs-read-ahead.mjs BASELINE NEW_OUTPUT [FILES=4096] [ROUNDS=8]
//          [CANDIDATE] [FILE_BYTES=8192] [SOURCE_REPO|-] [CONCURRENCY=1]
// Both binaries must already be release-built. Run no other benchmark/build in
// parallel. Every started worker settles before cleanup; failures retain state.
assert.equal(process.platform, 'darwin', 'requires macOS time -l and native APFS clones');
const [binaryArgument, outputArgument, fileCountArgument = '4096', roundArgument = '8', candidateArgument, fileSizeArgument = '8192', sourceArgument, concurrencyArgument = '1'] = process.argv.slice(2);
const binary = fs.realpathSync(binaryArgument);
const candidate = candidateArgument ? fs.realpathSync(candidateArgument) : null;
const files = Number(fileCountArgument), rounds = Number(roundArgument);
const fileSize = Number(fileSizeArgument);
const concurrency = Number(concurrencyArgument);
const stageDiagnostics = process.env.RIFTRI_BENCH_STAGE_DIAGNOSTICS === '1';
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 10);
assert.ok(Number.isInteger(fileSize) && fileSize >= 1 && fileSize <= 1024 * 1024);
assert.ok(Number.isInteger(files) && files >= 32 && files <= 20000);
assert.ok(files * fileSize <= 256 * 1024 ** 2, 'synthetic fixture is limited to 256 MiB');
assert.ok(Number.isInteger(rounds) && rounds >= 1 && rounds <= 20);
assert.ok(!fs.existsSync(outputArgument));
fs.mkdirSync(outputArgument);
const output = fs.realpathSync(outputArgument);
const repo = path.join(output, 'repository'), state = path.join(output, 'state');
fs.mkdirSync(repo);
const env = { ...process.env, PATH: '/usr/bin:/bin:/usr/sbin:/sbin' };
for (const key of Object.keys(env)) if (key.startsWith('GIT_') || key.startsWith('RIFTRI_')) delete env[key];
Object.assign(env, { GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_SYSTEM: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' });
const checksum = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const report = { schemaVersion: 2, timeoutPolicy: 'POSIX process group, TERM then KILL after one second',
  stageDiagnostics,
  binary, candidate, binarySha256: checksum(binary), candidateSha256: candidate ? checksum(candidate) : null,
  files, fileSize, rounds, concurrency, batches: [], platform: process.platform, release: os.release(), cpu: os.cpus()[0].model,
  totalMemory: os.totalmem(), startedAt: new Date().toISOString(),
  note: 'Diagnostic phase attribution and paired evaluation on a loaded host. No timing threshold and no excluded samples.', samples: [] };
const save = () => fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(report, null, 2));
function run(command, args, cwd = repo) {
  const result = spawnSync(command, args, { cwd, env, timeout: 120000, maxBuffer: 128 * 1024 ** 2 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command} ${args.join(' ')}\n${result.stderr}`);
  return result.stdout;
}
const git = (args, cwd) => run('/usr/bin/git', args, cwd);
git(['init', '--quiet', '-b', 'main']);
for (const [key, value] of Object.entries({ 'user.name': 'Startup fixture', 'user.email': 'fixture@example.invalid', 'commit.gpgSign': 'false', 'core.autocrlf': 'false', 'core.fsmonitor': 'false', 'core.untrackedCache': 'false' })) git(['config', '--local', key, value]);
const manifest = [];
if (sourceArgument && sourceArgument !== '-') {
  report.sourceCommit = git(['rev-parse', 'HEAD'], sourceArgument).toString().trim();
  report.sourceTree = git(['rev-parse', 'HEAD^{tree}'], sourceArgument).toString().trim();
  const archive = git(['archive', '--format=tar', report.sourceCommit], sourceArgument);
  const unpack = spawnSync('/usr/bin/tar', ['-xf', '-', '-C', repo], { env, input: archive });
  assert.ifError(unpack.error); assert.equal(unpack.status, 0, unpack.stderr.toString());
} else {
  for (let i = 0; i < files; i++) {
    const relative = `src/d${Math.floor(i / 128)}/file-${String(i).padStart(5, '0')}.txt`;
    const file = path.join(repo, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const bytes = Buffer.alloc(fileSize, i % 251);
    fs.writeFileSync(file, bytes);
    if (i % 128 === 0) fs.chmodSync(file, 0o755);
    manifest.push({ relative, bytes, mode: i % 128 === 0 ? 0o111 : 0 });
  }
  fs.symlinkSync(manifest[0].relative, path.join(repo, 'source-link'));
}
git(['add', '--all']);
git(['commit', '--quiet', '-m', 'test: startup profiling fixture']);
manifest.length = 0;
const tracked = git(['ls-files', '-z']);
assert.deepEqual(Buffer.from(tracked.toString()), tracked, 'this manual manifest requires UTF-8 fixture paths');
for (const relative of tracked.toString().split('\0').filter(Boolean)) {
  const file = path.join(repo, relative), metadata = fs.lstatSync(file);
  manifest.push({ relative, link: metadata.isSymbolicLink() ? fs.readlinkSync(file) : null,
    bytes: metadata.isFile() ? fs.readFileSync(file) : null, mode: metadata.mode & 0o111 });
}
const privateFile = manifest.find(entry => entry.bytes !== null);
assert.ok(privateFile);
report.files = manifest.length;
report.logicalBytes = manifest.reduce((sum, entry) => sum + (entry.bytes?.length ?? 0), 0);
report.commit = git(['rev-parse', 'HEAD']).toString().trim();
report.tree = git(['rev-parse', 'HEAD^{tree}']).toString().trim();
if (report.sourceTree) assert.equal(report.tree, report.sourceTree, 'archive attributes must not change the measured source tree');
report.git = git(['--version']).toString().trim();
report.version = run(binary, ['--version']).toString().trim();
const live = new Set();
function verify(directory) {
  for (const entry of manifest) {
    const file = path.join(directory, entry.relative);
    if (entry.link !== null) assert.equal(fs.readlinkSync(file), entry.link);
    else {
      assert.deepEqual(fs.readFileSync(file), entry.bytes);
      assert.equal(fs.lstatSync(file).mode & 0o111, entry.mode);
    }
  }
}
function clean(directory) {
  assert.equal(git(['status', '--porcelain=v1', '-z', '--untracked-files=all'], directory).length, 0);
}
function remove(directory) {
  assert.equal(path.dirname(directory), output);
  clean(directory);
  run(binary, ['worktree', 'remove', directory, '--state-dir', state, '--no-progress']);
  assert.ok(!fs.existsSync(directory));
  live.delete(directory);
}
function verifySample(sample) {
  clean(sample.view); verify(sample.view); verify(sample.base);
  assert.equal(git(['rev-parse', 'HEAD'], sample.view).toString().trim(), report.commit);
}
async function create(round, label = 'baseline', executable = binary, worker = 0) {
  const key = `${round}-${label}-${worker}`;
  const view = path.join(output, `view-${key}`), trace = path.join(output, `git-${key}.jsonl`);
  assert.ok(!fs.existsSync(view));
  live.add(view);
  const sample = { round, label, worker, view, load: os.loadavg(), freeMemory: os.freemem(), phases: [] };
  const start = performance.now();
  let partial = '';
  const stageSampler = stageDiagnostics ? createStageSampler({ binary:executable, output, key }) : null;
  const result = await timedProcess('/usr/bin/time', ['-l', executable, 'worktree', 'add', view, 'HEAD', '--detach', '--state-dir', state, '--json'], {
    cwd: repo, env: { ...env, GIT_TRACE2_EVENT: trace }, timeoutMs: 120000,
    onStderr(chunk) {
      partial += chunk;
      const lines = partial.split('\n'); partial = lines.pop();
      for (const line of lines) {
        if (line.startsWith('riftri: ')) sample.phases.push({ milliseconds: performance.now() - start, line });
        stageSampler?.observe(line);
      }
    },
  });
  const { stdout, stderr, code, signal, timedOut, error } = result;
  if (stageSampler) sample.stackSamples = await stageSampler.stop();
  sample.milliseconds = performance.now() - start;
  sample.completedAt = performance.now();
  Object.assign(sample, { code, signal, timedOut, error, success: false });
  fs.writeFileSync(path.join(output, `add-${key}.log`), stderr + stdout);
  sample.resources = stderr.split('\n').filter(line => line && !line.startsWith('riftri: '));
  // Retain every attempt before asserting: a failed worker is evidence, not
  // a sample to discard or silently retry. Failed batches never get medians.
  report.samples.push(sample); save();
  assert.equal(error, null, JSON.stringify(error));
  assert.equal(timedOut, false, `worker ${key} timed out after 120 seconds; process group stopped`);
  assert.equal(code, 0, `worker ${key} exited with ${code}, signal ${signal}\n${stderr}`);
  const receipt = JSON.parse(stdout);
  sample.reused = receipt.reused_base; sample.base = receipt.base_path;
  const events = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse);
  const commands = new Map();
  for (const event of events) {
    if (event.event === 'start') commands.set(event.sid, { argv: event.argv, regions: [], counters: [] });
    const command = commands.get(event.sid);
    if (!command) continue;
    if (event.event === 'exit') { command.seconds = event.t_abs; command.code = event.code; }
    if (event.event === 'region_leave' && ['index', 'status', 'unpack_trees'].includes(event.category)) command.regions.push({ category: event.category, label: event.label, seconds: event.t_rel });
    if (event.event === 'data' && ['index', 'status'].includes(event.category)) command.counters.push({ category: event.category, key: event.key, value: event.value });
  }
  sample.gitCommands = [...commands.values()];
  sample.success = true; save();
  console.log(JSON.stringify(sample));
  return sample;
}
try {
  const anchor = await create(0);
  verifySample(anchor);
  assert.equal(anchor.reused, false);
  for (let round = 1; round <= rounds; round++) {
    const variants = candidate
      ? (round % 2 ? [['baseline', binary], ['advice', candidate]] : [['advice', candidate], ['baseline', binary]])
      : [['baseline', binary]];
    for (const [label, executable] of variants) {
      const batchStart = performance.now();
      const outcomes = await Promise.allSettled(Array.from({ length: concurrency }, (_, worker) => create(round, label, executable, worker)));
      const failures = outcomes.filter(outcome => outcome.status === 'rejected');
      assert.equal(failures.length, 0, failures.map(outcome => String(outcome.reason)).join('\n'));
      const batchSamples = outcomes.map(outcome => outcome.value);
      report.batches.push({ round, label, concurrency, milliseconds: Math.max(...batchSamples.map(sample => sample.completedAt)) - batchStart });
      save();
      for (const sample of batchSamples) {
        verifySample(sample);
        assert.equal(sample.reused, true); assert.equal(sample.base, anchor.base);
        const target = path.join(sample.view, privateFile.relative);
        fs.writeFileSync(target, Buffer.concat([privateFile.bytes, Buffer.from('private')]));
        assert.ok(git(['status', '--porcelain=v1'], sample.view).length > 0);
        verify(anchor.view); verify(anchor.base);
        fs.writeFileSync(target, privateFile.bytes);
        remove(sample.view);
      }
    }
  }
  remove(anchor.view);
  run(binary, ['gc', '--apply', '--yes', '--state-dir', state, '--no-progress']);
  report.final = JSON.parse(run(binary, ['status', '--state-dir', state, '--json']));
  assert.equal(report.final.operations.active_views, 0);
  assert.deepEqual(report.final.bases, []);
  assert.deepEqual(report.final.diagnostic_issues, []);
  verify(repo); report.complete = true; save();
} catch (error) {
  report.failure = String(error.stack); report.retained = [...live]; save();
  throw error; // Preserve unexpected state for inspection; never force cleanup.
}
