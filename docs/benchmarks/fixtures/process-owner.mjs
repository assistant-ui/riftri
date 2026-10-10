import {createProcessScope} from '../process-scope.mjs';

const descendant = `
  process.on('SIGTERM', () => {});
  process.stdout.write('ready');
  setInterval(() => {}, 1000);
`;
// The wrapper exits on TERM and its child closes its inherited pipes. Merely
// waiting for wrapper.close is not proof that its workload has stopped.
const wrapper = `
  const {spawn} = require('node:child_process');
  process.on('SIGTERM', () => process.exit(0));
  const child = spawn(process.execPath, ['-e', ${JSON.stringify(descendant)}], {stdio: ['ignore', 'pipe', 'ignore']});
  child.stdout.once('data', () => process.stderr.write(JSON.stringify({wrapper: process.pid, descendant: child.pid}) + '\\n'));
  setInterval(() => {}, 1000);
`;
const listenersBefore = ['SIGINT', 'SIGTERM', 'SIGHUP'].map(s => process.listenerCount(s));
let reason;
const scope = createProcessScope({budgetMs: Number(process.argv[2]), killGraceMs: 100,
  onCancel(value) { reason = value; }});
try {
  const outcomes = await Promise.all(Array.from({length: 4}, () => {
    let partial = '';
    return scope.run(process.execPath, ['-e', wrapper], {timeoutMs: 10000,
      onStderr(chunk) {
        partial += chunk;
        if (partial.endsWith('\n')) { process.send({ready: JSON.parse(partial)}); partial = ''; }
      }});
  }));
  let refusedNextCommand = false;
  try { await scope.run(process.execPath, ['-e', 'process.exit(0)']); }
  catch { refusedNextCommand = true; }
  await scope.close();
  process.send({outcomes, reason, refusedNextCommand,
    listenersRestored: listenersBefore.every((n, i) => process.listenerCount(['SIGINT', 'SIGTERM', 'SIGHUP'][i]) === n)});
  process.exitCode = reason ? 1 : 0;
} finally {
  await scope.close();
  process.disconnect();
}
