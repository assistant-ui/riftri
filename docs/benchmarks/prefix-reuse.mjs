// Small, dependency-free screen of the exact production prefix scanner.
// CHECKOUT NEW_OUTPUT REFERENCE_MANIFEST [test-only]. No worktrees are created.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {createProcessScope} from './process-scope.mjs';

const [checkoutArg, outputArg, manifestArg, mode] = process.argv.slice(2);
const checkout = path.resolve(checkoutArg), output = path.resolve(outputArg);
assert.ok(!fs.existsSync(output), 'retain every attempt; output must be new');
fs.mkdirSync(output);
const baselineCommit = 'fa6dfeddc6a8b9ae47d5f140fcaa2fdbad690860';
const sourcePath = 'crates/riftri-core/src/worktree.rs';
const before = execFileSync('git', ['show', `${baselineCommit}:${sourcePath}`], {cwd: checkout, encoding: 'utf8'});
const after = fs.readFileSync(path.join(checkout, sourcePath), 'utf8');
const extract = (source, name) => {
  const match = new RegExp(`^([ \\t]*)fn ${name}\\(`, 'm').exec(source);
  assert.ok(match, name);
  const start = match.index;
  const end = source.indexOf(`\n${match[1]}}\n`, start);
  assert.ok(end > start, name);
  assert.ok(end - start < 5000, 'unexpected extraction boundary');
  return source.slice(start, end + match[1].length + 3);
};
const functions = source => ['has_ascii_case_alias', ...(source.includes('fn has_ascii_case_alias_with(') ? ['has_ascii_case_alias_with'] : []), 'ascii_lowercase_path'].map(name => extract(source, name)).join('\n');
const tests = ['ascii_case_alias_scan_reuses_exact_adjacent_prefixes', 'ascii_case_alias_scan_reuse_matches_original_for_arbitrary_orders', 'ascii_case_alias_scan_matches_replacing_map', 'ascii_case_alias_scan_preserves_non_utf8_path_bytes'];
const baseline = functions(before), candidate = functions(after);
let reference;
if (fs.statSync(manifestArg).isDirectory()) {
  assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], {cwd:manifestArg,encoding:'utf8'}).trim(), '038cd9f82b418afe9e6d0080648738f78586fbca');
  reference = [];
  const walk = dir => { for(const name of fs.readdirSync(dir).sort()) {
    if (dir === manifestArg && name === '.git') continue;
    const file = path.join(dir,name), stat = fs.lstatSync(file);
    if (stat.isDirectory()) walk(file);
    else reference.push(path.relative(manifestArg,file));
  }};
  walk(manifestArg);
} else reference = JSON.parse(fs.readFileSync(manifestArg)).reference.map(entry => entry.path);
assert.equal(reference.length, 5864);
const referencePathsSha256 = createHash('sha256').update(JSON.stringify(reference)).digest('hex');
assert.equal(referencePathsSha256, '477bda79ecff933c8dbca4a70e7f608bdc705b1b9585eb641c50f3913c107dd0');
const fixtures = {
  reference,
  nested: Array.from({length: 20000}, (_, i) => `packages/project-${String(Math.floor(i / 128)).padStart(4, '0')}/src/components/shared/nested/file-${String(i).padStart(6, '0')}`),
  flat: Array.from({length: 20000}, (_, i) => `file-${String(i).padStart(6, '0')}`),
};
for (const [name, paths] of Object.entries(fixtures)) fs.writeFileSync(path.join(output, `${name}.paths`), paths.join('\0') + '\0');
const source = `use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf, Component};
mod baseline { use super::*; ${baseline} pub fn scan(paths: &[PathBuf]) -> bool { has_ascii_case_alias(paths) } }
mod candidate { use super::*; ${candidate} pub fn scan(paths: &[PathBuf]) -> bool { has_ascii_case_alias(paths) }
  #[cfg(test)] mod tests { use super::*; ${tests.map(name => '#[test]\n' + extract(after, name)).join('\n')} }
}
#[test]
#[ignore]
fn reports_prefix_latency() {
  use std::hint::black_box;
  let root = std::env::var_os("PREFIX_FIXTURE").unwrap();
  let paths: Vec<PathBuf> = std::fs::read(root).unwrap().split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(OsString::from_vec(p.to_vec()))).collect();
  assert_eq!(baseline::scan(&paths), candidate::scan(&paths));
  let control = std::env::var_os("PREFIX_CONTROL").is_some();
  for round in 0..12 {
    for label in if round % 2 == 0 { ["A", "B"] } else { ["B", "A"] } {
      let start = std::time::Instant::now();
      for _ in 0..32 {
        let found = if label == "A" || control { baseline::scan(black_box(&paths)) } else { candidate::scan(black_box(&paths)) };
        assert!(!black_box(found));
      }
      println!("PREFIX_SAMPLE {} {} {}", round, label, start.elapsed().as_secs_f64() * 1000.0 / 32.0);
    }
  }
}
`;
fs.writeFileSync(path.join(output, 'screen.rs'), source);
const sha = x => createHash('sha256').update(x).digest('hex');
const report = {complete: false, baselineCommit, referencePathsSha256, baselineSourceSha256: sha(baseline), candidateSourceSha256: sha(candidate),
  sourceSha256: sha(source), rustc: execFileSync('rustc', ['-Vv'], {encoding:'utf8'}),
  host: {release:os.release(), cpu:os.cpus()[0].model, load:os.loadavg(), freeMemory:os.freemem()},
  plan: {rounds: 12, iterations: 32, controlTolerance: 0.05, maximumTargetRatio: 0.75, maximumFlatRatio: 1.05,
    note: 'Phase-only screen, not creation-speed evidence. Fixed case order, every pair retained. No favorable retries.'}, cases: []};
