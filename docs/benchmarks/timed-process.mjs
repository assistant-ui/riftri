import { spawn } from 'node:child_process';

// A time wrapper is not the workload: stopping just /usr/bin/time can leave
// Riftri and Git running. POSIX benchmarks own a separate process group and
// finish timeout handling before reporting that a worker has settled.
export async function timedProcess(command, args, options = {}) {
  const { timeoutMs = 120000, killGraceMs = 1000 } = options;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || !Number.isFinite(killGraceMs) || killGraceMs < 0) {
    throw new TypeError('timeout and termination grace must be finite, nonnegative durations');
  }
  const start = performance.now();
  const group = process.platform !== 'win32';
  const child = spawn(command, args, {
    cwd: options.cwd, env: options.env, detached: group, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '', stderr = '', error = null;
  let timedOut = false, terminationFinished = false, closed = null;
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; options.onStderr?.(chunk); });
  child.on('error', (value) => { error = { message: value.message, code: value.code }; });
  return new Promise((resolve) => {
    const finish = () => {
      if (!closed || (timedOut && !terminationFinished)) return;
      resolve({ ...closed, stdout, stderr, error, timedOut, milliseconds: performance.now() - start });
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
    const deadline = setTimeout(() => {
      timedOut = true;
      signal('SIGTERM');
      // Do not cancel this sweep when the wrapper closes: descendants can
      // ignore TERM and close inherited pipes, yet remain in the owned group.
      setTimeout(() => {
        signal('SIGKILL');
        terminationFinished = true;
        finish();
      }, killGraceMs);
    }, timeoutMs);
    child.on('close', (code, exitSignal) => {
      clearTimeout(deadline);
      closed = { code, signal: exitSignal };
      finish();
    });
  });
}
