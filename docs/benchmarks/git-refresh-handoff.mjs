import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';

// Mechanism check only: no Riftri startup or latency improvement is measured.
const [output] = process.argv.slice(2);
assert.ok(output && !fs.existsSync(output), 'use a fresh owned fixture path');
fs.mkdirSync(output);
const repo = path.join(output, 'repository');
fs.mkdirSync(repo);
const env = { ...process.env, PATH:'/usr/bin:/bin:/usr/sbin:/sbin' };
for (const key of Object.keys(env)) if (/^(GIT_|RIFTRI_)/.test(key)) delete env[key];
Object.assign(env, { GIT_CONFIG_GLOBAL:'/dev/null', GIT_CONFIG_SYSTEM:'/dev/null', GIT_CONFIG_NOSYSTEM:'1' });
function run(args, extra = {}) {
  const result = spawnSync('/usr/bin/git', args, { cwd:repo, env:{...env,...extra}, timeout:30000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr.toString());
  return result.stdout;
}
run(['init','--quiet','-b','main']);
for (const [key, value] of Object.entries({ 'user.name':'Refresh fixture', 'user.email':'fixture@example.invalid', 'commit.gpgSign':'false', 'core.fsmonitor':'false', 'core.untrackedCache':'false', 'core.autocrlf':'false' })) run(['config','--local',key,value]);
for (let i=0; i<257; i++) {
  const file=path.join(repo,`file-${i}.txt`);
  fs.writeFileSync(file, Buffer.alloc(8192,i%251));
  // Keep this mechanism fixture out of Git's racy-timestamp case without a sleep.
  const old=new Date('2020-01-01T00:00:00Z');
  fs.utimesSync(file,old,old);
}
run(['add','--all']);
run(['commit','--quiet','-m','test: disposable refresh fixture']);
const report = { schemaVersion:1, date:'2026-10-09', scope:'Git refresh-handoff mechanism only; not a Riftri startup benchmark',
  git:run(['--version']).toString().trim(), platform:process.platform, release:os.release(),
  trackedEntries:257, cases:[], complete:false };
const status = ['status','--porcelain=v1','-z','--untracked-files=all'];
for (const optionalLocks of ['0','1']) {
  for (const noRefresh of [false,true]) {
    const key = `locks-${optionalLocks}-no-refresh-${noRefresh}`;
    const index = path.join(output,`${key}.index`);
    const extra = { GIT_INDEX_FILE:index, GIT_OPTIONAL_LOCKS:optionalLocks };
    const observations=[];
    const commands = [['reset','--mixed','--quiet',...(noRefresh?['--no-refresh']:[]),'HEAD'],status,status];
    for (const [step,args] of commands.entries()) {
      const trace=path.join(output,`${key}-${step}.jsonl`);
      assert.equal(run(args,{...extra,GIT_TRACE2_EVENT:trace}).length,0);
      const events=fs.readFileSync(trace,'utf8').trim().split('\n').map(JSON.parse);
      observations.push({step,command:args,indexSha256:createHash('sha256').update(fs.readFileSync(index)).digest('hex'),
        refreshScans:events.filter(e=>e.event==='data'&&e.category==='index'&&e.key==='refresh/sum_scan').map(e=>Number(e.value)),
        lstatCounts:events.filter(e=>e.event==='data'&&e.category==='index'&&e.key==='preload/sum_lstat').map(e=>Number(e.value))});
    }
    const [reset,first,second]=observations;
    assert.deepEqual(reset.refreshScans,noRefresh?[]:[257]);
    assert.deepEqual(first.refreshScans,noRefresh?[257]:[0]);
    assert.deepEqual(second.refreshScans,noRefresh&&optionalLocks==='0'?[257]:[0]);
    assert.equal(first.indexSha256===reset.indexSha256,!(noRefresh&&optionalLocks==='1'));
    assert.equal(first.indexSha256,second.indexSha256);
    report.cases.push({optionalLocks,noRefresh,observations});
  }
}
assert.equal(run(status).length,0);
report.complete=true;
fs.writeFileSync(path.join(output,'results.json'),JSON.stringify(report,null,2)+'\n');
console.log(JSON.stringify(report,null,2));
