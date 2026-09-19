// Manual benchmark: node checkout-config-batching.mjs BEFORE AFTER SOURCE COMMIT NEW_OUTPUT_DIR
// Uses an independent exact-tree snapshot; never creates worktrees in SOURCE.
// Optional RIFTRI_BENCH_SINGLE_ROUNDS, RIFTRI_BENCH_BATCH_ROUNDS, and
// RIFTRI_BENCH_WORKERS also support follow-up creation optimizations.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const [before, after, source, revision, outputArgument] = process.argv.slice(2);
assert.ok(before && after && source && revision && outputArgument, 'Expected BEFORE AFTER SOURCE COMMIT NEW_OUTPUT_DIR');
function count(name, fallback, maximum) {
  const value = Number(process.env[name] ?? fallback);
  assert.ok(Number.isInteger(value) && value >= 1 && value <= maximum, `${name} must be 1..${maximum}`);
  return value;
}
const singleRounds = count('RIFTRI_BENCH_SINGLE_ROUNDS', 8, 30);
const batchRounds = count('RIFTRI_BENCH_BATCH_ROUNDS', 2, 30);
const workers = count('RIFTRI_BENCH_WORKERS', 9, 16);
const binaries = { before: path.resolve(before), after: path.resolve(after) };
const output = path.resolve(outputArgument);
assert.ok(!fs.existsSync(output), 'Output directory must not already exist');
fs.mkdirSync(output);
const repository = path.join(output, 'repository');
fs.mkdirSync(repository);
const emptyConfig = path.join(output, 'empty-config');
fs.writeFileSync(emptyConfig, '');
const env = { ...process.env, GIT_CONFIG_GLOBAL: emptyConfig, GIT_CONFIG_SYSTEM: emptyConfig, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_COUNT: '0' };
for (const key of ['GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_TRACE2_EVENT', 'RIFTRI_BYPASS', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT']) delete env[key];
function exec(command, args, cwd = repository, input) {
  const result = spawnSync(command, args, { cwd, env, input, maxBuffer: 100e6, timeout: 60000 });
  assert.equal(result.status, 0, `${command} ${args.join(' ')}\n${result.stderr}`);
  return result.stdout;
}
const commit = exec('git', ['rev-parse', `${revision}^{commit}`], path.resolve(source)).toString().trim();
const tree = exec('git', ['rev-parse', `${commit}^{tree}`], path.resolve(source)).toString().trim();
exec('tar', ['-xf', '-', '-C', repository], output, exec('git', ['archive', commit], path.resolve(source)));
exec('git', ['init', '--quiet']);
for (const [key, value] of [['user.name', 'Riftri Benchmark'], ['user.email', 'benchmark@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) exec('git', ['config', key, value]);
exec('git', ['add', '--all']);
exec('git', ['commit', '--quiet', '-m', 'test: exact-tree benchmark fixture']);
const fixtureTree = exec('git', ['rev-parse', 'HEAD^{tree}']).toString().trim();
assert.equal(fixtureTree, tree);
exec(binaries.before, ['enable', repository]);

const namesBuffer = exec('git', ['ls-files', '-z']);
assert.ok(Buffer.from(namesBuffer.toString()).equals(namesBuffer), 'This benchmark requires UTF-8 filenames');
const names = namesBuffer.toString().split('\0').filter(Boolean);
function fingerprint(directory, name) {
  const file = path.join(directory, name);
  const stat = fs.lstatSync(file);
  const symlink = stat.isSymbolicLink();
  const bytes = symlink ? fs.readlinkSync(file, { encoding: 'buffer' }) : fs.readFileSync(file);
  return { name, symlink, executable: stat.mode & 0o111, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
}
const manifest = names.map(name => fingerprint(repository, name));
const result = { commit, tree, fixtureTree, hardware: { platform: process.platform, arch: process.arch, os: os.release(), cpu: os.cpus()[0].model, memory: os.totalmem() }, files: names.length, logicalBytes: manifest.reduce((sum, file) => sum + file.size, 0), binaries, singleRounds, batchRounds, workers, startedAt: new Date().toISOString(), git: exec('git', ['--version']).toString().trim(), cases: [], batches: [] };
function save() { fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(result, null, 2)); }
function verify(directory) {
  assert.equal(exec('git', ['status', '--porcelain=v1', '-z', '--untracked-files=all'], directory).length, 0);
  for (const expected of manifest) assert.deepEqual(fingerprint(directory, expected.name), expected);
}
async function create(version, mode, label, warm = true) {
  const destination = path.join(output, label);
  assert.ok(!fs.existsSync(destination));
  const trace = path.join(output, `${label}.trace2.jsonl`);
  const args = mode === 'explicit'
    ? ['worktree', 'add', '--detach', '--repository', repository, destination, 'HEAD']
    : ['exec', '--', 'git', 'worktree', 'add', '--detach', destination, 'HEAD'];
  const started = process.hrtime.bigint();
  const child = spawn(binaries[version], args, { cwd: repository, env: { ...env, GIT_TRACE2_EVENT: trace }, timeout: 60000 });
  let stdout = '', stderr = '';
  let partial = '';
  const phases = [];
  child.stdout.on('data', data => { stdout += data; });
  child.stderr.on('data', data => {
    stderr += data;
    partial += data;
    const lines = partial.split('\n');
    partial = lines.pop();
    for (const line of lines) {
      if (line.startsWith('riftri: ')) phases.push({ line, seconds: Number(process.hrtime.bigint() - started) / 1e9 });
    }
  });
  const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('close', resolve); });
  const seconds = Number(process.hrtime.bigint() - started) / 1e9;
  fs.writeFileSync(path.join(output, `${label}.log`), stdout + stderr);
  assert.equal(code, 0, stderr);
  if (warm) assert.match(stdout + stderr, mode === 'explicit' ? /^Base: reused$/m : /\(reused base\)/);
  else assert.match(stdout + stderr, /^Base: created$/m);
  // The shim deliberately emits only a short stderr message. Read its durable
  // record instead of assuming it shares the explicit command's output format.
  const operations = path.join(repository, '.git/riftri/operations');
  const decodePath = value => {
    assert.equal(value.encoding, 'unix-bytes', 'This harness targets Unix/APFS');
    const bytes = Buffer.from(value.units);
    assert.ok(Buffer.from(bytes.toString()).equals(bytes), 'Expected UTF-8 fixture paths');
    return bytes.toString();
  };
  const journals = fs.readdirSync(operations).filter(name => name.endsWith('.json')).map(name => JSON.parse(fs.readFileSync(path.join(operations, name))));
  const matches = journals.filter(journal => decodePath(journal.destination) === destination);
  assert.equal(matches.length, 1);
  const base = decodePath(matches[0].base_path);
  const starts = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse).filter(event => event.event === 'start');
  const record = { version, mode, label, seconds, destination, base, backend: matches[0].backend, phases, gitStarts: starts.length, configGets: starts.filter(event => event.argv.includes('config') && event.argv.includes('--get')).length, configBatches: starts.filter(event => event.argv.includes('--get-regexp')).length };
  result.cases.push(record); save();
  return record;
}
function remove(record) {
  verify(record.destination);
  exec(binaries[record.version], ['worktree', 'remove', '--repository', repository, record.destination]);
  assert.ok(!fs.existsSync(record.destination));
  record.verifiedAndRemoved = true; save();
}

// Cold creation samples use an empty base cache, separate from cached timings.
for (const version of ['before', 'after']) {
  const cold = await create(version, 'explicit', `cold-${version}`, false);
  remove(cold);
  exec(binaries[version], ['gc', repository, '--apply']);
}
// Keep a baseline-created anchor alive: both versions must reuse its exact base.
const anchor = await create('before', 'explicit', 'anchor', false);
verify(anchor.destination);
for (let round = 0; round < singleRounds; round++) {
  for (const mode of ['explicit', 'shim']) {
    for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
      const record = await create(version, mode, `single-${round}-${mode}-${version}`);
      assert.equal(record.base, anchor.base, 'checkout profile must remain compatible');
      remove(record);
      console.log(`${record.label}: ${record.seconds.toFixed(3)}s, ${record.gitStarts} Git starts`);
    }
  }
}
for (let round = 0; round < batchRounds; round++) {
  for (const mode of ['explicit', 'shim']) {
  for (const version of round % 2 ? ['after', 'before'] : ['before', 'after']) {
    exec('sync', [], output);
    const freeBefore = fs.statfsSync(output);
    const started = process.hrtime.bigint();
    const records = await Promise.all(Array.from({ length: workers }, (_, worker) => create(version, mode, `parallel-${round}-${mode}-${version}-${worker}`)));
    const seconds = Number(process.hrtime.bigint() - started) / 1e9;
    exec('sync', [], output);
    const freeAfter = fs.statfsSync(output);
    const volumeDeltaBytes = (freeBefore.bfree * freeBefore.bsize) - (freeAfter.bfree * freeAfter.bsize);
    result.batches.push({ round, version, mode, workers, seconds, volumeDeltaBytes }); save();
    for (const record of records) assert.equal(record.base, anchor.base);
    // Private-write test is outside the timed region. Verify all peers and base.
    const file = manifest.find(entry => !entry.symlink && entry.size > 0).name;
    const target = path.join(records[0].destination, file);
    const original = fs.readFileSync(target);
    fs.writeFileSync(target, Buffer.concat([original, Buffer.from('\nprivate benchmark edit\n')]));
    verify(anchor.destination);
    for (const record of records.slice(1)) verify(record.destination);
    for (const expected of manifest) assert.deepEqual(fingerprint(anchor.base, expected.name), expected);
    fs.writeFileSync(target, original);
    for (const record of records) remove(record);
    console.log(`parallel-${round}-${mode}-${version}: ${seconds.toFixed(3)}s`);
  }
  }
}
remove(anchor);
exec(binaries.after, ['gc', repository, '--apply']);
const status = exec(binaries.after, ['status', repository]).toString();
for (const pattern of [/Active views: 0\n/, /Retained bases: 0\n/, /Pending adds: 0\n/, /Pending removals: 0\n/, /State issues: 0\n/]) assert.match(status, pattern);
fs.writeFileSync(path.join(output, 'final-status.log'), status);
verify(repository);
result.completedAt = new Date().toISOString();
result.allViewsVerifiedAndRemoved = result.cases.every(record => record.verifiedAndRemoved);
save();
console.log(`Complete: ${path.join(output, 'results.json')}`);
