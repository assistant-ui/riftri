// node sparse-cone.mjs BINARY SOURCE COMMIT NEW_OUTPUT_DIR SPARSE_DIR
// Read-only access to SOURCE; all Git and Riftri mutations use a fresh fixture.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const [binaryArg, sourceArg, revision, outputArg, cone] = process.argv.slice(2);
assert.ok(binaryArg && sourceArg && revision && outputArg && cone,
  'Expected BINARY SOURCE COMMIT NEW_OUTPUT_DIR SPARSE_DIR');
const binary = path.resolve(binaryArg), source = path.resolve(sourceArg), output = path.resolve(outputArg);
assert.ok(!fs.existsSync(output), 'Output directory must not already exist');
fs.mkdirSync(output);
const repository = path.join(output, 'repository');
fs.mkdirSync(repository);
const config = path.join(output, 'empty-config');
fs.writeFileSync(config, '');
const env = { ...process.env, GIT_CONFIG_GLOBAL: config, GIT_CONFIG_SYSTEM: config,
  GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_COUNT: '0' };
for (const key of ['GIT_CONFIG_PARAMETERS', 'GIT_CONFIG', 'GIT_DIR', 'GIT_WORK_TREE',
  'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_TRACE2_EVENT', 'RIFTRI_BYPASS', 'RIFTRI_SHIM_ACTIVE', 'RIFTRI_REAL_GIT']) delete env[key];
function run(command, args, cwd = repository, input) {
  const result = spawnSync(command, args, { cwd, env, input, timeout: 120000, maxBuffer: 100e6 });
  assert.equal(result.status, 0, `${command} ${args.join(' ')}\n${result.stderr}`);
  return result.stdout;
}
const commit = run('git', ['rev-parse', `${revision}^{commit}`], source).toString().trim();
const tree = run('git', ['rev-parse', `${commit}^{tree}`], source).toString().trim();
run('tar', ['-xf', '-', '-C', repository], output, run('git', ['archive', commit], source));
run('git', ['init', '--quiet']);
for (const [key, value] of [['user.name', 'Riftri Benchmark'], ['user.email', 'benchmark@example.invalid'],
  ['core.autocrlf', 'false'], ['commit.gpgSign', 'false']]) run('git', ['config', key, value]);
run('git', ['add', '--all']);
run('git', ['commit', '--quiet', '-m', 'test: sparse benchmark fixture']);
const fixtureTree = run('git', ['rev-parse', 'HEAD^{tree}']).toString().trim();
assert.equal(fixtureTree, tree);

function manifest(directory) {
  const entries = [];
  function visit(relative) {
    for (const name of fs.readdirSync(path.join(directory, relative))) {
      if (!relative && name === '.git') continue;
      const entry = path.join(relative, name), file = path.join(directory, entry);
      const stat = fs.lstatSync(file);
      if (stat.isDirectory()) visit(entry);
      else {
        const bytes = stat.isSymbolicLink() ? fs.readlinkSync(file, { encoding: 'buffer' }) : fs.readFileSync(file);
        entries.push({ path: entry, symlink: stat.isSymbolicLink(), mode: stat.mode & 0o111,
          bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
      }
    }
  }
  visit('');
  return entries.sort((a, b) => a.path.localeCompare(b.path));
}
function free() { const s = fs.statfsSync(output); return s.bfree * s.bsize; }
function clean(directory) {
  assert.equal(run('git', ['status', '--porcelain=v1', '-z', '--untracked-files=all'], directory).length, 0);
}

// Git itself supplies the expected cone shape and skip-worktree bits.
const reference = path.join(output, 'git-reference');
run('git', ['worktree', 'add', '--detach', '--no-checkout', reference, 'HEAD']);
run('git', ['sparse-checkout', 'set', '--cone', '--', cone], reference);
run('git', ['reset', '--hard', 'HEAD'], reference);
clean(reference);
const sparseManifest = manifest(reference), fullManifest = manifest(repository);
const sparseIndex = run('git', ['ls-files', '-t', '-z'], reference);
run('git', ['worktree', 'remove', reference]);
const result = { commit, tree, fixtureTree, cone, binary,
  startedAt: new Date().toISOString(), node: process.version, hardware: { platform: process.platform, arch: process.arch,
    os: os.release(), cpu: os.cpus()[0].model, memory: os.totalmem() },
  git: run('git', ['--version']).toString().trim(),
  full: { files: fullManifest.length, logicalBytes: fullManifest.reduce((n, e) => n + e.bytes, 0) },
  sparse: { files: sparseManifest.length, logicalBytes: sparseManifest.reduce((n, e) => n + e.bytes, 0) }, cases: [] };
const bases = {};
for (let round = 0; round < 4; round++) {
  for (const profile of round % 2 ? ['sparse', 'full'] : ['full', 'sparse']) {
    const destination = path.join(output, `${profile}-${round}`);
    run('sync', [], output);
    const before = free(), started = process.hrtime.bigint();
    const receipt = JSON.parse(run(binary, ['worktree', 'add', '--detach', '--json', '--repository', repository,
      ...(profile === 'sparse' ? ['--sparse-dir', cone] : []), destination, 'HEAD']));
    const seconds = Number(process.hrtime.bigint() - started) / 1e9;
    run('sync', [], output);
    const volumeDeltaBytes = before - free();
    assert.equal(receipt.reused_base, round !== 0);
    if (round === 0) bases[profile] = receipt.base_path;
    assert.equal(receipt.base_path, bases[profile]);
    assert.deepEqual(manifest(destination), profile === 'sparse' ? sparseManifest : fullManifest);
    if (profile === 'sparse') assert.deepEqual(run('git', ['ls-files', '-t', '-z'], destination), sparseIndex);
    clean(destination);
    result.cases.push({ round, profile, seconds, volumeDeltaBytes, backend: receipt.backend, reusedBase: receipt.reused_base });
    run(binary, ['worktree', 'remove', '--repository', repository, destination]);
  }
}
assert.notEqual(bases.full, bases.sparse);
run(binary, ['gc', repository, '--apply']);
const status = JSON.parse(run(binary, ['status', repository, '--json']));
assert.deepEqual(status.bases, []);
assert.deepEqual(status.worktrees, []);
assert.deepEqual(status.diagnostic_issues, []);
for (const [name, value] of Object.entries(status.operations)) {
  if (name.startsWith('pending_') || name === 'active_views') assert.equal(value, 0, name);
}
result.finalStatus = status;
result.completedAt = new Date().toISOString();
clean(repository);
fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(result, null, 2));
console.log(JSON.stringify(result, null, 2));
