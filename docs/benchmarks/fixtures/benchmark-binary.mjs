// Test-only wrapper: exercise cancellation at a known stage while all other
// operations run the real Riftri. These timings are never benchmark evidence.
import fs from 'node:fs';
import {spawn} from 'node:child_process';

const args = process.argv.slice(2);
const stage = process.env.BENCH_TEST_BLOCK;
const block = (stage === 'create' && args[0] === 'worktree' && args[1] === 'add' && !args[2].includes('view-0-baseline-0')) ||
  (stage === 'remove' && args[0] === 'worktree' && args[1] === 'remove');
if (block) {
  process.on('SIGTERM', () => {});
  fs.appendFileSync(process.env.BENCH_TEST_READY, JSON.stringify({pid: process.pid, group: process.ppid}) + '\n');
  process.stderr.write('test workload ready; retaining output before cancellation\n');
  setInterval(() => {}, 1000);
} else {
  const child = spawn(process.env.BENCH_TEST_REAL_BINARY, args, {stdio: 'inherit'});
  child.on('error', error => { console.error(error); process.exitCode = 1; });
  child.on('exit', (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code;
  });
}
