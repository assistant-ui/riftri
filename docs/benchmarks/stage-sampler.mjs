import fs from 'node:fs';
import path from 'node:path';
import { timedProcess } from './timed-process.mjs';

// Diagnostic runs only: sampling perturbs timings. Never enable this when
// collecting headline performance comparisons. A worker supplies its own PID
// through the temporary stage-instrumented build, not process-name discovery.
// Prefer its direct Git child when present: a parent's poll stack cannot
// explain what Git is waiting for. Never select an unrelated Git process.
export async function captureStageStack({ pid, binary, gitBinary, output, key, index }, {
  platform = process.platform, canonicalize = fs.realpathSync, processRunner = timedProcess,
} = {}) {
  if (platform !== 'darwin') return { skipped:'macOS sample is unavailable' };
  const parse = stdout => stdout.split('\n').flatMap(line => {
    const match = /^\s*([1-9][0-9]*)\s+([0-9]+)\s+(.+?)\s*$/.exec(line);
    if (!match || !Number.isSafeInteger(Number(match[1])) || !Number.isSafeInteger(Number(match[2]))) return [];
    try { return [{ pid:Number(match[1]), ppid:Number(match[2]), binary:canonicalize(match[3]) }]; }
    catch { return []; }
  });
  const identity = async target => {
    const result = await processRunner('/bin/ps', ['-p', String(target), '-o', 'pid=,ppid=,comm='], { timeoutMs:2000 });
    if (result.code !== 0 || result.timedOut) return null;
    const rows = parse(result.stdout);
    return rows.length === 1 && rows[0].pid === target ? rows[0] : null;
  };
  let expected, expectedGit;
  try { expected = canonicalize(binary); expectedGit = gitBinary ? canonicalize(gitBinary) : undefined; }
  catch { return { skipped:'expected executable unavailable' }; }
  const parent = await identity(pid);
  if (!parent) return { skipped:'worker no longer identifiable' };
  if (parent.binary !== expected) return { skipped:'worker executable changed' };
  let target = parent, role = 'riftri';
  if (expectedGit) {
    const listing = await processRunner('/bin/ps', ['-axo', 'pid=,ppid=,comm='], { timeoutMs:2000 });
    if (listing.code !== 0 || listing.timedOut) return { skipped:'child discovery unavailable' };
    const children = parse(listing.stdout).filter(row => row.ppid === pid && row.binary === expectedGit);
    if (children.length > 1) return { skipped:'multiple matching Git children' };
    // Revalidate the parent after discovery, including when falling back to it.
    if ((await identity(pid))?.binary !== expected) return { skipped:'worker changed during child discovery' };
    if (children.length === 1) {
      const child = await identity(children[0].pid);
      if (!child || child.ppid !== pid || child.binary !== expectedGit) return { skipped:'Git child changed before sampling' };
      target = child; role = 'git-child';
    }
  }
  const file = path.join(output, `stack-${key}-${index}.txt`);
  const result = await processRunner('/usr/bin/sample', [String(target.pid), '1', '5', '-mayDie', '-file', file], { timeoutMs:10000 });
  return { pid:target.pid, parentPid:pid, role, binary:target.binary, file,
    code:result.code, signal:result.signal, timedOut:result.timedOut, error:result.error, stdout:result.stdout, stderr:result.stderr };
}

export function createStageSampler({ binary, gitBinary, output, key, intervalMs = 15000, capture = captureStageStack }) {
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
        pending = Promise.resolve().then(() => capture({ pid, binary, gitBinary, output, key, index }))
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
