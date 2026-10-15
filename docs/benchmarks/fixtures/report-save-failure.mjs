// Test-only preload. Inject a failure at the runner's exact receipt path;
// never exhaust the volume or interfere with Git/Riftri's own writes.
import fs from 'node:fs';
import path from 'node:path';

const temporary = path.join(fs.realpathSync(path.dirname(process.env.BENCH_TEST_OUTPUT)),
  path.basename(process.env.BENCH_TEST_OUTPUT), 'results.json.tmp');
const operation = process.env.BENCH_TEST_REPORT_FAULT;
const write = fs.writeFileSync, rename = fs.renameSync;
const fail = () => { throw Object.assign(new Error('ENOSPC: injected receipt persistence failure'), {code: 'ENOSPC'}); };
fs.writeFileSync = function(file, data, ...args) {
  if (file === temporary) {
    const report = JSON.parse(data);
    if ((operation === 'write' && report.failure) || (operation === 'complete' && report.complete)) fail();
  }
  return write.call(this, file, data, ...args);
};
fs.renameSync = function(from, to) {
  if (from === temporary && operation === 'rename' && JSON.parse(fs.readFileSync(from)).failure) fail();
  return rename.call(this, from, to);
};
