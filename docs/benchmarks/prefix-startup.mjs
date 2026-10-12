// Prospective full-creation gate for the isolated prefix-reuse experiment.
// Uses the unchanged production supervisor and retains every fixture/attempt.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {createProcessScope} from './process-scope.mjs';

const median = values => {
  const sorted=values.toSorted((a,b)=>a-b), middle=Math.floor(sorted.length/2);
  return sorted.length%2 ? sorted[middle] : (sorted[middle-1]+sorted[middle])/2;
};
export function evaluatePairs(pairs, kind, control) {
  assert.ok(['cached','fourway','cold'].includes(kind));
  assert.equal(pairs.length, kind==='cold'?3:8);
  assert.ok(pairs.every(p=>Number.isFinite(p.a)&&p.a>0&&Number.isFinite(p.b)&&p.b>0));
  const aMedianMs=median(pairs.map(p=>p.a)), bMedianMs=median(pairs.map(p=>p.b));
  const ratio=bMedianMs/aMedianMs, paired=median(pairs.map(p=>p.b/p.a));
  const fasterPairs=pairs.filter(p=>p.b<p.a).length, maxTailRatio=Math.max(...pairs.map(p=>p.b/p.a));
  const passed=control ? Math.abs(ratio-1)<=.05&&Math.abs(paired-1)<=.05
    : ratio<=(kind==='cached'?.97:1.05)&&paired<=(kind==='cached'?.97:1.05)
      &&maxTailRatio<=1.25&&(kind!=='cached'||fasterPairs>=6);
  return {aMedianMs,bMedianMs,ratio,paired,fasterPairs,maxTailRatio,passed};
}

async function main() {
  const [baselineArg,candidateArg,referenceArg,outputArg]=process.argv.slice(2);
  const baseline=fs.realpathSync(baselineArg), candidate=fs.realpathSync(candidateArg), reference=fs.realpathSync(referenceArg);
  const output=path.resolve(outputArg);
  assert.ok(!fs.existsSync(output), 'never overwrite a previous attempt');
  fs.mkdirSync(output);
  const sha=file=>createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const report={complete:false,baselineSha256:sha(baseline),candidateSha256:sha(candidate),
    plan:{cachedPairs:8,fourwayPairs:8,coldPairs:3,controlTolerance:.05,cachedMaximumRatio:.97,minimumCachedWins:6,otherMaximumRatio:1.05,maximumTailRatio:1.25},
    note:'All attempts retained. Controls first, no retries or excluded samples. Cold means a new repository/base, not a cleared OS page cache.',runs:[],cases:[]};
  assert.notEqual(report.baselineSha256,report.candidateSha256,'independent production and candidate binaries required');
  const save=()=>fs.writeFileSync(path.join(output,'evaluation.json'),JSON.stringify(report,null,2));
  save();
  const expiresAt=performance.now()+40*60*1000;
  const scope=createProcessScope({budgetMs:40*60*1000,killGraceMs:10000,onCancel:reason=>{report.cancellation=reason;save();}});
  const runner=fileURLToPath(new URL('./apfs-startup.mjs',import.meta.url));
  const run=async(name,first,second,rounds,concurrency)=>{
    const budget=Math.min(8*60*1000,Math.floor(expiresAt-performance.now()-20000));
    assert.ok(budget>=30000,'insufficient remaining budget for another fixture');
    const dir=path.join(output,name), record={name,startedAt:new Date().toISOString()};
    report.runs.push(record);save();
    const log=fs.openSync(path.join(output,`${name}.log`),'wx');
    try {
      record.process=await scope.run(process.execPath,[runner,first,dir,'4096',String(rounds),second,'8192',reference,String(concurrency),String(budget)],
        {timeoutMs:budget+5000,maxBuffer:32*1024**2,onStdout:bytes=>fs.writeSync(log,bytes),onStderr:bytes=>fs.writeSync(log,bytes)});
    } finally {fs.closeSync(log);save();}
    assert.equal(record.process.code,0,record.process.stdout+record.process.stderr);
    assert.equal(record.process.error,null); assert.equal(record.process.timedOut,false); assert.equal(record.process.cancelled,false);
    const result=JSON.parse(fs.readFileSync(path.join(dir,'results.json')));
    assert.equal(result.complete,true); assert.equal(result.files,5864);
    assert.equal(result.sourceCommit,'038cd9f82b418afe9e6d0080648738f78586fbca');
    assert.equal(result.binarySha256,sha(first)); assert.equal(result.candidateSha256,sha(second));
    assert.equal(result.samples.length,1+rounds*2*concurrency);
    assert.ok(result.samples.every(s=>s.success&&s.settled&&!s.timedOut&&!s.cancelled&&s.code===0));
    assert.equal(result.final.operations.active_views,0); assert.deepEqual(result.final.bases,[]); assert.deepEqual(result.final.diagnostic_issues,[]);
    return result;
  };
  try {
    for (const control of [true,false]) {
      const second=control?baseline:candidate, prefix=control?'control':'candidate';
      for(const [kind,concurrency] of [['cached',1],['fourway',4]]) {
        const result=await run(`${prefix}-${kind}`,baseline,second,8,concurrency);
        assert.equal(result.batches.length,16);
        const pairs=Array.from({length:8},(_,i)=>{
          const batch=result.batches.filter(b=>b.round===i+1);
          assert.deepEqual(batch.map(b=>b.label),i%2?['advice','baseline']:['baseline','advice']);
          return {a:batch.find(b=>b.label==='baseline').milliseconds,b:batch.find(b=>b.label==='advice').milliseconds};
        });
        const item={kind,control,pairs,...evaluatePairs(pairs,kind,control)};
        report.cases.push(item);save();console.log(JSON.stringify(item));
      }
      // Each cold observation starts in a fresh repository/state. The runner's
      // extra cached pair is verified and retained, but not scored as cold.
      const pairs=[];
      for(let round=0;round<3;round++) {
        const values={};
        for(const label of round%2?['B','A']:['A','B']) {
          const binary=label==='A'?baseline:second;
          const result=await run(`${prefix}-cold-${round}-${label}`,binary,binary,1,1);
          assert.equal(result.samples[0].round,0); assert.equal(result.samples[0].reused,false);
          values[label.toLowerCase()]=result.samples[0].milliseconds;
        }
        pairs.push(values);
      }
      const item={kind:'cold',control,pairs,...evaluatePairs(pairs,'cold',control)};
      report.cases.push(item);save();console.log(JSON.stringify(item));
    }
    scope.check();report.complete=true;report.qualified=report.cases.every(c=>c.passed);save();
  } catch(error) {report.failure=error.stack;save();throw error;}
  finally {await scope.close();}
}
if (process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) await main();
