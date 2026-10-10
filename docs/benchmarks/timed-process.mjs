import { spawn } from 'node:child_process';

// A time wrapper is not the workload: stopping just /usr/bin/time can leave
// Riftri and Git running. POSIX benchmarks own a separate process group and
// finish timeout/cancellation handling before reporting a settled worker.
// This supervises cooperative POSIX cancellation, not an uncatchable SIGKILL
// of the Node owner or descendants that deliberately leave the owned group.
export async function timedProcess(command, args, options = {}) {
  const { timeoutMs = 120000, killGraceMs = 1000, maxBuffer = 128 * 1024 ** 2 } = options;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2147483647 ||
      !Number.isFinite(killGraceMs) || killGraceMs < 0 || killGraceMs > 2147483647) {
    throw new TypeError('timeout and grace must be valid Node timer durations');
  }
  if (!Number.isSafeInteger(maxBuffer) || maxBuffer <= 0) throw new TypeError('positive maxBuffer required');
  const start = performance.now();
  if (options.signal?.aborted) return {code: null, signal: null, stdout: options.encoding === null ? Buffer.alloc(0) : '',
    stderr: options.encoding === null ? Buffer.alloc(0) : '', error: null, timedOut: false,
    cancelled: true, started: false, milliseconds: 0};
  const group = process.platform !== 'win32';
  const child = spawn(command, args, {
    cwd: options.cwd, env: options.env, detached: group, stdio: [options.input === undefined ? 'ignore' : 'pipe', 'pipe', 'pipe'],
  });
  const stdout = [], stderr = [];
  let bytes = 0, error = null;
  let timedOut = false, cancelled = false, terminating = false, terminationFinished = false, closed = null;
  return new Promise((resolve) => {
    const finish = () => {
      if (!closed || (terminating && !terminationFinished)) return;
      options.signal?.removeEventListener('abort', abort);
      const output = chunks => options.encoding === null ? Buffer.concat(chunks) : Buffer.concat(chunks).toString('utf8');
      resolve({ ...closed, stdout: output(stdout), stderr: output(stderr), error, timedOut, cancelled,
        started: Boolean(child.pid), pid: child.pid, milliseconds: performance.now() - start });
    };
    const signal = (name) => {
      if (!child.pid) return;
      try {
        if (group) process.kill(-child.pid, name);
        else child.kill(name);
      } catch (value) {
        if (value.code !== 'ESRCH') error ??= { message: value.message, code: value.code };
      }
    };
    const stop = () => {
      if (terminating) return;
      terminating = true;
      clearTimeout(deadline);
      signal('SIGTERM');
      // Do not cancel this sweep when the wrapper closes: descendants can
      // ignore TERM and close inherited pipes, yet remain in the owned group.
      setTimeout(() => {
        signal('SIGKILL');
        terminationFinished = true;
        finish();
      }, killGraceMs);
    };
    const abort = () => { cancelled = true; stop(); };
    const fail = value => {
      error ??= {message: value.message, code: value.code};
      stop();
    };
    const collect = (chunks, callback) => chunk => {
      bytes += chunk.length;
      if (bytes <= maxBuffer) chunks.push(chunk);
      else fail(Object.assign(new Error('command output exceeds maxBuffer'), {code: 'MAX_BUFFER'}));
      try { callback?.(chunk); } catch (value) { fail(value); }
    };
    const deadline = setTimeout(() => { timedOut = true; stop(); }, timeoutMs);
    child.stdout.on('data', collect(stdout, options.onStdout));
    child.stderr.on('data', collect(stderr, options.onStderr));
    child.on('error', value => { error ??= {message: value.message, code: value.code}; });
    child.stdin?.on('error', value => { if (value.code !== 'EPIPE') fail(value); });
    child.stdin?.end(options.input);
    options.signal?.addEventListener('abort', abort, {once: true});
    if (options.signal?.aborted) abort();
    child.on('close', (code, exitSignal) => {
      clearTimeout(deadline);
      closed = { code, signal: exitSignal };
      finish();
    });
  });
}
