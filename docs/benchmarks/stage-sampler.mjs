import fs from 'node:fs';
import path from 'node:path';
import { timedProcess } from './timed-process.mjs';

// Diagnostic runs only: sampling perturbs timings. Never enable this when
// collecting headline performance comparisons. A worker supplies its own PID
// through the temporary stage-instrumented build, not process-name discovery.
export async function captureStageStack({ pid, binary, output, key, index }) {
  if (process.platform !== 'darwin') return { skipped:'macOS sample is unavailable' };
  const identity = await timedProcess('/bin/ps', ['-p', String(pid), '-o', 'comm='], { timeoutMs:2000 });
  if (identity.code !== 0 || identity.timedOut) return { skipped:'worker no longer identifiable' };
  let executable;
  try { executable = fs.realpathSync(identity.stdout.trim()); } catch { return { skipped:'worker executable unavailable' }; }
  if (executable !== fs.realpathSync(binary)) return { skipped:'worker executable changed' };
  const file = path.join(output, `stack-${key}-${index}.txt`);
  const result = await timedProcess('/usr/bin/sample', [String(pid), '1', '5', '-mayDie', '-file', file], { timeoutMs:10000 });
  return { pid, file, code:result.code, signal:result.signal, timedOut:result.timedOut, error:result.error, stdout:result.stdout, stderr:result.stderr };
}

export function createStageSampler({ binary, output, key, intervalMs = 15000, capture = captureStageStack }) {
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) throw new TypeError('sampling interval must be positive');
  if (!/^[a-zA-Z0-9-]+$/.test(key)) throw new TypeError('invalid diagnostic worker key');
  let pid, timer, pending, stopped = false;
  const results = [];
  return {
    observe(line) {
      if (stopped || pid !== undefined) return;
      const match = /^riftri: (?:apfs|journal)-stage: start \S+ pid=([1-9][0-9]*)$/.exec(line);
      if (!match || !Number.isSafeInteger(Number(match[1]))) return;
      pid = Number(match[1]);
      timer = setInterval(() => {
        if (stopped || pending) return;
        const index = results.length;
        // Promise boundary also catches a synchronously throwing observer.
        pending = Promise.resolve().then(() => capture({ pid, binary, output, key, index }))
          .then(result => results.push(result), error => results.push({ error:String(error?.message ?? error) }))
          .finally(() => { pending = undefined; });
      }, intervalMs);
    },
    async stop() {
      stopped = true;
      clearInterval(timer);
      // A sample subprocess has its own bounded process-group timeout. Do
      // not leave it running or writing an artifact after its worker settles.
      await pending;
      return [...results];
    },
  };
}
