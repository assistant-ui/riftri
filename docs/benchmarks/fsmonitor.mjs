// Disposable macOS/APFS experiment. No source repository or global config is changed.
// node docs/benchmarks/fsmonitor.mjs /path/to/riftri /path/to/NEW-output-directory
// BENCH_FILES (32..20000), BENCH_CREATION_ROUNDS, BENCH_COLD_ROUNDS,
// and BENCH_STATUS_ROUNDS control fixture size and rotating paired samples.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { distribution } from './latency-summary.mjs';

const [binaryArgument, outputArgument] = process.argv.slice(2);
assert.ok(binaryArgument && outputArgument);
const binary = fs.realpathSync(binaryArgument);
assert.equal(process.platform, 'darwin', 'This experiment targets macOS FSMonitor on APFS.');
assert.ok(!fs.existsSync(outputArgument), 'Refuse to reuse an existing output directory.');
fs.mkdirSync(outputArgument);
const output = fs.realpathSync(outputArgument);
const fixture = path.join(output, 'fixture');
const repo = path.join(fixture, 'repository');
const state = path.join(fixture, 'state');
let sockets;
const files = Number(process.env.BENCH_FILES ?? 4096);
const creationRounds = Number(process.env.BENCH_CREATION_ROUNDS ?? 6);
const coldRounds = Number(process.env.BENCH_COLD_ROUNDS ?? 3);
const statusRounds = Number(process.env.BENCH_STATUS_ROUNDS ?? 20);
for (const count of [files, statusRounds]) assert.ok(Number.isInteger(count) && count > 0);
for (const count of [creationRounds, coldRounds]) assert.ok(Number.isInteger(count) && count >= 0);
assert.ok(files >= 32 && files <= 20000);
const space = fs.statfsSync(output);
const requiredSpace = Math.max(64 * 1024 ** 2, files * 8192 * 8);
assert.ok(space.bavail * space.bsize > requiredSpace, `Require ${requiredSpace} free bytes for this fixture.`);
fs.mkdirSync(repo, { recursive: true });
const env = { ...process.env, PATH: '/usr/bin:/bin:/usr/sbin:/sbin' };
for (const key of Object.keys(env)) if (key.startsWith('GIT_') || key.startsWith('RIFTRI_')) delete env[key];
Object.assign(env, { GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_SYSTEM: '/dev/null', GIT_CONFIG_NOSYSTEM: '1', RIFTRI_BYPASS: '1' });
const variants = [
  { name: 'off', fsmonitor: false, untracked: false },
  { name: 'fsmonitor', fsmonitor: true, untracked: false },
  { name: 'fsmonitor-untracked', fsmonitor: true, untracked: true },
];
const disabled = ['-c', 'core.fsmonitor=false', '-c', 'core.untrackedCache=false'];
const statusArgs = ['-c', 'status.renames=false', 'status', '--porcelain=v1', '-z', '--untracked-files=all'];
const report = {
  note: 'Local paired synthetic CLI experiment, not a T3 app benchmark or isolated allocation test. Cold means empty Riftri base cache, not cold OS cache. Wall times include process startup. No timing outliers are removed.',
  startedAt: new Date().toISOString(), platform: process.platform, arch: process.arch,
  os: os.release(), cpu: os.cpus()[0].model, files, creationRounds, coldRounds, statusRounds,
  binary, variants, creation: [], status: [], correctness: [], traces: [], resources: [], commands: 0,
};
function save() { fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(report, null, 2)); }
function run(command, args, cwd = repo, extra = {}) {
  const start = performance.now();
  const result = spawnSync(command, args, { cwd, env: { ...env, ...extra }, maxBuffer: 30 * 1024 ** 2, timeout: 120000 });
  const milliseconds = performance.now() - start;
  report.commands++;
  assert.ifError(result.error);
  return { code: result.status, stdout: result.stdout, stderr: result.stderr.toString(), milliseconds };
}
function ok(command, args, cwd = repo, extra) {
  const result = run(command, args, cwd, extra);
  assert.equal(result.code, 0, `${command} ${args.join(' ')}\n${result.stderr}`);
  return result;
}
function git(args, cwd = repo, extra) { return ok('git', [...disabled, ...args], cwd, extra); }
function monitorStatus(cwd) {
  return run('git', ['-c', 'core.fsmonitor=true', 'fsmonitor--daemon', 'status'], cwd);
}
function monitorStop(cwd) {
  const current = monitorStatus(cwd);
  if (current.code === 0) git(['-c', 'core.fsmonitor=true', 'fsmonitor--daemon', 'stop'], cwd);
  else assert.equal(current.code, 1, `Unexpected monitor status at ${cwd}: ${current.stderr}`);
  const stopped = monitorStatus(cwd);
  assert.equal(stopped.code, 1, `Monitor still running at ${cwd}: ${stopped.stderr}`);
}
function setCommon(variant) {
  git(['config', '--local', 'core.fsmonitor', String(variant.fsmonitor)]);
  git(['config', '--local', 'core.untrackedCache', String(variant.untracked)]);
}
function status(view, oracle = false, trace) {
  return ok('git', [...(oracle ? ['--no-optional-locks', ...disabled] : []), ...statusArgs], view,
    trace ? { GIT_TRACE2_EVENT: trace } : undefined);
}
const live = new Set();
let serial = 0;
let anchor;
function create(variant, label) {
  setCommon(variant);
  const view = path.join(fixture, `${label}-${serial++}`);
  live.add(view); // Include partially created views in conservative cleanup diagnostics.
  const created = ok(binary, ['worktree', 'add', view, 'HEAD', '--detach', `--state-dir=${state}`, '--json', '--no-progress']);
  const receipt = JSON.parse(created.stdout);
  assert.equal(status(view, true).stdout.length, 0, 'New worktree must be independently clean.');
  assert.equal(git(['rev-parse', 'HEAD'], view).stdout.toString().trim(), report.commit);
  const monitorRunning = monitorStatus(view).code === 0;
  if (variant.fsmonitor) assert.ok(monitorRunning, 'Creation must really use FSMonitor, not silently fall back.');
  git(['config', '--worktree', 'core.fsmonitor', String(variant.fsmonitor)], view);
  git(['config', '--worktree', 'core.untrackedCache', String(variant.untracked)], view);
  return { variant: variant.name, view, base: receipt.base_path, reused: receipt.reused_base, milliseconds: created.milliseconds, monitorRunning };
}
function remove(view) {
  if (!fs.existsSync(view)) return;
  monitorStop(view);
  git(['config', '--worktree', 'core.fsmonitor', 'false'], view);
  assert.equal(status(view, true).stdout.length, 0, `Refuse to remove dirty fixture: ${view}`);
  ok(binary, ['worktree', 'remove', view, `--state-dir=${state}`, '--no-progress']);
  assert.ok(!fs.existsSync(view));
  live.delete(view);
}
function gc() { ok(binary, ['gc', '--apply', '--yes', `--state-dir=${state}`, '--no-progress']); }
function rotated(values, round) { return values.slice(round % values.length).concat(values.slice(0, round % values.length)); }
function rememberTrace(view, variant, scenario) {
  const trace = path.join(output, `${variant}-${scenario}.trace.jsonl`);
  const result = status(view, false, trace);
  assert.deepEqual(result.stdout, status(view, true).stdout);
  const events = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse);
  report.traces.push({ variant, scenario, milliseconds: result.milliseconds,
    counters: events.filter(event => event.event === 'data' && ['index', 'fsmonitor', 'status'].includes(event.category))
      .map(({ category, key, value }) => ({ category, key, value })),
  });
}
function collectResources() {
  const expected = new Map();
  for (const trace of report.traces) {
    for (const counter of trace.counters) {
      const match = counter.key.includes('/token') && counter.value.match(/^builtin:\d+\.(\d+)\./);
      if (match) expected.set(Number(match[1]), trace.variant);
    }
  }
  const processes = ok('ps', ['-axo', 'pid=,rss=,time=,command=']).stdout.toString().split('\n');
  for (const line of processes) {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\S+)\s+(.+)$/);
    if (!match || !/git.*fsmonitor--daemon run/.test(match[4])) continue;
    const pid = Number(match[1]);
    // Read-only correlation with daemon PIDs in this fixture's observed Git tokens.
    // Detached daemons chdir to HOME and can place long-path sockets elsewhere.
    if (expected.has(pid)) report.resources.push({ pid, variant: expected.get(pid), rssKiB: Number(match[2]), cumulativeCpuTime: match[3] });
  }
  report.resourcesComplete = expected.size === 2 && report.resources.length === expected.size;
}
let failed;
try {
  report.version = ok(binary, ['--version']).stdout.toString().trim();
  report.git = git(['--version']).stdout.toString().trim();
  git(['init', '--quiet', '-b', 'main']);
  // Long Unix socket paths otherwise make Git fall back to HOME. Keep the
  // daemon's IPC files in a separate, short, task-owned native directory.
  sockets = fs.realpathSync(fs.mkdtempSync('/tmp/riftri-fsm-'));
  git(['config', '--local', 'fsmonitor.socketDir', sockets]);
  for (const [key, value] of Object.entries({ 'user.name': 'FSMonitor fixture', 'user.email': 'fixture@example.invalid', 'commit.gpgSign': 'false', 'extensions.worktreeConfig': 'true', 'core.fsmonitor': 'false', 'core.untrackedCache': 'false' })) git(['config', '--local', key, value]);
  const content = Buffer.alloc(8192, 'a');
  for (let i = 0; i < files; i++) {
    const directory = path.join(repo, 'src', `d${Math.floor(i / 128)}`);
    fs.mkdirSync(directory, { recursive: true });
    fs.writeFileSync(path.join(directory, `file-${String(i).padStart(5, '0')}.txt`), content);
  }
  fs.writeFileSync(path.join(repo, 'README.md'), 'FSMonitor correctness and performance fixture\n');
  fs.writeFileSync(path.join(repo, '.gitignore'), 'build-output/\n');
  fs.writeFileSync(path.join(repo, 'run.sh'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  fs.symlinkSync('README.md', path.join(repo, 'readme-link'));
  git(['add', '--all']);
  git(['commit', '--quiet', '-m', 'test: disposable FSMonitor fixture']);
  report.commit = git(['rev-parse', 'HEAD']).stdout.toString().trim();
  report.trackedFiles = git(['ls-files', '-z']).stdout.toString().split('\0').filter(Boolean).length;
  report.logicalBytes = files * content.length;
  // Explicit capability probe in the disposable main checkout; never enable globally.
  git(['-c', 'core.fsmonitor=true', 'fsmonitor--daemon', 'start']);
  assert.equal(monitorStatus(repo).code, 0);
  monitorStop(repo);
  git(['update-index', '--test-untracked-cache']);
  save();
  console.log(`Fixture ready: ${report.trackedFiles} tracked files, ${report.version}, ${report.git}`);
  for (let round = 0; round < coldRounds; round++) {
    for (const variant of rotated(variants.slice(0, 2), round)) {
      const item = create(variant, `cold-${round}-${variant.name}`);
      assert.equal(item.reused, false);
      report.creation.push({ ...item, round, cache: 'cold' });
      remove(item.view); gc(); save();
      console.log(`Cold ${round} ${variant.name}: ${item.milliseconds.toFixed(1)} ms`);
    }
  }
  anchor = create(variants[0], 'anchor');
  for (let round = 0; round < creationRounds; round++) {
    for (const variant of rotated(variants, round)) {
      const item = create(variant, `cached-${round}-${variant.name}`);
      assert.equal(item.reused, true);
      assert.equal(item.base, anchor.base);
      report.creation.push({ ...item, round, cache: 'cached' });
      remove(item.view); save();
      console.log(`Cached ${round} ${variant.name}: ${item.milliseconds.toFixed(1)} ms`);
    }
  }
  const views = variants.map(variant => ({ ...create(variant, `status-${variant.name}`), settings: variant }));
  setCommon(variants[0]);
  const editNames = ['src/d0/file-00000.txt', 'src/d0/file-00001.txt', 'src/d0/file-00002.txt'];
  for (const item of views) {
    for (let warm = 0; warm < 4; warm++) assert.equal(status(item.view).stdout.length, 0);
    rememberTrace(item.view, item.variant, 'clean-warmup');
  }
  for (const scenario of ['clean', 'three-edits', 'three-untracked']) {
    for (let round = 0; round < statusRounds; round++) {
      if (scenario === 'three-edits') {
        for (const item of views) for (const name of editNames) fs.writeFileSync(path.join(item.view, name), Buffer.alloc(8192, 98 + round % 20));
      } else if (scenario === 'three-untracked') {
        for (const item of views) for (let index = 0; index < 3; index++) fs.writeFileSync(path.join(item.view, `untracked-${index}.txt`), `${round}\n`);
      }
      const sampled = [];
      for (const item of rotated(views, round)) {
        const result = status(item.view);
        report.status.push({ variant: item.variant, scenario, round, milliseconds: result.milliseconds });
        sampled.push({ item, result });
      }
      for (const { item, result } of sampled) {
        assert.deepEqual(result.stdout, status(item.view, true).stdout, `Full-scan oracle disagrees: ${scenario}/${item.variant}/${round}`);
        assert.equal(result.stdout.toString().split('\0').filter(Boolean).length, scenario === 'clean' ? 0 : 3);
      }
    }
    for (const item of views) {
      rememberTrace(item.view, item.variant, scenario);
      if (scenario === 'three-edits') git(['restore', '--source=HEAD', '--worktree', '--', ...editNames], item.view);
      if (scenario === 'three-untracked') for (let index = 0; index < 3; index++) fs.unlinkSync(path.join(item.view, `untracked-${index}.txt`));
      assert.equal(status(item.view).stdout.length, 0);
    }
    save(); console.log(`Passed ${statusRounds} paired ${scenario} status rounds for all three settings.`);
  }
  collectResources();
  const correctnessView = views.find(item => item.variant === 'fsmonitor-untracked').view;
  const target = path.join(correctnessView, editNames[0]);
  const original = fs.readFileSync(target);
  const cases = [
    ['same-size-restored-mtime', () => { const s = fs.statSync(target); fs.writeFileSync(target, Buffer.alloc(original.length, 'z')); fs.utimesSync(target, s.atime, s.mtime); }],
    ['atomic-replacement', () => { const s = fs.statSync(target); fs.writeFileSync(`${target}.new`, Buffer.alloc(original.length, 'y')); fs.utimesSync(`${target}.new`, s.atime, s.mtime); fs.renameSync(`${target}.new`, target); }],
    ['tracked-deletion', () => fs.unlinkSync(target)],
    ['rename', () => fs.renameSync(target, path.join(correctnessView, 'renamed.txt'))],
    ['executable-bit', () => fs.chmodSync(path.join(correctnessView, 'run.sh'), 0o644)],
    ['symlink-target', () => { fs.unlinkSync(path.join(correctnessView, 'readme-link')); fs.symlinkSync('run.sh', path.join(correctnessView, 'readme-link')); }],
    ['nested-untracked', () => { fs.mkdirSync(path.join(correctnessView, 'new-dir')); fs.writeFileSync(path.join(correctnessView, 'new-dir', 'new.txt'), 'new\n'); }],
    ['staged-and-unstaged', () => { fs.writeFileSync(target, 'stage this\n'); git(['add', '--', editNames[0]], correctnessView); fs.writeFileSync(target, 'later edit\n'); }],
    ['monitor-restart', () => { monitorStop(correctnessView); fs.writeFileSync(target, 'edited while monitor stopped\n'); }],
  ];
  for (const [name, mutate] of cases) {
    mutate();
    const watched = status(correctnessView);
    const full = status(correctnessView, true);
    assert.ok(full.stdout.length > 0, `${name} must produce a real Git change`);
    assert.deepEqual(watched.stdout, full.stdout, `Missed change: ${name}`);
    assert.deepEqual(fs.readFileSync(path.join(anchor.view, editNames[0])), original, 'Peer must not change.');
    assert.deepEqual(fs.readFileSync(path.join(anchor.base, editNames[0])), original, 'Base must not change.');
    if (name === 'same-size-restored-mtime') {
      const refused = run(binary, ['worktree', 'remove', correctnessView, `--state-dir=${state}`, '--no-progress']);
      assert.notEqual(refused.code, 0, 'Dirty managed removal must be refused.');
      assert.ok(fs.existsSync(correctnessView));
    }
    report.correctness.push({ name, passed: true, status: full.stdout.toString() });
    git(['restore', '--source=HEAD', '--staged', '--worktree', '--', ...editNames, 'run.sh', 'readme-link'], correctnessView);
    if (fs.existsSync(path.join(correctnessView, 'renamed.txt'))) fs.unlinkSync(path.join(correctnessView, 'renamed.txt'));
    if (fs.existsSync(path.join(correctnessView, 'new-dir'))) { fs.unlinkSync(path.join(correctnessView, 'new-dir', 'new.txt')); fs.rmdirSync(path.join(correctnessView, 'new-dir')); }
    assert.equal(status(correctnessView).stdout.length, 0);
  }
  const warmedBeforeRestart = status(correctnessView).milliseconds;
  monitorStop(correctnessView);
  const restart = status(correctnessView);
  assert.equal(restart.stdout.length, 0);
  report.restart = { warmedBeforeRestart, firstCheckAfterRestart: restart.milliseconds, nextCheck: status(correctnessView).milliseconds };
  report.measurementsComplete = true;
  save(); console.log('All mutation, isolation, dirty-removal, and monitor-restart checks passed.');
} catch (error) {
  failed = error;
  report.failure = { message: error.message, stack: error.stack };
  save();
} finally {
  const errors = [];
  if (fs.existsSync(path.join(repo, '.git'))) {
    try { setCommon(variants[0]); } catch (error) { errors.push(error.message); }
    for (const view of [...live].reverse()) {
      try { remove(view); } catch (error) { errors.push(`${view}: ${error.message}`); }
    }
    try { monitorStop(repo); } catch (error) { errors.push(error.message); }
    if (fs.existsSync(state)) {
      try {
        gc();
        const receipt = JSON.parse(ok(binary, ['status', '--json', `--state-dir=${state}`]).stdout);
        assert.equal(receipt.operations.active_views, 0);
        assert.equal(receipt.bases.length, 0);
        assert.equal(receipt.diagnostic_issues.length, 0);
        report.finalState = receipt;
      } catch (error) { errors.push(error.message); }
    }
    try { assert.equal(git(['worktree', 'list', '--porcelain']).stdout.toString().match(/^worktree /gm)?.length, 1); } catch (error) { errors.push(error.message); }
  }
  report.cleanupErrors = errors;
  report.cleanupComplete = errors.length === 0 && live.size === 0;
  // Preserve fixtures on failure. Only a fully verified, task-owned fixture is retired.
  if (!failed && report.cleanupComplete) {
    assert.equal(path.dirname(fixture), output);
    assert.equal(path.basename(fixture), 'fixture');
    fs.rmSync(fixture, { recursive: true });
    report.fixtureRemoved = true;
    if (sockets) {
      assert.equal(path.dirname(sockets), fs.realpathSync('/tmp'));
      assert.ok(path.basename(sockets).startsWith('riftri-fsm-'));
      fs.rmSync(sockets, { recursive: true });
      report.socketsRemoved = true;
    }
  } else if (sockets) {
    report.retainedSocketDirectory = sockets;
  }
  report.finishedAt = new Date().toISOString();
  report.summary = [];
  for (const kind of ['creation', 'status']) {
    const groups = new Map();
    for (const row of report[kind]) {
      const key = `${kind}/${row.cache ?? row.scenario}/${row.variant}`;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(row.milliseconds);
    }
    for (const [group, values] of groups) report.summary.push({ group, ...distribution(values) });
  }
  save();
  for (const row of report.summary) console.log(`${row.group}: p50=${row.p50.toFixed(1)} ms p95=${row.p95.toFixed(1)} ms n=${row.samples}`);
  console.log(`cleanupComplete=${report.cleanupComplete}; retained report: ${path.join(output, 'results.json')}`);
  if (errors.length) throw new Error(`Conservative cleanup needs attention: ${errors.join('\n')}`, { cause: failed });
}
if (failed) throw failed;
