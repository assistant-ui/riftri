// Test-only preload. Inject a failure at the runner's exact receipt path;
// never exhaust the volume or interfere with Git/Riftri's own writes.
import fs from 'node:fs';
import path from 'node:path';

const temporary = path.join(fs.realpathSync(path.dirname(process.env.BENCH_TEST_OUTPUT)),
  path.basename(process.env.BENCH_TEST_OUTPUT), 'results.json.tmp');
const operation = process.env.BENCH_TEST_REPORT_FAULT;
const write = fs.writeFileSync, rename = fs.renameSync;
const fail = () => { throw Object.assign(new Error('ENOSPC: injected receipt persistence failure'), {code: 'ENOSPC'}); };
const failed = report => report.failure || report.error;
fs.writeFileSync = function(file, data, ...args) {
  if (file === temporary || file === temporary.slice(0, -4)) {
    const report = JSON.parse(data);
    if ((operation === 'write' && failed(report)) || (operation === 'complete' && report.complete) ||
        (operation === 'cancel' && (report.cancellation || report.cancelled))) {
      // Model a partial write, not just a failure before opening the file.
      write.call(this, file, '{"injectedPartialWrite":', ...args);
      fail();
    }
  }
  return write.call(this, file, data, ...args);
};
fs.renameSync = function(from, to) {
  if (from === temporary && operation === 'rename' && failed(JSON.parse(fs.readFileSync(from)))) fail();
  return rename.call(this, from, to);
};
