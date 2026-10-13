import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';
import {distribution} from './latency-summary.mjs';

// Offline only: never execute a recorded binary or mutate a measured fixture.
// Trace2 format: https://git-scm.com/docs/api-trace2 (sid, cmd_name, exit,
// region_leave, data). Inclusive regions/child processes are NOT additive.
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const duration = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const indexCounters = new Map([
  ['preload/sum_lstat', 'PreloadLstat'], ['refresh/sum_lstat', 'RefreshLstat'],
  ['refresh/sum_scan', 'RefreshScan'],
]);
const indexRegions = new Map([['preload', 'PreloadMs'], ['refresh', 'RefreshMs']]);
// Recognize only the canonical filter command, never a shell substring or a
// path named "git-lfs". These are observed launches, not a byte/work counter.
const isLfsFilter = child => child.childClass === 'subprocess' && (
  (child.useShell === true && child.argv.length === 1 && child.argv[0] === 'git-lfs filter-process') ||
  (child.useShell === false && child.argv.length === 2 && child.argv[0] === 'git-lfs' && child.argv[1] === 'filter-process'));
const counter = value => {
  assert.ok((typeof value === 'string' && /^\d+$/.test(value)) ||
    (typeof value === 'number' && value >= 0), 'invalid Trace2 counter');
  const number = Number(value);
  assert.ok(Number.isSafeInteger(number) && number >= 0, 'invalid Trace2 counter');
  return number;
};

export function profileTrace(text) {
  assert.equal(typeof text, 'string');
  assert.ok(text === '' || text.endsWith('\n'), 'Trace2 must end with a newline; possible truncation');
  const sessions = new Map();
  const issues = [];
  for (const line of text.split('\n').filter(line => line.trim())) {
    const e = JSON.parse(line);
    assert.ok(e && typeof e.event === 'string' && typeof e.sid === 'string' && e.sid, 'invalid Trace2 event');
    if (!sessions.has(e.sid)) sessions.set(e.sid, {sid: e.sid, argv: null, name: null,
      milliseconds: null, code: null, regions: [], counters: [], children: []});
    const c = sessions.get(e.sid);
    if (e.event === 'start') {
      assert.equal(c.argv, null, `duplicate start: ${e.sid}`);
      assert.ok(Array.isArray(e.argv) && e.argv.length > 0 && e.argv.every(a => typeof a === 'string'), 'invalid argv');
      c.argv = e.argv;
    } else if (e.event === 'cmd_name') {
      assert.ok(typeof e.name === 'string' && e.name, 'invalid command name');
      // Aliases may emit more than one name; retain the final resolved name.
      c.name = e.name;
    } else if (e.event === 'exit') {
      assert.equal(c.milliseconds, null, `duplicate exit: ${e.sid}`);
      assert.ok(duration(e.t_abs), 'invalid Trace2 duration');
      assert.ok(Number.isInteger(e.code), 'invalid exit code');
      c.milliseconds = e.t_abs * 1000;
      c.code = e.code;
    } else if (e.event === 'region_leave' && e.category === 'index' && indexRegions.has(e.label)) {
      assert.ok(duration(e.t_rel), 'invalid Trace2 region duration');
      c.regions.push({thread: e.thread, label: e.label, nesting: e.nesting, milliseconds: e.t_rel * 1000});
    } else if (e.event === 'data' && e.category === 'index' && indexCounters.has(e.key)) {
      c.counters.push({thread: e.thread, key: e.key, value: counter(e.value)});
    } else if (e.event === 'child_start') {
      assert.ok(Number.isSafeInteger(e.child_id) && e.child_id >= 0, 'invalid child id');
      assert.ok(!c.children.some(child => child.id === e.child_id), `duplicate child start: ${e.sid}/${e.child_id}`);
      assert.ok(Array.isArray(e.argv) && e.argv.length && e.argv.every(a => typeof a === 'string'), 'invalid child argv');
      c.children.push({id: e.child_id, argv: e.argv, childClass: e.child_class ?? null,
        useShell: e.use_shell ?? null, milliseconds: null, code: null});
    } else if (e.event === 'child_exit') {
      assert.ok(Number.isSafeInteger(e.child_id) && e.child_id >= 0, 'invalid child id');
      const child = c.children.find(child => child.id === e.child_id);
      if (!child) {
        issues.push(`child exit without start: ${e.sid}/${e.child_id}`);
        continue;
      }
      assert.equal(child.milliseconds, null, `duplicate child exit: ${e.sid}/${e.child_id}`);
      assert.ok(duration(e.t_rel), 'invalid child duration');
      assert.ok(Number.isInteger(e.code), 'invalid child exit code');
      child.milliseconds = e.t_rel * 1000;
      child.code = e.code;
    }
  }
  const commands = [...sessions.values()];
  if (!commands.length) issues.push('no Git sessions');
  for (const c of commands) {
    // Slash-separated sid ancestry, not event order: children can interleave.
    const slash = c.sid.lastIndexOf('/');
    c.parentSid = slash < 0 ? null : c.sid.slice(0, slash);
    if (!c.argv || c.milliseconds === null || !c.name) issues.push(`incomplete Git session: ${c.sid}`);
    for (const child of c.children.filter(isLfsFilter)) {
      if (child.milliseconds === null || child.code !== 0) issues.push(`unfinished or failed LFS filter: ${c.sid}/${child.id}`);
    }
  }
  const metrics = {gitStarts: commands.filter(c => c.argv).length}, unavailable = {};
  function unique(metric, values) {
    metrics[metric] = values.length === 1 ? values[0] : null;
    if (metrics[metric] === null) unavailable[metric] = `expected one observation, found ${values.length}`;
  }
  for (const name of ['reset', 'status']) {
    const matches = commands.filter(c => c.name === name && c.parentSid === null);
    const c = matches.length === 1 && matches[0].code === 0 ? matches[0] : null;
    unique(`${name}Ms`, c && c.milliseconds !== null ? [c.milliseconds] : []);
    for (const [label, suffix] of indexRegions) unique(`${name}${suffix}`,
      c ? c.regions.filter(r => r.thread === 'main' && r.label === label).map(r => r.milliseconds) : []);
    for (const [key, suffix] of indexCounters) unique(`${name}${suffix}`,
      c ? c.counters.filter(r => r.thread === 'main' && r.key === key).map(r => r.value) : []);
    const filters = c?.children.filter(isLfsFilter) ?? [];
    unique(`${name}LfsFilterStarts`, c ? [filters.length] : []);
    // Multiple children may overlap. Keep each lifetime on the command, and
    // only expose a scalar when there is exactly one successfully exited child.
    unique(`${name}LfsFilterMs`, filters.length === 1 && filters[0].code === 0 && filters[0].milliseconds !== null
      ? [filters[0].milliseconds] : []);
    if (matches.length !== 1 || !c) unavailable[`${name}Ms`] = `expected one successful top-level ${name}, found ${matches.length} commands`;
  }
  return {complete: issues.length === 0, issues, commands, metrics, unavailable};
}

