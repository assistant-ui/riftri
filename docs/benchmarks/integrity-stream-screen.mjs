// Experimental phase-only screen. No result here establishes a startup win.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {createProcessScope} from './process-scope.mjs';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const median = values => { const sorted = [...values].sort((a, b) => a - b); return (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2; };
const [mode, ...args] = process.argv.slice(2);
if (mode === 'freeze') {
  const [buildLog, output, checkout] = args;
  const artifacts = fs.readFileSync(buildLog, 'utf8').trim().split('\n').map(JSON.parse);
  const tests = artifacts.filter(a => a.executable && a.target?.name === 'riftri_core' && a.profile?.test);
  assert.equal(tests.length, 1);
  assert.equal(tests[0].profile.opt_level, '3');
  fs.copyFileSync(tests[0].executable, output, fs.constants.COPYFILE_EXCL);
  fs.writeFileSync(`${output}.json`, JSON.stringify({
    commit: execFileSync('git', ['rev-parse', 'HEAD'], {cwd: checkout, encoding: 'utf8'}).trim(),
    binarySha256: sha(fs.readFileSync(output)),
    rustc: execFileSync('rustc', ['-Vv'], {encoding: 'utf8'}),
  }, null, 2), {flag: 'wx'});
} else {
  assert.equal(mode, 'measure');
  const [previous, candidate, reference, output] = args.map(p => path.resolve(p));
  assert.equal(process.platform, 'darwin');
  assert.ok(!fs.existsSync(output), 'never overwrite or retry a measured run');
  fs.mkdirSync(output);
  const identity = binary => {
    const record = JSON.parse(fs.readFileSync(`${binary}.json`));
    assert.equal(record.binarySha256, sha(fs.readFileSync(binary)));
    return record;
  };
  const plan = {
    pairs: 8, minimumFreeGiB: 8, budgetMs: 12 * 60 * 1000,
    order: ['control-reference', 'control-small', 'candidate-reference', 'candidate-large', 'candidate-small'],
    controlMedianAndPairedTolerance: 0.05,
    candidateReferenceAndLargeMaximumRatio: 0.85, minimumWins: 6,
    candidateSmallMaximumRatio: 1.05, maximumPairedTail: 2,
    overallPhaseMinimumSpeedupBeforeStartup: 2.5,
    note: 'Compare to the prior small-file prototype to isolate large-file streaming. The unchanged 2.5x serial-oracle screen is still required before full-startup evaluation. No outlier removal or favorable retries.',
  };
  const report = {complete: false, kind: 'integrity-only screen; not adoption evidence', plan,
    startedAt: new Date().toISOString(), previous: identity(previous), candidate: identity(candidate),
    sources: Object.fromEntries(['integrity-stream-screen.mjs', 'process-scope.mjs', 'timed-process.mjs'].map(name => [name, sha(fs.readFileSync(new URL(name, import.meta.url)))])),
    host: {release: os.release(), cpu: os.cpus()[0].model, memory: os.totalmem()}, cases: []};
  assert.equal(report.previous.commit, 'e3ce4c20d73dd68fa6ac6533b34167022cd2a6a6');
  const save = () => { fs.writeFileSync(`${output}/results.tmp`, JSON.stringify(report, null, 2)); fs.renameSync(`${output}/results.tmp`, `${output}/results.json`); };
  save();
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_') && !k.startsWith('RIFTRI_')));
  Object.assign(env, {PATH: '/usr/bin:/bin:/usr/sbin:/sbin', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null'});
  const scope = createProcessScope({budgetMs: plan.budgetMs, onCancel: reason => {report.cancellation = reason; save();}});
  const run = async (command, argv, options = {}) => {
    const result = await scope.run(command, argv, {env, ...options});
    assert.ok(result.code === 0 && !result.error && !result.cancelled && !result.timedOut, JSON.stringify(result));
    return result;
  };
  const preflight = () => {
    const stat = fs.statfsSync(output);
    assert.equal(stat.type, 26, 'APFS required');
    assert.ok(stat.bavail * stat.bsize >= plan.minimumFreeGiB * 1024 ** 3);
    return {time: new Date().toISOString(), freeBytes: stat.bavail * stat.bsize, freeMemory: os.freemem(), load: os.loadavg()};
  };
  try {
    report.preflight = preflight();
    const small = `${output}/small`, large = `${output}/large`, real = `${output}/reference`;
    for (const dir of [small, large, real]) fs.mkdirSync(dir);
    for (let d = 0; d < 96; d++) {
      const dir = `${small}/directory-${String(d).padStart(3, '0')}`;
      fs.mkdirSync(dir);
      for (let f = 0; f < 64; f++) fs.writeFileSync(`${dir}/file-${String(f).padStart(3, '0')}`, Buffer.alloc(12 * 1024, f));
    }
    for (let f = 0; f < 64; f++) fs.writeFileSync(`${large}/file-${String(f).padStart(3, '0')}`, Buffer.alloc(1024 * 1024 + f, f));
    const sourceCommit = '038cd9f82b418afe9e6d0080648738f78586fbca';
    assert.equal((await run('/usr/bin/git', ['-C', reference, 'rev-parse', 'HEAD'])).stdout.trim(), sourceCommit);
    assert.equal((await run('/usr/bin/git', ['-C', reference, 'rev-parse', 'HEAD^{tree}'])).stdout.trim(), 'c4de7922b24126e860cb77652f5d080e04c8c396');
    const archive = await run('/usr/bin/git', ['-C', reference, 'archive', '--format=tar', sourceCommit], {encoding: null});
    report.referenceArchiveSha256 = sha(archive.stdout);
    await run('/usr/bin/tar', ['-xf', '-', '-C', real], {input: archive.stdout});
    const manifests = {};
    for (const [name, root] of Object.entries({small, large, reference: real})) {
      const entries = [];
      const walk = dir => { for (const name of fs.readdirSync(dir).sort()) {
        const file = path.join(dir, name), stat = fs.lstatSync(file);
        if (stat.isDirectory()) walk(file);
        else if (stat.isSymbolicLink()) entries.push({path: path.relative(root, file), link: fs.readlinkSync(file)});
        else { assert.ok(stat.isFile()); entries.push({path: path.relative(root, file), bytes: stat.size, mode: stat.mode & 0o777, sha256: sha(fs.readFileSync(file))}); }
      }};
      walk(root); manifests[name] = entries;
    }
    assert.equal(manifests.reference.length, 5864);
    assert.equal(manifests.reference.reduce((n, e) => n + (e.bytes ?? 0), 0), 73235115);
    fs.writeFileSync(`${output}/manifests.json`, JSON.stringify(manifests));
    report.manifestSha256 = sha(JSON.stringify(manifests)); save();
    for (const name of plan.order) {
      const control = name.startsWith('control');
      const fixture = name.endsWith('reference') ? real : name.endsWith('small') ? small : large;
      const record = {name, complete: false, samples: [], pairs: []};
      report.cases.push(record); save();
      for (let round = 0; round < plan.pairs; round++) {
        for (const label of (round % 2 === 0 ? ['A', 'B'] : ['B', 'A'])) {
          const sample = {round, label, preflight: preflight(), attemptedAt: new Date().toISOString()};
          record.samples.push(sample); save();
          const log = fs.openSync(`${output}/${name}-${round}-${label}.log`, 'wx');
          try {
            sample.process = await scope.run(label === 'A' || control ? previous : candidate,
              ['base_integrity::tests::reports_pipelined_integrity_latency', '--exact', '--ignored', '--nocapture', '--test-threads=1'],
              {env: {...env, RIFTRI_INTEGRITY_FIXTURE: fixture}, timeoutMs: 120000,
                onStdout: chunk => fs.writeSync(log, chunk), onStderr: chunk => fs.writeSync(log, chunk)});
          } finally { fs.closeSync(log); save(); }
          const p = sample.process;
          assert.ok(p.code === 0 && !p.error && !p.timedOut && !p.cancelled);
          sample.innerSamples = [...p.stdout.matchAll(/RIFTRI_INTEGRITY_PIPELINE (\{[^\n]+\})/g)].map(m => JSON.parse(m[1]));
          assert.equal(sample.innerSamples.length, 16);
          for (let i = 0; i < 16; i++) {
            const inner = sample.innerSamples[i], r = i >> 1;
            assert.equal(inner.round, r);
            assert.equal(inner.parallel, i % 2 === 0 ? r % 2 !== 0 : r % 2 === 0);
            assert.ok(Number.isFinite(inner.milliseconds) && inner.milliseconds > 0);
          }
          sample.parallelMedian = median(sample.innerSamples.filter(s => s.parallel).map(s => s.milliseconds));
          sample.serialMedian = median(sample.innerSamples.filter(s => !s.parallel).map(s => s.milliseconds));
          save();
        }
        const a = record.samples.find(s => s.round === round && s.label === 'A');
        const b = record.samples.find(s => s.round === round && s.label === 'B');
        record.pairs.push({round, a: a.parallelMedian, b: b.parallelMedian, ratio: b.parallelMedian / a.parallelMedian}); save();
      }
      const ratio = median(record.pairs.map(p => p.b)) / median(record.pairs.map(p => p.a));
      const paired = median(record.pairs.map(p => p.ratio));
      const tail = Math.max(...record.pairs.map(p => p.ratio));
      const wins = record.pairs.filter(p => p.ratio < 1).length;
      const b = record.samples.filter(s => s.label === 'B');
      const speedup = median(b.map(s => s.serialMedian)) / median(b.map(s => s.parallelMedian));
      const limit = name.endsWith('small') ? plan.candidateSmallMaximumRatio : plan.candidateReferenceAndLargeMaximumRatio;
      const passed = control
        ? Math.abs(ratio - 1) <= plan.controlMedianAndPairedTolerance && Math.abs(paired - 1) <= plan.controlMedianAndPairedTolerance && record.pairs.every(p => p.ratio >= 0.5 && p.ratio <= 2)
        : ratio <= limit && paired <= limit && tail <= plan.maximumPairedTail && (name.endsWith('small') || wins >= plan.minimumWins);
      record.evaluation = {medianRatio: ratio, pairedMedianRatio: paired, maximumPairedRatio: tail, wins, serialOracleSpeedup: speedup, passed};
      record.complete = true; save();
      console.log(JSON.stringify({name, ...record.evaluation}));
    }
    scope.check();
    report.complete = true;
    report.largeFileStepQualified = report.cases.every(c => c.evaluation.passed);
    report.proceedToStartup = report.largeFileStepQualified && report.cases.filter(c => ['candidate-reference', 'candidate-small'].includes(c.name)).every(c => c.evaluation.serialOracleSpeedup >= plan.overallPhaseMinimumSpeedupBeforeStartup);
    save();
  } catch (error) { report.failure = error.stack; save(); process.exitCode = 1; console.error(error); }
  finally { await scope.close(); }
}
