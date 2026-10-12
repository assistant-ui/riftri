import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {profileTrace, profileStartup, readStartupProfile} from './startup-profile.mjs';

const event = (sid, type, fields = {}) => ({event: type, sid, thread: 'main', ...fields});
function command(sid, name, seconds, scan) {
  return [
    event(sid, 'start', {argv: ['git', '-C', 'status', name], t_abs: 0.001}),
    event(sid, 'cmd_name', {name}),
    event(sid, 'region_enter', {category: 'index', label: 'refresh', nesting: 1}),
    event(sid, 'data', {category: 'index', key: 'refresh/sum_scan', value: String(scan)}),
    event(sid, 'region_leave', {category: 'index', label: 'refresh', nesting: 1, t_rel: seconds / 2}),
    event(sid, 'exit', {t_abs: seconds, code: 0}),
    event(sid, 'atexit', {t_abs: seconds + 0.001, code: 0}),
  ];
}
const trace = () => [...command('reset', 'reset', 0.8, 5864), ...command('status', 'status', 0.04, 0)];
const jsonl = events => events.map(e => JSON.stringify(e)).join('\n') + '\n';
const script = fileURLToPath(new URL('./startup-profile.mjs', import.meta.url));
const sample = (round, label, worker = 0) => ({round, label, worker, reused: round > 0,
  started: true, settled: true, success: true, code: 0, signal: null, error: null,
  timedOut: false, cancelled: false, milliseconds: 2000});
function report(concurrency = 1) {
  return {schemaVersion: 3, runner: 'supervised-startup-v1', complete: true,
    binarySha256: 'a'.repeat(64), candidateSha256: 'b'.repeat(64), candidate: '/unused/candidate',
    rounds: 1, concurrency, git: 'fixture', tree: 'example',
    samples: [sample(0, 'baseline'), ...['baseline', 'advice'].flatMap(label =>
      Array.from({length: concurrency}, (_, worker) => sample(1, label, worker)))],
    batches: ['baseline', 'advice'].map(label => ({round: 1, label, concurrency, milliseconds: 2200})),
    final: {operations: {active_views: 0}, bases: [], diagnostic_issues: []}};
}
const profile = r => profileStartup(r, () => jsonl(trace()));

test('keeps reset, status, nested commands and inclusive regions separate', () => {
  const child = command('reset/child', 'status', 0.1, 99);
  const events = trace();
  events.splice(2, 0, ...child); // Interleaved child, not a second final status.
  events.splice(3, 0, event('reset', 'region_leave', {
    category: 'index', label: 'preload', nesting: 1, thread: 'worker', t_rel: 10,
  }));
  const result = profileTrace(jsonl(events));
  assert.equal(result.complete, true);
  assert.equal(result.commands.length, 3);
  assert.equal(result.metrics.gitStarts, 3);
  assert.equal(result.metrics.resetMs, 800); // Not 801 from atexit, or 900 with child.
  assert.equal(result.metrics.resetRefreshMs, 400); // Included in resetMs, not added to it.
  assert.equal(result.metrics.statusMs, 40);
  assert.equal(result.metrics.statusRefreshScan, 0);
  assert.equal(result.metrics.resetRefreshScan, 5864);
  assert.equal(result.metrics.resetPreloadMs, null); // Worker region is not main-thread wall time.
  assert.equal(result.commands.find(c => c.sid === 'reset/child').parentSid, 'reset');
});

test('missing or repeated counters and regions are unknown, never zero or summed', () => {
  const events = trace();
  events.splice(3, 0, event('reset', 'data', {category: 'index', key: 'refresh/sum_scan', value: '5864'}));
  events.splice(3, 0, event('reset', 'region_leave', {category: 'index', label: 'refresh', nesting: 2, t_rel: 0.1}));
  const result = profileTrace(jsonl(events));
  assert.equal(result.metrics.resetRefreshScan, null);
  assert.equal(result.metrics.resetRefreshMs, null);
  assert.equal(result.metrics.statusRefreshLstat, null);
  assert.equal(result.metrics.statusRefreshScan, 0);
  assert.ok(result.unavailable.resetRefreshScan);
  assert.ok(result.unavailable.resetRefreshMs);
});

test('multiple top-level resets/statuses are ambiguous, not silently summed or picked', () => {
  const result = profileTrace(jsonl([...trace(), ...command('extra', 'status', 0.02, 99)]));
  assert.equal(result.metrics.statusMs, null);
  assert.equal(result.metrics.statusRefreshScan, null);
  assert.equal(result.metrics.resetMs, 800);
});

test('replays an unchanged real trace from the first cached production sample', () => {
  // Run 38295860743, prefix-startup/candidate-cached/git-1-baseline-0.jsonl.
  // Production fa6dfed, not the candidate; this is a parser fixture, NOT an
  // accepted comparison or a representative speedup sample. Retained verbatim.
  const bytes = fs.readFileSync(new URL('./fixtures/startup-trace-38295860743.jsonl', import.meta.url));
  assert.equal(createHash('sha256').update(bytes).digest('hex'),
    '4456bb93ac38da9a5aca59b9520efcbd0c4190592620e0770d2e708e32ac67d8');
  const result = profileTrace(bytes.toString());
  assert.equal(result.complete, true);
  assert.equal(result.metrics.gitStarts, 16);
  assert.equal(result.commands.filter(c => c.parentSid !== null).length, 1);
  assert.equal(result.metrics.resetMs, 1.279010 * 1000);
  assert.equal(result.metrics.resetRefreshMs, 1.258457 * 1000);
  assert.equal(result.metrics.resetPreloadMs, 0.009931 * 1000);
  assert.equal(result.metrics.statusMs, 0.047024 * 1000);
  assert.equal(result.metrics.resetRefreshScan, 5864);
  assert.equal(result.metrics.statusRefreshScan, 0);
  assert.equal(result.metrics.statusRefreshLstat, 0);
  assert.equal(result.metrics.statusPreloadLstat, 5864);
});