const save = () => fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(report, null, 2));
save();
const scope = createProcessScope({budgetMs:12*60*1000, onCancel:reason=>{report.cancellation=reason;save();}});
const command = async (program, args, options = {}) => {
  const result = await scope.run(program, args, {timeoutMs:240000, maxBuffer:8 * 1024 ** 2, ...options});
  return {...result, status:result.code};
};
try {
  report.build = await command('rustc', ['--edition=2024', '-C', 'opt-level=3', '--test', path.join(output, 'screen.rs'), '-o', path.join(output, 'screen')]); save();
  assert.equal(report.build.status, 0, report.build.stderr);
  report.binarySha256 = sha(fs.readFileSync(path.join(output, 'screen')));
  report.tests = await command(path.join(output, 'screen'), ['--skip', 'reports_prefix_latency']); save();
  assert.equal(report.tests.status, 0, report.tests.stdout + report.tests.stderr);
  if (mode !== 'test-only') {
    const median = xs => { const a=xs.toSorted((a,b)=>a-b); return (a[5]+a[6])/2; };
    for (const [name, control] of [['reference', true], ['nested', true], ['flat', true], ['reference', false], ['nested', false], ['flat', false]]) {
      const env = {...process.env, PREFIX_FIXTURE: path.join(output, `${name}.paths`)};
      delete env.PREFIX_CONTROL;
      if (control) env.PREFIX_CONTROL = '1';
      const item = {name, control, startedAt:new Date().toISOString(), host:{load:os.loadavg(), freeMemory:os.freemem()}};
      report.cases.push(item); save();
      const log = fs.openSync(path.join(output, `${control?'control':'candidate'}-${name}.log`), 'wx');
      try {
        item.process = await command(path.join(output, 'screen'), ['reports_prefix_latency', '--exact', '--ignored', '--nocapture'],
          {env, onStdout:bytes=>fs.writeSync(log,bytes), onStderr:bytes=>fs.writeSync(log,bytes)});
      } finally {fs.closeSync(log); save();}
      assert.equal(item.process.status, 0, item.process.stdout + item.process.stderr);
      assert.equal(item.process.error, null);
      assert.equal(item.process.timedOut, false);
      assert.equal(item.process.cancelled, false);
      item.samples = [...item.process.stdout.matchAll(/PREFIX_SAMPLE (\d+) ([AB]) ([\d.e+-]+)/g)].map(m => ({round:Number(m[1]),label:m[2],ms:Number(m[3])}));
      assert.equal(item.samples.length, 24);
      const pairs = Array.from({length:12}, (_,round) => {
        const rows = item.samples.slice(round*2,round*2+2);
        assert.deepEqual(rows.map(s=>[s.round,s.label]), (round%2===0?['A','B']:['B','A']).map(label=>[round,label]));
        for (const row of rows) assert.ok(Number.isFinite(row.ms) && row.ms>0);
        return {a:rows.find(s=>s.label==='A').ms,b:rows.find(s=>s.label==='B').ms};
      });
      const a=median(pairs.map(p=>p.a)), b=median(pairs.map(p=>p.b)), ratio=b/a, paired=median(pairs.map(p=>p.b/p.a));
      const limit=name==='flat'?1.05:0.75;
      item.evaluation={aMedianMs:a,bMedianMs:b,ratio,paired,passed:control ? Math.abs(ratio-1)<=0.05 && Math.abs(paired-1)<=0.05 : ratio<=limit && paired<=limit}; save();
      console.log(JSON.stringify({name,control,...item.evaluation}));
    }
    report.phaseQualified = report.cases.every(c=>c.evaluation.passed);
  }
  scope.check(); report.complete = true; save();
} catch(error) { report.failure=error.stack; save(); throw error; }
finally {await scope.close();}