// Strictly the supervised startup runner's receipts, not arbitrary Trace2
// collections. readTrace is injected for tests; the CLI resolves only sibling
// filenames derived from validated keys, never paths stored in results.json.
export function profileStartup(report, readTrace) {
  assert.equal(report.schemaVersion, 3, 'unsupported startup schema');
  assert.equal(report.runner, 'supervised-startup-v1', 'unsupported startup runner');
  assert.ok(Number.isInteger(report.rounds) && report.rounds >= 1 && report.rounds <= 20, 'invalid rounds');
  assert.ok(Number.isInteger(report.concurrency) && report.concurrency >= 1 && report.concurrency <= 10, 'invalid concurrency');
  assert.ok(Array.isArray(report.samples), 'missing samples');
  const labels = report.candidate ? ['baseline', 'advice'] : ['baseline'];
  for (const value of [report.binarySha256, ...(report.candidate ? [report.candidateSha256] : [])]) {
    assert.match(value, /^[a-f0-9]{64}$/, 'missing binary identity');
  }
  const issues = [], expected = new Set(['0-baseline-0']), seen = new Set();
  for (let round = 1; round <= report.rounds; round++) for (const label of labels) {
    for (let worker = 0; worker < report.concurrency; worker++) expected.add(`${round}-${label}-${worker}`);
  }
  if (report.complete !== true || report.failure || report.cancellation) issues.push('fixture did not complete');
  const final = report.final;
  if (final?.operations?.active_views !== 0 || !Array.isArray(final?.bases) || final.bases.length ||
    !Array.isArray(final?.diagnostic_issues) || final.diagnostic_issues.length) issues.push('fixture cleanup not verified');
  const batches = new Set();
  for (const b of report.batches ?? []) {
    const key = `${b.round}-${b.label}`;
    if (!Number.isInteger(b.round) || b.round < 1 || b.round > report.rounds || !labels.includes(b.label) ||
      b.concurrency !== report.concurrency || !duration(b.milliseconds) || batches.has(key)) issues.push(`invalid batch: ${key}`);
    batches.add(key);
  }
  if (batches.size !== report.rounds * labels.length) issues.push('missing batches');
  const samples = report.samples.map(s => {
    assert.ok(Number.isInteger(s.round) && s.round >= 0 && s.round <= report.rounds, 'invalid sample round');
    assert.ok(Number.isInteger(s.worker) && s.worker >= 0 && s.worker < report.concurrency, 'invalid sample worker');
    assert.ok(labels.includes(s.label), 'invalid sample label');
    const key = `${s.round}-${s.label}-${s.worker}`, problems = [];
    if (seen.has(key) || !expected.has(key)) problems.push(`duplicate or unexpected attempt: ${key}`);
    seen.add(key);
    if (s.started !== true || s.settled !== true || s.success !== true || s.code !== 0 ||
      s.timedOut !== false || s.cancelled !== false || s.error || s.signal ||
      !duration(s.milliseconds) || s.reused !== (s.round > 0)) problems.push('attempt not successfully verified');
    let trace = null, traceSha256 = null;
    try {
      const text = readTrace(s);
      traceSha256 = hash(text);
      trace = profileTrace(text);
      problems.push(...trace.issues);
      if (s.success === true) {
        // A file cut exactly between commands still has a newline and only
        // complete sessions. Match the runner's separately recorded inventory
        // before treating that prefix (or another attempt's trace) as complete.
        assert.ok(Array.isArray(s.gitCommands), 'missing receipt command inventory');
        assert.deepEqual(trace.commands.map(c => ({argv: c.argv, milliseconds: c.milliseconds, code: c.code})),
          s.gitCommands.map(c => ({argv: c.argv, milliseconds: c.seconds * 1000, code: c.code})),
          'trace differs from receipt command inventory');
      }
    } catch (error) { problems.push(error.message); }
    return {key, round: s.round, label: s.label, worker: s.worker, reused: s.reused ?? null,
      concurrency: s.round === 0 ? 1 : report.concurrency, traceSha256, complete: problems.length === 0,
      issues: problems, metrics: {totalMs: duration(s.milliseconds) ? s.milliseconds : null, ...trace?.metrics},
      unavailable: trace?.unavailable ?? {}, commands: trace?.commands ?? []};
  });
  for (const key of expected) if (!seen.has(key)) issues.push(`missing attempt: ${key}`);
  const complete = issues.length === 0 && samples.every(s => s.complete);
  const groups = new Map();
  for (const s of samples) {
    const key = JSON.stringify([s.label, s.reused, s.concurrency]);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(s);
  }
  return {schemaVersion: 1, complete, speedupClaim: false, issues, git: report.git, tree: report.tree,
    note: 'Offline observations only. Regions are included in command time; child processes may overlap. Do not add these values or infer unmeasured Riftri time. Cold means a fresh base, not a cleared OS cache. Worker distributions are not batch latency or independent replicates.',
    samples, groups: [...groups.values()].map(rows => {
      const {label, reused, concurrency} = rows[0];
      const keys = new Set(rows.flatMap(r => Object.keys(r.metrics)));
      return {label, reused, concurrency, samples: rows.length,
        binarySha256: label === 'baseline' ? report.binarySha256 : report.candidateSha256,
        metrics: Object.fromEntries([...keys].map(key => {
          const values = rows.map(r => r.metrics[key]).filter(v => v !== null && v !== undefined);
          return [key, {observations: values.length, distribution: complete && values.length === rows.length ? distribution(values) : null}];
        }))};
    })};
}

function readRegular(file) {
  assert.ok(fs.lstatSync(file).isFile(), `expected a regular, non-symlink file: ${file}`);
  return fs.readFileSync(file, 'utf8');
}

export function readStartupProfile(file) {
  const text = readRegular(file), directory = path.dirname(file);
  const result = profileStartup(JSON.parse(text), s =>
    readRegular(path.join(directory, `git-${s.round}-${s.label}-${s.worker}.jsonl`)));
  return {inputSha256: hash(text), ...result};
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    const raw = process.argv.length === 4 && process.argv[2] === '--trace';
    assert.ok(raw || process.argv.length === 3, 'usage: node startup-profile.mjs [--trace] /path/to/results.json-or-trace.jsonl');
    const text = raw ? readRegular(process.argv[3]) : null;
    const result = raw ? {schemaVersion: 1, inputSha256: hash(text), speedupClaim: false,
      note: 'Raw trace observations only, not a verified benchmark receipt. Child lifetimes overlap parent time; do not add them or infer bytes processed.',
      ...profileTrace(text)} : readStartupProfile(process.argv[2]);
    console.log(JSON.stringify(result, null, 2));
    if (!result.complete) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