test('truncated and malformed traces cannot produce a complete profile', () => {
  assert.throws(() => profileTrace(jsonl(trace()).trimEnd()), /newline/);
  assert.throws(() => profileTrace('{broken}\n'), /JSON|position|property/i);
  assert.throws(() => profileTrace('null\n'), /event/);
  assert.throws(() => profileTrace(jsonl([...trace(), event('reset', 'start', {argv: ['git']})])), /duplicate start/);
  const result = profileTrace(jsonl(trace().filter(e => !(e.sid === 'reset' && e.event === 'exit'))));
  assert.equal(result.complete, false);
  assert.equal(result.metrics.resetMs, null);
  assert.ok(result.issues.some(issue => issue.includes('reset')));
  assert.equal(profileTrace('').complete, false);
});

test('invalid numbers and unsuccessful named commands do not become timing evidence', () => {
  for (const value of ['', '-1', '1.2', 'NaN', '9007199254740992']) {
    const events = trace();
    events.find(e => e.sid === 'reset' && e.event === 'data').value = value;
    assert.throws(() => profileTrace(jsonl(events)), /counter/);
  }
  for (const t_abs of [-1, null, '0.8']) {
    const events = trace();
    events.find(e => e.sid === 'reset' && e.event === 'exit').t_abs = t_abs;
    assert.throws(() => profileTrace(jsonl(events)), /duration/);
  }
  const events = trace();
  events.find(e => e.sid === 'reset' && e.event === 'exit').code = 1;
  assert.equal(profileTrace(jsonl(events)).metrics.resetMs, null);
});

test('labels, base reuse and concurrent worker latencies remain separate', () => {
  const result = profile(report(4));
  assert.equal(result.complete, true);
  assert.equal(result.samples.length, 9);
  assert.deepEqual(result.groups.map(g => [g.label, g.reused, g.concurrency, g.samples]), [
    ['baseline', false, 1, 1], ['baseline', true, 4, 4], ['advice', true, 4, 4],
  ]);
  assert.equal(result.groups[1].metrics.totalMs.distribution.p50, 2000);
  assert.equal(result.groups[1].metrics.totalMs.observations, 4);
  assert.equal(result.groups[1].metrics.statusRefreshScan.distribution.p50, 0);
  assert.equal(result.groups[1].metrics.statusRefreshLstat.distribution, null);
  assert.equal(result.groups[1].metrics.statusRefreshLstat.observations, 0);
  assert.equal(result.groups[1].binarySha256, 'a'.repeat(64));
  assert.equal(result.groups[2].binarySha256, 'b'.repeat(64));
  assert.equal(result.speedupClaim, false);
});

test('failed, missing and duplicate attempts suppress all aggregate distributions', () => {
  const mutations = [
    r => { r.complete = false; },
    r => { r.samples[1].success = false; },
    r => { r.samples[1].settled = false; },
    r => { r.samples.pop(); },
    r => { r.samples.push({...r.samples[1]}); },
    r => { r.final.bases = ['retained']; },
    r => { r.samples[1].reused = false; },
    r => { r.batches.pop(); },
  ];
  for (const mutate of mutations) {
    const r = report(); mutate(r);
    const result = profile(r);
    assert.equal(result.complete, false);
    assert.equal(result.samples.length, r.samples.length, 'retain every attempt');
    assert.ok(result.groups.every(g => Object.values(g.metrics).every(m => m.distribution === null)));
  }
  const r = report();
  const result = profileStartup(r, s => {
    if (s.round === 1) throw new Error('missing trace');
    return jsonl(trace());
  });
  assert.equal(result.complete, false);
  assert.equal(result.samples.length, 3);
  assert.ok(result.samples[1].issues.includes('missing trace'));
});

test('read-only CLI uses local trace names, not recorded machine paths or executables', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-startup-profile-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true})); // Only this test-owned fixture.
  const file = path.join(root, 'results.json'), r = report();
  fs.writeFileSync(file, JSON.stringify(r));
  for (const s of r.samples) fs.writeFileSync(path.join(root, `git-${s.round}-${s.label}-${s.worker}.jsonl`), jsonl(trace()));
  const before = fs.readdirSync(root).map(name => [name, fs.readFileSync(path.join(root, name))]);
  const result = readStartupProfile(file);
  assert.equal(result.complete, true);
  assert.match(result.inputSha256, /^[a-f0-9]{64}$/);
  assert.ok(result.samples.every(s => /^[a-f0-9]{64}$/.test(s.traceSha256)));
  const cli = spawnSync(process.execPath, [script, file], {encoding: 'utf8', timeout: 10000});
  assert.equal(cli.status, 0, cli.stderr);
  assert.equal(JSON.parse(cli.stdout).complete, true);
  assert.deepEqual(fs.readdirSync(root).map(name => [name, fs.readFileSync(path.join(root, name))]), before);
  fs.unlinkSync(path.join(root, 'git-1-advice-0.jsonl'));
  const missing = spawnSync(process.execPath, [script, file], {encoding: 'utf8', timeout: 10000});
  assert.equal(missing.status, 1);
  assert.equal(JSON.parse(missing.stdout).samples.length, 3);
  r.samples[1].label = '../outside';
  fs.writeFileSync(file, JSON.stringify(r));
  assert.throws(() => readStartupProfile(file), /label/);
});
